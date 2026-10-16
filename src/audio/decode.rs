//! One decoder thread per track: demux/decode with Symphonia, trim encoder
//! delay/padding, map channels to stereo and resample to the engine rate.
//! The thread may block on file or HTTP reads; the audio actor only takes
//! ready PCM, so a stalled stream never blocks transport or output.
use super::{http, mp4, resample::Resampler, Source, CHANNELS, RATE};
use crate::{Error, Result};
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::Duration,
};
use symphonia::core::{
    audio::{Channels, Position},
    codecs::audio::{AudioDecoder, AudioDecoderOptions},
    errors::Error as DecodeError,
    formats::{probe::Hint, FormatOptions, FormatReader, SeekMode, SeekTo, TrackType},
    io::{MediaSource, MediaSourceStream},
    meta::MetadataOptions,
    units::{TimeBase, Timestamp},
};

const DECODE_FAILED: &str = "Audio decoding or streaming failed";
const SEEK_FAILED: &str = "Audio seeking failed";

/// PCM produced by the thread, consumed by the actor.
#[derive(Default)]
struct Produced {
    pcm: VecDeque<f32>,
    /// Frames the actor wants available beyond what it already holds.
    want: usize,
    finished: bool,
    error: Option<&'static str>,
    duration: Option<u64>,
    warnings: u64,
    stop: bool,
}
#[derive(Default)]
struct Shared {
    state: Mutex<Produced>,
    changed: Condvar,
}
impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Produced> {
        crate::mutex_lock(&self.state)
    }
}

pub(super) struct Decoder {
    pub transcoded: bool,
    pub http: Option<http::Reader>,
    pub source: Source,
    pcm: VecDeque<f32>,
    pub eof: bool,
    /// Frames handed to the output (zero for an untouched successor).
    pub frames: u64,
    pub warnings: u64,
    shared: Arc<Shared>,
    worker: Option<thread::JoinHandle<()>>,
}

fn safe_uri(url: &str) -> Result<(String, String)> {
    let mut uri = url::Url::parse(url)?;
    if !matches!(uri.scheme(), "file" | "http" | "https")
        || !uri.username().is_empty()
        || uri.password().is_some()
        || uri.fragment().is_some()
    {
        return Err(Error::Input("Invalid audio source"));
    }
    if uri.scheme() == "file" && !matches!(uri.host_str(), None | Some("") | Some("localhost")) {
        return Err(Error::Input("Invalid audio source"));
    }
    let mut token = String::new();
    let pairs: Vec<_> = uri
        .query_pairs()
        .filter_map(|(k, v)| {
            if k.eq_ignore_ascii_case("X-Plex-Token") {
                token = v.into_owned();
                None
            } else {
                Some((k.into_owned(), v.into_owned()))
            }
        })
        .collect();
    uri.set_query(None);
    if !pairs.is_empty() {
        uri.query_pairs_mut().extend_pairs(pairs);
    }
    Ok((uri.to_string(), token))
}

