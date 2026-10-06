pub mod dsp;
mod http;
use crate::{Error, Result};
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as app;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::{mpsc, Arc, Mutex},
    thread,
    time::Duration,
};

pub const RATE: u32 = 48000;
const CHANNELS: usize = 2;
const BLOCK: usize = 512;
/// Maximum crossfade tail: 12s at 48kHz stereo.
const MAX_TAIL_FRAMES: usize = RATE as usize * 12;
/// Appsrc queue cap (~0.25s of F32 stereo) and backpressure watermark (~0.2s).
const OUTPUT_MAX_BYTES: u64 = RATE as u64 * 8 / 4;
const OUTPUT_BACKPRESSURE_BYTES: u64 = RATE as u64 * 8 / 5;

fn lock_state(state: &Arc<Mutex<Snapshot>>) -> std::sync::MutexGuard<'_, Snapshot> {
    crate::mutex_lock(state)
}

fn lock_capture(capture: &Arc<Mutex<Vec<f32>>>) -> std::sync::MutexGuard<'_, Vec<f32>> {
    crate::mutex_lock(capture)
}

#[derive(Clone, Default)]
pub struct Source {
    pub id: u64,
    pub url: String,
    pub duration: u64,
    pub resume: u64,
    pub listened: u64,
    pub gain: f32,
    pub album_gain: Option<f32>,
    pub album_normalization: bool,
    pub album: String,
}
impl Source {
    fn normalization_gain(&self, mode: dsp::NormalizationMode) -> f32 {
        match mode {
            dsp::NormalizationMode::Track => self.gain,
            dsp::NormalizationMode::Album => self.album_gain.unwrap_or(self.gain),
            dsp::NormalizationMode::Auto => {
                if self.album_normalization {
                    self.album_gain.unwrap_or(self.gain)
                } else {
                    self.gain
                }
            }
        }
    }
}
#[derive(Clone, Default, Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub id: u64,
    pub playing: bool,
    pub paused: bool,
    pub buffering: bool,
    pub loaded: bool,
    pub position: u64,
    pub duration: u64,
    pub listened: u64,
    pub volume: f64,
    pub seekable: bool,
    pub error: String,
}
#[derive(Debug)]
pub enum Event {
    Transition {
        old: u64,
        new: u64,
        listened: u64,
        position: u64,
    },
    End {
        id: u64,
        listened: u64,
        position: u64,
    },
    Error {
        id: u64,
        message: &'static str,
    },
    Seeked(u64),
}
enum Control {
    Load(Source, bool),
    Prepare(Option<Source>),
    Play,
    Pause,
    Stop,
    Seek(u64),
    Volume(f64),
    Configure(dsp::Config),
    Shutdown,
}

