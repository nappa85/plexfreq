//! Rust owns worker lifetime, audio transport and maintenance; Qt only polls events.
mod platform;
mod pulse;
use crate::{
    audio::{self, Engine, Event, Source},
    Command, Core, Error, PlaybackSource, Prepared, Result,
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
                if toggle && state.playing {
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
        let request = Request {
            command,
            echo,
            view: class.view_bound.then_some(view),
            foreground,
            page,
            paused: flags.paused,
        };
        if self
            .requests
            .try_send(WorkerMsg::Request(Box::new(request)))
            .is_err()
        {
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
    pub fn poll(&self) -> Value {
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
        if playback.id != 0 {
            let mut presented = crate::mutex_lock(&self.shared.presented);
            if playback.id != presented.shown {
                if let Some(plan) = presented.plans.get(&playback.id).cloned() {
                    presented.shown = playback.id;
                    self.shared.merge(&plan);
                    events.push(update(plan));
                }
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
fn apply_checkpoint(
    core: &mut Core,
    current: u64,
    newest: &mut BTreeMap<u64, u64>,
    checkpoint: &Checkpoint,
) {
    let state = &checkpoint.snapshot;
    if newest
        .get(&state.id)
        .is_some_and(|seq| checkpoint.seq <= *seq)
    {
        return;
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
    let started = Engine::new(sink).and_then(|engine| {
        *crate::mutex_lock(&shared.audio) = Some(engine.handle.clone());
        Ok((engine, Core::new(directory.clone())?))
    });
    let (engine, mut core) = match started {
        Ok(started) => started,
        Err(error) => return abandon(&shared, receiver, &events, &error),
    };
    let _ = engine.handle.configure(core.audio_config());
    *crate::mutex_lock(&shared.control) = Some(core.download_control());
    let platform = observers.then(|| platform::start(shared.clone(), events.clone()));
    let mut current = 0u64;
    let mut serial = 0u64;
    let mut prepared: Option<(u64, Prepared)> = None;
    let mut retry_prepare = false;
    let mut maintenance = Instant::now();
    let mut cache_tick = Instant::now();
    let mut history_tick = Instant::now();
    let mut newest_checkpoint = BTreeMap::new();
    while !shared.stop.load(Ordering::SeqCst) {
        if retry_prepare {
            retry_prepare = !prepare(&mut core, &shared, &engine, &mut serial, &mut prepared);
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
                                current = new;
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
                    retry_prepare =
                        !prepare(&mut core, &shared, &engine, &mut serial, &mut prepared);
                }
                Event::End {
                    id,
                    listened,
                    position,
                } => {
                    let radio = core.radio_active();
                    let _ = record(&mut core, id, current, position, listened, position);
                    match core.execute(Command::Next { automatic: true }) {
                        Ok(data) => {
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
                            shared.merge(&data);
                            let _ = events.send(update(visible_plan(&data)).into());
                            retry_prepare =
                                !prepare(&mut core, &shared, &engine, &mut serial, &mut prepared);
                        }
                        Err(error) if radio => {
                            let _=events.send(json!({"request":{"op":"next"},"response":{"ok":false,"error":error.to_string()}}).into());
                        }
                        Err(_) => {}
                    }
                }
                Event::Error { id, message } => {
                    let snapshot = engine.handle.snapshot();
                    let _ = record(
                        &mut core,
                        id,
                        current,
                        snapshot.position,
                        snapshot.listened,
                        snapshot.duration,
                    );
                    let source = playback_source(&shared);
                    if id == current
                        && message == "Audio decoding or streaming failed"
                        && source != Some(PlaybackSource::Cache)
                    {
                        let cached = core.execute(Command::CachedPlayback);
                        let fallback = cached.or_else(|error| {
                            if source == Some(PlaybackSource::Transcode) {
                                Err(error)
                            } else {
                                core.execute(Command::TranscodedPlayback)
                            }
                        });
                        if let Ok(mut data) = fallback {
                            data["resumePosition"] = json!(snapshot.position);
                            data["resumeListened"] = json!(snapshot.listened);
                            if start_plan(
                                &data,
                                &shared,
                                &engine,
                                &mut serial,
                                &mut current,
                                snapshot.paused,
                            )
                            .is_ok()
                            {
                                shared.merge(&data);
                                let _ = events.send(update(visible_plan(&data)).into());
                                retry_prepare = !prepare(
                                    &mut core,
                                    &shared,
                                    &engine,
                                    &mut serial,
                                    &mut prepared,
                                );
                                continue;
                            }
                        }
                    }
                    let _=events.send(json!({"request":{"op":"_audio_error"},"response":{"ok":false,"error":message}}).into());
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
            Err(_) => break,
        };
        // Drain non-blocking checkpoint overflow first so pause/stop scrobble
        // data isn't lost when the request queue was momentarily full.
        // The GUI thread never blocks; the worker applies them in order.
        for checkpoint in shared.take_checkpoints() {
            apply_checkpoint(&mut core, current, &mut newest_checkpoint, &checkpoint);
        }
        match input {
            Some(WorkerMsg::Shutdown) => break,
            Some(WorkerMsg::Checkpoint(checkpoint)) => {
                apply_checkpoint(&mut core, current, &mut newest_checkpoint, &checkpoint);
                continue;
            }
            Some(WorkerMsg::Request(request)) => {
                let Request {
                    command,
                    echo,
                    view,
                    foreground,
                    page,
                    paused,
                } = *request;
                let op = command.op();
                let class = command.class();
                let (keyed, tags) = correlation(&command);
                let logout = matches!(command, Command::Logout);
                crate::diagnostics::event(format_args!("core request begin op={op}"));
                let request_started = Instant::now();
                if class.transport {
                    crate::diagnostics::event(format_args!("core transport begin op={op}"));
                    let snapshot = engine.handle.snapshot();
                    let _ = record(
                        &mut core,
                        snapshot.id,
                        current,
                        snapshot.position,
                        snapshot.listened,
                        snapshot.duration,
                    );
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
                            recovered = Some(recover(
                                &mut core,
                                &directory,
                                &shared,
                                &engine,
                                &mut prepared,
                                current,
                            ));
                            Err(Error::Input("Backend panic"))
                        }
                    }
                };
                let cancelled = core.plex.cancelled();
                core.plex.bind_view(None);
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
                                current = 0;
                                prepared = None;
                                retry_prepare = false;
                                *crate::mutex_lock(&shared.presented) = Presented::default();
                                data["track"] = Value::Null;
                                data["items"] = json!([]);
                                data["detail"] = json!({});
                                data["playlistChoices"] = json!([]);
                                data["hasMore"] = json!(false);
                                data["queue"] = json!({"items":[],"repeat":"off","shuffled":false});
                                if logout {
                                    data["servers"] = json!([]);
                                    data["libraries"] = json!([]);
                                }
                            }
                            if data.get("stream").is_some() {
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
                shared.release(foreground, page.is_some());
                crate::diagnostics::event(format_args!(
                    "core request complete op={op} success={} elapsed_ms={}",
                    response["ok"] == true,
                    request_started.elapsed().as_millis()
                ));
                let _ = events.send(Outgoing {
                    view,
                    event: json!({"request":echo,"response":response}),
                });
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
                    retry_prepare =
                        !prepare(&mut core, &shared, &engine, &mut serial, &mut prepared);
                }
            }
            None => {}
        }
        if maintenance.elapsed() >= Duration::from_secs(5) {
            let state = engine.handle.snapshot();
            let _ = record(
                &mut core,
                state.id,
                current,
                state.position,
                state.listened,
                state.duration,
            );
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
    }
    let state = engine.handle.snapshot();
    let _ = record(
        &mut core,
        state.id,
        current,
        state.position,
        state.listened,
        state.duration,
    );
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
    let _ = engine.handle.configure(core.audio_config());
    *crate::mutex_lock(&shared.control) = Some(core.download_control());
    // Network facts arrive only on change; replay the last known ones.
    let network = {
        let model = crate::mutex_lock(&shared.model);
        (
            model["networkWifi"].as_bool(),
            model["networkOnline"].as_bool(),
        )
    };
    if let (Some(wifi), Some(online)) = network {
        let _ = core.execute(Command::NetworkState {
            wifi,
            online,
            live_hint: false,
        });
    }
    crate::diagnostics::event(format_args!("core rebuilt from persisted state"));
    core.execute(Command::Status)
}
/// Marks the runtime stopped and releases the transport/download handles when
/// the worker exits, including by unwinding. Platform observers are joined
/// before this runs; `Platform::drop` raises the same stop flag itself.
struct Exit<'a>(&'a Shared);
impl Drop for Exit<'_> {
    fn drop(&mut self) {
        self.0.stop.store(true, Ordering::SeqCst);
        *crate::mutex_lock(&self.0.control) = None;
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
) -> bool {
    if let Err(error) = engine.handle.prepare(None) {
        crate::diagnostics::event(format_args!("successor clear failed: {error}"));
        return false;
    }
    // A replaced successor's plan is no longer live.
    if let Some((id, _)) = prepared.take() {
        crate::mutex_lock(&shared.presented).plans.remove(&id);
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
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
