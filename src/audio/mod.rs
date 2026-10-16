mod decode;
pub mod dsp;
pub(crate) mod formats;
mod http;
mod mp4;
mod output;
mod pulse;
mod resample;
use crate::{Error, Result};
use decode::Decoder;
use output::Output;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    rc::Rc,
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use tokio::sync::watch;

pub const RATE: u32 = 48000;
const CHANNELS: usize = 2;
const BLOCK: usize = 512;
/// Maximum crossfade tail: 12s at 48kHz stereo.
const MAX_TAIL_FRAMES: usize = RATE as usize * 12;
/// Queued (not yet audible) PCM above which the actor stops feeding the
/// output: headroom for scheduler contention without long volume/EQ lag.
const OUTPUT_BACKPRESSURE_BYTES: u64 = RATE as u64 * 8 / 2;
const DECODE_AHEAD_FRAMES: usize = RATE as usize * 2;

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
/// The transport fields that external presenters (MPRIS, diagnostics) react
/// to. Position is deliberately absent: it changes continuously and is read on
/// demand, so publishing it would turn change notification back into polling.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub(crate) struct Change {
    pub id: u64,
    pub playing: bool,
    pub paused: bool,
    pub buffering: bool,
    pub loaded: bool,
    pub volume: u64,
    pub seekable: bool,
}
impl From<&Snapshot> for Change {
    fn from(state: &Snapshot) -> Self {
        Self {
            id: state.id,
            playing: state.playing,
            paused: state.paused,
            buffering: state.buffering,
            loaded: state.loaded,
            volume: state.volume.to_bits(),
            seekable: state.seekable,
        }
    }
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
    Recording(bool),
    Stop,
    Seek(u64),
    Volume(f64),
    Wake,
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
    volume: Arc<Mutex<Option<f64>>>,
    changes: Arc<watch::Sender<Change>>,
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
    pub(crate) fn recording(&self, active: bool) -> Result<()> {
        self.send(Control::Recording(active))
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
        let mut pending = crate::mutex_lock(&self.volume);
        let wake = pending.is_none();
        *pending = Some(value.clamp(0., 1.));
        if !wake {
            return Ok(());
        }
        match self.tx.try_send(Control::Wake) {
            Ok(()) | Err(mpsc::TrySendError::Full(_)) => Ok(()),
            Err(mpsc::TrySendError::Disconnected(_)) => {
                Err(Error::Input("Audio command queue is unavailable"))
            }
        }
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
    /// Notified only when a [`Change`] field differs, never for position.
    pub(crate) fn changes(&self) -> watch::Receiver<Change> {
        self.changes.subscribe()
    }
}
pub struct Engine {
    pub handle: Handle,
    events: mpsc::Receiver<Event>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Engine {
    pub fn new(sink: Sink) -> Result<Self> {
        let (tx, rx) = mpsc::sync_channel(128);
        let (events, receiver) = mpsc::channel();
        let state = Arc::new(Mutex::new(Snapshot {
            volume: 0.8,
            ..Default::default()
        }));
        let capture = Arc::new(Mutex::new(Vec::new()));
        let volume = Arc::new(Mutex::new(None));
        let changes = Arc::new(watch::channel(Change::from(&*lock_state(&state))).0);
        let handle = Handle {
            tx,
            state: state.clone(),
            capture: capture.clone(),
            volume: volume.clone(),
            changes: changes.clone(),
        };
        let closer = http::SessionCloser::new()?;
        let worker = thread::Builder::new()
            .name("plexfreq-audio".into())
            .spawn(move || {
                let mut actor = Actor::new(sink, state, capture, events, closer);
                actor.changes = changes;
                actor.run(rx, volume)
            })?;
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
    recording: bool,
    volume: f32,
    config: dsp::Config,
    processor: dsp::Processor,
    ended: bool,
    fade: Option<(usize, usize)>,
    diagnostics: ProgressDiagnostics,
    changes: Arc<watch::Sender<Change>>,
    /// PulseAudio client connection shared by successive output streams.
    pulse: Option<Rc<pulse::Connection>>,
}
struct ProgressDiagnostics {
    sampled: Instant,
    reported: Instant,
    progressed: Instant,
    previous: Option<(u64, bool, bool, bool, u64)>,
    stalled: bool,
}
impl ProgressDiagnostics {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            sampled: now,
            reported: now,
            progressed: now,
            previous: None,
            stalled: false,
        }
    }
    fn observe(&mut self, now: Instant, state: &Snapshot, desired: bool) -> Option<&'static str> {
        if self.previous.is_some() && now.duration_since(self.sampled) < Duration::from_secs(1) {
            return None;
        }
        self.sampled = now;
        let key = (
            state.id,
            desired,
            state.playing,
            state.buffering,
            state.position,
        );
        let changed = self
            .previous
            .is_none_or(|old| (old.0, old.1, old.2, old.3) != (key.0, key.1, key.2, key.3));
        if changed
            || self.previous.is_none_or(|old| old.4 != state.position)
            || !desired
            || state.paused
        {
            self.progressed = now;
        }
        self.previous = Some(key);
        let stalled = desired
            && state.loaded
            && now.duration_since(self.progressed) >= Duration::from_secs(2);
        let reason = if stalled != self.stalled {
            Some(if stalled {
                "stalled"
            } else {
                "progress-restored"
            })
        } else if changed {
            Some("state-change")
        } else if now.duration_since(self.reported)
            >= Duration::from_secs(if desired { 10 } else { 60 })
        {
            Some("heartbeat")
        } else {
            None
        };
        self.stalled = stalled;
        if reason.is_some() {
            self.reported = now;
        }
        reason
    }
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
            recording: false,
            volume: 0.8,
            processor: dsp::Processor::new(RATE, &config),
            config,
            ended: false,
            fade: None,
            diagnostics: ProgressDiagnostics::new(),
            changes: Arc::new(watch::channel(Change::default()).0),
            pulse: None,
        }
    }
    /// Wake change subscribers only when a presented transport field changed.
    fn announce(&self) {
        let change = Change::from(&*lock_state(&self.state));
        self.changes.send_if_modified(|current| {
            let modified = *current != change;
            *current = change;
            modified
        });
    }
    fn load(&mut self, source: Source, paused: bool) -> Result<()> {
        self.intent = source.id;
        self.current = None;
        self.next = None;
        self.output = None;
        self.segments.clear();
        lock_capture(&self.capture).clear();
        let decoder = Decoder::new(source.clone(), &self.closer)?;
        let output = Output::new(self.mode, self.capture.clone(), &mut self.pulse)?;
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
    fn prepare(&mut self, source: Option<Source>) {
        // A replaced successor restarts any crossfade from silence; the old
        // fade's progress belongs to frames the new successor never mixed.
        self.fade = None;
        self.next = None;
        let Some(next) = source else {
            return;
        };
        let id = next.id;
        match Decoder::new(next, &self.closer) {
            Ok(decoder) => self.next = Some(decoder),
            Err(_) => {
                // A bad successor must never kill current playback;
                // report it like a decode-fill failure and keep playing.
                let _ = self.events.send(Event::Error {
                    id,
                    message: "Next track decoding failed",
                });
            }
        }
    }
    /// Detach the prepared successor before reloading the audible track: an
    /// untouched decoder is kept, a partly consumed one restarts from its source.
    fn take_successor(&mut self) -> Option<std::result::Result<Decoder, Source>> {
        if let Some(successor) = self.segments.get(1) {
            // The successor already took over decoding but is not audible yet.
            return Some(Err(successor.source.clone()));
        }
        let next = self.next.take()?;
        Some(if next.frames == 0 {
            Ok(next)
        } else {
            Err(next.source.clone())
        })
    }
    fn control(&mut self, control: Control) -> Result<bool> {
        match control {
            Control::Load(source, paused) => self.load(source, paused)?,
            Control::Prepare(source) => {
                crate::diagnostics::event(format_args!(
                    "audio prepare occurrence={:?}",
                    source.as_ref().map(|s| s.id)
                ));
                self.prepare(source);
            }
            Control::Play => {
                crate::diagnostics::event(format_args!(
                    "audio control play occurrence={}",
                    self.intent
                ));
                self.desired = true;
                // A paused, full appsrc cannot drain. Resume before pump's
                // backpressure check, which otherwise prevents this forever.
                if let Some(out) = &mut self.output {
                    if !self.recording {
                        out.set_playing(true)?;
                    }
                }
            }
            Control::Pause => {
                crate::diagnostics::event(format_args!(
                    "audio control pause occurrence={}",
                    self.intent
                ));
                self.desired = false;
                if let Some(out) = &mut self.output {
                    out.set_playing(false)?;
                }
            }
            Control::Recording(active) => {
                // Keep logical play intent separate from a temporary recording
                // hold. User/disconnect Pause clears it even during the hold.
                if active {
                    self.recording = true;
                }
                self.service_output()?;
                self.recording = active;
                crate::diagnostics::event(format_args!(
                    "audio recording hold active={active} resume_intent={}",
                    self.desired
                ));
                if let Some(out) = &mut self.output {
                    out.set_playing(self.desired && !active)?;
                }
            }
            Control::Stop => {
                crate::diagnostics::event(format_args!(
                    "audio control stop occurrence={}",
                    self.intent
                ));
                self.desired = false;
                self.current = None;
                self.next = None;
                self.output = None;
                self.segments.clear();
                lock_state(&self.state).position = 0;
            }
            Control::Seek(position) => {
                crate::diagnostics::event(format_args!(
                    "audio control seek occurrence={} position_ms={position}",
                    self.intent
                ));
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
                    let successor = self.take_successor();
                    self.load(source, !self.desired)?;
                    match successor {
                        Some(Ok(decoder)) => self.next = Some(decoder),
                        Some(Err(source)) => self.prepare(Some(source)),
                        None => {}
                    }
                    let _ = self.events.send(Event::Seeked(position));
                }
            }
            Control::Volume(volume) => {
                crate::diagnostics::event(format_args!("audio volume={volume:.3}"));
                self.volume = volume as f32;
            }
            Control::Configure(config) => {
                crate::diagnostics::event(format_args!("audio DSP configuration changed"));
                // Keep EQ filter history so a gain change does not click.
                self.processor.reconfigure(RATE, &config);
                self.config = config;
            }
            Control::Wake => {}
            Control::Shutdown => return Ok(false),
        }
        Ok(true)
    }
    fn error(&mut self, message: &'static str) {
        let id = self.current.as_ref().map_or(self.intent, |d| d.source.id);
        crate::diagnostics::event(format_args!("audio actor error occurrence={id}: {message}"));
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
    fn service_output(&mut self) -> Result<()> {
        if let Some(output) = &mut self.output {
            if self.recording {
                // A policy cork during recording must not erase the saved
                // intent; explicit Pause controls still clear it normally.
                output.service(&mut false)?;
            } else {
                output.service(&mut self.desired)?;
            }
        }
        Ok(())
    }
    fn pump(&mut self) -> Result<()> {
        // Service policy requests even when decoding has ended or the PCM queue
        // is full. Clear play intent before any path can restart the pipeline.
        self.service_output()?;
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
            (Some(_), Some(next)) if next.eof => tail.min(next.remaining().max(1)),
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
            if next.fill(tail.max(DECODE_AHEAD_FRAMES)).is_err() {
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
        current.fill(tail + DECODE_AHEAD_FRAMES)?;
        if let Some(segment) = self.segments.back_mut() {
            segment.source.duration = current.source.duration;
        }
        let Some(output) = &mut self.output else {
            return Ok(());
        };
        if !self.desired || self.recording {
            return Ok(());
        };
        if !output.wants_more(OUTPUT_BACKPRESSURE_BYTES) {
            return Ok(());
        };
        let available = current.remaining();
        // After EOS was queued, the output cannot accept a successor's PCM;
        // the natural End then lets the runtime start it on a fresh output.
        if !self.ended
            && current.eof
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
                    output.set_playing(true)?;
                    return Ok(());
                }
            } else if available > 0 && !same_album && eff_tail > 0 && !next.eof {
                return Ok(());
            }
            if current.remaining() == 0 {
                let Some(mut next) = self.next.take() else {
                    return Err(Error::Input("Audio output is unavailable"));
                };
                // Backdate only by frames the successor actually overlapped.
                let overlap = self.fade.take().map_or(0, |(_, done)| done as u64);
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
            output.set_playing(true)?;
        } else if current.eof && current.remaining() == 0 && (self.next.is_none() || self.ended) {
            if !self.ended {
                output.finish()?;
                if let Some(segment) = self.segments.back_mut() {
                    segment.end = Some(output.frames);
                }
                self.ended = true;
            }
        } else if output.queued_bytes() == 0 {
            // Starved: hold the stream instead of letting the sink underrun.
            output.set_playing(false)?;
        }
        Ok(())
    }
    fn publish(&mut self) {
        let Some(output) = &mut self.output else {
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
                crate::diagnostics::event(format_args!(
                    "audio audible transition old={} new={} heard_ms={heard}",
                    old.source.id, new.source.id
                ));
            }
        }
        let Some(segment) = self.segments.front() else {
            return;
        };
        let frames = position.saturating_sub(segment.start);
        let media = segment.source.resume + frames * 1000 / RATE as u64;
        let listened = segment.source.listened + frames * 1000 / RATE as u64;
        let eos = output.eof && self.desired && !self.recording;
        if eos {
            crate::diagnostics::event(format_args!(
                "audio natural end occurrence={} position_ms={media} heard_ms={listened}",
                segment.source.id
            ));
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
            playing: self.desired && !self.recording && output.playing(),
            paused: !self.desired || self.recording,
            buffering: self.desired && !self.recording && !output.playing(),
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
    fn run(mut self, receiver: mpsc::Receiver<Control>, volume: Arc<Mutex<Option<f64>>>) {
        let mut previous_wake = Instant::now();
        loop {
            let now = Instant::now();
            let wake_ms = now.duration_since(previous_wake).as_millis();
            if self.desired && wake_ms > 1000 {
                crate::diagnostics::event(format_args!(
                    "audio actor delayed iteration elapsed_ms={wake_ms}"
                ));
            }
            previous_wake = now;
            if let Some(value) = crate::mutex_lock(&volume).take() {
                let _ = self.control(Control::Volume(value));
            }
            while let Ok(control) = receiver.try_recv() {
                match self.control(control) {
                    Ok(false) => return,
                    Ok(true) => {}
                    Err(_) => self.error("Audio playback failed"),
                }
            }
            // Catch up after a delayed wake rather than supplying only 10.7ms
            // of PCM per tick. Bound each batch so transport remains responsive.
            for _ in 0..8 {
                if let Err(error) = self.pump() {
                    let message = match error {
                        Error::Input(message) => message,
                        _ => "Audio playback failed",
                    };
                    self.error(message);
                    break;
                }
                if !self.desired
                    || self.recording
                    || self
                        .output
                        .as_mut()
                        .is_none_or(|out| !out.wants_more(OUTPUT_BACKPRESSURE_BYTES))
                {
                    break;
                }
            }
            self.publish();
            self.announce();
            self.diagnose();
            // Idle/paused actors wake on commands rather than spinning at 200 Hz.
            let wait = Duration::from_millis(if self.desired && !self.recording {
                5
            } else {
                100
            });
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
    fn diagnose(&mut self) {
        if !crate::diagnostics::enabled() {
            return;
        }
        let state = lock_state(&self.state).clone();
        let now = Instant::now();
        let Some(reason) = self.diagnostics.observe(now, &state, self.desired) else {
            return;
        };
        let decoder = self.current.as_ref();
        let output = self.output.as_mut().map(|out| {
            (
                out.corked(),
                out.playing(),
                out.frames,
                out.queued_bytes(),
                out.eof,
                out.warnings,
                (
                    out.position_clock.available,
                    out.position_clock.raw,
                    out.position_clock.offset,
                ),
            )
        });
        crate::diagnostics::event(format_args!(
            "audio snapshot reason={reason} occurrence={} desired={} playing={} buffering={} position_ms={} heard_ms={} no_progress_ms={} output_corked={:?} output_running={:?} output_frames={} output_queue_bytes={} output_eof={} decoder_pcm_frames={} decoder_eof={} decoder_warnings={} http_received_and_queue_bytes={:?} next_pcm_frames={} output_underflows={} output_clock={:?}",
            state.id, self.desired, state.playing, state.buffering, state.position, state.listened,
            now.duration_since(self.diagnostics.progressed).as_millis(), output.map(|o| o.0), output.map(|o| o.1),
            output.map_or(0, |o| o.2), output.map_or(0, |o| o.3), output.is_some_and(|o| o.4),
            decoder.map_or(0, Decoder::remaining), decoder.is_some_and(|d| d.eof), decoder.map_or(0, |d| d.warnings),
            decoder.and_then(|d| d.http.as_ref().map(http::Reader::diagnostics)), self.next.as_ref().map_or(0, Decoder::remaining),
            output.map_or(0, |o| o.5), output.map(|o| o.6)
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use output::PositionClock;
    #[test]
    fn output_clock_reset_keeps_gapless_occurrence_time_advancing() {
        let mut clock = PositionClock::default();
        let rate = u64::from(RATE);
        let before = 5_333 * rate;
        let submitted = before + rate;
        assert_eq!(clock.observe(Some(before), submitted), before);
        assert_eq!(clock.observe(None, submitted), before);
        assert_eq!(clock.observe(Some(before - 10), submitted), before);
        assert_eq!(clock.offset, 0);
        // A route reconnect can restart the sink position without resetting
        // the appsrc timestamps or the gapless segment start coordinates.
        assert_eq!(clock.observe(Some(0), submitted), before);
        assert_eq!(clock.observe(Some(0), submitted), before);
        assert_eq!(clock.observe(Some(rate / 2), submitted), before + rate / 2);
        assert_eq!(clock.observe(Some(rate), submitted), submitted);
        // Repeated resets retain progress; queued frames are never heard early.
        assert_eq!(clock.observe(Some(0), submitted), submitted);
        assert_eq!(clock.observe(Some(rate), submitted), submitted);
        assert_eq!(
            clock.observe(Some(rate), submitted + rate),
            submitted + rate
        );
    }

    #[test]
    fn volume_flood_leaves_room_for_transport_and_retains_latest_value() {
        let (tx, rx) = mpsc::sync_channel(128);
        let handle = Handle {
            tx,
            state: Arc::new(Mutex::new(Snapshot::default())),
            capture: Arc::new(Mutex::new(Vec::new())),
            volume: Arc::new(Mutex::new(None)),
            changes: Arc::new(watch::channel(Change::default()).0),
        };
        for n in 0..10000 {
            handle.volume(n as f64 / 10000.).unwrap();
        }
        handle.load(Source::default(), false).unwrap();
        handle.play().unwrap();
        handle.pause().unwrap();
        assert_eq!(rx.try_iter().count(), 4);
        assert_eq!(*crate::mutex_lock(&handle.volume), Some(0.9999));
    }

    #[test]
    fn diagnostics_distinguish_stall_recovery_pause_and_heartbeat() {
        let mut monitor = ProgressDiagnostics::new();
        let now = monitor.sampled;
        let mut state = Snapshot {
            id: 1,
            loaded: true,
            playing: true,
            ..Default::default()
        };
        assert_eq!(monitor.observe(now, &state, true), Some("state-change"));
        assert_eq!(
            monitor.observe(now + Duration::from_secs(1), &state, true),
            None
        );
        assert_eq!(
            monitor.observe(now + Duration::from_secs(2), &state, true),
            Some("stalled")
        );
        assert_eq!(
            monitor.observe(now + Duration::from_secs(3), &state, true),
            None
        );
        state.position = 1000;
        assert_eq!(
            monitor.observe(now + Duration::from_secs(4), &state, true),
            Some("progress-restored")
        );
        state.playing = false;
        assert_eq!(
            monitor.observe(now + Duration::from_secs(5), &state, false),
            Some("state-change")
        );
        assert_eq!(
            monitor.observe(now + Duration::from_secs(10), &state, false),
            None
        );
        assert_eq!(
            monitor.observe(now + Duration::from_secs(65), &state, false),
            Some("heartbeat")
        );
        // Recording retains play intent while deliberately publishing paused.
        state.paused = true;
        assert_eq!(
            monitor.observe(now + Duration::from_secs(66), &state, true),
            Some("state-change")
        );
        assert_eq!(
            monitor.observe(now + Duration::from_secs(70), &state, true),
            None
        );
    }
    fn actor(sink: Sink, capture: Arc<Mutex<Vec<f32>>>) -> Actor {
        let (events, _) = mpsc::channel();
        Actor::new(
            sink,
            Arc::new(Mutex::new(Snapshot::default())),
            capture,
            events,
            http::SessionCloser::new().unwrap(),
        )
    }
    fn output(sink: Sink, capture: Arc<Mutex<Vec<f32>>>) -> Output {
        Output::new(sink, capture, &mut None).unwrap()
    }

    #[test]
    fn recording_resumes_only_retained_play_intent_and_pause_cancels_resume() {
        let capture = Arc::new(Mutex::new(Vec::new()));
        let mut actor = actor(Sink::Capture, capture.clone());
        let mut out = output(Sink::Capture, capture.clone());
        out.push(&vec![0.; BLOCK * CHANNELS]).unwrap();
        actor.output = Some(out);
        actor.control(Control::Recording(true)).unwrap();
        actor.control(Control::Recording(false)).unwrap();
        assert!(!actor.desired); // An already paused app must not start.
        assert!(actor.output.as_ref().unwrap().corked());
        assert!(capture.lock().unwrap().is_empty());
        actor.control(Control::Play).unwrap();
        // The queued block reaches the sink once playback is allowed.
        assert_eq!(capture.lock().unwrap().len(), BLOCK * CHANNELS);
        actor.control(Control::Recording(true)).unwrap();
        assert!(actor.desired && actor.recording);
        // A policy cork request during the hold must not erase the intent.
        actor.output.as_mut().unwrap().injected.cork_requests = 1;
        actor.pump().unwrap();
        assert!(actor.desired);
        let out = actor.output.as_mut().unwrap();
        assert!(out.corked() && !out.playing());
        // PCM supplied while held is retained, not rendered.
        out.push(&vec![0.; BLOCK * CHANNELS]).unwrap();
        assert_eq!(capture.lock().unwrap().len(), BLOCK * CHANNELS);
        actor.control(Control::Recording(false)).unwrap();
        assert!(actor.desired && !actor.recording);
        assert!(actor.output.as_ref().unwrap().playing());
        assert_eq!(capture.lock().unwrap().len(), 2 * BLOCK * CHANNELS);
        actor.control(Control::Recording(true)).unwrap();
        actor.control(Control::Pause).unwrap(); // User or Bluetooth disconnect.
        actor.control(Control::Recording(false)).unwrap();
        assert!(!actor.desired && !actor.recording);
        let out = actor.output.as_mut().unwrap();
        out.push(&vec![0.; BLOCK * CHANNELS]).unwrap();
        assert!(out.corked() && !out.playing());
        assert_eq!(capture.lock().unwrap().len(), 2 * BLOCK * CHANNELS);
    }
    #[test]
    fn policy_cork_clears_intent_and_uncork_cannot_resume_it() {
        let capture = Arc::new(Mutex::new(Vec::new()));
        let mut actor = actor(Sink::Fake, capture.clone());
        let mut out = output(Sink::Fake, capture);
        for _ in 0..(OUTPUT_BACKPRESSURE_BYTES as usize / (BLOCK * 8) + 2) {
            out.push(&vec![0.; BLOCK * CHANNELS]).unwrap();
        }
        out.set_playing(true).unwrap();
        assert!(out.queued_bytes() > OUTPUT_BACKPRESSURE_BYTES);
        // A cork request followed by an uncork request (e.g. after a call).
        out.injected.cork_requests = 1;
        out.injected.uncork_requests = 1;
        actor.output = Some(out);
        actor.desired = true;
        actor.pump().unwrap();
        assert!(!actor.desired);
        let out = actor.output.as_mut().unwrap();
        assert!(out.corked() && !out.playing());
        // A corked sink holds position: nothing is consumed.
        let position = out.position();
        thread::sleep(Duration::from_millis(30));
        assert_eq!(out.position(), position);
        actor.pump().unwrap();
        assert!(!actor.desired && actor.output.as_ref().unwrap().corked());
        actor.control(Control::Play).unwrap();
        assert!(actor.desired && actor.output.as_ref().unwrap().playing());
    }
    #[test]
    fn output_failure_reports_error_without_resuming() {
        let capture = Arc::new(Mutex::new(Vec::new()));
        let (events, received) = mpsc::channel();
        let state = Arc::new(Mutex::new(Snapshot::default()));
        let mut actor = Actor::new(
            Sink::Fake,
            state.clone(),
            capture.clone(),
            events,
            http::SessionCloser::new().unwrap(),
        );
        let mut out = output(Sink::Fake, capture);
        out.injected.failed = true;
        actor.output = Some(out);
        actor.desired = true;
        let error = actor.pump().unwrap_err();
        let Error::Input(message) = error else {
            panic!("{error:?}")
        };
        actor.error(message);
        assert!(!actor.desired && actor.output.is_none());
        assert!(matches!(
            received.try_recv(),
            Ok(Event::Error {
                message: "Audio output is unavailable",
                ..
            })
        ));
        assert_eq!(state.lock().unwrap().error, "Audio output is unavailable");
    }
    #[test]
    fn play_releases_a_paused_output_with_a_full_queue() {
        let capture = Arc::new(Mutex::new(Vec::new()));
        let mut actor = actor(Sink::Fake, capture.clone());
        let mut out = output(Sink::Fake, capture);
        // Pause with more PCM queued than pump's backpressure watermark.
        for _ in 0..(OUTPUT_BACKPRESSURE_BYTES as usize / (BLOCK * 8) + 2) {
            out.push(&vec![0.; BLOCK * CHANNELS]).unwrap();
        }
        assert!(out.queued_bytes() > OUTPUT_BACKPRESSURE_BYTES);
        actor.output = Some(out);
        actor.control(Control::Play).unwrap();
        let out = actor.output.as_mut().unwrap();
        assert!(out.playing());
        let until = Instant::now() + Duration::from_secs(2);
        while out.queued_bytes() > OUTPUT_BACKPRESSURE_BYTES {
            assert!(Instant::now() < until, "Play left a full output paused");
            thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn simulated_sinks_account_frames_on_long_continuous_sessions() {
        // ~106.7 hours of frames overflowed the old u64 nanosecond PTS.
        let start = u64::MAX / 1_000_000_000 + 1;
        let mut out = output(Sink::Fake, Arc::new(Mutex::new(Vec::new())));
        out.frames = start;
        let mut out_position = out.position();
        assert_eq!(out_position, 0);
        out.set_playing(true).unwrap();
        out.push(&[0.; BLOCK * CHANNELS]).unwrap();
        thread::sleep(Duration::from_millis(50));
        out_position = out.position();
        assert!(out_position > 0 && out_position <= start + BLOCK as u64);
        let capture = Arc::new(Mutex::new(Vec::new()));
        let mut out = output(Sink::Capture, capture.clone());
        out.frames = start;
        out.set_playing(true).unwrap();
        out.push(&[0.25; BLOCK * CHANNELS]).unwrap();
        assert_eq!(out.frames, start + BLOCK as u64);
        assert_eq!(capture.lock().unwrap().len(), BLOCK * CHANNELS);
    }
    #[test]
    fn finished_simulated_output_reports_eof_after_playing_out() {
        let mut out = output(Sink::Fake, Arc::new(Mutex::new(Vec::new())));
        out.set_playing(true).unwrap();
        out.push(&vec![0.; RATE as usize / 20 * CHANNELS]).unwrap();
        out.finish().unwrap();
        out.service(&mut true).unwrap();
        assert!(!out.eof, "queued frames are not yet audible");
        thread::sleep(Duration::from_millis(80));
        out.service(&mut true).unwrap();
        assert!(out.eof);
        assert_eq!(out.position(), out.frames);
    }
}