impl Decoder {
    pub fn new(source: Source, closer: &http::SessionCloser) -> Result<Self> {
        crate::diagnostics::event(format_args!(
            "decoder create occurrence={} source={} resume_ms={} duration_ms={}",
            source.id,
            if source.url.starts_with("file:") {
                "cache/local"
            } else {
                "remote"
            },
            source.resume,
            source.duration
        ));
        let (uri, token) = safe_uri(&source.url)?;
        let (uri, transcoded) = crate::quality::with_offset(&uri, source.resume)?;
        let parsed = url::Url::parse(&uri)?;
        let mut hint = Hint::new();
        if let Some(extension) = parsed
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .and_then(|name| name.rsplit_once('.'))
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .filter(|extension| extension.len() <= 5 && !transcoded)
        {
            hint.with_extension(&extension);
        }
        let (input, http) = if parsed.scheme() == "file" {
            let path = parsed
                .to_file_path()
                .map_err(|_| Error::Input("Invalid audio source"))?;
            (Input::File(path), None)
        } else {
            let (reader, stream) = http::Reader::new(uri, token, closer.clone())?;
            (Input::Http(stream), Some(reader))
        };
        let seek = (source.resume > 0 && !transcoded).then_some(source.resume);
        let shared = Arc::new(Shared::default());
        let worker = {
            let shared = shared.clone();
            let id = source.id;
            thread::Builder::new()
                .name("plexfreq-decode".into())
                .spawn(move || {
                    let result = decode(&shared, input, hint, seek, id);
                    let mut state = shared.lock();
                    state.finished = true;
                    if let Err(message) = result {
                        state.error.get_or_insert(message);
                    }
                    drop(state);
                    shared.changed.notify_all();
                })?
        };
        Ok(Self {
            transcoded,
            http,
            source,
            pcm: VecDeque::new(),
            eof: false,
            frames: 0,
            warnings: 0,
            shared,
            worker: Some(worker),
        })
    }
    /// Collect decoded PCM and ask the thread to keep `frames` ready.
    pub fn fill(&mut self, frames: usize) -> Result<()> {
        let mut state = self.shared.lock();
        if let Some(message) = state.error {
            eprintln!(
                "PlexFreq audio decoder error occurrence={}: {message}",
                self.source.id
            );
            return Err(if message == SEEK_FAILED {
                Error::Input("Audio seeking failed")
            } else {
                Error::Input("Audio decoding or streaming failed")
            });
        }
        if let Some(duration) = state.duration.filter(|_| !self.transcoded) {
            self.source.duration = duration;
        }
        self.warnings = state.warnings;
        let missing = (frames * CHANNELS).saturating_sub(self.pcm.len());
        if !state.pcm.is_empty() && missing > 0 {
            let take = missing.min(state.pcm.len());
            self.pcm.extend(state.pcm.drain(..take));
        }
        let want = frames.saturating_sub(self.pcm.len() / CHANNELS);
        let wake = want > state.pcm.len() / CHANNELS && state.want != want;
        state.want = want;
        self.eof = state.finished && state.pcm.is_empty();
        drop(state);
        if wake {
            self.shared.changed.notify_all();
        }
        Ok(())
    }
    pub fn take(&mut self, frames: usize) -> Vec<f32> {
        let count = (frames * CHANNELS).min(self.pcm.len());
        self.frames += count as u64 / CHANNELS as u64;
        self.pcm.drain(..count).collect()
    }
    pub fn remaining(&self) -> usize {
        self.pcm.len() / CHANNELS
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        self.shared.lock().stop = true;
        self.shared.changed.notify_all();
        if let Some(reader) = &self.http {
            // Releases a thread blocked on network bytes.
            reader.stop();
        }
        if let Some(worker) = self.worker.take() {
            // Bounded: every blocking point re-checks `stop` at least every
            // 100 ms, and one packet decode takes milliseconds.
            let _ = worker.join();
        }
    }
}

enum Input {
    File(std::path::PathBuf),
    Http(http::Source),
}

/// Track timestamps to engine-independent frame positions at `rate`.
#[derive(Clone, Copy)]
struct Timeline {
    base: Option<TimeBase>,
    rate: u32,
}
impl Timeline {
    fn frames(&self, ts: i64) -> i64 {
        match self.base {
            Some(base) => (i128::from(ts) * i128::from(base.numer.get()) * i128::from(self.rate)
                / i128::from(base.denom.get()))
            .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64,
            None => ts,
        }
    }
    fn timestamp(&self, frames: i64) -> i64 {
        match self.base {
            Some(base) => (i128::from(frames) * i128::from(base.denom.get())
                / (i128::from(base.numer.get()) * i128::from(self.rate.max(1))))
            .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64,
            None => frames,
        }
    }
}

/// Stereo downmix weights for one source channel (left, right).
fn weights(position: Position) -> (f32, f32) {
    const H: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let left = Position::FRONT_LEFT
        | Position::FRONT_LEFT_CENTER
        | Position::FRONT_LEFT_WIDE
        | Position::TOP_FRONT_LEFT
        | Position::BOTTOM_FRONT_LEFT;
    let right = Position::FRONT_RIGHT
        | Position::FRONT_RIGHT_CENTER
        | Position::FRONT_RIGHT_WIDE
        | Position::TOP_FRONT_RIGHT
        | Position::BOTTOM_FRONT_RIGHT;
    let surround_left = Position::REAR_LEFT
        | Position::SIDE_LEFT
        | Position::TOP_REAR_LEFT
        | Position::TOP_SIDE_LEFT;
    let surround_right = Position::REAR_RIGHT
        | Position::SIDE_RIGHT
        | Position::TOP_REAR_RIGHT
        | Position::TOP_SIDE_RIGHT;
    if position.intersects(left) {
        (1., 0.)
    } else if position.intersects(right) {
        (0., 1.)
    } else if position.intersects(surround_left) {
        (H, 0.)
    } else if position.intersects(surround_right) {
        (0., H)
    } else if position.intersects(Position::LFE1 | Position::LFE2) {
        (0., 0.)
    } else {
        // Centre channels (front, rear, top, bottom).
        (H, H)
    }
}

