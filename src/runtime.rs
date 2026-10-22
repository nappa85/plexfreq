//! Rust owns worker lifetime, audio transport and maintenance; Qt only polls events.
mod platform;
mod pulse;
use crate::{
    audio::{self, Engine, Event, Source},
    plex::Probe,
    Command, Core, Error, PlaybackSource, Prepared, Result, Skip,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    panic::{catch_unwind, AssertUnwindSafe},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::watch;

/// Messages on the typed worker channel. Admission (`Shared::send`) is the
/// only producer of `Request`; checkpoints and shutdown are internal and have
/// no wire spelling, so QML/FFI can never forge them.
pub(crate) enum WorkerMsg {
    Request(Box<Request>),
    Checkpoint(Checkpoint),
    Shutdown,
}

impl WorkerMsg {
    /// Short description for tests (never the payload: a request may carry
    /// credentials).
    #[cfg(test)]
    pub(crate) fn describe(&self) -> &'static str {
        match self {
            Self::Request(request) => request.command.op(),
            Self::Checkpoint(_) => "checkpoint",
            Self::Shutdown => "shutdown",
        }
    }
}

/// One admitted command with its typed routing facts.
pub(crate) struct Request {
    command: Command,
    /// The request as submitted, echoed back in its reply event (Qt reads `op`).
    echo: Value,
    /// Navigation generation for view-bound commands.
    view: Option<u64>,
    /// Counted in the busy indicator.
    foreground: bool,
    /// Counted in the loading-more indicator; appends at this start offset.
    page: Option<usize>,
    /// Load a resulting stream paused (MPRIS next/previous while paused).
    paused: bool,
    /// Counted in `Shared::queued_transport` until dequeued.
    counted: bool,
}

#[derive(Clone)]
pub(crate) struct Checkpoint {
    snapshot: audio::Snapshot,
    reset: bool,
    seq: u64,
}

/// Wire-level routing flags QML may attach to a request.
#[derive(Clone, Copy, Default)]
struct Flags {
    /// `_background`: never raise the busy indicator.
    background: bool,
    /// `_refresh`: reload a listing in place without bumping the view.
    refresh: bool,
    /// `_replace`: a listing with a start offset still replaces the list.
    replace: bool,
    paused: bool,
}
impl Flags {
    fn wire(request: &Value) -> Self {
        Self {
            background: request["_background"] == true,
            refresh: request["_refresh"] == true,
            replace: request["_replace"] == true,
            paused: false,
        }
    }
}

/// An event for Qt plus the navigation generation of the request it answers,
/// so delivery can discard replies made stale after they finished.
pub(crate) struct Outgoing {
    view: Option<u64>,
    event: Value,
}
impl From<Value> for Outgoing {
    fn from(event: Value) -> Self {
        Self { view: None, event }
    }
}

/// How a reply's items relate to the presented list (`_listAction`).
#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum ListAction {
    Append,
    Replace,
    None,
}

/// Plans of the live audio occurrences (the audible one and its prepared
/// successor) and the occurrence whose identity the UI currently shows. One
/// lock, so the worker's transition handling and `poll` agree on who
/// publishes a newly audible track: exactly one of them does.
#[derive(Default)]
struct Presented {
    plans: BTreeMap<u64, Value>,
    shown: u64,
}