#[derive(Clone, Copy)]
pub enum Sink {
    Pulse,
    Fake,
    Capture,
}
#[derive(Clone)]
pub struct Handle {
    tx: mpsc::SyncSender<Control>,
    state: Arc<Mutex<Snapshot>>,
    capture: Arc<Mutex<Vec<f32>>>,
}
impl Handle {
    fn send(&self, control: Control) -> Result<()> {
        self.tx
            .try_send(control)
            .map_err(|_| Error::Input("Audio command queue is unavailable"))
    }
    pub fn load(&self, source: Source, paused: bool) -> Result<()> {
        self.send(Control::Load(source, paused))
    }
    pub fn prepare(&self, source: Option<Source>) -> Result<()> {
        self.send(Control::Prepare(source))
    }
    pub fn play(&self) -> Result<()> {
        self.send(Control::Play)
    }
    pub fn pause(&self) -> Result<()> {
        self.send(Control::Pause)
    }
    pub fn stop(&self) -> Result<()> {
        self.send(Control::Stop)
    }
    pub fn seek(&self, position: u64) -> Result<()> {
        self.send(Control::Seek(position))
    }
    pub fn volume(&self, value: f64) -> Result<()> {
        if !value.is_finite() {
            return Err(Error::Input("Invalid audio volume"));
        }
        self.send(Control::Volume(value.clamp(0., 1.)))
    }
    pub fn configure(&self, config: dsp::Config) -> Result<()> {
        config.validate()?;
        self.send(Control::Configure(config))
    }
    pub fn snapshot(&self) -> Snapshot {
        lock_state(&self.state).clone()
    }
    pub fn captured(&self) -> Vec<f32> {
        lock_capture(&self.capture).clone()
    }
}
pub struct Engine {
    pub handle: Handle,
    events: mpsc::Receiver<Event>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Engine {
    pub fn new(sink: Sink) -> Result<Self> {
        gst::init().map_err(|_| Error::Input("Audio framework initialization failed"))?;
        let mut factories = vec![
            "uridecodebin",
            "decodebin",
            "audioconvert",
            "audioresample",
            "appsrc",
            "appsink",
        ];
        factories.push(match sink {
            Sink::Pulse => "pulsesink",
            Sink::Fake => "fakesink",
            Sink::Capture => "appsink",
        });
        for factory in factories {
            if gst::ElementFactory::find(factory).is_none() {
                return Err(Error::Input("Required audio plugins are unavailable"));
            }
        }
        let (tx, rx) = mpsc::sync_channel(128);
        let (events, receiver) = mpsc::channel();
        let state = Arc::new(Mutex::new(Snapshot {
            volume: 0.8,
            ..Default::default()
        }));
        let capture = Arc::new(Mutex::new(Vec::new()));
        let handle = Handle {
            tx,
            state: state.clone(),
            capture: capture.clone(),
        };
        let closer = http::SessionCloser::new()?;
        let worker = thread::Builder::new()
            .name("plexfreq-audio".into())
            .spawn(move || Actor::new(sink, state, capture, events, closer).run(rx))?;
        Ok(Self {
            handle,
            events: receiver,
            worker: Some(worker),
        })
    }
    pub fn events(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.handle.tx.send(Control::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn caps() -> gst::Caps {
    gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("layout", "interleaved")
        .field("channels", 2i32)
        .field("rate", RATE as i32)
        .build()
}
fn element(factory: &str) -> Result<gst::Element> {
    gst::ElementFactory::make(factory)
        .build()
        .map_err(|_| Error::Input("Required audio plugins are unavailable"))
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
struct Decoder {
    transcoded: bool,
    http: Option<http::Reader>,
    source: Source,
    pipeline: gst::Pipeline,
    sink: app::AppSink,
    pcm: VecDeque<f32>,
    eof: bool,
    seek: Option<u64>,
    frames: u64,
}
impl Decoder {
    fn new(source: Source, closer: &http::SessionCloser) -> Result<Self> {
        let (uri, token) = safe_uri(&source.url)?;
        let (uri, transcoded) = crate::quality::with_offset(&uri, source.resume)?;
        let pipeline = gst::Pipeline::new();
        let remote = uri.starts_with("http:") || uri.starts_with("https:");
        let decoder = element(if remote { "decodebin" } else { "uridecodebin" })?;
        let mut reader = None;
        if remote {
            let input = app::AppSrc::builder()
                .format(gst::Format::Bytes)
                .stream_type(if transcoded {
                    app::AppStreamType::Stream
                } else {
                    app::AppStreamType::Seekable
                })
                .block(true)
                .max_bytes(512 * 1024)
                .build();
            pipeline
                .add_many([input.upcast_ref(), &decoder])
                .map_err(|_| Error::Input("Audio pipeline creation failed"))?;
            input
                .link(&decoder)
                .map_err(|_| Error::Input("Audio pipeline creation failed"))?;
            reader = Some(http::Reader::new(&input, uri, token, closer.clone())?);
        } else {
            decoder.set_property("uri", uri);
            pipeline
                .add(&decoder)
                .map_err(|_| Error::Input("Audio pipeline creation failed"))?;
        }
        let convert = element("audioconvert")?;
        let resample = element("audioresample")?;
        let sink = app::AppSink::builder()
            .caps(&caps())
            .sync(false)
            .max_buffers(8)
            .build();
        pipeline
            .add_many([&convert, &resample, sink.upcast_ref()])
            .map_err(|_| Error::Input("Audio pipeline creation failed"))?;
        gst::Element::link_many([&convert, &resample, sink.upcast_ref()])
            .map_err(|_| Error::Input("Audio pipeline creation failed"))?;
        let target = convert
            .static_pad("sink")
            .ok_or(Error::Input("Audio pipeline creation failed"))?;
        decoder.connect_pad_added(move |_, pad| {
            if target.is_linked() {
                return;
            }
            let audio = pad
                .current_caps()
                .and_then(|c| c.structure(0).map(|s| s.name().starts_with("audio/")))
                .unwrap_or(false);
            if audio {
                let _ = pad.link(&target);
            }
        });
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| Error::Input("Audio decoder initialization failed"))?;
        let seek = if source.resume > 0 && !transcoded {
            Some(source.resume)
        } else {
            None
        };
        Ok(Self {
            transcoded,
            http: reader,
            source,
            pipeline,
            sink,
            pcm: VecDeque::new(),
            eof: false,
            seek,
            frames: 0,
        })
    }
    fn fill(&mut self, frames: usize) -> Result<()> {
        if let Some(message) = self
            .pipeline
            .bus()
            .ok_or(Error::Input("Audio decoding or streaming failed"))?
            .pop_filtered(&[gst::MessageType::Error])
        {
            if let gst::MessageView::Error(error) = message.view() {
                eprintln!(
                    "PlexFreq audio decoder error: {}; debug: {}",
                    error.error(),
                    error.debug().as_deref().unwrap_or("unavailable")
                );
            }
            return Err(Error::Input("Audio decoding or streaming failed"));
        }
        if let Some(duration) = self
            .pipeline
            .query_duration::<gst::ClockTime>()
            .filter(|_| !self.transcoded)
        {
            self.source.duration = duration.mseconds();
        }
        for _ in 0..16 {
            if self.pcm.len() >= frames * CHANNELS {
                break;
            }
            let Some(sample) = self.sink.try_pull_sample(gst::ClockTime::ZERO) else {
                self.eof = self.sink.is_eos();
                break;
            };
            if let Some(position) = self.seek.take() {
                self.pipeline
                    .seek_simple(
                        gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                        gst::ClockTime::from_mseconds(position),
                    )
                    .map_err(|_| Error::Input("Audio seeking failed"))?;
                self.pcm.clear();
                continue;
            }
            let buffer = sample
                .buffer()
                .ok_or(Error::Input("Invalid decoded audio"))?;
            let map = buffer
                .map_readable()
                .map_err(|_| Error::Input("Invalid decoded audio"))?;
            if map.len() % 8 != 0 {
                return Err(Error::Input("Invalid decoded audio"));
            }
            let mut first = 0usize;
            let mut end = map.len() / 8;
            if let (Some(pts), Some(segment)) = (
                buffer.pts(),
                sample
                    .segment()
                    .and_then(|segment| segment.downcast_ref::<gst::ClockTime>()),
            ) {
                let frames = |time: gst::ClockTime| {
                    ((u128::from(time.nseconds()) * u128::from(RATE) + 500_000_000) / 1_000_000_000)
                        .min(usize::MAX as u128) as usize
                };
                if let Some(start) = segment.start() {
                    if pts < start {
                        first = frames(start - pts).min(end);
                    }
                }
                if let Some(stop) = segment.stop() {
                    end = if pts >= stop {
                        0
                    } else {
                        end.min(frames(stop - pts))
                    };
                }
            }
            // Container edit lists/segment limits, not Plex database duration,
            // define encoder priming/padding boundaries at the PCM sink.
            first = first.min(end);
            self.pcm.extend(
                map.as_slice()[first * 8..end * 8]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            );
        }
        Ok(())
    }
    fn take(&mut self, frames: usize) -> Vec<f32> {
        let count = (frames * CHANNELS).min(self.pcm.len());
        self.frames += count as u64 / CHANNELS as u64;
        self.pcm.drain(..count).collect()
    }
    fn remaining(&self) -> usize {
        self.pcm.len() / CHANNELS
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        if let Some(reader) = &self.http {
            reader.stop();
        }
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

struct Output {
    pipeline: gst::Pipeline,
    source: app::AppSrc,
    frames: u64,
    eof: bool,
}
impl Output {
    fn new(mode: Sink, capture: Arc<Mutex<Vec<f32>>>) -> Result<Self> {
        let pipeline = gst::Pipeline::new();
        let source = app::AppSrc::builder()
            .caps(&caps())
            .format(gst::Format::Time)
            .max_bytes(OUTPUT_MAX_BYTES)
            .build();
        let convert = element("audioconvert")?;
        let sink = match mode {
            Sink::Pulse => {
                let sink = element("pulsesink")?;
                sink.set_property("client-name", "PlexFreq");
                sink.set_property(
                    "stream-properties",
                    pulse_properties(
                        &std::fs::read_to_string("/etc/os-release").unwrap_or_default(),
                    ),
                );
                sink
            }
            Sink::Fake => {
                let sink = element("fakesink")?;
                sink.set_property("sync", true);
                sink
            }
            Sink::Capture => {
                let sink = app::AppSink::builder().sync(false).build();
                sink.set_callbacks(
                    app::AppSinkCallbacks::builder()
                        .new_sample(move |sink| {
                            let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                            let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                            let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                            let mut pcm =
                                capture.lock().unwrap_or_else(|poison| poison.into_inner());
                            if pcm.len() < RATE as usize * 120 * 2 {
                                pcm.extend(
                                    map.as_slice()
                                        .as_chunks::<4>()
                                        .0
                                        .iter()
                                        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                                );
                            }
                            Ok(gst::FlowSuccess::Ok)
                        })
                        .build(),
                );
                sink.upcast()
            }
        };
        pipeline
            .add_many([source.upcast_ref(), &convert, &sink])
            .map_err(|_| Error::Input("Audio output creation failed"))?;
        gst::Element::link_many([source.upcast_ref(), &convert, &sink])
            .map_err(|_| Error::Input("Audio output creation failed"))?;
        Ok(Self {
            pipeline,
            source,
            frames: 0,
            eof: false,
        })
    }
    fn state(&self, state: gst::State) -> Result<()> {
        self.pipeline
            .set_state(state)
            .map_err(|_| Error::Input("Audio output is unavailable"))?;
        Ok(())
    }
    fn push(&mut self, pcm: &[f32]) -> Result<()> {
        let frames = pcm.len() as u64 / CHANNELS as u64;
        let mut buffer = gst::Buffer::with_size(pcm.len() * 4)
            .map_err(|_| Error::Input("Audio buffer allocation failed"))?;
        {
            let buffer = buffer
                .get_mut()
                .ok_or(Error::Input("Audio buffer allocation failed"))?;
            buffer.set_pts(gst::ClockTime::from_nseconds(
                self.frames * 1_000_000_000 / RATE as u64,
            ));
            buffer.set_duration(gst::ClockTime::from_nseconds(
                frames * 1_000_000_000 / RATE as u64,
            ));
            let mut map = buffer
                .map_writable()
                .map_err(|_| Error::Input("Audio buffer allocation failed"))?;
            for (bytes, value) in map
                .as_mut_slice()
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(pcm)
            {
                bytes.copy_from_slice(&value.to_le_bytes());
            }
        }
        self.source
            .push_buffer(buffer)
            .map_err(|_| Error::Input("Audio output failed"))?;
        self.frames += frames;
        Ok(())
    }
    fn position(&self) -> u64 {
        self.pipeline
            .query_position::<gst::ClockTime>()
            .map_or(0, |p| p.nseconds() * RATE as u64 / 1_000_000_000)
            .min(self.frames)
    }
}
fn pulse_properties(os_release: &str) -> gst::Structure {
    // Sailfish mainvolume/stream-restore recognizes x-maemo, not the generic
    // desktop music role. Let the existing user-session policy own volume keys.
    let sailfish = os_release
        .lines()
        .any(|line| line == "ID=sailfishos" || line == "ID=\"sailfishos\"");
    gst::Structure::builder("props")
        .field("media.role", if sailfish { "x-maemo" } else { "music" })
        .field("application.name", "PlexFreq")
        .field("media.name", "PlexFreq")
        .build()
}
impl Drop for Output {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

struct Segment {
    source: Source,
    start: u64,
    end: Option<u64>,
    notified: bool,
}
struct Actor {
    closer: http::SessionCloser,
    intent: u64,
    mode: Sink,
    state: Arc<Mutex<Snapshot>>,
    capture: Arc<Mutex<Vec<f32>>>,
    events: mpsc::Sender<Event>,
    current: Option<Decoder>,
    next: Option<Decoder>,
    output: Option<Output>,
    segments: VecDeque<Segment>,
    desired: bool,
    volume: f32,
    config: dsp::Config,
    processor: dsp::Processor,
    ended: bool,
    fade: Option<(usize, usize)>,
}
impl Actor {
    fn new(
        mode: Sink,
        state: Arc<Mutex<Snapshot>>,
        capture: Arc<Mutex<Vec<f32>>>,
        events: mpsc::Sender<Event>,
        closer: http::SessionCloser,
    ) -> Self {
        let config = dsp::Config::default();
        Self {
            closer,
            intent: 0,
            mode,
            state,
            capture,
            events,
            current: None,
            next: None,
            output: None,
            segments: VecDeque::new(),
            desired: false,
            volume: 0.8,
            processor: dsp::Processor::new(RATE, &config),
            config,
            ended: false,
            fade: None,
        }
    }
    fn load(&mut self, source: Source, paused: bool) -> Result<()> {
        self.intent = source.id;
        self.current = None;
        self.next = None;
        self.output = None;
        self.segments.clear();
        lock_capture(&self.capture).clear();
        let decoder = Decoder::new(source.clone(), &self.closer)?;
        let output = Output::new(self.mode, self.capture.clone())?;
        self.segments.push_back(Segment {
            source,
            start: 0,
            end: None,
            notified: true,
        });
        self.current = Some(decoder);
        self.output = Some(output);
        self.desired = !paused;
        self.ended = false;
        self.fade = None;
        self.processor = dsp::Processor::new(RATE, &self.config);
        Ok(())
    }
    fn control(&mut self, control: Control) -> Result<bool> {
        match control {
            Control::Load(source, paused) => self.load(source, paused)?,
            Control::Prepare(source) => {
                if let Some(next) = source {
                    let id = next.id;
                    match Decoder::new(next, &self.closer) {
                        Ok(decoder) => self.next = Some(decoder),
                        Err(_) => {
                            // A bad successor must never kill current playback;
                            // report it like a decode-fill failure and keep playing.
                            self.next = None;
                            let _ = self.events.send(Event::Error {
                                id,
                                message: "Next track decoding failed",
                            });
                        }
                    }
                } else {
                    self.next = None;
                }
            }
            Control::Play => self.desired = true,
            Control::Pause => {
                self.desired = false;
                if let Some(out) = &self.output {
                    out.state(gst::State::Paused)?;
                }
            }
            Control::Stop => {
                self.desired = false;
                self.current = None;
                self.next = None;
                self.output = None;
                self.segments.clear();
                lock_state(&self.state).position = 0;
            }
            Control::Seek(position) => {
                if let Some(segment) = self.segments.front() {
                    let mut source = segment.source.clone();
                    // Duration may still be unknown (0) while the decoder
                    // probes the stream: don't clamp every seek to zero.
                    source.resume = if source.duration > 0 {
                        position.min(source.duration)
                    } else {
                        position
                    };
                    source.listened = lock_state(&self.state).listened;
                    self.load(source, !self.desired)?;
                    let _ = self.events.send(Event::Seeked(position));
                }
            }
            Control::Volume(volume) => self.volume = volume as f32,
            Control::Configure(config) => {
                self.processor = dsp::Processor::new(RATE, &config);
                self.config = config;
            }
            Control::Shutdown => return Ok(false),
        }
        Ok(true)
    }
    fn error(&mut self, message: &'static str) {
        let id = self.current.as_ref().map_or(self.intent, |d| d.source.id);
        self.desired = false;
        self.current = None;
        self.next = None;
        self.output = None;
        let _ = self.events.send(Event::Error { id, message });
        let mut state = lock_state(&self.state);
        state.playing = false;
        state.paused = false;
        state.buffering = false;
        state.error = message.into();
    }
    fn pump(&mut self) -> Result<()> {
        let tail = (self.config.crossfade_ms as usize * RATE as usize / 1000).min(MAX_TAIL_FRAMES);
        // Effective overlap can never exceed a fully-decoded short successor.
        // Without this clamp a <tail successor stalls: the normal path
        // reserves the full tail while the join path waits for more successor
        // frames that will never arrive.
        let eff_tail = match (&self.current, &self.next) {
            (Some(current), Some(next))
                if !current.source.album.is_empty()
                    && current.source.album == next.source.album =>
            {
                0
            }
            (Some(_), Some(next)) if next.eof => tail.min(next.remaining().max(1).min(tail.max(1))),
            _ => tail,
        };
        // When the successor is empty, fall back to a gapless handoff.
        let eff_tail = match &self.next {
            Some(next)
                if next.eof
                    && next.remaining() == 0
                    && !matches!(&self.current, Some(c) if c.remaining() == 0) =>
            {
                0
            }
            _ => eff_tail,
        };
        if let Some(next) = &mut self.next {
            if next.fill(tail.max(BLOCK * 2)).is_err() {
                let id = next.source.id;
                let _ = self.events.send(Event::Error {
                    id,
                    message: "Next track decoding failed",
                });
                self.next = None;
            }
        }
        let Some(current) = &mut self.current else {
            return Ok(());
        };
        current.fill(tail + BLOCK * 4)?;
        if let Some(segment) = self.segments.back_mut() {
            segment.source.duration = current.source.duration;
        }
        let Some(output) = &mut self.output else {
            return Ok(());
        };
        let Some(bus) = output.pipeline.bus() else {
            return Err(Error::Input("Audio output is unavailable"));
        };
        while let Some(message) = bus.pop() {
            match message.view() {
                gst::MessageView::Error(error) => {
                    eprintln!(
                        "PlexFreq audio output error: {}; debug: {}",
                        error.error(),
                        error.debug().as_deref().unwrap_or("unavailable")
                    );
                    return Err(Error::Input("Audio output is unavailable"));
                }
                gst::MessageView::Eos(_) => output.eof = true,
                _ => {}
            }
        }
        if !self.desired {
            return Ok(());
        };
        if output.source.current_level_bytes() > OUTPUT_BACKPRESSURE_BYTES {
            return Ok(());
        };
        let available = current.remaining();
        if current.eof
            && self.next.is_some()
            && (available == 0 || eff_tail > 0 && available <= eff_tail)
        {
            let Some(next) = self.next.as_mut() else {
                return Err(Error::Input("Audio output is unavailable"));
            };
            let same_album =
                !current.source.album.is_empty() && current.source.album == next.source.album;
            if available > 0 && !same_album && eff_tail > 0 && next.remaining() >= available {
                let (total, done) = *self.fade.get_or_insert((available, 0));
                let frames = BLOCK.min(available);
                let mut old = current.take(frames);
                let mut new = next.take(frames);
                if self.config.normalization {
                    let old_gain = dsp::amplitude(
                        current
                            .source
                            .normalization_gain(self.config.normalization_mode),
                    );
                    let new_gain = dsp::amplitude(
                        next.source
                            .normalization_gain(self.config.normalization_mode),
                    );
                    for value in &mut old {
                        *value *= old_gain;
                    }
                    for value in &mut new {
                        *value *= new_gain;
                    }
                }
                let mut pcm = vec![0.; old.len()];
                dsp::crossfade(&old, &new, &mut pcm, done, total);
                self.processor.process(&mut pcm, 0., self.volume);
                output.push(&pcm)?;
                self.fade = Some((total, done + frames));
                if current.remaining() > 0 {
                    output.state(gst::State::Playing)?;
                    return Ok(());
                }
            } else if available > 0 && !same_album && eff_tail > 0 && !next.eof {
                return Ok(());
            }
            if current.remaining() == 0 {
                let Some(mut next) = self.next.take() else {
                    return Err(Error::Input("Audio output is unavailable"));
                };
                let overlap = self.fade.take().map_or(0, |(total, _)| total as u64);
                if let Some(segment) = self.segments.back_mut() {
                    segment.end = Some(output.frames);
                }
                let source = next.source.clone();
                next.frames = 0;
                self.segments.push_back(Segment {
                    source,
                    start: output.frames.saturating_sub(overlap),
                    end: None,
                    notified: false,
                });
                self.current = Some(next);
                return Ok(());
            }
        }
        let Some(current) = self.current.as_mut() else {
            return Ok(());
        };
        let reserve_fade = current.eof
            && eff_tail > 0
            && self.next.as_ref().is_some_and(|next| {
                current.source.album.is_empty() || current.source.album != next.source.album
            });
        let frames = if !current.eof || reserve_fade {
            current.remaining().saturating_sub(eff_tail).min(BLOCK)
        } else {
            current.remaining().min(BLOCK)
        };
        if frames > 0 {
            let mut pcm = current.take(frames);
            self.processor.process(
                &mut pcm,
                if self.config.normalization {
                    current
                        .source
                        .normalization_gain(self.config.normalization_mode)
                } else {
                    0.
                },
                self.volume,
            );
            output.push(&pcm)?;
            output.state(gst::State::Playing)?;
        } else if current.eof && current.remaining() == 0 && self.next.is_none() {
            if !self.ended {
                let _ = output.source.end_of_stream();
                if let Some(segment) = self.segments.back_mut() {
                    segment.end = Some(output.frames);
                }
                self.ended = true;
            }
        } else if output.source.current_level_bytes() == 0 {
            output.state(gst::State::Paused)?;
        }
        Ok(())
    }
    fn publish(&mut self) {
        let Some(output) = &self.output else {
            let mut state = lock_state(&self.state);
            state.playing = false;
            state.paused = false;
            state.loaded = false;
            state.volume = self.volume as f64;
            return;
        };
        let position = if output.eof {
            output.frames
        } else {
            output.position()
        };
        while self.segments.len() > 1 && position >= self.segments[1].start {
            let Some(old) = self.segments.pop_front() else {
                break;
            };
            let Some(new) = self.segments.front_mut() else {
                break;
            };
            if !new.notified {
                new.notified = true;
                let heard = old.source.listened
                    + position
                        .min(old.end.unwrap_or(position))
                        .saturating_sub(old.start)
                        * 1000
                        / RATE as u64;
                let _ = self.events.send(Event::Transition {
                    old: old.source.id,
                    new: new.source.id,
                    listened: heard,
                    position: old.source.duration,
                });
            }
        }
        let Some(segment) = self.segments.front() else {
            return;
        };
        let frames = position.saturating_sub(segment.start);
        let media = segment.source.resume + frames * 1000 / RATE as u64;
        let listened = segment.source.listened + frames * 1000 / RATE as u64;
        let eos = output.eof && self.desired;
        if eos {
            self.desired = false;
            let _ = self.events.send(Event::End {
                id: segment.source.id,
                listened,
                position: media,
            });
        }
        let mut state = lock_state(&self.state);
        *state = Snapshot {
            id: segment.source.id,
            playing: self.desired && output.pipeline.current_state() == gst::State::Playing,
            paused: !self.desired,
            buffering: self.desired && output.pipeline.current_state() != gst::State::Playing,
            loaded: true,
            position: if segment.source.duration > 0 {
                media.min(segment.source.duration)
            } else {
                media
            },
            duration: segment.source.duration,
            listened,
            volume: self.volume as f64,
            seekable: segment.source.duration > 0,
            error: String::new(),
        };
    }
    fn run(mut self, receiver: mpsc::Receiver<Control>) {
        loop {
            while let Ok(control) = receiver.try_recv() {
                match self.control(control) {
                    Ok(false) => return,
                    Ok(true) => {}
                    Err(_) => self.error("Audio playback failed"),
                }
            }
            if let Err(error) = self.pump() {
                let message = match error {
                    Error::Input(message) => message,
                    _ => "Audio playback failed",
                };
                self.error(message);
            }
            self.publish();
            // Idle/paused actors wake on commands rather than spinning at 200 Hz.
            let wait = Duration::from_millis(if self.desired { 5 } else { 100 });
            match receiver.recv_timeout(wait) {
                Ok(control) => match self.control(control) {
                    Ok(false) => return,
                    Ok(true) => {}
                    Err(_) => self.error("Audio playback failed"),
                },
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_volume_policy_roles_are_platform_specific() {
        gst::init().unwrap();
        assert_eq!(
            pulse_properties("ID=sailfishos\n")
                .get::<String>("media.role")
                .unwrap(),
            "x-maemo"
        );
        assert_eq!(
            pulse_properties("ID=gentoo\n")
                .get::<String>("media.role")
                .unwrap(),
            "music"
        );
    }
    #[test]
    fn framework_uri_never_contains_token() {
        let (uri, token) =
            safe_uri("https://fixture.invalid/audio?x=1&X-Plex-Token=fixture-secret").unwrap();
        assert_eq!(token, "fixture-secret");
        assert_eq!(uri, "https://fixture.invalid/audio?x=1");
    }
}