/// Per-channel (left, right) gains mapping `channels` to stereo. Mono is
/// duplicated at full level; more than two channels are downmixed with
/// ITU-style weights normalized so the mix cannot exceed full scale.
fn stereo_matrix(channels: &Channels, count: usize) -> Vec<(f32, f32)> {
    match count {
        0 => Vec::new(),
        1 => vec![(1., 1.)],
        2 => vec![(1., 0.), (0., 1.)],
        _ => {
            let positions: Vec<Position> = match channels {
                Channels::Positioned(set) => set.iter().collect(),
                _ => Vec::new(),
            };
            let positions = if positions.len() == count {
                positions
            } else {
                // Unpositioned channels follow the WAVE default order.
                Position::from_wave_channel_count(count as u32)
                    .map(|set| set.iter().collect())
                    .unwrap_or_default()
            };
            let mut matrix: Vec<(f32, f32)> = if positions.len() == count {
                positions.into_iter().map(weights).collect()
            } else {
                (0..count)
                    .map(|n| match n {
                        0 => (1., 0.),
                        1 => (0., 1.),
                        _ => (0., 0.),
                    })
                    .collect()
            };
            let left: f32 = matrix.iter().map(|w| w.0).sum();
            let right: f32 = matrix.iter().map(|w| w.1).sum();
            for weight in &mut matrix {
                weight.0 /= left.max(1.);
                weight.1 /= right.max(1.);
            }
            matrix
        }
    }
}

fn to_stereo(interleaved: &[f32], matrix: &[(f32, f32)], out: &mut Vec<f32>) {
    let count = matrix.len();
    if count == 0 {
        return;
    }
    out.reserve(interleaved.len() / count * CHANNELS);
    for frame in interleaved.chunks_exact(count) {
        let (mut left, mut right) = (0f32, 0f32);
        for (sample, weight) in frame.iter().zip(matrix) {
            left += sample * weight.0;
            right += sample * weight.1;
        }
        out.push(left);
        out.push(right);
    }
}

struct Stage {
    resampler: Option<Resampler>,
    rate: u32,
    matrix: Vec<(f32, f32)>,
    channels: usize,
    stereo: Vec<f32>,
}
impl Stage {
    fn new() -> Self {
        Self {
            resampler: None,
            rate: 0,
            matrix: Vec::new(),
            channels: 0,
            stereo: Vec::new(),
        }
    }
    /// Convert interleaved source frames, appending engine-rate stereo PCM.
    fn push(&mut self, rate: u32, channels: &Channels, samples: &[f32], out: &mut Vec<f32>) {
        let count = channels.count();
        if count == 0 || rate == 0 {
            return;
        }
        if rate != self.rate {
            self.finish(out);
            self.rate = rate;
            self.resampler = (rate != RATE).then(|| Resampler::new(rate, RATE)).flatten();
        }
        if count != self.channels {
            self.channels = count;
            self.matrix = stereo_matrix(channels, count);
        }
        self.stereo.clear();
        to_stereo(samples, &self.matrix, &mut self.stereo);
        match &mut self.resampler {
            Some(resampler) => resampler.process(&self.stereo, out),
            None => out.extend_from_slice(&self.stereo),
        }
    }
    fn finish(&mut self, out: &mut Vec<f32>) {
        if let Some(resampler) = &mut self.resampler {
            resampler.finish(out);
        }
        self.resampler = None;
        self.rate = 0;
    }
}

/// The demuxer's byte source: a local file or the HTTP window.
enum Media {
    File(std::fs::File),
    Http(http::Source),
}
impl std::io::Read for Media {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::File(file) => file.read(out),
            Self::Http(stream) => stream.read(out),
        }
    }
}
impl std::io::Seek for Media {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        match self {
            Self::File(file) => file.seek(to),
            Self::Http(stream) => stream.seek(to),
        }
    }
}
impl MediaSource for Media {
    fn is_seekable(&self) -> bool {
        match self {
            Self::File(file) => file.is_seekable(),
            Self::Http(stream) => stream.is_seekable(),
        }
    }
    fn byte_len(&self) -> Option<u64> {
        match self {
            Self::File(file) => file.byte_len(),
            Self::Http(stream) => stream.byte_len(),
        }
    }
}