#[derive(Clone)]
pub(crate) struct Shared {
    pub(crate) requests: mpsc::SyncSender<WorkerMsg>,
    pub(crate) audio: Arc<Mutex<Option<audio::Handle>>>,
    pub(crate) model: Arc<Mutex<Value>>,
    pub(crate) control: Arc<Mutex<Option<crate::cache::DownloadControl>>>,
    pub(crate) stop: Arc<AtomicBool>,
    /// What the image provider may fetch artwork with (see `crate::artwork`).
    pub(crate) artwork: Arc<crate::artwork::Source>,
    foreground: Arc<AtomicUsize>,
    pages: Arc<AtomicUsize>,
    view: Arc<AtomicU64>,
    admission: Arc<Mutex<()>>,
    presented: Arc<Mutex<Presented>>,
    pub(crate) seeks: Arc<Mutex<Vec<i64>>>,
    /// Checkpoints that found the request queue full, coalesced to the newest
    /// per audio occurrence. Bounded by the number of distinct occurrences,
    /// which only the worker creates (one active plus one prepared at a time).
    checkpoint_overflow: Arc<Mutex<BTreeMap<u64, Checkpoint>>>,
    checkpoint_seq: Arc<AtomicU64>,
    /// Bumped on every presented-model write and queued seek, so MPRIS reacts
    /// to changes instead of polling the model.
    revision: Arc<watch::Sender<u64>>,
    /// Mirrors `view` so view-bound HTTP requests can abort when it moves.
    navigation: Arc<watch::Sender<u64>>,
    /// Count of user transport actions (engine controls and queued playback
    /// requests), bumped at admission. The audio engine stamps failures with
    /// it, so an automatic skip can tell that the user acted since.
    pub(crate) acted: Arc<AtomicU64>,
    /// Playback requests admitted but not yet taken by the worker.
    queued_transport: Arc<AtomicUsize>,
    /// Whether downloads should yield to the live stream (see
    /// `yield_downloads`). Written, like the control, under `control`'s lock.
    cache_hold: Arc<AtomicBool>,
    /// Gapless transitions committed by the worker.
    #[cfg(test)]
    transitions: Arc<AtomicUsize>,
}
impl Shared {
    fn new(requests: mpsc::SyncSender<WorkerMsg>, model: Value) -> Self {
        Self {
            requests,
            audio: Arc::new(Mutex::new(None)),
            model: Arc::new(Mutex::new(model)),
            control: Arc::new(Mutex::new(None)),
            stop: Arc::new(AtomicBool::new(false)),
            artwork: Arc::default(),
            foreground: Arc::new(AtomicUsize::new(0)),
            pages: Arc::new(AtomicUsize::new(0)),
            view: Arc::new(AtomicU64::new(0)),
            admission: Arc::new(Mutex::new(())),
            presented: Arc::new(Mutex::new(Presented::default())),
            seeks: Arc::new(Mutex::new(Vec::new())),
            checkpoint_overflow: Arc::new(Mutex::new(BTreeMap::new())),
            checkpoint_seq: Arc::new(AtomicU64::new(0)),
            revision: Arc::new(watch::channel(0).0),
            navigation: Arc::new(watch::channel(0).0),
            acted: Arc::new(AtomicU64::new(0)),
            queued_transport: Arc::new(AtomicUsize::new(0)),
            cache_hold: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            transitions: Arc::new(AtomicUsize::new(0)),
        }
    }
    pub(crate) fn touched(&self) {
        self.revision
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
    pub(crate) fn revisions(&self) -> watch::Receiver<u64> {
        self.revision.subscribe()
    }
    /// Transport changes of the current audio engine, once it exists.
    pub(crate) fn audio_changes(&self) -> Option<watch::Receiver<audio::Change>> {
        crate::mutex_lock(&self.audio)
            .as_ref()
            .map(audio::Handle::changes)
    }
    fn current_view(&self) -> u64 {
        let _guard = crate::mutex_lock(&self.admission);
        self.view.load(Ordering::SeqCst)
    }
    /// A view-bound reply whose navigation generation is no longer current.
    fn stale(&self, view: Option<u64>) -> bool {
        view.is_some_and(|view| view != self.current_view())
    }
    /// Undo the busy/loading-more admission counted by `send`.
    fn release(&self, foreground: bool, page: bool) {
        if foreground {
            self.foreground.fetch_sub(1, Ordering::SeqCst);
        }
        if page {
            self.pages.fetch_sub(1, Ordering::SeqCst);
        }
    }
    fn checkpoint(&self, snapshot: audio::Snapshot, reset: bool) {
        // Checkpoints carry listened/scrobble data: never block the GUI thread
        // on a full request queue, and never silently drop them either.
        // Fast path is non-blocking; backpressure spills to an overflow keyed
        // by occurrence that the worker drains every iteration. Within one
        // occurrence a newer checkpoint supersedes an older one (listened and
        // position only grow or are explicitly reset); distinct occurrences
        // are never evicted.
        // The sequence lets the worker discard a stale queued checkpoint that it
        // reaches after a newer one drained from the overflow.
        let seq = self.checkpoint_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let checkpoint = Checkpoint {
            snapshot,
            reset,
            seq,
        };
        if let Err(mpsc::TrySendError::Full(WorkerMsg::Checkpoint(checkpoint))) =
            self.requests.try_send(WorkerMsg::Checkpoint(checkpoint))
        {
            let mut overflow = crate::mutex_lock(&self.checkpoint_overflow);
            // Concurrent callers (GUI, MPRIS) can reach the lock out of
            // sequence order: keep the newest by sequence, not by arrival.
            match overflow.entry(checkpoint.snapshot.id) {
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    if entry.get().seq < checkpoint.seq {
                        entry.insert(checkpoint);
                    }
                }
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(checkpoint);
                }
            }
        }
    }
    /// Admit one wire request (QML/FFI JSON), parsed into a `Command` here
    /// and nowhere else.
    pub(crate) fn send(&self, request: Value) -> Result<Value> {
        let command = Command::parse(&request)?;
        self.admit(command, Flags::wire(&request), request)
    }
    /// Admit a platform request (MPRIS) whose resulting stream keeps the
    /// current paused state.
    pub(crate) fn send_paused(&self, request: Value, paused: bool) -> Result<Value> {
        let command = Command::parse(&request)?;
        let flags = Flags {
            paused,
            ..Flags::wire(&request)
        };
        self.admit(command, flags, request)
    }
    fn accepted(&self, reset_items: bool) -> Value {
        json!({"accepted":true,"busy":self.busy(),"loadingMore":self.loading_more(),"resetItems":reset_items})
    }
    /// Run a transport control against the audio engine on the calling thread.
    fn transport(&self, command: Command) -> Result<Value> {
        crate::diagnostics::event(format_args!("transport request op={}", command.op()));
        if !matches!(
            command,
            Command::AudioSeek { .. } | Command::AudioVolume { .. }
        ) {
            self.acted.fetch_add(1, Ordering::SeqCst);
        }
        let handle = crate::mutex_lock(&self.audio)
            .clone()
            .ok_or(Error::Input("Audio engine is starting"))?;
        let mut checkpoint = false;
        match command {
            Command::AudioPause => {
                handle.pause()?;
                checkpoint = true;
            }
            Command::AudioStop => {
                let snapshot = handle.snapshot();
                self.checkpoint(snapshot, true);
                handle.stop()?;
            }
            Command::AudioSeek { position } => handle.seek(position)?,
            Command::AudioVolume { volume } => handle.volume(volume)?,
            Command::AudioPlay | Command::AudioToggle => {
                let toggle = matches!(command, Command::AudioToggle);
                let state = handle.snapshot();
                // The logical intent, not audibility: a buffering, held-failure
                // or recording-held track still toggles to pause.
                if toggle && state.loaded && state.intent {
                    handle.pause()?;
                } else if state.loaded {
                    handle.play()?;
                } else {
                    let model = crate::mutex_lock(&self.model);
                    let (command, echo) = if model["track"].is_object() {
                        (Command::Resume, json!({"op":"resume"}))
                    } else {
                        (
                            Command::SelectTrack { index: 0 },
                            json!({"op":"select_track","index":0}),
                        )
                    };
                    drop(model);
                    return self.admit(command, Flags::default(), echo);
                }
                checkpoint = toggle;
            }
            _ => return Err(Error::Input("Unknown audio command")),
        }
        if checkpoint {
            self.checkpoint(handle.snapshot(), false);
        }
        Ok(self.accepted(false))
    }
    fn admit(&self, command: Command, flags: Flags, echo: Value) -> Result<Value> {
        let class = command.class();
        if class.engine {
            return self.transport(command);
        }
        if let Command::DownloadPolicy { wifi_only, paused } = &command {
            self.hint_policy(*wifi_only, *paused);
        }
        let page =
            (class.listing && command.start() > 0 && !flags.replace).then_some(command.start());
        let foreground = !(flags.background || page.is_some() || class.passive);
        let replace = class.listing && page.is_none() && !flags.refresh;
        let _admission = crate::mutex_lock(&self.admission);
        let view = self
            .view
            .load(Ordering::SeqCst)
            .wrapping_add(u64::from(replace));
        if foreground {
            self.foreground.fetch_add(1, Ordering::SeqCst);
        }
        if page.is_some() {
            self.pages.fetch_add(1, Ordering::SeqCst);
        }
        // Counted before it is visible to the worker, so the dequeue can
        // never see it uncounted.
        let counted = class.transport || matches!(command, Command::Resume);
        if counted {
            self.acted.fetch_add(1, Ordering::SeqCst);
            self.queued_transport.fetch_add(1, Ordering::SeqCst);
        }
        let request = Request {
            command,
            echo,
            view: class.view_bound.then_some(view),
            foreground,
            page,
            paused: flags.paused,
            counted,
        };
        if self
            .requests
            .try_send(WorkerMsg::Request(Box::new(request)))
            .is_err()
        {
            if counted {
                self.queued_transport.fetch_sub(1, Ordering::SeqCst);
            }
            if foreground {
                self.foreground.fetch_sub(1, Ordering::SeqCst);
            }
            if page.is_some() {
                self.pages.fetch_sub(1, Ordering::SeqCst);
            }
            return Err(Error::Input("Backend command queue is unavailable"));
        }
        self.view.store(view, Ordering::SeqCst);
        // Still under admission: aborts in-flight requests of the old view.
        self.navigation.send_if_modified(|current| {
            let moved = *current != view;
            *current = view;
            moved
        });
        let mut accepted = self.accepted(replace);
        accepted["clearError"] = json!(foreground);
        Ok(accepted)
    }
    #[cfg(test)]
    fn plan_count(&self) -> usize {
        crate::mutex_lock(&self.presented).plans.len()
    }
    /// Checkpoints that spilled past a full request queue: the newest of each
    /// occurrence, in sequence order.
    fn take_checkpoints(&self) -> Vec<Checkpoint> {
        let mut checkpoints: Vec<_> =
            std::mem::take(&mut *crate::mutex_lock(&self.checkpoint_overflow))
                .into_values()
                .collect();
        checkpoints.sort_by_key(|checkpoint| checkpoint.seq);
        checkpoints
    }
    pub(crate) fn playback(&self) -> audio::Snapshot {
        crate::mutex_lock(&self.audio).as_ref().map_or(
            audio::Snapshot {
                volume: 0.8,
                ..Default::default()
            },
            |a| a.snapshot(),
        )
    }
    pub(crate) fn busy(&self) -> bool {
        self.foreground.load(Ordering::SeqCst) > 0
    }
    fn loading_more(&self) -> bool {
        self.pages.load(Ordering::SeqCst) > 0
    }
    pub(crate) fn merge(&self, data: &Value) {
        let mut model = crate::mutex_lock(&self.model);
        if let Some(fields) = data.as_object() {
            for (key, value) in fields {
                if !matches!(
                    key.as_str(),
                    "stream" | "items" | "lyrics" | "lyricsKey" | "similarArtists" | "similarKey"
                ) {
                    model[key] = value.clone();
                }
            }
        }
        drop(model);
        self.touched();
    }
    /// Install a (new) download control, carrying the current hold over.
    fn install_control(&self, control: Option<crate::cache::DownloadControl>) {
        let mut slot = crate::mutex_lock(&self.control);
        if let Some(control) = &control {
            control.hold(self.cache_hold.load(Ordering::SeqCst));
        }
        *slot = control;
    }
    /// Record and apply whether downloads yield to the live stream.
    fn hold_downloads(&self, held: bool) {
        let slot = crate::mutex_lock(&self.control);
        self.cache_hold.store(held, Ordering::SeqCst);
        if let Some(control) = slot.as_ref() {
            control.hold(held);
        }
    }
    pub(crate) fn hint_network(&self, wifi: bool) {
        if let Some(control) = crate::mutex_lock(&self.control).as_ref() {
            control.network(wifi);
        }
    }
    pub(crate) fn hint_policy(&self, wifi_only: bool, paused: bool) {
        if let Some(control) = crate::mutex_lock(&self.control).as_ref() {
            control.policy(wifi_only, paused);
        }
    }
}
pub struct Runtime {
    shared: Shared,
    events: mpsc::Receiver<Outgoing>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Runtime {
    pub fn new(directory: PathBuf) -> Result<Self> {
        let sink = if std::env::var("PLEXFREQ_AUDIO_SINK").as_deref() == Ok("fake") {
            audio::Sink::Fake
        } else {
            audio::Sink::Pulse
        };
        Self::spawn(directory, sink, true)
    }
    /// Start the Core worker. `observers` starts the platform (D-Bus, MPRIS,
    /// libpulse recording) observers; fixtures without a session bus or
    /// PulseAudio leave them off so host state cannot influence playback.
    pub(crate) fn spawn(directory: PathBuf, sink: audio::Sink, observers: bool) -> Result<Self> {
        let (requests, receiver) = mpsc::sync_channel(256);
        let (events, output) = mpsc::channel();
        let shared = Shared::new(
            requests,
            json!({"queue":{"items":[],"repeat":"off","shuffled":false},"track":null}),
        );
        let state = shared.clone();
        let worker = thread::Builder::new()
            .name("plexfreq-core".into())
            .spawn(move || worker(directory, sink, observers, state, receiver, events))?;
        Ok(Self {
            shared,
            events: output,
            worker: Some(worker),
        })
    }
    pub fn submit(&self, request: Value) -> Result<Value> {
        self.shared.send(request)
    }
    /// The artwork source shared with the image provider.
    pub(crate) fn artwork(&self) -> Arc<crate::artwork::Source> {
        self.shared.artwork.clone()
    }
    pub fn poll(&self) -> Value {
        // Held while draining: an update the worker sends under this lock
        // (a radio refill) is delivered before any successor published below.
        let mut presented = crate::mutex_lock(&self.shared.presented);
        // A reply can finish before navigation but remain unpolled until after
        // it. Check generations at delivery as well as on the Core worker.
        let mut events: Vec<_> = self
            .events
            .try_iter()
            .map(|Outgoing { view, mut event }| {
                if self.shared.stale(view) {
                    event["response"]["data"] = json!({"_discarded":true});
                }
                event
            })
            .collect();
        let mut playback = self.shared.playback();
        // Decoder-free startup still has durable media position/length to show.
        // This is a presentation projection only: keep the zero occurrence and
        // loaded/seekable flags, and never feed it into audio checkpoints.
        if playback.id == 0 {
            let model = crate::mutex_lock(&self.shared.model);
            if model["track"].is_object() {
                playback.duration = model["track"]["duration"].as_u64().unwrap_or(0);
                playback.position = model["resumePosition"].as_u64().unwrap_or(0);
                if playback.duration > 0 {
                    playback.position = playback.position.min(playback.duration);
                }
            }
        }
        // The engine switches its occurrence at the audible transition: show
        // the prepared plan then, even while the worker is busy. The worker
        // publishes the same transition only if this did not.
        if playback.id != 0 && playback.id != presented.shown {
            if let Some(plan) = presented.plans.get(&playback.id).cloned() {
                presented.shown = playback.id;
                self.shared.merge(&plan);
                events.push(update(plan));
            }
        }
        json!({"events":events,"playback":playback,"busy":self.shared.busy(),"loadingMore":self.shared.loading_more()})
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        crate::diagnostics::event(format_args!("shutdown runtime requested"));
        self.shared.stop.store(true, Ordering::SeqCst);
        let _ = self.shared.requests.try_send(WorkerMsg::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
            crate::diagnostics::event(format_args!("shutdown core joined"));
        }
    }
}

/// Request fields copied into the reply so Qt can correlate it: a key that is
/// added unless the reply was discarded, and tags that are always added.
type Correlation = (Option<(&'static str, String)>, Vec<(&'static str, Value)>);
fn correlation(command: &Command) -> Correlation {
    match command {
        Command::Lyrics { key } => (Some(("lyricsKey", key.clone())), Vec::new()),
        Command::SimilarArtists { key } => (Some(("similarKey", key.clone())), Vec::new()),
        Command::SmartRules { key } => (None, vec![("smartKey", json!(key))]),
        Command::FilterOptions {
            section,
            kind,
            field,
        } => (
            None,
            vec![
                ("filterSection", json!(section)),
                ("filterKind", json!(kind)),
                ("filterField", json!(field)),
            ],
        ),
        _ => (None, Vec::new()),
    }
}
fn update(data: Value) -> Value {
    json!({"request":{"op":"_update"},"response":{"ok":true,"data":data}})
}
fn source(id: u64, plan: &Value) -> Result<Source> {
    let url = plan["stream"]
        .as_str()
        .ok_or(Error::Input("Invalid audio source"))?
        .to_string();
    let track = &plan["track"];
    // Prefer track gain, fall back to album gain so normalized albums without
    // per-track analysis still play at a consistent level. Search all streams
    // for track gain first: per-stream fallback would let album gain in an
    // early stream shadow track gain in a later one.
    let streams = track["Media"][0]["Part"][0]["Stream"].as_array();
    let find_gain = |name: &str| {
        streams.and_then(|streams| {
            streams
                .iter()
                .filter_map(|stream| stream[name].as_f64())
                .map(|n| n as f32)
                .find(|n| n.is_finite())
        })
    };
    let album_gain = find_gain("albumGain");
    let gain = find_gain("gain").or(album_gain).unwrap_or(0.);
    Ok(Source {
        id,
        url,
        duration: track["duration"].as_u64().unwrap_or(0),
        resume: plan["resumePosition"].as_u64().unwrap_or(0),
        listened: plan["resumeListened"].as_u64().unwrap_or(0),
        gain,
        album_gain,
        album_normalization: plan["albumNormalization"] == true,
        album: track["parentRatingKey"].as_str().unwrap_or("").into(),
    })
}
/// Typed source of the presented track (None before any plan).
fn playback_source(shared: &Shared) -> Option<PlaybackSource> {
    serde_json::from_value(crate::mutex_lock(&shared.model)["playbackSource"].clone()).ok()
}
fn visible_plan(plan: &Value) -> Value {
    let mut visible = plan.clone();
    if let Some(object) = visible.as_object_mut() {
        object.remove("stream");
    }
    visible
}
fn artwork_delta(shared: &Shared, data: &mut Value) {
    let mut changed = false;
    for (field, target) in [
        ("localArtwork", "artwork"),
        ("localAlbumArtwork", "albumArtwork"),
    ] {
        let Some(object) = data.as_object_mut() else {
            continue;
        };
        if let Some(value) = object
            .remove(field)
            .and_then(|v| v.as_str().map(str::to_string))
            .filter(|s| !s.is_empty())
        {
            let mut model = crate::mutex_lock(&shared.model);
            if model["track"].is_object() && model["track"][target] != value {
                model["track"][target] = json!(value);
                changed = true;
            }
        }
    }
    if changed {
        data["track"] = crate::mutex_lock(&shared.model)["track"].clone();
        shared.touched();
    }
}
fn record(
    core: &mut Core,
    id: u64,
    current: u64,
    position: u64,
    listened: u64,
    duration: u64,
) -> Result<()> {
    // Zero is the idle/startup sentinel, not an audible occurrence. A restored
    // queue can have a saved track before any decoder is loaded; checkpointing
    // that empty snapshot would overwrite its durable resume position with zero.
    if id == 0 || id != current {
        return Ok(());
    };
    let status = core.execute(Command::Status)?;
    let key = status["track"]["ratingKey"]
        .as_str()
        .unwrap_or("")
        .to_string();
    if key.is_empty() {
        return Ok(());
    };
    core.execute(Command::PlaybackEvent {
        key: key.clone(),
        occurrence: status["playbackOccurrence"].as_str().unwrap_or("").into(),
        listened,
        duration,
    })?;
    core.execute(Command::SavePlayback {
        key,
        position,
        generation: status["playbackGeneration"].as_u64(),
    })?;
    Ok(())
}
/// Newest applied checkpoint sequence of the most recent occurrences.
const CHECKPOINT_OCCURRENCES: usize = 8;
/// Apply a GUI-thread checkpoint unless a newer one for the same occurrence was
/// already applied. Overflow checkpoints are drained before older queued ones,
/// so arrival order alone could move the saved position backwards. Sequences
/// are tracked per occurrence: an interleaved checkpoint of another occurrence
/// must not re-admit a stale one. Occurrence ids only grow, so the oldest
/// entries are dropped first.
///
/// Returns whether a checkpoint of the current occurrence was applied, and if
/// so whether it was a stop (`reset`).
fn apply_checkpoint(
    core: &mut Core,
    current: u64,
    newest: &mut BTreeMap<u64, u64>,
    checkpoint: &Checkpoint,
) -> Option<bool> {
    let state = &checkpoint.snapshot;
    if newest
        .get(&state.id)
        .is_some_and(|seq| checkpoint.seq <= *seq)
    {
        return None;
    }
    newest.insert(state.id, checkpoint.seq);
    while newest.len() > CHECKPOINT_OCCURRENCES {
        newest.pop_first();
    }
    let _ = record(
        core,
        state.id,
        current,
        if checkpoint.reset { 0 } else { state.position },
        state.listened,
        state.duration,
    );
    (state.id == current && current != 0).then_some(checkpoint.reset)
}
fn worker(
    directory: PathBuf,
    sink: audio::Sink,
    observers: bool,
    shared: Shared,
    receiver: mpsc::Receiver<WorkerMsg>,
    events: mpsc::Sender<Outgoing>,
) {
    let _exit = Exit(&shared);
    let started = Engine::with_recovery(sink, shared.acted.clone()).and_then(|engine| {
        *crate::mutex_lock(&shared.audio) = Some(engine.handle.clone());
        Ok((engine, Core::new(directory.clone())?))
    });
    let (engine, mut core) = match started {
        Ok(started) => started,
        Err(error) => return abandon(&shared, receiver, &events, &error),
    };
    let _ = engine.handle.configure(core.audio_config());
    shared.artwork.set(core.artwork_snapshot());
    shared.install_control(Some(core.download_control()));
    // Owned here: dropped (aborted) before `_exit` releases the control.
    let _yield = crate::rt::spawn(yield_downloads(
        shared.clone(),
        engine.handle.changes(),
        HOLD_RELEASE,
    ));
    let platform = observers.then(|| platform::start(shared.clone(), events.clone()));
    let mut current = 0u64;
    let mut serial = 0u64;
    let mut prepared: Option<(u64, Prepared)> = None;
    let mut retry_prepare = false;
    // The radio refill in flight off the worker, if any.
    let mut refill: Option<mpsc::Receiver<crate::RefillFetched>> = None;
    let mut maintenance = Instant::now();
    let mut cache_tick = Instant::now();
    let mut history_tick = Instant::now();
    let mut newest_checkpoint = BTreeMap::new();
    // The occurrence the user stopped (a reset checkpoint): the engine applies
    // Stop asynchronously, so until then its snapshot still carries the old
    // position and must not overwrite the reset.
    let mut stopped: Option<u64> = None;
    let mut recovery = recovery::Recovery::new();
    core.set_health_epoch(recovery.epoch_handle.clone());
    // Direction of travel of the current occurrence (Previous → back): an
    // automatic skip over a gone item continues the same way.
    let mut back = false;
    let mut failures = Failures::default();
    macro_rules! held {
        () => {
            recovery
                .hold_prepare
                .as_ref()
                .is_some_and(|(occurrence, _)| *occurrence == current)
        };
    }
    macro_rules! playback {
        () => {
            Playback {
                core: &mut core,
                directory: &directory,
                shared: &shared,
                engine: &engine,
                events: &events,
                serial: &mut serial,
                current: &mut current,
                prepared: &mut prepared,
                recovery: &mut recovery,
                back: &mut back,
                failures: &mut failures,
            }
        };
    }
    // Recent panicking iterations: a deterministic panic must not rebuild
    // Core forever.
    let mut panics: Vec<Instant> = Vec::new();
    while !shared.stop.load(Ordering::SeqCst) {
        // A panic anywhere in an iteration (event handling, ticks, request
        // bookkeeping) costs that iteration only: Core is rebuilt from its
        // persisted state, as for a panicking request.
        let flow = catch_unwind(AssertUnwindSafe(|| -> Flow {
            #[cfg(test)]
            crate::test_hooks::check(&directory, "iteration");
            // Background health: apply finished probes, start new ones for fresh
            // failures, and settle a current-track failure whose verdict is in.
            // Runs before any request is taken this iteration, so a playback
            // request still in the channel counts as pending user intent.
            for (key, probe) in recovery.drain(&mut core) {
                if probe == Probe::Gone
                    && recovery
                        .hold_prepare
                        .as_ref()
                        .is_some_and(|(_, held)| *held == key)
                {
                    recovery.hold_prepare = None;
                    retry_prepare = true;
                }
                if recovery.waiting.as_ref().is_some_and(|w| w.key == key) {
                    if let Some(waiting) = recovery.waiting.take() {
                        retry_prepare |= playback!().settle_failure(waiting, Some(probe));
                    }
                }
            }
            // One new suspect per pass; a dropped one fails again on a later
            // cache attempt.
            // A pending failure dropped by an epoch change (e.g. the same server
            // saved again) still reports, unless playback moved on.
            if let Some(waiting) = recovery.orphaned.take() {
                if core.current_key() == Some(waiting.key.as_str()) && current == waiting.occurrence
                {
                    playback!().give_up(waiting.occurrence, Some(waiting.message.to_owned()));
                }
            }
            if let Some(key) = core
                .take_cache_suspects()
                .into_iter()
                .find(|key| core.queue_contains(key))
            {
                recovery.request(&core, &key, recovery::Class::Background);
            }
            for key in core.take_probe_requests() {
                recovery.request(&core, &key, recovery::Class::Background);
            }
            if recovery
                .waiting
                .as_ref()
                .is_some_and(|waiting| waiting.deadline <= Instant::now())
            {
                if let Some(waiting) = recovery.waiting.take() {
                    retry_prepare |= playback!().settle_failure(waiting, None);
                }
            }
            // A held failure: give up at its deadline (silently if the user
            // paused meanwhile), or run a due transcode restart once playing and
            // online.
            // Retry a resolution a full engine queue refused.
            if let Some(pending) = &mut failures.pending {
                if let Some((resolution, expiry)) = pending.unsent.take() {
                    if let Err(resolution) =
                        engine
                            .handle
                            .resolve(pending.failed, pending.seq, resolution, expiry)
                    {
                        pending.unsent = Some((*resolution, expiry));
                    }
                }
            }
            if failures.pending.is_none() {
                if let Some(held) = &failures.held {
                    let now = Instant::now();
                    let snapshot = engine.handle.snapshot();
                    let engine_holds = snapshot.failed && snapshot.id == held.id;
                    let same = core.current_key() == Some(held.key.as_str())
                        && core.occurrence() == held.occurrence
                        && held.epoch == recovery.epoch_handle.load(Ordering::SeqCst);
                    if !engine_holds && recovery.waiting.is_none() {
                        // The user stopped or replaced it; the engine let it go.
                        failures.held = None;
                    } else if engine_holds && (!same || core.offline_mode()) {
                        // Core moved on (server or queue change): release it.
                        let id = held.id;
                        recovery.waiting = None;
                        playback!().give_up(id, None);
                    } else if now >= held.deadline {
                        let (id, message) = (held.id, held.message);
                        recovery.waiting = None;
                        let banner = snapshot.intent.then(|| message.to_owned());
                        playback!().give_up(id, banner);
                    } else if held.restart_due.is_some_and(|due| {
                        due <= now
                        && snapshot.intent
                        && !core.known_offline()
                        // A queued request decides instead of a restart.
                        && shared.queued_transport.load(Ordering::SeqCst) == 0
                    }) && core.is_gone(&held.key)
                    {
                        // Learned gone meanwhile: the gone path, not a restart.
                        let id = held.id;
                        if let Some(held) = &mut failures.held {
                            held.restart_due = None;
                        }
                        playback!().verdict_or_give_up(id);
                    } else if held.restart_due.is_some_and(|due| {
                        due <= now
                            && snapshot.intent
                            && !core.known_offline()
                            && shared.queued_transport.load(Ordering::SeqCst) == 0
                    }) {
                        let (id, progress, admitted) = (held.id, held.progress, held.admitted);
                        let occurrence = held.occurrence.clone();
                        if let Some(held) = &mut failures.held {
                            held.restart_due = None;
                        }
                        if !admitted {
                            // Deferred while paused or offline: admit now; the
                            // restart itself follows after its backoff.
                            match failures.admit_restart(&occurrence, progress) {
                                Some(attempt) => {
                                    if let Some(held) = &mut failures.held {
                                        held.admitted = true;
                                        held.restart_due = Some(now + restart_backoff(attempt));
                                    }
                                }
                                None => playback!().verdict_or_give_up(id),
                            }
                        } else {
                            match core.execute(Command::RestartTranscode) {
                                Ok(data) => playback!().send_resolve(
                                    id,
                                    PendingKind::Same(with_resume(
                                        data,
                                        snapshot.position,
                                        snapshot.listened,
                                    )),
                                ),
                                Err(_) => playback!().verdict_or_give_up(id),
                            }
                        }
                    }
                }
            }
            // A radio refill owed by a move that did not wait for it is fetched
            // off the worker and applied here, between messages. It only
            // appends, so queued commands keep their indices.
            if let Some(receiver) = &refill {
                match receiver.try_recv() {
                    Ok(fetched) => {
                        refill = None;
                        // Under the presentation lock: `poll` must not publish the
                        // prepared plan between the check and its update.
                        let mut presented = crate::mutex_lock(&shared.presented);
                        let landed = match &mut prepared {
                            // Already shown: its transition commits the prepared
                            // queue, and the refill is fetched again for it.
                            Some((id, _)) if presented.shown == *id => None,
                            Some((id, next)) => {
                                let landed = core.refill_apply_landed(fetched, Some(next));
                                if landed.is_some() {
                                    if let Some(plan) = presented.plans.get_mut(id) {
                                        *plan = visible_plan(&next.plan);
                                    }
                                }
                                landed
                            }
                            None => core.refill_apply_landed(fetched, None),
                        };
                        // Sent under the lock: `poll` delivers it before it can
                        // publish the successor, whose queue already has the items.
                        let appended = landed.is_some();
                        if let Some(data) = landed {
                            shared.merge(&data);
                            let _ = events.send(update(data).into());
                        }
                        drop(presented);
                        if appended {
                            core.refill_persist();
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                    // The fetch thread died: drop the debt, the next move refills.
                    Err(mpsc::TryRecvError::Disconnected) => {
                        refill = None;
                        core.refill_abandon(prepared.as_mut().map(|(_, next)| next));
                    }
                }
            }
            if refill.is_none() {
                if let Some(job) = core.refill_job() {
                    let (sender, receiver) = mpsc::channel();
                    let spawned = std::thread::Builder::new()
                        .name("plexfreq-radio".into())
                        .spawn(move || {
                            let _ = sender.send(job.fetch());
                        });
                    match spawned {
                        Ok(_) => refill = Some(receiver),
                        Err(error) => {
                            crate::diagnostics::event(format_args!(
                                "radio refill thread failed: {error}"
                            ));
                            core.refill_abandon(prepared.as_mut().map(|(_, next)| next));
                        }
                    }
                }
            }
            // No successor for a held failure: its resolution replaces playback.
            if retry_prepare && failures.held.is_none() && failures.pending.is_none() {
                retry_prepare = !prepare(
                    &mut core,
                    &shared,
                    &engine,
                    &mut serial,
                    &mut prepared,
                    held!(),
                );
            }
            for event in engine.events() {
                match event {
                    Event::Transition {
                        old,
                        new,
                        listened,
                        position,
                    } => {
                        if core.station_radio_active() {
                            let _ = core.execute(Command::Timeline {
                                state: "stopped".into(),
                                position,
                                duration: position,
                                continuing: true,
                            });
                        }
                        let _ = record(&mut core, old, current, position, listened, position);
                        if let Some((id, next)) = prepared.take() {
                            if id == new {
                                if let Ok(data) = core.commit_prepared(next) {
                                    failures.clear();
                                    current = new;
                                    back = false;
                                    #[cfg(test)]
                                    shared.transitions.fetch_add(1, Ordering::SeqCst);
                                    let mut presented = crate::mutex_lock(&shared.presented);
                                    // Only the audible occurrence remains; `prepare`
                                    // below adds its successor.
                                    presented.plans.retain(|id, _| *id == new);
                                    shared.merge(&data);
                                    if presented.shown != new {
                                        presented.shown = new;
                                        let _ = events.send(update(data).into());
                                    }
                                }
                            }
                        }
                        retry_prepare = !prepare(
                            &mut core,
                            &shared,
                            &engine,
                            &mut serial,
                            &mut prepared,
                            held!(),
                        );
                    }
                    Event::End {
                        id,
                        listened,
                        position,
                    } => {
                        let radio = core.radio_active();
                        let _ = record(&mut core, id, current, position, listened, position);
                        let next = core.execute(Command::Next { automatic: true });
                        let gone_notice = core.take_gone_notice();
                        match next {
                            Ok(data) => {
                                failures.clear();
                                if let Err(error) = start_plan(
                                    &data,
                                    &shared,
                                    &engine,
                                    &mut serial,
                                    &mut current,
                                    false,
                                ) {
                                    let _ = events.send(json!({"request":{"op":"next"},"response":{"ok":false,"error":error.to_string()}}).into());
                                    continue;
                                }
                                back = false;
                                shared.merge(&data);
                                let _ = events.send(update(visible_plan(&data)).into());
                                if gone_notice {
                                    audio_error(&events, GONE_MESSAGE);
                                }
                                retry_prepare = !prepare(
                                    &mut core,
                                    &shared,
                                    &engine,
                                    &mut serial,
                                    &mut prepared,
                                    held!(),
                                );
                            }
                            Err(error) if radio => {
                                let _=events.send(json!({"request":{"op":"next"},"response":{"ok":false,"error":error.to_string()}}).into());
                            }
                            // Never stop silently at a track end.
                            Err(error) => audio_error(&events, &error.to_string()),
                        }
                    }
                    Event::Error {
                        id,
                        recoverable: true,
                        position,
                        listened,
                        duration,
                        progress,
                        early_end,
                        ..
                    } => {
                        if failures.pending.is_some() {
                            // Not expected (outcomes precede new failures on the
                            // event channel); the pending outcome decides.
                            crate::diagnostics::event(format_args!(
                                "held failure while resolving occurrence={id}"
                            ));
                            continue;
                        }
                        if id != current {
                            // Not the item the worker plays: end it rather than
                            // leave it held.
                            playback!().give_up(id, Some(stream_failure().to_owned()));
                            continue;
                        }
                        // The engine dropped its successor with the failing decoder.
                        if let Some((prepared_id, _)) = prepared.take() {
                            crate::mutex_lock(&shared.presented)
                                .plans
                                .remove(&prepared_id);
                        }
                        // A restarted transcode that again ends cleanly where the
                        // previous attempt did: the media is shorter than its
                        // metadata, so this is the track's real end.
                        #[cfg(test)]
                        crate::test_hooks::check(&directory, "held_failure");
                        let occurrence = core.occurrence().to_owned();
                        if early_end
                            && failures.episode.as_ref().is_some_and(|episode| {
                                episode.occurrence == occurrence
                                    && episode
                                        .early_at
                                        .is_some_and(|at| position <= at + REPEATED_EARLY_END_MS)
                            })
                        {
                            crate::diagnostics::event(format_args!(
                            "transcode ends early again at position_ms={position}; treating as the track end occurrence={id}"
                        ));
                            failures.episode = None;
                            playback!().end_held(id, listened, position);
                            continue;
                        }
                        let _ = record(&mut core, id, current, position, listened, duration);
                        failures.held = core.current_key().map(|key| Held {
                            id,
                            key: key.to_owned(),
                            occurrence: core.occurrence().to_owned(),
                            message: stream_failure(),
                            deadline: Instant::now() + HELD_LIMIT,
                            restart_due: None,
                            progress,
                            admitted: false,
                            epoch: 0,
                        });
                        if let Some(held) = &mut failures.held {
                            held.epoch = recovery.epoch_handle.load(Ordering::SeqCst);
                        } else {
                            playback!().give_up(id, Some(stream_failure().to_owned()));
                            continue;
                        }
                        playback!().recover_failure(id, progress, position, duration, listened);
                        if let Some(episode) = &mut failures.episode {
                            if episode.occurrence == occurrence {
                                episode.early_at = early_end.then_some(position);
                            }
                        }
                    }
                    Event::Resolved {
                        failed,
                        seq,
                        outcome,
                    } => {
                        let pending = match failures.pending.take() {
                            Some(pending) if pending.seq == seq => pending,
                            Some(pending)
                                if outcome == audio::Outcome::Expired
                                    && pending.failed == failed =>
                            {
                                // The engine expired the failure before our
                                // resolution reached it.
                                if playback!().settle_resolution(pending, outcome) {
                                    retry_prepare = true;
                                }
                                continue;
                            }
                            other => {
                                failures.pending = other;
                                // The engine's own expiry of a failure still held
                                // here: report it.
                                if outcome == audio::Outcome::Expired
                                    && failures.held.as_ref().is_some_and(|held| held.id == failed)
                                {
                                    failures.held = None;
                                    recovery.waiting = None;
                                    audio_error(&events, stream_failure());
                                }
                                continue;
                            }
                        };
                        if playback!().settle_resolution(pending, outcome) {
                            retry_prepare = !prepare(
                                &mut core,
                                &shared,
                                &engine,
                                &mut serial,
                                &mut prepared,
                                held!(),
                            );
                        }
                    }
                    Event::Error { id, message, .. } => {
                        // A gapless successor that failed to pre-load: drop it
                        // silently, check whether it is gone, and keep playing.
                        if prepared
                            .as_ref()
                            .is_some_and(|(prepared_id, _)| *prepared_id == id)
                        {
                            let key = prepared
                                .take()
                                .and_then(|(_, next)| next.key().map(str::to_owned));
                            crate::mutex_lock(&shared.presented).plans.remove(&id);
                            let _ = engine.handle.prepare(None);
                            crate::diagnostics::event(format_args!(
                                "prepared successor failed occurrence={id}: {message}"
                            ));
                            if let Some(key) = key {
                                // Held for this occurrence whether or not a check
                                // could start: re-preparing would fetch it again.
                                recovery.request(&core, &key, recovery::Class::Background);
                                recovery.hold_prepare = Some((current, key));
                            }
                            continue;
                        }
                        if id != current && message == "Next track decoding failed" {
                            // A successor already replaced: nothing to report.
                            crate::diagnostics::event(format_args!(
                                "stale successor error occurrence={id}: {message}"
                            ));
                            continue;
                        }
                        let snapshot = engine.handle.snapshot();
                        let _ = record(
                            &mut core,
                            id,
                            current,
                            snapshot.position,
                            snapshot.listened,
                            snapshot.duration,
                        );
                        audio_error(&events, message);
                    }
                    Event::Seeked(position) => {
                        crate::mutex_lock(&shared.seeks).push(position as i64 * 1000);
                        shared.touched();
                        let _ = events.send(json!({"kind":"seeked","position":position}).into());
                    }
                }
            }
            let input = match receiver.recv_timeout(Duration::from_millis(25)) {
                Ok(input) => Some(input),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(_) => return Flow::Stop,
            };
            // Guarded from the moment it leaves the channel: even a panic
            // before the request is handled answers it and releases its
            // admission counters.
            let mut guard = match &input {
                Some(WorkerMsg::Request(request)) => Some(Admitted::new(&shared, &events, request)),
                _ => None,
            };
            // Drain non-blocking checkpoint overflow first so pause/stop scrobble
            // data isn't lost when the request queue was momentarily full.
            // The GUI thread never blocks; the worker applies them in order.
            for checkpoint in shared.take_checkpoints() {
                if let Some(reset) =
                    apply_checkpoint(&mut core, current, &mut newest_checkpoint, &checkpoint)
                {
                    stopped = reset.then_some(current);
                }
            }
            match input {
                Some(WorkerMsg::Shutdown) => return Flow::Stop,
                Some(WorkerMsg::Checkpoint(checkpoint)) => {
                    if let Some(reset) =
                        apply_checkpoint(&mut core, current, &mut newest_checkpoint, &checkpoint)
                    {
                        stopped = reset.then_some(current);
                    }
                    return Flow::Next;
                }
                Some(WorkerMsg::Request(request)) => {
                    let Request {
                        command,
                        echo,
                        view,
                        page,
                        paused,
                        ..
                    } = *request;
                    let Some(mut admitted) = guard.take() else {
                        unreachable!("a dequeued request has its guard")
                    };
                    #[cfg(test)]
                    crate::test_hooks::check(&directory, "admitted");
                    let previous = matches!(command, Command::Previous);
                    let op = command.op();
                    let class = command.class();
                    let (keyed, tags) = correlation(&command);
                    let logout = matches!(command, Command::Logout);
                    crate::diagnostics::event(format_args!("core request begin op={op}"));
                    let request_started = Instant::now();
                    if class.transport {
                        crate::diagnostics::event(format_args!("core transport begin op={op}"));
                        let snapshot = engine.handle.snapshot();
                        if stopped != Some(snapshot.id) {
                            let _ = record(
                                &mut core,
                                snapshot.id,
                                current,
                                snapshot.position,
                                snapshot.listened,
                                snapshot.duration,
                            );
                        }
                        stopped = None;
                    }
                    let command_started = Instant::now();
                    let mut recovered = None;
                    // The successor is prepared only after the reply is queued: the
                    // engine cannot make it audible (and `poll` cannot show it)
                    // before the reply presenting its predecessor.
                    let mut reprepare = false;
                    // A view-bound command's Plex requests abort as soon as navigation
                    // makes its reply stale, instead of finishing a discarded transfer.
                    if let Some(view) = view {
                        core.plex
                            .bind_view(Some((shared.navigation.subscribe(), view)));
                    }
                    let result = if let Command::AudioConfig { config } = command {
                        core.set_audio_config(config.clone()).and_then(|()| {
                            engine.handle.configure(config.clone())?;
                            Ok(json!({"audioConfig":config}))
                        })
                    } else {
                        // Contain a Core panic to the request that caused it: the reply
                        // clears its busy state, and the Core it may have left
                        // partially mutated is replaced from persisted state.
                        match catch_unwind(AssertUnwindSafe(|| core.execute(command))) {
                            Ok(result) => result,
                            Err(_) => {
                                crate::diagnostics::event(format_args!(
                                "core request panicked op={op}; rebuilding from persisted state"
                            ));
                                recovery.reset();
                                failures.clear();
                                recovered = Some(recover(
                                    &mut core,
                                    &directory,
                                    &shared,
                                    &engine,
                                    &mut prepared,
                                    current,
                                    recovery.epoch_handle.clone(),
                                ));
                                Err(Error::Input("Backend panic"))
                            }
                        }
                    };
                    let cancelled = core.plex.cancelled();
                    core.plex.bind_view(None);
                    // Navigation stepped over gone items to a natural end.
                    let gone_notice = core.take_gone_notice();
                    if cancelled {
                        crate::diagnostics::event(format_args!(
                            "core request cancelled by navigation op={op} elapsed_ms={}",
                            command_started.elapsed().as_millis()
                        ));
                    }
                    if class.transport {
                        crate::diagnostics::event(format_args!(
                            "core transport complete op={op} success={} elapsed_ms={}",
                            result.is_ok(),
                            command_started.elapsed().as_millis()
                        ));
                    }
                    let mut audio_failure = None;
                    let mut response = match result {
                        // Navigation aborted it: the reply is stale by construction.
                        _ if cancelled => json!({"ok":true,"data":{"_discarded":true}}),
                        Ok(mut data) => {
                            if shared.stale(view) {
                                data = json!({"_discarded":true});
                            } else {
                                if class.resets_session {
                                    let _ = engine.handle.stop();
                                    failures.clear();
                                    current = 0;
                                    prepared = None;
                                    retry_prepare = false;
                                    *crate::mutex_lock(&shared.presented) = Presented::default();
                                    data["track"] = Value::Null;
                                    data["items"] = json!([]);
                                    data["detail"] = json!({});
                                    data["playlistChoices"] = json!([]);
                                    data["hasMore"] = json!(false);
                                    data["queue"] =
                                        json!({"items":[],"repeat":"off","shuffled":false});
                                    if logout {
                                        data["servers"] = json!([]);
                                        data["libraries"] = json!([]);
                                    }
                                }
                                if data.get("stream").is_some() {
                                    failures.clear();
                                    let started = start_plan(
                                        &data,
                                        &shared,
                                        &engine,
                                        &mut serial,
                                        &mut current,
                                        paused,
                                    );
                                    if let Err(error) = started {
                                        audio_failure = Some(error);
                                    } else {
                                        back = previous;
                                        reprepare = true;
                                    }
                                } else if class.reprepare {
                                    reprepare = true;
                                }
                                if let Some((field, key)) = &keyed {
                                    data[*field] = json!(key);
                                }
                                if let Some(items) = data["items"].as_array() {
                                    let mut artwork = items.clone();
                                    if data["detail"].is_object() {
                                        artwork.insert(0, data["detail"].clone());
                                    }
                                    if data["track"].is_object() {
                                        artwork.insert(0, data["track"].clone());
                                    }
                                    let _ = core.execute(Command::CacheArtwork {
                                        items: artwork
                                            .into_iter()
                                            .filter_map(|i| serde_json::from_value(i).ok())
                                            .collect(),
                                    });
                                }
                                artwork_delta(&shared, &mut data);
                                shared.merge(&data);
                            }
                            json!({"ok":true,"data":visible_plan(&data)})
                        }
                        Err(error) => json!({"ok":false,"error":error.to_string()}),
                    };
                    if let Some(error) = audio_failure {
                        response = json!({"ok":false,"error":error.to_string()});
                    }
                    // Correlate failures as well as successes; the presentation must be
                    // able to leave its loading state without accepting a stale reply.
                    let stale = shared.stale(view);
                    if stale {
                        response["data"] = json!({"_discarded":true});
                    }
                    if let Some((field, key)) = keyed {
                        if !(cancelled || stale) {
                            if !response["data"].is_object() {
                                response["data"] = json!({});
                            }
                            response["data"][field] = json!(key);
                        }
                    }
                    if (class.listing || !tags.is_empty()) && !response["data"].is_object() {
                        response["data"] = json!({});
                    }
                    for (field, value) in tags {
                        response["data"][field] = value;
                    }
                    if response["data"].is_object() {
                        if let Some(start) = page {
                            response["data"]["_pageStart"] = json!(start);
                        }
                        let action = if !class.listing {
                            ListAction::None
                        } else if page.is_some() && response["data"]["replaceItems"] != true {
                            ListAction::Append
                        } else {
                            ListAction::Replace
                        };
                        response["data"]["_listAction"] = json!(action);
                    }
                    // Before the reply: its artwork ids must resolve.
                    shared.artwork.set(core.artwork_snapshot());
                    admitted.release();
                    crate::diagnostics::event(format_args!(
                        "core request complete op={op} success={} elapsed_ms={}",
                        response["ok"] == true,
                        request_started.elapsed().as_millis()
                    ));
                    let _ = events.send(Outgoing {
                        view,
                        event: json!({"request":echo,"response":response}),
                    });
                    if gone_notice && response["ok"] == true && !(cancelled || stale) {
                        audio_error(&events, GONE_MESSAGE);
                    }
                    // After the failed reply, present the rebuilt session (queue,
                    // track, settings) so the UI drops whatever the panicking
                    // request may already have shown.
                    if recovered.is_some() {
                        reprepare = true;
                    }
                    match recovered {
                        Some(Ok(status)) => {
                            shared.merge(&status);
                            let _ = events.send(update(status).into());
                        }
                        Some(Err(error)) => {
                            let _ = events.send(json!({"request":{"op":"status"},"response":{"ok":false,"error":error.to_string()}}).into());
                        }
                        None => {}
                    }
                    if reprepare {
                        retry_prepare = !prepare(
                            &mut core,
                            &shared,
                            &engine,
                            &mut serial,
                            &mut prepared,
                            held!(),
                        );
                    }
                }
                None => {}
            }
            if maintenance.elapsed() >= Duration::from_secs(5) {
                let state = engine.handle.snapshot();
                if stopped != Some(state.id) {
                    let _ = record(
                        &mut core,
                        state.id,
                        current,
                        state.position,
                        state.listened,
                        state.duration,
                    );
                }
                if state.loaded && state.playing {
                    let _ = core.execute(Command::Timeline {
                        state: "playing".into(),
                        position: state.position,
                        duration: state.duration,
                        continuing: false,
                    });
                }
                maintenance = Instant::now();
            }
            if cache_tick.elapsed() >= Duration::from_secs(2) {
                let _ = core.execute(Command::RefreshDownloads);
                if let Ok(data) = core.execute(Command::CacheStatus) {
                    let mut data = data;
                    artwork_delta(&shared, &mut data);
                    data["history"] =
                        core.execute(Command::Status).unwrap_or_default()["history"].clone();
                    let _ = events.send(update(data).into());
                }
                cache_tick = Instant::now();
            }
            if history_tick.elapsed() >= Duration::from_secs(30) {
                if crate::mutex_lock(&shared.model)["networkOnline"] == true {
                    if let Ok(data) = core.execute(Command::SyncHistory) {
                        let _ = events.send(update(data).into());
                    }
                }
                history_tick = Instant::now();
            }
            Flow::Next
        }));
        match flow {
            Ok(Flow::Next) => {}
            Ok(Flow::Stop) => break,
            Err(_) => {
                let now = Instant::now();
                panics.retain(|at| now.duration_since(*at) < PANIC_WINDOW);
                panics.push(now);
                crate::diagnostics::event(format_args!(
                    "worker iteration panicked ({} in {}s); rebuilding from persisted state",
                    panics.len(),
                    PANIC_WINDOW.as_secs()
                ));
                if panics.len() >= PANIC_LIMIT {
                    crate::diagnostics::event(format_args!("worker keeps panicking; stopping"));
                    break;
                }
                // A panicking tick waits for its interval again.
                maintenance = now;
                cache_tick = now;
                history_tick = now;
                let rebuilt = catch_unwind(AssertUnwindSafe(|| {
                    recovery.reset();
                    failures.clear();
                    let recovered = recover(
                        &mut core,
                        &directory,
                        &shared,
                        &engine,
                        &mut prepared,
                        current,
                        recovery.epoch_handle.clone(),
                    );
                    // Before publishing: the rebuilt state's artwork ids must resolve.
                    shared.artwork.set(core.artwork_snapshot());
                    match recovered {
                        Ok(status) => {
                            shared.merge(&status);
                            let _ = events.send(update(status).into());
                        }
                        Err(error) => {
                            let _ = events.send(json!({"request":{"op":"status"},"response":{"ok":false,"error":error.to_string()}}).into());
                        }
                    }
                    // The panic may have interrupted the handling of a held
                    // failure: nothing would resolve it any more.
                    let snapshot = engine.handle.snapshot();
                    if snapshot.failed {
                        playback!().give_up(snapshot.id, None);
                    }
                }));
                if rebuilt.is_err() {
                    crate::diagnostics::event(format_args!("worker recovery panicked; stopping"));
                    break;
                }
                retry_prepare = true;
            }
        }
    }
    // Under admission: answer every request still queued (the runtime may
    // outlive a stopped worker) and apply the checkpoints the GUI queued
    // before shutdown (pause, stop); then no request can be queued any more.
    let admission = crate::mutex_lock(&shared.admission);
    let mut queued = shared.take_checkpoints();
    for message in receiver.try_iter() {
        match message {
            WorkerMsg::Checkpoint(checkpoint) => queued.push(checkpoint),
            WorkerMsg::Request(request) => {
                shared.release(request.foreground, request.page.is_some());
                let _ = events.send(Outgoing {
                    view: request.view,
                    event: json!({"request":request.echo,"response":{"ok":false,"error":"Backend command queue is unavailable"}}),
                });
            }
            WorkerMsg::Shutdown => {}
        }
    }
    drop(receiver);
    drop(admission);
    for checkpoint in queued {
        if let Some(reset) =
            apply_checkpoint(&mut core, current, &mut newest_checkpoint, &checkpoint)
        {
            stopped = reset.then_some(current);
        }
    }
    let state = engine.handle.snapshot();
    if stopped != Some(state.id) {
        let _ = record(
            &mut core,
            state.id,
            current,
            state.position,
            state.listened,
            state.duration,
        );
    }
    let _ = engine.handle.stop();
    crate::diagnostics::event(format_args!(
        "shutdown checkpoint complete; joining platform"
    ));
    drop(platform);
}
/// Replace a Core that panicked mid-command with one loaded from the session
/// files through the startup path, then re-attach what the worker had wired
/// to the old one. The audible occurrence keeps playing; its prepared
/// successor came from the old Core and is prepared again by the caller
/// once the rebuilt state is published.
///
/// The old Core is dropped first (its cache/download workers own the
/// directory); a read-only `Core::inspect` stands in meanwhile and remains,
/// persisting nothing, if the writable rebuild fails.
fn recover(
    core: &mut Core,
    directory: &std::path::Path,
    shared: &Shared,
    engine: &Engine,
    prepared: &mut Option<(u64, Prepared)>,
    current: u64,
    health_epoch: Arc<AtomicU64>,
) -> Result<Value> {
    // The prepared successor holds the old Core's queue.
    *prepared = None;
    let _ = engine.handle.prepare(None);
    crate::mutex_lock(&shared.presented)
        .plans
        .retain(|id, _| *id == current);
    let placeholder = Core::inspect(directory.into())?;
    drop(std::mem::replace(core, placeholder));
    let rebuilt = Core::new(directory.into());
    let rebuilt = rebuilt.inspect_err(|error| {
        crate::diagnostics::event(format_args!("core rebuild failed: {error}"));
    })?;
    *core = rebuilt;
    core.set_health_epoch(health_epoch);
    let _ = engine.handle.configure(core.audio_config());
    shared.install_control(Some(core.download_control()));
    // Network facts arrive only on change; replay the last known ones.
    let network = {
        let model = crate::mutex_lock(&shared.model);
        (
            model["networkWifi"].as_bool(),
            model["networkOnline"].as_bool(),
            model["networkCellular"].as_bool().unwrap_or(false),
        )
    };
    if let (Some(wifi), Some(online), cellular) = network {
        let _ = core.execute(Command::NetworkState {
            wifi,
            online,
            cellular,
            live_hint: false,
        });
    }
    crate::diagnostics::event(format_args!("core rebuilt from persisted state"));
    core.execute(Command::Status)
}
/// Panicking worker iterations tolerated within [`PANIC_WINDOW`].
const PANIC_LIMIT: usize = 3;
const PANIC_WINDOW: Duration = Duration::from_secs(60);
/// How a worker iteration ends.
enum Flow {
    Next,
    Stop,
}
/// An admitted request the worker is handling. Dropped without
/// [`Admitted::release`] (a panic before the reply), it releases the admission
/// counters and answers the request with an error, so the UI cannot stay
/// busy waiting for it.
struct Admitted<'a> {
    shared: &'a Shared,
    events: &'a mpsc::Sender<Outgoing>,
    view: Option<u64>,
    echo: Value,
    foreground: bool,
    page: bool,
    released: bool,
}
impl<'a> Admitted<'a> {
    /// Take charge of a request just dequeued: it no longer counts as queued
    /// user intent.
    fn new(shared: &'a Shared, events: &'a mpsc::Sender<Outgoing>, request: &Request) -> Self {
        if request.counted {
            shared.queued_transport.fetch_sub(1, Ordering::SeqCst);
        }
        Self {
            shared,
            events,
            view: request.view,
            echo: request.echo.clone(),
            foreground: request.foreground,
            page: request.page.is_some(),
            released: false,
        }
    }
    /// The reply is being sent normally: release the counters now.
    fn release(&mut self) {
        if !self.released {
            self.released = true;
            self.shared.release(self.foreground, self.page);
        }
    }
}
impl Drop for Admitted<'_> {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.release();
        let _ = self.events.send(Outgoing {
            view: self.view,
            event: json!({"request":self.echo,"response":{"ok":false,"error":"Backend panic"}}),
        });
    }
}
/// Marks the runtime stopped and releases the transport/download handles when
/// the worker exits, including by unwinding. Platform observers are joined
/// before this runs; `Platform::drop` raises the same stop flag itself.
struct Exit<'a>(&'a Shared);
impl Drop for Exit<'_> {
    fn drop(&mut self) {
        self.0.stop.store(true, Ordering::SeqCst);
        // A retained image provider must not keep fetching with the credential.
        self.0.artwork.set(None);
        self.0.hold_downloads(false);
        self.0.install_control(None);
        *crate::mutex_lock(&self.0.audio) = None;
    }
}
/// Startup failed: report it, then answer every request already admitted so
/// busy/loading-more counters cannot stay raised with no worker to clear them.
fn abandon(
    shared: &Shared,
    receiver: mpsc::Receiver<WorkerMsg>,
    events: &mpsc::Sender<Outgoing>,
    error: &Error,
) {
    let error = error.to_string();
    let _ = events
        .send(json!({"request":{"op":"status"},"response":{"ok":false,"error":error}}).into());
    // Admission holds this lock while enqueueing: after the drain no request can
    // slip in before the receiver drops and later sends fail as disconnected.
    let _admission = crate::mutex_lock(&shared.admission);
    for message in receiver.try_iter() {
        let WorkerMsg::Request(request) = message else {
            continue;
        };
        shared.release(request.foreground, request.page.is_some());
        let _ = events.send(Outgoing {
            view: request.view,
            event: json!({"request":request.echo,"response":{"ok":false,"error":error}}),
        });
    }
    drop(receiver);
}
/// A failure banner for the presentation (the error text is translated by
/// the adapter).
fn audio_error(events: &mpsc::Sender<Outgoing>, message: &str) {
    let _ = events.send(
        json!({"request":{"op":"_audio_error"},"response":{"ok":false,"error":message}}).into(),
    );
}
const GONE_MESSAGE: &str = crate::GONE_TRACK;
/// How long the worker pursues a held failure before giving up.
const HELD_LIMIT: Duration = Duration::from_secs(60);
/// Downloads keep yielding until the live stream has played this long
/// without starving.
const HOLD_RELEASE: Duration = Duration::from_secs(10);

