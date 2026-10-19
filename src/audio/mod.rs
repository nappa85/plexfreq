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
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
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
/// A held failure the runtime never resolves is abandoned after this long.
const RECOVERY_LIMIT: Duration = Duration::from_secs(90);
/// Slack over the queued audio's own length before a drain is forced.
const DRAIN_MARGIN: Duration = Duration::from_secs(3);
const STREAM_FAILED: &str = "Audio decoding or streaming failed";
/// Decoded audio a fresh remote load or seek waits for before it starts
/// (above PulseAudio's 300 ms prebuffer).
const START_BUFFER_FRAMES: usize = RATE as usize / 2;
/// Decoded audio a stalled remote stream waits for before it resumes, so a
/// slow link gives a few long pauses instead of many short stutters.
const RESUME_BUFFER_FRAMES: usize = RATE as usize * 5;
/// Un-paused waiting after which a gate settles for `START_BUFFER_FRAMES`.
const GATE_PATIENCE: Duration = Duration::from_secs(15);
/// A Start gate counts as starving (for the cache yield) after this long.
const START_STARVED_AFTER: Duration = Duration::from_secs(1);
/// A remote successor of a remote track starts its download once the
/// current one is within this much of its end (plus the crossfade)...
const PRELOAD_LEAD_MS: u64 = 60_000;
/// ...unless the current stream stalled this recently...
const PRELOAD_QUIET: Duration = Duration::from_secs(10);
/// ...or earlier, once the current input window holds this much audio
/// (advisory: estimated from the bitrate).
const PRELOAD_AHEAD_MS: u64 = 20_000;

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
    /// The current track's stream failed and its recovery is pending
    /// (`Engine::with_recovery`): loaded, holding the user's intent.
    pub failed: bool,
    /// Logical play intent, independent of a recording hold or a failure.
    pub intent: bool,
    /// Playing is wanted but the remote stream is buffering after running
    /// dry (or has been starting for over a second): other downloads yield.
    pub starved: bool,
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
    pub failed: bool,
    pub intent: bool,
    pub starved: bool,
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
            failed: state.failed,
            intent: state.intent,
            starved: state.starved,
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
        /// The play/pause intent at failure, before the error cleared it.
        paused: bool,
        /// The runtime's user-action counter as seen at failure.
        acted: u64,
        /// The engine holds this failure until a `Handle::resolve`.
        recoverable: bool,
        /// Media position, listened time and duration at the failure.
        position: u64,
        listened: u64,
        duration: u64,
        /// Listened time since this decoder started (its own new audio).
        progress: u64,
    },
    /// The answer to `Handle::resolve` (or the engine's own expiry).
    Resolved {
        failed: u64,
        seq: u64,
        outcome: Outcome,
    },
    Seeked(u64),
}
/// How the runtime resolves a held failure.
pub enum Resolution {
    /// The same item from another source, at the held position and intent.
    Recover(Source),
    /// Another item (a gone skip), with its own position and the held intent.
    Replace(Source),
    /// Give up: the unloaded error state.
    Abandon,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Loaded(u64),
    /// The failure was no longer held (the user stopped or loaded something).
    Refused,
    Abandoned,
    /// The replacement could not be opened; the engine is unloaded.
    LoadFailed(u64),
    /// The engine gave up after `RECOVERY_LIMIT`.
    Expired,
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
    Resolve {
        failed: u64,
        seq: u64,
        resolution: Resolution,
        expiry: Instant,
    },
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
    /// Resolve the held failure `failed`; answered by `Event::Resolved` with
    /// `seq`. A replacement is refused after `expiry` or once the failure is
    /// no longer held. A full queue hands the resolution back for a retry.
    pub fn resolve(
        &self,
        failed: u64,
        seq: u64,
        resolution: Resolution,
        expiry: Instant,
    ) -> std::result::Result<(), Box<Resolution>> {
        self.tx
            .try_send(Control::Resolve {
                failed,
                seq,
                resolution,
                expiry,
            })
            .map_err(|error| {
                let control = match error {
                    mpsc::TrySendError::Full(control)
                    | mpsc::TrySendError::Disconnected(control) => control,
                };
                let Control::Resolve { resolution, .. } = control else {
                    unreachable!("sent a Resolve")
                };
                Box::new(resolution)
            })
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
        Self::with_acted(sink, Arc::new(AtomicU64::new(0)))
    }
    /// An engine whose errors report `acted`, the runtime's count of user
    /// transport actions, as it was when the failure happened.
    pub fn with_acted(sink: Sink, acted: Arc<AtomicU64>) -> Result<Self> {
        Self::build(sink, acted, false)
    }
    /// As `with_acted`, and a current-track stream failure is held
    /// (drained, then failed with the user's intent) until the runtime
    /// resolves it.
    pub fn with_recovery(sink: Sink, acted: Arc<AtomicU64>) -> Result<Self> {
        Self::build(sink, acted, true)
    }
    fn build(sink: Sink, acted: Arc<AtomicU64>, recover: bool) -> Result<Self> {
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
                actor.acted = acted;
                actor.recover = recover;
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
    /// A remote successor whose download waits for a healthy current stream.
    pending: Option<Source>,
    /// The remote current stream is buffering before (re)starting.
    gate: Option<Gate>,
    /// When the current stream last stalled (defers the successor).
    stalled: Option<Instant>,
    /// Start the output below its prebuffer after this pump batch.
    trigger: bool,
    /// The successor could not cover this boundary's crossfade: play the
    /// old tail unfaded instead of waiting with audible audio held back.
    fade_dropped: bool,
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
    /// Read-only view of the runtime's user-action counter (see `Event::Error`).
    acted: Arc<AtomicU64>,
    /// Hold current-track stream failures for the runtime (`with_recovery`).
    recover: bool,
    /// The current decoder failed: the queued audio plays out first.
    failing: Option<Failing>,
    /// A held failure awaiting `Control::Resolve`.
    failed: Option<Failed>,
    recovery_limit: Duration,
    gate_patience: Duration,
    preload_lead_ms: u64,
    /// Why the last deferred successor started (tests).
    #[cfg(test)]
    preload_reason: Option<&'static str>,
}
#[derive(Clone, Copy, PartialEq, Debug)]
enum GateKind {
    /// A fresh remote load or seek.
    Start,
    /// The stream ran dry while playing.
    Stall,
}
#[derive(Clone, Copy, Debug)]
struct Gate {
    kind: GateKind,
    since: Instant,
    /// Un-paused, non-recording time spent waiting.
    waited: Duration,
    tick: Instant,
}
impl Gate {
    fn new(kind: GateKind) -> Self {
        let now = Instant::now();
        Self {
            kind,
            since: now,
            waited: Duration::ZERO,
            tick: now,
        }
    }
    fn threshold(&self) -> usize {
        match self.kind {
            GateKind::Start => START_BUFFER_FRAMES,
            GateKind::Stall => RESUME_BUFFER_FRAMES,
        }
    }
}
struct Failing {
    /// Un-paused play time allowed before the drain is forced.
    budget: Duration,
    played: Duration,
    tick: Instant,
}
struct Failed {
    source: Source,
    position: u64,
    listened: u64,
    paused: bool,
    since: Instant,
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
            pending: None,
            gate: None,
            stalled: None,
            trigger: false,
            fade_dropped: false,
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
            acted: Arc::new(AtomicU64::new(0)),
            recover: false,
            failing: None,
            failed: None,
            recovery_limit: RECOVERY_LIMIT,
            gate_patience: GATE_PATIENCE,
            preload_lead_ms: PRELOAD_LEAD_MS,
            #[cfg(test)]
            preload_reason: None,
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
        self.failing = None;
        self.failed = None;
        self.intent = source.id;
        self.current = None;
        self.next = None;
        self.pending = None;
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
        // A remote stream starts once a little audio is decoded.
        self.gate = decoder.http.is_some().then(|| Gate::new(GateKind::Start));
        self.stalled = None;
        self.trigger = false;
        self.fade_dropped = false;
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
        self.fade_dropped = false;
        self.next = None;
        self.pending = None;
        let Some(next) = source else {
            return;
        };
        // A remote successor of a remote track waits: its download would
        // compete with a stream that may still be catching up (`pump`).
        let remote = |source: &Source| !source.url.starts_with("file:");
        if remote(&next) && self.current.as_ref().is_some_and(|c| c.http.is_some()) {
            crate::diagnostics::event(format_args!(
                "audio successor deferred occurrence={}",
                next.id
            ));
            self.pending = Some(next);
            return;
        }
        self.create_successor(next);
    }
    fn create_successor(&mut self, next: Source) {
        let id = next.id;
        match Decoder::with_window(next, &self.closer, http::SUCCESSOR_CAPACITY) {
            Ok(decoder) => self.next = Some(decoder),
            Err(_) => {
                // A bad successor must never kill current playback;
                // report it like a decode-fill failure and keep playing.
                let _ = self.events.send(Event::Error {
                    id,
                    message: "Next track decoding failed",
                    paused: !self.desired,
                    acted: self.acted.load(Ordering::SeqCst),
                    recoverable: false,
                    position: 0,
                    listened: 0,
                    duration: 0,
                    progress: 0,
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
        if let Some(pending) = self.pending.take() {
            return Some(Err(pending));
        }
        let next = self.next.take()?;
        // A remote successor restarts from its source: re-prepared, it
        // waits again for the reloaded (remote) stream.
        Some(if next.frames == 0 && next.http.is_none() {
            Ok(next)
        } else {
            Err(next.source.clone())
        })
    }
    /// Start a deferred successor once the current stream is healthy:
    /// decoded to its end, fully downloaded, a well filled input window, or
    /// near its end without a recent stall.
    fn start_pending(&mut self, tail: usize) {
        let (Some(current), Some(_)) = (&self.current, &self.pending) else {
            return;
        };
        let started = current.frames > 0 || current.remaining() > 0;
        let input = current.input();
        let duration = current.source.duration;
        let decoded = current.source.resume + current.frames * 1000 / RATE as u64;
        let tail_ms = (tail as u64) * 1000 / RATE as u64;
        let reason = if current.eof {
            Some("eof")
        } else if !started {
            None
        } else if input.is_none() {
            Some("local")
        } else if input.is_some_and(|(facts, _)| facts.complete) {
            Some("complete")
        } else if input.is_some_and(|(facts, _)| facts.buffered >= facts.capacity as u64 * 3 / 4) {
            Some("window")
        } else if input.is_some_and(|(facts, rate)| {
            rate.is_some_and(|rate| facts.buffered * 1000 / rate >= PRELOAD_AHEAD_MS)
        }) {
            Some("ahead")
        } else if duration > 0
            && duration.saturating_sub(decoded) <= self.preload_lead_ms + tail_ms
            && self.stalled.is_none_or(|at| at.elapsed() >= PRELOAD_QUIET)
        {
            Some("lead")
        } else {
            None
        };
        let Some(reason) = reason else {
            return;
        };
        let Some(next) = self.pending.take() else {
            return;
        };
        #[cfg(test)]
        {
            self.preload_reason = Some(reason);
        }
        crate::diagnostics::event(format_args!(
            "audio successor started occurrence={} reason={reason}",
            next.id
        ));
        self.create_successor(next);
    }
    fn control(&mut self, control: Control) -> Result<bool> {
        match control {
            Control::Load(source, paused) => self.load(source, paused)?,
            // Nothing to prepare after a failing decoder: the recovery
            // replaces the whole playback.
            Control::Prepare(_) if self.failing.is_some() || self.failed.is_some() => {}
            Control::Prepare(source) => {
                crate::diagnostics::event(format_args!(
                    "audio prepare occurrence={:?}",
                    source.as_ref().map(|s| s.id)
                ));
                self.prepare(source);
            }
            Control::Play if self.failed.is_some() => {
                if let Some(failed) = &mut self.failed {
                    failed.paused = false;
                }
                crate::diagnostics::event(format_args!("audio control play (failure held)"));
            }
            Control::Pause if self.failed.is_some() => {
                if let Some(failed) = &mut self.failed {
                    failed.paused = true;
                }
                crate::diagnostics::event(format_args!("audio control pause (failure held)"));
            }
            Control::Play => {
                crate::diagnostics::event(format_args!(
                    "audio control play occurrence={}",
                    self.intent
                ));
                self.desired = true;
                // A paused, full appsrc cannot drain. Resume before pump's
                // backpressure check, which otherwise prevents this forever.
                // A buffering remote stream is released by `pump` instead.
                if let Some(out) = &mut self.output {
                    if !self.recording && self.gate.is_none() {
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
                    out.set_playing(self.desired && !active && self.gate.is_none())?;
                }
            }
            Control::Stop => {
                crate::diagnostics::event(format_args!(
                    "audio control stop occurrence={}",
                    self.intent
                ));
                self.failing = None;
                self.failed = None;
                self.desired = false;
                self.current = None;
                self.next = None;
                self.pending = None;
                self.output = None;
                self.segments.clear();
                lock_state(&self.state).position = 0;
            }
            Control::Seek(position) => {
                crate::diagnostics::event(format_args!(
                    "audio control seek occurrence={} position_ms={position}",
                    self.intent
                ));
                // A seek within the failing track itself ends its drain; one
                // in an earlier, still audible track is an ordinary seek
                // there (the failing successor is prepared afresh).
                if self.failing.is_some() && self.segments.len() <= 1 {
                    self.enter_failed(false);
                }
                if let Some(failed) = &mut self.failed {
                    failed.position = if failed.source.duration > 0 {
                        position.min(failed.source.duration)
                    } else {
                        position
                    };
                    let _ = self.events.send(Event::Seeked(failed.position));
                } else if let Some(segment) = self.segments.front() {
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
            Control::Resolve {
                failed,
                seq,
                resolution,
                expiry,
            } => self.resolve(failed, seq, resolution, expiry),
            Control::Shutdown => return Ok(false),
        }
        Ok(true)
    }
    fn error(&mut self, message: &'static str) {
        // A backend failure while a failing decoder drains: finalize the
        // drain (Transition before Error) and hold that failure instead.
        if self.recover && self.failing.is_some() {
            return self.enter_failed(true);
        }
        let id = self.current.as_ref().map_or_else(
            || {
                self.segments
                    .back()
                    .map_or(self.intent, |segment| segment.source.id)
            },
            |d| d.source.id,
        );
        crate::diagnostics::event(format_args!("audio actor error occurrence={id}: {message}"));
        // Captured before clearing: a fallback or skip resumes this intent.
        let paused = !self.desired;
        let acted = self.acted.load(Ordering::SeqCst);
        self.desired = false;
        self.current = None;
        self.next = None;
        self.pending = None;
        self.output = None;
        self.failing = None;
        self.failed = None;
        if self.recover {
            // Nothing is held: a later Seek must not reopen the dead source.
            self.segments.clear();
        }
        let _ = self.events.send(Event::Error {
            id,
            message,
            paused,
            acted,
            recoverable: false,
            position: 0,
            listened: 0,
            duration: 0,
            progress: 0,
        });
        let mut state = lock_state(&self.state);
        state.playing = false;
        state.paused = false;
        state.buffering = false;
        state.failed = false;
        state.intent = false;
        state.error = message.into();
    }
    /// The current decoder failed: stop decoding and let the audio already
    /// submitted play out (the backend's own drain), then hold the failure.
    fn start_drain(&mut self) {
        let Some(current) = self.current.take() else {
            return;
        };
        crate::diagnostics::event(format_args!(
            "audio source failed; playing out queued audio occurrence={}",
            current.source.id
        ));
        self.next = None;
        self.pending = None;
        self.fade = None;
        let Some(output) = self.output.as_mut() else {
            return self.enter_failed(true);
        };
        if output.frames == 0 {
            return self.enter_failed(true);
        }
        if let Some(segment) = self.segments.back_mut() {
            segment.end = Some(output.frames);
        }
        // What was queued plays out, whatever the resume gate waited for.
        self.gate = None;
        let queued = output.frames.saturating_sub(output.position());
        if self.desired && !self.recording && output.set_playing(true).is_err() {
            return self.enter_failed(true);
        }
        if output.finish().is_err() {
            return self.enter_failed(true);
        }
        self.ended = true;
        self.failing = Some(Failing {
            budget: Duration::from_millis(queued * 1000 / RATE as u64) + DRAIN_MARGIN,
            played: Duration::ZERO,
            tick: Instant::now(),
        });
    }
    /// Advance a drain: done at the backend's drained signal, or forced once
    /// its un-paused time exceeds the queued audio plus a margin.
    fn drain_tick(&mut self) {
        let Some(failing) = &mut self.failing else {
            return;
        };
        let now = Instant::now();
        if self.desired && !self.recording && self.output.as_ref().is_some_and(|out| !out.corked())
        {
            failing.played += now.duration_since(failing.tick);
        }
        failing.tick = now;
        let eof = self.output.as_ref().is_some_and(|out| out.eof);
        if eof || failing.played >= failing.budget {
            self.enter_failed(!eof);
        }
    }
    /// Hold the failure of the decoder that last submitted audio. Every exit
    /// of a drain ends here; `forced` (bound, backend failure, nothing
    /// queued) first completes the segment timeline as a drain would, so any
    /// queued successor reports its `Transition` before this `Error`.
    fn enter_failed(&mut self, forced: bool) {
        self.failing = None;
        let end = self.output.as_ref().map_or(0, |out| out.frames);
        let position = match self.output.as_mut() {
            Some(out) if !forced && !out.eof => out.position(),
            _ => end,
        };
        self.promote(position);
        let Some(segment) = self.segments.front() else {
            return self.error(STREAM_FAILED);
        };
        let frames = position
            .min(segment.end.unwrap_or(position))
            .saturating_sub(segment.start);
        let heard = frames * 1000 / RATE as u64;
        let mut media = segment.source.resume + heard;
        if segment.source.duration > 0 {
            media = media.min(segment.source.duration);
        }
        let source = segment.source.clone();
        let listened = source.listened + heard;
        let paused = !self.desired;
        let id = source.id;
        crate::diagnostics::event(format_args!(
            "audio failure held occurrence={id} position_ms={media} heard_ms={heard} paused={paused}"
        ));
        self.desired = false;
        self.current = None;
        self.next = None;
        self.pending = None;
        self.output = None;
        self.segments.clear();
        let duration = source.duration;
        self.failed = Some(Failed {
            source,
            position: media,
            listened,
            paused,
            since: Instant::now(),
        });
        self.publish();
        self.announce();
        let _ = self.events.send(Event::Error {
            id,
            message: STREAM_FAILED,
            paused,
            acted: self.acted.load(Ordering::SeqCst),
            recoverable: true,
            position: media,
            listened,
            duration,
            progress: heard,
        });
    }
    fn resolve(&mut self, failed: u64, seq: u64, resolution: Resolution, expiry: Instant) {
        let held = self
            .failed
            .as_ref()
            .is_some_and(|held| held.source.id == failed);
        let outcome = match self.failed.take() {
            // Giving up is always honoured; a replacement only before its
            // expiry (a late one would override what the user did since).
            Some(held_failure)
                if held
                    && (matches!(resolution, Resolution::Abandon) || Instant::now() <= expiry) =>
            {
                match resolution {
                    Resolution::Abandon => {
                        self.abandon();
                        Outcome::Abandoned
                    }
                    Resolution::Recover(mut source) => {
                        source.resume = held_failure.position;
                        source.listened = held_failure.listened;
                        self.resolve_load(source, held_failure.paused)
                    }
                    Resolution::Replace(source) => self.resolve_load(source, held_failure.paused),
                }
            }
            other => {
                self.failed = other;
                Outcome::Refused
            }
        };
        crate::diagnostics::event(format_args!(
            "audio failure resolved occurrence={failed} seq={seq} outcome={outcome:?}"
        ));
        let _ = self.events.send(Event::Resolved {
            failed,
            seq,
            outcome,
        });
    }
    fn resolve_load(&mut self, source: Source, paused: bool) -> Outcome {
        let id = source.id;
        match self.load(source, paused) {
            Ok(()) => Outcome::Loaded(id),
            Err(_) => {
                self.unload("Audio playback failed");
                Outcome::LoadFailed(id)
            }
        }
    }
    /// The unloaded error state, without an `Event::Error`: the runtime
    /// learns it from the resolution outcome.
    fn unload(&mut self, message: &'static str) {
        self.desired = false;
        self.current = None;
        self.next = None;
        self.pending = None;
        self.output = None;
        self.segments.clear();
        self.failing = None;
        self.failed = None;
        let mut state = lock_state(&self.state);
        state.playing = false;
        state.paused = false;
        state.buffering = false;
        state.loaded = false;
        state.failed = false;
        state.intent = false;
        state.error = message.into();
    }
    fn abandon(&mut self) {
        self.unload(STREAM_FAILED);
    }
    /// Give up on a failure the runtime left unresolved.
    fn expire(&mut self) {
        let Some(failed) = &self.failed else {
            return;
        };
        if failed.since.elapsed() < self.recovery_limit {
            return;
        }
        let id = failed.source.id;
        crate::diagnostics::event(format_args!("audio failure expired occurrence={id}"));
        self.abandon();
        let _ = self.events.send(Event::Resolved {
            failed: id,
            seq: 0,
            outcome: Outcome::Expired,
        });
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
        // Fill first: the successor's start and the resume gate read the
        // freshly decoded state.
        if let Some(next) = &mut self.next {
            if next.fill(tail.max(DECODE_AHEAD_FRAMES)).is_err() {
                let id = next.source.id;
                let _ = self.events.send(Event::Error {
                    id,
                    message: "Next track decoding failed",
                    paused: !self.desired,
                    acted: self.acted.load(Ordering::SeqCst),
                    recoverable: false,
                    position: 0,
                    listened: 0,
                    duration: 0,
                    progress: 0,
                });
                self.next = None;
            }
        }
        // A gated stream decodes ahead as far as its threshold.
        let budget = tail
            + self.gate.map_or(DECODE_AHEAD_FRAMES, |gate| {
                DECODE_AHEAD_FRAMES.max(gate.threshold())
            });
        let Some(current) = &mut self.current else {
            return Ok(());
        };
        current.fill(budget)?;
        if let Some(segment) = self.segments.back_mut() {
            segment.source.duration = current.source.duration;
        }
        self.start_pending(tail);
        // Effective overlap can never exceed a fully-decoded short successor.
        // Without this clamp a <tail successor stalls: the normal path
        // reserves the full tail while the join path waits for more successor
        // frames that will never arrive.
        let eff_tail = match (&self.current, &self.next) {
            _ if self.fade_dropped => 0,
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
        if self.gate.is_some() {
            self.update_gate(eff_tail)?;
            if self.gate.is_some() {
                return Ok(());
            }
        }
        let Some(current) = self.current.as_mut() else {
            return Ok(());
        };
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
                // The successor cannot cover the overlap yet. Once the output
                // has run dry, play this tail unfaded rather than holding
                // audible audio back for a stalled download.
                if output.starved() {
                    crate::diagnostics::event(format_args!(
                        "audio crossfade dropped occurrence={} successor_frames={}",
                        next.source.id,
                        next.remaining()
                    ));
                    self.fade_dropped = true;
                    self.trigger = true;
                }
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
                // The successor is now the stream being played: full window.
                if let Some(reader) = &next.http {
                    reader.set_capacity(http::CAPACITY);
                }
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
        } else if output.starved() {
            // Run dry: hold the stream. A remote one resumes only with a
            // real buffer (`update_gate`); a local one with its next block.
            output.settle_drained();
            output.set_playing(false)?;
            if current.http.is_some() {
                crate::diagnostics::event(format_args!(
                    "audio buffer start kind=stall occurrence={}",
                    current.source.id
                ));
                self.gate = Some(Gate::new(GateKind::Stall));
                self.stalled = Some(Instant::now());
                self.trigger = false;
            }
        }
        Ok(())
    }
    /// Release a resume gate once enough audio is decoded, the track's end
    /// was decoded, or patience settles for less. A release below the
    /// output's prebuffer arms a trigger, so that audio plays now.
    fn update_gate(&mut self, eff_tail: usize) -> Result<()> {
        let (Some(gate), Some(current)) = (&mut self.gate, &self.current) else {
            self.gate = None;
            return Ok(());
        };
        // The server may report its emptied queue after the stall began.
        if gate.kind == GateKind::Stall {
            if let Some(output) = &mut self.output {
                output.settle_drained();
            }
        }
        let now = Instant::now();
        if self.desired && !self.recording {
            gate.waited += now.duration_since(gate.tick);
        }
        gate.tick = now;
        let pushable = if current.eof {
            current.remaining()
        } else {
            current.remaining().saturating_sub(eff_tail)
        };
        let reason = if pushable >= gate.threshold() {
            "buffered"
        } else if current.eof {
            "eof"
        } else if gate.waited >= self.gate_patience && pushable >= START_BUFFER_FRAMES {
            "patience"
        } else {
            return Ok(());
        };
        crate::diagnostics::event(format_args!(
            "audio buffer end kind={:?} reason={reason} occurrence={} waited_ms={} buffered_ms={}",
            gate.kind,
            current.source.id,
            gate.waited.as_millis(),
            pushable as u64 * 1000 / RATE as u64
        ));
        self.gate = None;
        // Only with audio to start: an armed trigger must not fire later on
        // an unrelated push.
        self.trigger |= reason != "buffered" && pushable > 0;
        if self.desired && !self.recording {
            if let Some(output) = &mut self.output {
                output.set_playing(true)?;
            }
        }
        Ok(())
    }
    /// Make audible every queued segment the output has reached, reporting
    /// each audible `Transition`.
    fn promote(&mut self, position: u64) {
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
    }
    fn publish(&mut self) {
        if let Some(failed) = &self.failed {
            let mut state = lock_state(&self.state);
            *state = Snapshot {
                id: failed.source.id,
                playing: false,
                paused: failed.paused || self.recording,
                buffering: !failed.paused && !self.recording,
                loaded: true,
                position: failed.position,
                duration: failed.source.duration,
                listened: failed.listened,
                volume: self.volume as f64,
                seekable: failed.source.duration > 0,
                error: String::new(),
                failed: true,
                intent: !failed.paused,
                starved: false,
            };
            return;
        }
        let Some(output) = &mut self.output else {
            let mut state = lock_state(&self.state);
            state.playing = false;
            state.paused = false;
            state.buffering = false;
            state.loaded = false;
            state.failed = false;
            state.intent = self.desired;
            state.starved = false;
            state.volume = self.volume as f64;
            return;
        };
        let position = if output.eof {
            output.frames
        } else {
            output.position()
        };
        let eof = output.eof;
        let playing = output.playing();
        self.promote(position);
        let Some(segment) = self.segments.front() else {
            return;
        };
        let frames = position.saturating_sub(segment.start);
        let media = segment.source.resume + frames * 1000 / RATE as u64;
        let listened = segment.source.listened + frames * 1000 / RATE as u64;
        // A drain's end is a failure, not the track's natural end.
        let eos = eof && self.desired && !self.recording && self.failing.is_none();
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
            playing: self.desired && !self.recording && playing,
            paused: !self.desired || self.recording,
            buffering: self.desired && !self.recording && !playing,
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
            failed: false,
            intent: self.desired,
            starved: self.desired
                && !self.recording
                && self.gate.is_some_and(|gate| {
                    gate.kind == GateKind::Stall || gate.since.elapsed() >= START_STARVED_AFTER
                }),
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
            self.tick();
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
    /// One iteration's work after controls: decode/submit a bounded batch,
    /// then publish and advance timers.
    fn tick(&mut self) {
        // Catch up after a delayed wake rather than supplying only 10.7ms
        // of PCM per tick. Bound each batch so transport remains responsive.
        let submitted = self.output.as_ref().map(|out| out.frames);
        for _ in 0..8 {
            if let Err(error) = self.pump() {
                let message = match error {
                    Error::Input(message) => message,
                    _ => "Audio playback failed",
                };
                if self.recover && message == STREAM_FAILED && self.current.is_some() {
                    self.start_drain();
                } else {
                    self.error(message);
                }
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
        // After a gate released below the prebuffer (or an unfaded
        // tail): start what this batch submitted.
        if self.trigger {
            if let Some(output) = &mut self.output {
                if submitted.is_some_and(|frames| output.frames > frames) {
                    crate::diagnostics::event(format_args!("audio output triggered"));
                    output.trigger();
                    self.trigger = false;
                }
            }
        }
        self.publish();
        self.drain_tick();
        self.expire();
        self.announce();
        self.diagnose();
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

#[cfg(test)]
#[path = "buffering_tests.rs"]
mod buffering_tests;

#[cfg(test)]
mod recovery_tests {
    //! The engine-held failure (Plan E1.4): drain, then hold with the
    //! user's intent until resolved.
    use super::*;

    fn wav(dir: &std::path::Path, name: &str, milliseconds: u32) -> String {
        let frames = 48 * milliseconds;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + frames * 4).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&48000u32.to_le_bytes());
        bytes.extend_from_slice(&(48000u32 * 4).to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(frames * 4).to_le_bytes());
        bytes.resize(bytes.len() + frames as usize * 4, 0x10);
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        url::Url::from_file_path(path).unwrap().to_string()
    }
    fn source(id: u64, url: &str, listened: u64) -> Source {
        Source {
            id,
            url: url.into(),
            duration: 60_000,
            listened,
            ..Default::default()
        }
    }
    struct Fixture {
        actor: Actor,
        events: mpsc::Receiver<Event>,
        state: Arc<Mutex<Snapshot>>,
        dir: tempfile::TempDir,
    }
    fn fixture() -> Fixture {
        let (sender, events) = mpsc::channel();
        let state = Arc::new(Mutex::new(Snapshot::default()));
        let mut actor = Actor::new(
            Sink::Fake,
            state.clone(),
            Arc::new(Mutex::new(Vec::new())),
            sender,
            http::SessionCloser::new().unwrap(),
        );
        actor.recover = true;
        Fixture {
            actor,
            events,
            state,
            dir: tempfile::tempdir().unwrap(),
        }
    }
    /// Run the actor's per-tick work until `done` or a timeout.
    fn run_until(fixture: &mut Fixture, done: impl Fn(&Actor) -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !done(&fixture.actor) {
            assert!(Instant::now() < end, "timed out");
            let _ = fixture.actor.pump();
            fixture.actor.publish();
            fixture.actor.drain_tick();
            fixture.actor.expire();
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn errors(events: &[Event]) -> Vec<(u64, bool, u64)> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Error {
                    id,
                    recoverable,
                    position,
                    ..
                } => Some((*id, *recoverable, *position)),
                _ => None,
            })
            .collect()
    }

    /// A playing track whose decoder fails with about 200 ms queued.
    fn failing(fixture: &mut Fixture, paused: bool) {
        let url = wav(fixture.dir.path(), "a.wav", 2000);
        fixture
            .actor
            .control(Control::Load(source(1, &url, 5_000), false))
            .unwrap();
        run_until(fixture, |actor| {
            actor.output.as_ref().is_some_and(|out| out.frames >= 9600)
        });
        if paused {
            fixture.actor.control(Control::Pause).unwrap();
        }
        fixture.actor.start_drain();
    }

    #[test]
    fn a_failing_stream_plays_out_its_queue_then_holds_the_intent() {
        let mut fixture = fixture();
        failing(&mut fixture, false);
        assert!(fixture.actor.failing.is_some());
        run_until(&mut fixture, |actor| actor.failed.is_some());
        let events: Vec<_> = fixture.events.try_iter().collect();
        let errors = errors(&events);
        assert_eq!(errors.len(), 1, "{events:?}");
        let (id, recoverable, position) = errors[0];
        assert!(recoverable && id == 1);
        // Everything submitted was played before the failure was held.
        assert!(position >= 200, "{position}");
        assert!(!events.iter().any(|e| matches!(e, Event::End { .. })));
        let state = fixture.state.lock().unwrap().clone();
        assert!(state.loaded && state.failed && state.intent && state.buffering);
        assert!(!state.playing && state.error.is_empty());
        assert!(state.listened >= 5_200, "listened counts the queued audio");
        // Transport changes the held intent; nothing reloads the dead source.
        fixture.actor.control(Control::Pause).unwrap();
        // Clamped to the decoded duration (2 s) like any Seek.
        fixture.actor.control(Control::Seek(30_000)).unwrap();
        fixture.actor.publish();
        assert_eq!(fixture.state.lock().unwrap().position, 2_000);
        fixture.actor.control(Control::Seek(1_500)).unwrap();
        fixture.actor.publish();
        let state = fixture.state.lock().unwrap().clone();
        assert!(state.failed && state.paused && !state.intent);
        assert_eq!(state.position, 1_500);
        assert!(fixture.actor.current.is_none() && fixture.actor.output.is_none());
        // Recover: the held position and intent win over the source's.
        let url = wav(fixture.dir.path(), "b.wav", 2000);
        fixture
            .actor
            .control(Control::Resolve {
                failed: 1,
                seq: 7,
                resolution: Resolution::Recover(source(2, &url, 0)),
                expiry: Instant::now() + Duration::from_secs(5),
            })
            .unwrap();
        let resolved: Vec<_> = fixture.events.try_iter().collect();
        assert!(
            matches!(
                resolved.as_slice(),
                [
                    Event::Seeked(2_000),
                    Event::Seeked(1_500),
                    Event::Resolved {
                        failed: 1,
                        seq: 7,
                        outcome: Outcome::Loaded(2)
                    }
                ]
            ),
            "{resolved:?}"
        );
        assert!(!fixture.actor.desired, "the held Pause is kept");
        let segment = fixture.actor.segments.front().unwrap();
        assert_eq!(segment.source.resume, 1_500);
        assert!(segment.source.listened >= 5_200);
    }

    #[test]
    fn stop_ends_a_held_failure_and_refuses_its_resolution() {
        let mut fixture = fixture();
        failing(&mut fixture, false);
        run_until(&mut fixture, |actor| actor.failed.is_some());
        fixture.actor.control(Control::Stop).unwrap();
        fixture
            .actor
            .control(Control::Resolve {
                failed: 1,
                seq: 1,
                resolution: Resolution::Abandon,
                expiry: Instant::now() + Duration::from_secs(5),
            })
            .unwrap();
        let events: Vec<_> = fixture.events.try_iter().collect();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::Resolved {
                outcome: Outcome::Refused,
                ..
            }
        )));
        fixture.actor.publish();
        assert!(!fixture.state.lock().unwrap().loaded);
    }

    #[test]
    fn a_paused_failure_is_held_paused_and_abandon_unloads() {
        let mut fixture = fixture();
        failing(&mut fixture, true);
        // A corked drain neither completes nor advances its bound: it
        // waits for Play.
        thread::sleep(Duration::from_millis(400));
        for _ in 0..5 {
            fixture.actor.publish();
            fixture.actor.drain_tick();
        }
        assert!(fixture.actor.failing.is_some(), "a paused drain waits");
        fixture.actor.control(Control::Play).unwrap();
        run_until(&mut fixture, |actor| actor.failed.is_some());
        fixture
            .actor
            .control(Control::Resolve {
                failed: 1,
                seq: 2,
                resolution: Resolution::Abandon,
                expiry: Instant::now() + Duration::from_secs(5),
            })
            .unwrap();
        fixture.actor.publish();
        let state = fixture.state.lock().unwrap().clone();
        assert!(!state.loaded && !state.failed && !state.error.is_empty());
        // A Seek after abandoning does not reopen the dead source.
        fixture.actor.control(Control::Seek(1000)).unwrap();
        assert!(fixture.actor.current.is_none() && fixture.actor.output.is_none());
    }

    #[test]
    fn an_unresolved_failure_expires() {
        let mut fixture = fixture();
        fixture.actor.recovery_limit = Duration::from_millis(50);
        failing(&mut fixture, false);
        run_until(&mut fixture, |actor| actor.failed.is_some());
        thread::sleep(Duration::from_millis(80));
        fixture.actor.expire();
        let events: Vec<_> = fixture.events.try_iter().collect();
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::Resolved {
                    failed: 1,
                    outcome: Outcome::Expired,
                    ..
                }
            )),
            "{events:?}"
        );
        assert!(fixture.actor.failed.is_none());
    }

    #[test]
    fn a_promoted_successor_fails_after_its_transition() {
        let mut fixture = fixture();
        let a = wav(fixture.dir.path(), "a.wav", 300);
        let b = wav(fixture.dir.path(), "b.wav", 5000);
        fixture
            .actor
            .control(Control::Load(source(1, &a, 0), false))
            .unwrap();
        fixture
            .actor
            .control(Control::Prepare(Some(source(2, &b, 0))))
            .unwrap();
        // Run until B took over decoding while A's tail is still queued.
        run_until(&mut fixture, |actor| {
            actor.segments.len() == 2
                && actor
                    .output
                    .as_ref()
                    .is_some_and(|out| out.frames > actor.segments[1].start)
        });
        fixture.actor.start_drain();
        run_until(&mut fixture, |actor| actor.failed.is_some());
        let events: Vec<_> = fixture.events.try_iter().collect();
        let transition = events
            .iter()
            .position(|e| matches!(e, Event::Transition { old: 1, new: 2, .. }));
        let error = events.iter().position(|e| {
            matches!(
                e,
                Event::Error {
                    id: 2,
                    recoverable: true,
                    ..
                }
            )
        });
        assert!(
            transition.is_some() && error.is_some() && transition < error,
            "{events:?}"
        );
    }

    #[test]
    fn a_forced_drain_still_reports_the_transition_first() {
        let mut fixture = fixture();
        let a = wav(fixture.dir.path(), "a.wav", 300);
        let b = wav(fixture.dir.path(), "b.wav", 5000);
        fixture
            .actor
            .control(Control::Load(source(1, &a, 0), false))
            .unwrap();
        fixture
            .actor
            .control(Control::Prepare(Some(source(2, &b, 0))))
            .unwrap();
        run_until(&mut fixture, |actor| {
            actor.segments.len() == 2
                && actor
                    .output
                    .as_ref()
                    .is_some_and(|out| out.frames > actor.segments[1].start)
        });
        // Paused: the drain cannot complete; force it as the bound would.
        fixture.actor.control(Control::Pause).unwrap();
        fixture.actor.start_drain();
        fixture.actor.enter_failed(true);
        let events: Vec<_> = fixture.events.try_iter().collect();
        let transition = events
            .iter()
            .position(|e| matches!(e, Event::Transition { old: 1, new: 2, .. }));
        let error = events.iter().position(|e| {
            matches!(
                e,
                Event::Error {
                    id: 2,
                    recoverable: true,
                    ..
                }
            )
        });
        assert!(
            transition.is_some() && error.is_some() && transition < error,
            "{events:?}"
        );
        assert!(fixture.actor.failed.as_ref().is_some_and(|f| f.paused));
    }

    #[test]
    fn giving_up_is_honoured_after_the_expiry_and_clears_buffering() {
        let mut fixture = fixture();
        failing(&mut fixture, false);
        run_until(&mut fixture, |actor| actor.failed.is_some());
        // A replacement past its expiry is refused; giving up never is.
        let url = wav(fixture.dir.path(), "b.wav", 1000);
        let past = Instant::now() - Duration::from_millis(1);
        fixture
            .actor
            .control(Control::Resolve {
                failed: 1,
                seq: 1,
                resolution: Resolution::Recover(source(2, &url, 0)),
                expiry: past,
            })
            .unwrap();
        fixture
            .actor
            .control(Control::Resolve {
                failed: 1,
                seq: 2,
                resolution: Resolution::Abandon,
                expiry: past,
            })
            .unwrap();
        let outcomes: Vec<_> = fixture
            .events
            .try_iter()
            .filter_map(|event| match event {
                Event::Resolved { seq, outcome, .. } => Some((seq, outcome)),
                _ => None,
            })
            .collect();
        assert_eq!(outcomes, [(1, Outcome::Refused), (2, Outcome::Abandoned)]);
        fixture.actor.publish();
        let state = fixture.state.lock().unwrap().clone();
        assert!(
            !state.loaded && !state.buffering && !state.failed,
            "{state:?}"
        );
    }

    #[test]
    fn stopping_a_held_failure_clears_buffering() {
        let mut fixture = fixture();
        failing(&mut fixture, false);
        run_until(&mut fixture, |actor| actor.failed.is_some());
        fixture.actor.publish();
        assert!(fixture.state.lock().unwrap().buffering);
        fixture.actor.control(Control::Stop).unwrap();
        fixture.actor.publish();
        let state = fixture.state.lock().unwrap().clone();
        assert!(
            !state.loaded && !state.buffering && !state.failed,
            "{state:?}"
        );
    }

    #[test]
    fn a_stalled_drain_is_forced_by_its_bound() {
        let mut fixture = fixture();
        failing(&mut fixture, false);
        // The simulated sink never reports drained while its bound is spent.
        if let Some(failing) = &mut fixture.actor.failing {
            failing.budget = Duration::ZERO;
        }
        if let Some(out) = &mut fixture.actor.output {
            out.eof = false;
        }
        fixture.actor.drain_tick();
        assert!(fixture.actor.failed.is_some());
        let events: Vec<_> = fixture.events.try_iter().collect();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::Error {
                recoverable: true,
                ..
            }
        )));
    }

    #[test]
    fn the_default_engine_keeps_todays_unloaded_error() {
        let (sender, events) = mpsc::channel();
        let mut actor = Actor::new(
            Sink::Fake,
            Arc::new(Mutex::new(Snapshot::default())),
            Arc::new(Mutex::new(Vec::new())),
            sender,
            http::SessionCloser::new().unwrap(),
        );
        assert!(!actor.recover);
        actor.error(STREAM_FAILED);
        assert!(matches!(
            events.try_recv(),
            Ok(Event::Error {
                recoverable: false,
                ..
            })
        ));
    }
}