fn open(input: Input) -> std::result::Result<Media, &'static str> {
    Ok(match input {
        Input::File(path) => Media::File(std::fs::File::open(path).map_err(|_| DECODE_FAILED)?),
        Input::Http(stream) => Media::Http(stream),
    })
}

/// Selected track id, its decoder and timeline, and its frame count if known.
type Opened = (u32, Box<dyn AudioDecoder>, Timeline, Option<u64>);

fn make_decoder(format: &dyn FormatReader) -> std::result::Result<Opened, &'static str> {
    let registry = symphonia::default::get_codecs();
    let track = format
        .tracks()
        .iter()
        .find(|track| {
            track
                .codec_params
                .as_ref()
                .and_then(|params| params.audio())
                .is_some_and(|params| registry.get_audio_decoder(params.codec).is_some())
        })
        .or_else(|| format.default_track(TrackType::Audio))
        .ok_or(DECODE_FAILED)?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|params| params.audio())
        .ok_or(DECODE_FAILED)?;
    let decoder = registry
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|error| {
            eprintln!("PlexFreq audio codec unavailable: {error}");
            DECODE_FAILED
        })?;
    let rate = params.sample_rate.unwrap_or(RATE);
    let timeline = Timeline {
        base: track.time_base,
        rate,
    };
    let frames = track.num_frames.or_else(|| {
        track
            .duration
            .map(|d| timeline.frames(d.get() as i64).max(0) as u64)
    });
    Ok((track.id, decoder, timeline, frames))
}