/// Make downloads yield the link while the live stream is starving (Plan
/// E3): held while the engine reports `starved`, released after `release`
/// of healthy playing, or at once when nothing is playing (paused, stopped,
/// unloaded or a held failure). Driven by engine changes on the shared
/// runtime, so a busy worker loop cannot delay it.
async fn yield_downloads(
    shared: Shared,
    mut changes: watch::Receiver<audio::Change>,
    release: Duration,
) {
    let mut deadline: Option<tokio::time::Instant> = None;
    loop {
        let change = *changes.borrow_and_update();
        let held = shared.cache_hold.load(Ordering::SeqCst);
        // Paused includes a recording hold, which keeps the intent.
        let playing = change.loaded && change.intent && !change.paused && !change.failed;
        let now = tokio::time::Instant::now();
        let want = if change.starved {
            deadline = None;
            true
        } else if !playing || !held {
            deadline = None;
            false
        } else {
            let until = *deadline.get_or_insert(now + release);
            now < until
        };
        if want != held {
            shared.hold_downloads(want);
            if !want {
                deadline = None;
            }
        }
        tokio::select! {
            changed = changes.changed() => if changed.is_err() {
                shared.hold_downloads(false);
                return;
            },
            _ = async {
                match deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            } => {}
        }
    }
}
/// A restarted transcode ending early within this much of where the previous
/// attempt ended early has reached the media's real end.
const REPEATED_EARLY_END_MS: u64 = 1000;
/// Restart attempts of an interrupted transcode without new audio.
const RESTART_ATTEMPTS: u32 = 4;
/// New audio that proves a restarted transcode works again (resets attempts).
const RESTART_PROGRESS_MS: u64 = 10_000;