fn decode(
    shared: &Shared,
    input: Input,
    hint: Hint,
    seek: Option<u64>,
    id: u64,
) -> std::result::Result<(), &'static str> {
    let (tap, moov) = mp4::Tap::new(open(input)?);
    let stream = MediaSourceStream::new(Box::new(tap), Default::default());
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|error| {
            eprintln!("PlexFreq audio probe failed occurrence={id}: {error}");
            DECODE_FAILED
        })?;
    let (mut track, mut decoder, mut timeline, frames) = make_decoder(format.as_ref())?;
    // Symphonia's MP4 demuxer does not apply edit lists or iTunSMPB.
    let gapless = (format.format_info().short_name == "isomp4")
        .then(|| crate::mutex_lock(&moov).take())
        .flatten()
        .and_then(|moov| mp4::gapless(&moov, timeline.rate));
    let delay = gapless.map_or(0, |g| g.delay as i64);
    let end = gapless
        .and_then(|g| g.valid)
        .map(|valid| delay + valid as i64);
    let valid = match gapless {
        Some(g) => g.valid.or(frames.map(|f| f.saturating_sub(g.delay))),
        None => frames,
    };
    if let Some(valid) = valid.filter(|v| *v > 0) {
        shared.lock().duration = Some(valid * 1000 / u64::from(timeline.rate.max(1)));
    }
    crate::diagnostics::event(format_args!(
        "decoder opened occurrence={id} format={} rate={} gapless={gapless:?}",
        format.format_info().short_name,
        timeline.rate
    ));
    // Frames (in the source timeline) before this position are not output.
    let mut start = delay;
    if let Some(milliseconds) = seek {
        let target = delay + (u128::from(milliseconds) * u128::from(timeline.rate) / 1000) as i64;
        let seeked = format
            .seek(
                SeekMode::Accurate,
                SeekTo::Timestamp {
                    ts: Timestamp::new(timeline.timestamp(target)),
                    track_id: track,
                },
            )
            .map_err(|error| {
                eprintln!("PlexFreq audio seek failed occurrence={id}: {error}");
                SEEK_FAILED
            })?;
        decoder.reset();
        start = timeline.frames(seeked.required_ts.get()).max(target);
    }
    let mut stage = Stage::new();
    let mut converted = Vec::new();
    let mut samples: Vec<f32> = Vec::new();
    let mut warnings = 0u64;
    loop {
        {
            let mut state = shared.lock();
            while !state.stop && state.pcm.len() / CHANNELS >= state.want.max(1) {
                state = shared
                    .changed
                    .wait_timeout(state, Duration::from_millis(100))
                    .map(|(guard, _)| guard)
                    .unwrap_or_else(|poison| poison.into_inner().0);
            }
            if state.stop {
                return Ok(());
            }
        }
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(DecodeError::ResetRequired) => {
                // A chained Ogg stream: continue with its new logical stream.
                let next = make_decoder(format.as_ref())?;
                (track, decoder, timeline) = (next.0, next.1, next.2);
                start = 0;
                continue;
            }
            Err(DecodeError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(DecodeError::DecodeError(_)) => {
                warnings += 1;
                shared.lock().warnings = warnings;
                continue;
            }
            Err(error) => {
                eprintln!("PlexFreq audio demux failed occurrence={id}: {error}");
                return Err(DECODE_FAILED);
            }
        };
        if packet.track_id != track {
            continue;
        }
        let buffer = match decoder.decode(&packet) {
            Ok(buffer) => buffer,
            Err(DecodeError::DecodeError(message)) => {
                warnings += 1;
                if warnings <= 10 || warnings.is_power_of_two() {
                    crate::diagnostics::event(format_args!(
                        "decoder warning occurrence={id} count={warnings}: {message}"
                    ));
                }
                shared.lock().warnings = warnings;
                continue;
            }
            Err(error) => {
                eprintln!("PlexFreq audio decode failed occurrence={id}: {error}");
                return Err(DECODE_FAILED);
            }
        };
        let decoded = buffer.frames();
        if decoded == 0 {
            continue;
        }
        let spec = buffer.spec().clone();
        samples.clear();
        buffer.copy_to_vec_interleaved(&mut samples);
        let count = spec.channels().count().max(1);
        // Packet trims mark delay/padding. Some decoders (MP3, Vorbis) apply
        // them; otherwise the decoded block still contains those frames.
        let trim_start = timeline.frames(packet.trim_start.get() as i64).max(0) as usize;
        let trim_end = timeline.frames(packet.trim_end.get() as i64).max(0) as usize;
        let duration = timeline.frames(packet.dur.get() as i64).max(0) as usize;
        let (mut first, mut last) = (0usize, decoded);
        if trim_start + trim_end > 0 && decoded > duration {
            first = trim_start.min(decoded);
            last = decoded.saturating_sub(trim_end).max(first);
        }
        if duration > 0 && last - first > duration {
            // A shortened final sample (e.g. MP4 stts) marks encoder padding.
            last = first + duration;
        }
        let position = timeline.frames(packet.pts.get()) + trim_start as i64;
        let lower = (start - position).clamp(0, (last - first) as i64) as usize;
        let upper = match end {
            Some(end) => (end - position).clamp(lower as i64, (last - first) as i64) as usize,
            None => last - first,
        };
        if upper > lower {
            converted.clear();
            stage.push(
                spec.rate(),
                spec.channels(),
                &samples[(first + lower) * count..(first + upper) * count],
                &mut converted,
            );
            if !converted.is_empty() {
                shared.lock().pcm.extend(converted.iter().copied());
            }
        }
        if end.is_some_and(|end| position + (last - first) as i64 >= end) {
            break;
        }
    }
    converted.clear();
    stage.finish(&mut converted);
    shared.lock().pcm.extend(converted.iter().copied());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_maps_duplicate_mono_and_bound_surround_downmix() {
        let mut out = Vec::new();
        let mono = Channels::Positioned(Position::FRONT_CENTER);
        to_stereo(&[0.5, -0.25], &stereo_matrix(&mono, 1), &mut out);
        assert_eq!(out, vec![0.5, 0.5, -0.25, -0.25]);
        let surround = Channels::Positioned(
            Position::FRONT_LEFT
                | Position::FRONT_RIGHT
                | Position::FRONT_CENTER
                | Position::LFE1
                | Position::REAR_LEFT
                | Position::REAR_RIGHT,
        );
        let matrix = stereo_matrix(&surround, 6);
        out.clear();
        // Full-scale on every channel never exceeds full scale after mixing.
        to_stereo(&[1.; 6], &matrix, &mut out);
        assert!(out.iter().all(|x| (*x - 1.).abs() < 1e-6), "{out:?}");
        out.clear();
        // Front left only reaches the left output; LFE is dropped.
        to_stereo(&[1., 0., 0., 1., 0., 0.], &matrix, &mut out);
        assert!(out[0] > 0.3 && out[1] == 0., "{out:?}");
        // Unpositioned channels follow WAVE order.
        let matrix = stereo_matrix(&Channels::Discrete(4), 4);
        out.clear();
        to_stereo(&[0., 1., 0., 0.], &matrix, &mut out);
        assert!(out[0] == 0. && out[1] > 0., "{out:?}");
    }

    #[test]
    fn framework_uri_never_contains_token() {
        let (uri, token) =
            safe_uri("https://fixture.invalid/audio?x=1&X-Plex-Token=fixture-secret").unwrap();
        assert_eq!(token, "fixture-secret");
        assert_eq!(uri, "https://fixture.invalid/audio?x=1");
    }
}