/// A current-track failure the engine holds (stale-queue/mobile Plan E1.4):
/// loaded, with the user's play intent, until the worker resolves it.
struct Held {
    id: u64,
    key: String,
    occurrence: String,
    message: &'static str,
    deadline: Instant,
    /// A transcode restart waiting for its backoff, the network or Play.
    restart_due: Option<Instant>,
    /// New audio before the failure, for a restart admitted only when it
    /// can run (a paused or offline failure spends no attempt meanwhile).
    progress: u64,
    admitted: bool,
    /// The health epoch at failure: a server or queue change ends it.
    epoch: u64,
}
/// A resolution sent to the engine, matched to its outcome by `seq`.
struct Pending {
    seq: u64,
    failed: u64,
    /// Not yet admitted by a full engine queue: retried each pass.
    unsent: Option<(audio::Resolution, Instant)>,
    /// The engine id the replacement loads under, if any.
    new: Option<u64>,
    kind: PendingKind,
}
enum PendingKind {
    /// The same item from another source.
    Same(Value),
    /// The next item of a gone skip.
    Replace { data: Value, back: bool },
    /// Give up, with a banner unless the user had paused.
    Abandon(Option<String>),
}
/// Restart attempts of one logical occurrence.
struct Episode {
    occurrence: String,
    attempts: u32,
    /// Media position where the last attempt's transcode ended cleanly early.
    early_at: Option<u64>,
}
#[derive(Default)]
struct Failures {
    held: Option<Held>,
    pending: Option<Pending>,
    seq: u64,
    episode: Option<Episode>,
}
impl Failures {
    /// A worker load supersedes any held failure: its late outcome is
    /// ignored (the engine already dropped the failure on that load).
    fn clear(&mut self) {
        self.held = None;
        self.pending = None;
    }
    /// Admit a transcode restart for `occurrence` after `progress` ms of new
    /// audio; returns the attempt number (1 = immediate).
    fn admit_restart(&mut self, occurrence: &str, progress: u64) -> Option<u32> {
        match &mut self.episode {
            Some(episode) if episode.occurrence == occurrence => {
                if progress >= RESTART_PROGRESS_MS {
                    episode.attempts = 1;
                } else if episode.attempts >= RESTART_ATTEMPTS {
                    return None;
                } else {
                    episode.attempts += 1;
                }
                Some(episode.attempts)
            }
            // A fresh episode needs proof the stream worked at all: a
            // transcode that never played goes to the gone check instead.
            _ if progress > 0 => {
                self.episode = Some(Episode {
                    occurrence: occurrence.to_owned(),
                    attempts: 1,
                    early_at: None,
                });
                Some(1)
            }
            _ => None,
        }
    }
}
fn restart_backoff(attempt: u32) -> Duration {
    match attempt {
        0 | 1 => Duration::ZERO,
        n => Duration::from_secs(1 << (n - 1).min(3)),
    }
}
/// The worker state an automatic skip touches.
struct Playback<'a> {
    core: &'a mut Core,
    directory: &'a std::path::Path,
    shared: &'a Shared,
    engine: &'a Engine,
    events: &'a mpsc::Sender<Outgoing>,
    serial: &'a mut u64,
    current: &'a mut u64,
    prepared: &'a mut Option<(u64, Prepared)>,
    recovery: &'a mut recovery::Recovery,
    back: &'a mut bool,
    failures: &'a mut Failures,
}
impl Playback<'_> {
    /// A user request is queued: it decides what plays next.
    fn superseded(&self) -> bool {
        self.shared.queued_transport.load(Ordering::SeqCst) > 0
    }
    /// Ask the engine to resolve its held failure `failed`. Nothing is
    /// published until the outcome (`settle_resolution`), except that the
    /// replacement's plan is registered for `poll` under its engine id.
    fn send_resolve(&mut self, failed: u64, kind: PendingKind) {
        // Engine events are ordered: a previous resolution is answered before
        // the engine can report another failure.
        debug_assert!(self.failures.pending.is_none());
        self.failures.seq += 1;
        let seq = self.failures.seq;
        let expiry = self
            .failures
            .held
            .as_ref()
            .map_or_else(|| Instant::now() + HELD_LIMIT, |held| held.deadline);
        let (resolution, new) = match &kind {
            PendingKind::Same(data) | PendingKind::Replace { data, .. } => {
                *self.serial += 1;
                match source(*self.serial, data) {
                    Ok(input) => {
                        crate::mutex_lock(&self.shared.presented)
                            .plans
                            .insert(*self.serial, visible_plan(data));
                        let resolution = if matches!(kind, PendingKind::Same(_)) {
                            audio::Resolution::Recover(input)
                        } else {
                            audio::Resolution::Replace(input)
                        };
                        (resolution, Some(*self.serial))
                    }
                    Err(_) => (audio::Resolution::Abandon, None),
                }
            }
            PendingKind::Abandon(_) => (audio::Resolution::Abandon, None),
        };
        crate::diagnostics::event(format_args!(
            "resolving held failure occurrence={failed} seq={seq} replacement={new:?}"
        ));
        let unsent = self
            .engine
            .handle
            .resolve(failed, seq, resolution, expiry)
            .err()
            .map(|resolution| (*resolution, expiry));
        self.failures.pending = Some(Pending {
            seq,
            failed,
            unsent,
            new,
            kind,
        });
    }
    /// A held failure that is really the track's end: record the listen and
    /// resolve the hold with the next item, as a gone skip does (the engine
    /// keeps the user's paused state), or end without a banner when nothing
    /// follows.
    fn end_held(&mut self, failed: u64, listened: u64, position: u64) {
        let radio = self.core.radio_active();
        let _ = record(
            self.core,
            failed,
            *self.current,
            position,
            listened,
            position,
        );
        let next = self.core.execute(Command::Next { automatic: true });
        let gone_notice = self.core.take_gone_notice();
        let data = match next {
            Ok(data) => data,
            Err(error) => {
                self.give_up(failed, (!radio).then(|| error.to_string()));
                return;
            }
        };
        if let Some((id, _)) = self.prepared.take() {
            crate::mutex_lock(&self.shared.presented).plans.remove(&id);
        }
        // Core moved on: the failed engine id must no longer record into
        // Core's (new) current item.
        *self.current = 0;
        if self.superseded() || data["stream"].as_str().unwrap_or("").is_empty() {
            self.present_unloaded(&data);
            self.give_up(failed, None);
            return;
        }
        if gone_notice {
            audio_error(self.events, GONE_MESSAGE);
        }
        self.send_resolve(failed, PendingKind::Replace { data, back: false });
    }
    fn give_up(&mut self, failed: u64, banner: Option<String>) {
        self.send_resolve(failed, PendingKind::Abandon(banner));
    }
    /// Present Core's current item without audio (a skip the user stopped,
    /// or that found nothing to play).
    fn present_unloaded(&mut self, data: &Value) {
        *self.current = 0;
        *crate::mutex_lock(&self.shared.presented) = Presented::default();
        let visible = visible_plan(data);
        self.shared.merge(&visible);
        let _ = self.events.send(update(visible).into());
    }
    /// Apply the engine's answer to the pending resolution. Returns whether
    /// gapless preparation must run.
    fn settle_resolution(&mut self, pending: Pending, outcome: audio::Outcome) -> bool {
        self.failures.held = None;
        let Pending {
            new, kind, failed, ..
        } = pending;
        if let (audio::Outcome::Loaded(loaded), PendingKind::Same(data))
        | (audio::Outcome::Loaded(loaded), PendingKind::Replace { data, .. }) = (outcome, &kind)
        {
            let mut presented = crate::mutex_lock(&self.shared.presented);
            presented.plans.retain(|id, _| *id == loaded);
            let fresh = presented.shown != loaded;
            presented.shown = loaded;
            // A failed successor stays held across a same-item recovery.
            if let Some((occurrence, _)) = &mut self.recovery.hold_prepare {
                if *occurrence == *self.current {
                    *occurrence = loaded;
                }
            }
            *self.current = loaded;
            drop(presented);
            self.shared.merge(data);
            if fresh {
                let _ = self.events.send(update(visible_plan(data)).into());
            }
            if let PendingKind::Replace { back, .. } = kind {
                *self.back = back;
            }
            self.recovery.waiting = None;
            return true;
        }
        if let Some(new) = new {
            crate::mutex_lock(&self.shared.presented).plans.remove(&new);
        }
        // Refused but still held: the replacement arrived after its expiry
        // (the user did not act). End it rather than leave it buffering.
        let snapshot = self.engine.handle.snapshot();
        if outcome == audio::Outcome::Refused && snapshot.failed && snapshot.id == failed {
            if let PendingKind::Replace { data, .. } = &kind {
                self.present_unloaded(data);
            }
            let banner = snapshot.intent.then(|| stream_failure().to_owned());
            self.give_up(failed, banner);
            return false;
        }
        let banner = match (&outcome, &kind) {
            // The user stopped or loaded something meanwhile.
            (audio::Outcome::Refused, _) => None,
            (_, PendingKind::Abandon(banner)) => banner.clone(),
            (audio::Outcome::Expired, _) => Some(stream_failure().to_owned()),
            _ => Some("Audio playback failed".to_owned()),
        };
        if let PendingKind::Replace { data, .. } = &kind {
            self.present_unloaded(data);
        }
        if let Some(banner) = banner {
            audio_error(self.events, &banner);
        }
        false
    }
    /// A held current-track failure: cache, then a transcode restart (or a
    /// transcode), then the gone check.
    fn recover_failure(
        &mut self,
        id: u64,
        progress: u64,
        position: u64,
        duration: u64,
        listened: u64,
    ) {
        let source = playback_source(self.shared);
        if source != Some(PlaybackSource::Cache) {
            if let Ok(data) = self.core.execute(Command::CachedPlayback) {
                return self
                    .send_resolve(id, PendingKind::Same(with_resume(data, position, listened)));
            }
            if source == Some(PlaybackSource::Transcode) {
                let crossfade = self.core.audio_config().crossfade_ms as u64;
                let near_end =
                    duration > 0 && duration.saturating_sub(position) <= 5000 + crossfade;
                let occurrence = self.core.occurrence().to_owned();
                let waiting = !self.engine.handle.snapshot().intent || self.core.known_offline();
                if !near_end {
                    if waiting {
                        // Admitted (and counted) only once it can run.
                        if let Some(held) = &mut self.failures.held {
                            held.restart_due = Some(Instant::now());
                        }
                        return;
                    }
                    if let Some(attempt) = self.failures.admit_restart(&occurrence, progress) {
                        let due = Instant::now() + restart_backoff(attempt);
                        crate::diagnostics::event(format_args!(
                            "transcode restart attempt={attempt} occurrence={id}"
                        ));
                        if let Some(held) = &mut self.failures.held {
                            held.restart_due = Some(due);
                            held.admitted = true;
                        }
                        // The loop top restarts once due, playing and online.
                        return;
                    }
                }
            } else if let Ok(data) = self.core.execute(Command::TranscodedPlayback) {
                return self
                    .send_resolve(id, PendingKind::Same(with_resume(data, position, listened)));
            }
        }
        self.verdict_or_give_up(id);
    }
    /// Every source failed: ask the server whether the item still exists.
    fn verdict_or_give_up(&mut self, id: u64) {
        let message = self
            .failures
            .held
            .as_ref()
            .map_or(stream_failure(), |held| held.message);
        if let Some(key) = self.core.current_key().map(str::to_owned) {
            let waiting = recovery::Waiting {
                key: key.clone(),
                occurrence: id,
                back: *self.back,
                message,
                deadline: Instant::now() + recovery::VERDICT_DEADLINE,
            };
            if self.core.is_gone(&key) {
                self.settle_failure(waiting, None);
                return;
            }
            if self
                .recovery
                .request(self.core, &key, recovery::Class::Priority)
            {
                self.recovery.waiting = Some(waiting);
                return;
            }
        }
        self.give_up(id, Some(message.to_owned()));
    }
    /// Decide on a current-track failure once its verdict (or deadline) is in.
    /// Returns whether gapless preparation must be retried.
    fn settle_failure(&mut self, waiting: recovery::Waiting, probe: Option<Probe>) -> bool {
        if self.core.current_key() != Some(waiting.key.as_str())
            || *self.current != waiting.occurrence
        {
            // The user (or playback) moved on: that load ended the failure;
            // the verdict is only recorded.
            return false;
        }
        let gone = probe == Some(Probe::Gone) || self.core.is_gone(&waiting.key);
        if !gone {
            self.give_up(waiting.occurrence, Some(waiting.message.to_owned()));
            return false;
        }
        if self.superseded() {
            self.give_up(waiting.occurrence, Some(GONE_MESSAGE.to_owned()));
            return false;
        }
        match self.core.peek_skip(waiting.back) {
            Skip::Ready | Skip::End => self.skip(&waiting),
            // A target that would wait on a metadata fetch, or nothing behind:
            // report instead of moving on its own.
            Skip::NeedsMetadata | Skip::Stuck => {
                self.give_up(waiting.occurrence, Some(GONE_MESSAGE.to_owned()));
                false
            }
        }
    }
    /// Step over a gone item in the direction of travel; the engine loads
    /// the next item with the user's current intent (`Resolution::Replace`).
    fn skip(&mut self, waiting: &recovery::Waiting) -> bool {
        let failed = waiting.occurrence;
        let command = if waiting.back {
            Command::Previous
        } else {
            Command::Next { automatic: false }
        };
        crate::diagnostics::event(format_args!(
            "skipping gone track occurrence={failed} back={}",
            waiting.back
        ));
        let result = catch_unwind(AssertUnwindSafe(|| self.core.execute(command)));
        let data = match result {
            Ok(Ok(data)) => data,
            Ok(Err(error)) => {
                self.core.take_gone_notice();
                self.give_up(failed, Some(error.to_string()));
                return false;
            }
            Err(_) => {
                crate::diagnostics::event(format_args!(
                    "gone skip panicked; rebuilding from persisted state"
                ));
                self.recovery.reset();
                match recover(
                    self.core,
                    self.directory,
                    self.shared,
                    self.engine,
                    self.prepared,
                    *self.current,
                    self.recovery.epoch_handle.clone(),
                ) {
                    Ok(status) => {
                        self.shared.merge(&status);
                        let _ = self.events.send(update(status).into());
                    }
                    Err(error) => audio_error(self.events, &error.to_string()),
                }
                self.give_up(failed, Some("Backend panic".to_owned()));
                return true;
            }
        };
        self.core.take_gone_notice();
        // The previous successor belongs to the queue position just left.
        if let Some((id, _)) = self.prepared.take() {
            crate::mutex_lock(&self.shared.presented).plans.remove(&id);
        }
        // Core moved on: the failed engine id must no longer record into
        // Core's (new) current item.
        *self.current = 0;
        if self.superseded() {
            // The user's queued request decides; show where Core now is.
            self.present_unloaded(&data);
            self.give_up(failed, None);
            return false;
        }
        if data["stream"].as_str().unwrap_or("").is_empty() {
            self.present_unloaded(&data);
            self.give_up(failed, Some(GONE_MESSAGE.to_owned()));
            return false;
        }
        self.send_resolve(
            failed,
            PendingKind::Replace {
                data,
                back: waiting.back,
            },
        );
        false
    }
}
/// The message of an unrecovered stream failure.
fn stream_failure() -> &'static str {
    "Audio decoding or streaming failed"
}
fn with_resume(mut data: Value, position: u64, listened: u64) -> Value {
    data["resumePosition"] = json!(position);
    data["resumeListened"] = json!(listened);
    data
}
fn start_plan(
    data: &Value,
    shared: &Shared,
    engine: &Engine,
    serial: &mut u64,
    current: &mut u64,
    paused: bool,
) -> Result<()> {
    if data["stream"].as_str().unwrap_or("").is_empty() {
        engine.handle.stop()?;
        return Ok(());
    };
    *serial += 1;
    let input = source(*serial, data)?;
    // Hold the presentation lock across the load: `poll` must not see the new
    // occurrence audible before it is recorded as shown, or the caller's
    // update would be published twice.
    let mut presented = crate::mutex_lock(&shared.presented);
    engine.handle.load(input, paused)?;
    *current = *serial;
    presented.plans.clear();
    presented.plans.insert(*current, visible_plan(data));
    presented.shown = *current;
    Ok(())
}
fn prepare(
    core: &mut Core,
    shared: &Shared,
    engine: &Engine,
    serial: &mut u64,
    prepared: &mut Option<(u64, Prepared)>,
    hold: bool,
) -> bool {
    if let Err(error) = engine.handle.prepare(None) {
        crate::diagnostics::event(format_args!("successor clear failed: {error}"));
        return false;
    }
    // A replaced successor's plan is no longer live.
    if let Some((id, _)) = prepared.take() {
        crate::mutex_lock(&shared.presented).plans.remove(&id);
    }
    // The successor failed to pre-load and is being checked: no gapless
    // retry until its verdict (a gone item is then stepped over).
    if hold {
        return true;
    }
    if let Ok(Some(next)) = core.prepare_next() {
        *serial += 1;
        if let Ok(source) = source(*serial, &next.plan) {
            if let Err(error) = engine.handle.prepare(Some(source)) {
                crate::diagnostics::event(format_args!("successor admission failed: {error}"));
                return false;
            }
            crate::mutex_lock(&shared.presented)
                .plans
                .insert(*serial, visible_plan(&next.plan));
            *prepared = Some((*serial, next));
        }
    }
    true
}

#[cfg(test)]
mod fixture_tests;
mod recovery;
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Plan E3: downloads yield while the engine is starving, keep yielding
    /// for the release time of healthy playing, stop at once when nothing
    /// plays, and a replaced control inherits the hold.
    #[test]
    fn downloads_yield_to_a_starving_stream() {
        let (requests, _receiver) = mpsc::sync_channel(4);
        let shared = Shared::new(requests, json!({}));
        let dir = tempfile::tempdir().unwrap();
        let cache =
            crate::cache::Cache::new(dir.path().into(), Default::default(), "fixture").unwrap();
        let gate = cache.gate();
        shared.install_control(Some(cache.control(Default::default())));
        let (changes, receiver) = watch::channel(audio::Change::default());
        let task = crate::rt::spawn(yield_downloads(
            shared.clone(),
            receiver,
            Duration::from_millis(300),
        ));
        let wait = |held: bool| {
            let until = Instant::now() + Duration::from_secs(3);
            while gate.held() != held {
                assert!(Instant::now() < until, "hold never became {held}");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let playing = audio::Change {
            loaded: true,
            intent: true,
            playing: true,
            ..Default::default()
        };
        changes.send_replace(audio::Change {
            starved: true,
            playing: false,
            ..playing
        });
        wait(true);
        // Playing again: still held until the release time has passed.
        let resumed = Instant::now();
        changes.send_replace(playing);
        wait(false);
        assert!(resumed.elapsed() >= Duration::from_millis(250));
        // A pause releases at once.
        changes.send_replace(audio::Change {
            starved: true,
            ..playing
        });
        wait(true);
        let paused = Instant::now();
        changes.send_replace(audio::Change {
            intent: false,
            playing: false,
            paused: true,
            ..playing
        });
        wait(false);
        assert!(paused.elapsed() < Duration::from_millis(250));
        // So does a recording hold (intent kept, output paused).
        changes.send_replace(audio::Change {
            starved: true,
            ..playing
        });
        wait(true);
        let recording = Instant::now();
        changes.send_replace(audio::Change {
            playing: false,
            paused: true,
            ..playing
        });
        wait(false);
        assert!(recording.elapsed() < Duration::from_millis(250));
        // A control installed while held is held too.
        changes.send_replace(audio::Change {
            starved: true,
            ..playing
        });
        wait(true);
        let replacement = tempfile::tempdir().unwrap();
        let other =
            crate::cache::Cache::new(replacement.path().into(), Default::default(), "fixture")
                .unwrap();
        shared.install_control(Some(other.control(Default::default())));
        assert!(other.gate().held());
        // The engine going away releases the hold and ends the task.
        drop(changes);
        let until = Instant::now() + Duration::from_secs(3);
        while other.gate().held() {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(task.join(Some(Duration::from_secs(1))));
    }

    #[test]
    fn idle_restart_checkpoints_preserve_saved_position() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = crate::store::Settings::default();
        settings
            .playback
            .queue
            .replace(
                vec![crate::model::Item {
                    rating_key: "1".into(),
                    kind: crate::model::ItemKind::Track,
                    duration: 2_400_026,
                    ..Default::default()
                }],
                0,
            )
            .unwrap();
        settings.playback.position = 1_266_236;
        settings.playback.listened = 1_266_236;
        settings.playback.occurrence = "saved-occurrence".into();
        crate::store::save(dir.path(), &settings).unwrap();
        {
            let mut core = Core::new(dir.path().into()).unwrap();
            // Startup has a selected saved track but no audio occurrence. Both
            // maintenance and shutdown use record; a pre-load toggle can also
            // enqueue this empty snapshot as a checkpoint.
            record(&mut core, 0, 0, 0, 0, 0).unwrap();
            apply_checkpoint(
                &mut core,
                0,
                &mut BTreeMap::new(),
                &Checkpoint {
                    snapshot: audio::Snapshot::default(),
                    reset: false,
                    seq: 0,
                },
            );
            assert_eq!(
                core.execute(Command::Status).unwrap()["resumePosition"],
                1_266_236
            );
        }
        let mut core = Core::new(dir.path().into()).unwrap();
        let status = core.execute(Command::Status).unwrap();
        assert_eq!(status["resumePosition"], 1_266_236);
        assert_eq!(status["resumeListened"], 1_266_236);
        assert_eq!(status["playbackOccurrence"], "saved-occurrence");
        assert!(status.get("stream").is_none());
        // Obsolete audio occurrences cannot overwrite the active one, but a
        // genuine seek to zero must still be saved.
        record(&mut core, 6, 7, 0, 0, 2_400_026).unwrap();
        assert_eq!(
            core.execute(Command::Status).unwrap()["resumePosition"],
            1_266_236
        );
        record(&mut core, 7, 7, 600_000, 1_266_236, 2_400_026).unwrap();
        assert_eq!(
            crate::store::load(dir.path()).unwrap().playback.position,
            600_000
        );
        record(&mut core, 7, 7, 0, 1_266_236, 2_400_026).unwrap();
        assert_eq!(crate::store::load(dir.path()).unwrap().playback.position, 0);
    }

    #[test]
    fn stale_queued_checkpoint_cannot_rewind_a_newer_overflow_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = crate::store::Settings::default();
        settings
            .playback
            .queue
            .replace(
                vec![crate::model::Item {
                    rating_key: "1".into(),
                    kind: crate::model::ItemKind::Track,
                    duration: 2_400_026,
                    ..Default::default()
                }],
                0,
            )
            .unwrap();
        crate::store::save(dir.path(), &settings).unwrap();
        let mut core = Core::new(dir.path().into()).unwrap();
        let checkpoint = |seq: u64, position: u64| Checkpoint {
            seq,
            reset: false,
            snapshot: audio::Snapshot {
                id: 7,
                position,
                listened: position,
                duration: 2_400_026,
                ..Default::default()
            },
        };
        let mut newest = BTreeMap::new();
        // The worker drains the newer overflow checkpoint before reaching the
        // older one still queued in the request channel.
        apply_checkpoint(&mut core, 7, &mut newest, &checkpoint(2, 600_000));
        apply_checkpoint(&mut core, 7, &mut newest, &checkpoint(1, 300_000));
        assert_eq!(
            crate::store::load(dir.path()).unwrap().playback.position,
            600_000
        );
        apply_checkpoint(&mut core, 7, &mut newest, &checkpoint(3, 900_000));
        assert_eq!(
            crate::store::load(dir.path()).unwrap().playback.position,
            900_000
        );
        // A checkpoint of another (already audible successor) occurrence
        // drained in between must not re-admit an older queued one.
        let mut successor = checkpoint(4, 10);
        successor.snapshot.id = 8;
        apply_checkpoint(&mut core, 7, &mut newest, &successor);
        apply_checkpoint(&mut core, 7, &mut newest, &checkpoint(2, 600_000));
        assert_eq!(
            crate::store::load(dir.path()).unwrap().playback.position,
            900_000
        );
    }

    #[test]
    fn navigation_discards_replies_that_finished_before_the_next_poll() {
        let (requests, _receiver) = mpsc::sync_channel(8);
        let (events, output) = mpsc::channel();
        let runtime = Runtime {
            shared: Shared::new(requests, json!({})),
            events: output,
            worker: None,
        };
        runtime.shared.view.store(1, Ordering::SeqCst);
        events
            .send(Outgoing {
                view: Some(1),
                event: json!({"request":{"op":"browse"},"response":{"ok":true,"data":{"items":[{"ratingKey":"old"}]}}}),
            })
            .unwrap();
        events
            .send(Outgoing {
                view: Some(1),
                event: json!({"request":{"op":"similar_artists"},"response":{"ok":false,"error":"old error"}}),
            })
            .unwrap();
        runtime
            .submit(json!({"op":"detail","key":"2","start":0}))
            .unwrap();
        events
            .send(Outgoing {
                view: Some(2),
                event: json!({"request":{"op":"detail"},"response":{"ok":true,"data":{"items":[{"ratingKey":"new"}]}}}),
            })
            .unwrap();
        let result = runtime.poll();
        assert_eq!(
            result["events"][0]["response"]["data"],
            json!({"_discarded":true})
        );
        assert_eq!(
            result["events"][1]["response"]["data"],
            json!({"_discarded":true})
        );
        assert_eq!(
            result["events"][2]["response"]["data"]["items"][0]["ratingKey"],
            "new"
        );
        // Fill admission without a worker, then reject a replacement. Good
        // replies for the admitted view and busy counters must survive.
        for _ in 0..7 {
            runtime.submit(json!({"op":"status"})).unwrap();
        }
        let foreground = runtime.shared.foreground.load(Ordering::SeqCst);
        assert!(runtime.submit(json!({"op":"browse"})).is_err());
        assert_eq!(runtime.shared.current_view(), 2);
        assert_eq!(runtime.shared.foreground.load(Ordering::SeqCst), foreground);
        events
            .send(Outgoing {
                view: Some(2),
                event: json!({"request":{"op":"detail"},"response":{"ok":true,"data":{"items":[1]}}}),
            })
            .unwrap();
        assert_eq!(
            runtime.poll()["events"][0]["response"]["data"]["items"],
            json!([1])
        );
    }

    // Production review (unfixed): track gain must win over album gain
    // globally, not just within the first stream that happens to have a value.
    // Current `find_map(|s| gain.or(albumGain))` picks albumGain from stream 0
    // and never looks at track gain in stream 1.
    #[test]
    fn track_gain_preferred_over_album_gain_across_streams() {
        let plan = json!({
            "stream": "file:///tmp/fixture.mp3",
            "resumePosition": 0,
            "resumeListened": 0,
            "track": {
                "duration": 1000,
                "parentRatingKey": "1",
                "Media": [{"Part": [{"Stream": [{"albumGain": 10.0}, {"gain": 1.0}]}]}]
            }
        });
        let source = source(7, &plan).unwrap();
        assert_eq!(
            source.gain, 1.0,
            "track gain (1.0) must win over album gain (10.0) from another stream"
        );
        let mut album_plan = plan.clone();
        album_plan["normalizationMode"] = json!("album");
        assert_eq!(super::source(8, &album_plan).unwrap().album_gain, Some(10.));
        album_plan["track"]["Media"][0]["Part"][0]["Stream"][0]["albumGain"] = json!(1e300);
        assert_eq!(super::source(9, &album_plan).unwrap().album_gain, None);
    }
}
