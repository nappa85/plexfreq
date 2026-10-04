//! Rust owns worker lifetime, audio transport and maintenance; Qt only polls events.
mod platform;
use crate::{
    audio::{self, Engine, Event, Source},
    Command, Core, Error, Prepared, Result,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone)]
pub(crate) struct Shared {
    pub(crate) requests: mpsc::SyncSender<Value>,
    pub(crate) audio: Arc<Mutex<Option<audio::Handle>>>,
    pub(crate) model: Arc<Mutex<Value>>,
    pub(crate) control: Arc<Mutex<Option<crate::cache::DownloadControl>>>,
    pub(crate) stop: Arc<AtomicBool>,
    foreground: Arc<AtomicUsize>,
    pages: Arc<AtomicUsize>,
    view: Arc<AtomicU64>,
    plans: Arc<Mutex<BTreeMap<u64, Value>>>,
    shown: Arc<AtomicU64>,
    pub(crate) seeks: Arc<Mutex<Vec<i64>>>,
}
impl Shared {
    pub(crate) fn send(&self, request: Value) -> Result<Value> {
        let op = request["op"]
            .as_str()
            .ok_or(Error::Input("Invalid request data"))?;
        if op.starts_with("audio_") && op != "audio_config" {
            let handle = self
                .audio
                .lock()
                .unwrap()
                .clone()
                .ok_or(Error::Input("Audio engine is starting"))?;
            match op {
                "audio_pause" => handle.pause()?,
                "audio_stop" => {
                    let snapshot = handle.snapshot();
                    let _ = self.requests.try_send(
                        json!({"op":"_audio_checkpoint","snapshot":snapshot,"reset":true}),
                    );
                    handle.stop()?;
                }
                "audio_seek" => handle.seek(
                    request["position"]
                        .as_u64()
                        .ok_or(Error::Input("Invalid seek position"))?,
                )?,
                "audio_volume" => handle.volume(
                    request["volume"]
                        .as_f64()
                        .ok_or(Error::Input("Invalid audio volume"))?,
                )?,
                "audio_play" | "audio_toggle" => {
                    let state = handle.snapshot();
                    if op == "audio_toggle" && state.playing {
                        handle.pause()?;
                    } else if state.loaded {
                        handle.play()?;
                    } else {
                        let model = self.model.lock().unwrap();
                        let input = if model["track"].is_object() {
                            json!({"op":"resume"})
                        } else {
                            json!({"op":"select_track","index":0})
                        };
                        drop(model);
                        return self.send(input);
                    }
                }
                _ => return Err(Error::Input("Unknown audio command")),
            }
            if op == "audio_pause" || op == "audio_toggle" {
                let _ = self.requests.try_send(
                    json!({"op":"_audio_checkpoint","snapshot":handle.snapshot(),"reset":false}),
                );
            }
            return Ok(
                json!({"accepted":true,"busy":self.busy(),"loadingMore":self.loading_more(),"resetItems":false}),
            );
        }
        let listing = is_listing(op);
        let page = is_page(op, &request);
        let foreground = !passive(op, &request);
        let replace = listing && !page && request["_refresh"] != true;
        let view_bound = listing || op == "similar_artists";
        let mut request = request;
        if replace {
            self.view.fetch_add(1, Ordering::SeqCst);
        }
        if view_bound {
            request["_view"] = json!(self.view.load(Ordering::SeqCst));
        }
        if foreground {
            self.foreground.fetch_add(1, Ordering::SeqCst);
        }
        if page {
            self.pages.fetch_add(1, Ordering::SeqCst);
        }
        request["_foreground"] = json!(foreground);
        request["_page"] = json!(page);
        if self.requests.try_send(request).is_err() {
            if foreground {
                self.foreground.fetch_sub(1, Ordering::SeqCst);
            }
            if page {
                self.pages.fetch_sub(1, Ordering::SeqCst);
            }
            return Err(Error::Input("Backend command queue is unavailable"));
        }
        Ok(
            json!({"accepted":true,"busy":self.busy(),"loadingMore":self.loading_more(),"resetItems":replace,"clearError":foreground}),
        )
    }
    pub(crate) fn playback(&self) -> audio::Snapshot {
        self.audio.lock().unwrap().as_ref().map_or(
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
        let mut model = self.model.lock().unwrap();
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
    }
    pub(crate) fn hint_network(&self, wifi: bool) {
        if let Some(control) = self.control.lock().unwrap().as_ref() {
            control.network(wifi);
        }
    }
    pub(crate) fn hint_policy(&self, wifi_only: bool, paused: bool) {
        if let Some(control) = self.control.lock().unwrap().as_ref() {
            control.policy(wifi_only, paused);
        }
    }
}
pub struct Runtime {
    shared: Shared,
    events: mpsc::Receiver<Value>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Runtime {
    pub fn new(directory: PathBuf) -> Result<Self> {
        let (requests, receiver) = mpsc::sync_channel(256);
        let (events, output) = mpsc::channel();
        let shared = Shared {
            requests,
            audio: Arc::new(Mutex::new(None)),
            model: Arc::new(Mutex::new(
                json!({"queue":{"items":[],"repeat":"off","shuffled":false},"track":null}),
            )),
            control: Arc::new(Mutex::new(None)),
            stop: Arc::new(AtomicBool::new(false)),
            foreground: Arc::new(AtomicUsize::new(0)),
            pages: Arc::new(AtomicUsize::new(0)),
            view: Arc::new(AtomicU64::new(0)),
            plans: Arc::new(Mutex::new(BTreeMap::new())),
            shown: Arc::new(AtomicU64::new(0)),
            seeks: Arc::new(Mutex::new(Vec::new())),
        };
        let state = shared.clone();
        let worker = thread::Builder::new()
            .name("plexfreq-core".into())
            .spawn(move || worker(directory, state, receiver, events))?;
        Ok(Self {
            shared,
            events: output,
            worker: Some(worker),
        })
    }
    pub fn submit(&self, request: Value) -> Result<Value> {
        if request["op"] == "download_policy" {
            self.shared.hint_policy(
                request["wifi_only"].as_bool().unwrap_or(false),
                request["paused"].as_bool().unwrap_or(false),
            );
        }
        self.shared.send(request)
    }
    pub fn poll(&self) -> Value {
        let mut events: Vec<_> = self.events.try_iter().collect();
        let playback = self.shared.playback();
        if playback.id != 0 && playback.id != self.shared.shown.load(Ordering::SeqCst) {
            if let Some(plan) = self.shared.plans.lock().unwrap().get(&playback.id).cloned() {
                self.shared.shown.store(playback.id, Ordering::SeqCst);
                self.shared.merge(&plan);
                events.push(update(plan));
            }
        }
        json!({"events":events,"playback":playback,"busy":self.shared.busy(),"loadingMore":self.shared.loading_more()})
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        let _ = self.shared.requests.try_send(json!({"op":"_shutdown"}));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn is_listing(op: &str) -> bool {
    matches!(
        op,
        "browse"
            | "library_browse"
            | "detail"
            | "artist_albums"
            | "children"
            | "playlist_items"
            | "playlists"
            | "search"
            | "offline_browse"
            | "cached_tracks"
            | "jump_artist"
    )
}
fn is_page(op: &str, input: &Value) -> bool {
    is_listing(op) && input["start"].as_u64().unwrap_or(0) > 0 && input["_replace"] != true
}
fn passive(op: &str, input: &Value) -> bool {
    input["_background"] == true
        || is_page(op, input)
        || matches!(
            op,
            "cache_status"
                | "cache_artwork"
                | "lyrics"
                | "similar_artists"
                | "alphabet"
                | "timeline"
                | "save_playback"
                | "playback_event"
                | "sync_history"
                | "playlist_choices"
                | "network_state"
        )
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
    let gain = track["Media"][0]["Part"][0]["Stream"]
        .as_array()
        .and_then(|s| s.iter().find_map(|s| s["gain"].as_f64()))
        .unwrap_or(0.) as f32;
    Ok(Source {
        id,
        url,
        duration: track["duration"].as_u64().unwrap_or(0),
        resume: plan["resumePosition"].as_u64().unwrap_or(0),
        listened: plan["resumeListened"].as_u64().unwrap_or(0),
        gain,
        album: track["parentRatingKey"].as_str().unwrap_or("").into(),
    })
}
fn visible_plan(plan: &Value) -> Value {
    let mut visible = plan.clone();
    visible.as_object_mut().unwrap().remove("stream");
    visible
}
fn artwork_delta(shared: &Shared, data: &mut Value) {
    let mut changed = false;
    for (field, target) in [
        ("localArtwork", "artwork"),
        ("localAlbumArtwork", "albumArtwork"),
    ] {
        if let Some(value) = data
            .as_object_mut()
            .unwrap()
            .remove(field)
            .and_then(|v| v.as_str().map(str::to_string))
            .filter(|s| !s.is_empty())
        {
            let mut model = shared.model.lock().unwrap();
            if model["track"].is_object() && model["track"][target] != value {
                model["track"][target] = json!(value);
                changed = true;
            }
        }
    }
    if changed {
        data["track"] = shared.model.lock().unwrap()["track"].clone();
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
    if id != current {
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
fn worker(
    directory: PathBuf,
    shared: Shared,
    receiver: mpsc::Receiver<Value>,
    events: mpsc::Sender<Value>,
) {
    let sink = if std::env::var("PLEXFREQ_AUDIO_SINK").as_deref() == Ok("fake") {
        audio::Sink::Fake
    } else {
        audio::Sink::Pulse
    };
    let engine = match Engine::new(sink) {
        Ok(engine) => engine,
        Err(error) => {
            let _=events.send(json!({"request":{"op":"status"},"response":{"ok":false,"error":error.to_string()}}));
            return;
        }
    };
    *shared.audio.lock().unwrap() = Some(engine.handle.clone());
    let mut core = match Core::new(directory) {
        Ok(core) => core,
        Err(error) => {
            let _=events.send(json!({"request":{"op":"status"},"response":{"ok":false,"error":error.to_string()}}));
            return;
        }
    };
    let _ = engine.handle.configure(core.audio_config());
    *shared.control.lock().unwrap() = Some(core.download_control());
    let platform = platform::start(shared.clone(), events.clone());
    let mut current = 0u64;
    let mut serial = 0u64;
    let mut prepared: Option<(u64, Prepared)> = None;
    let mut maintenance = Instant::now();
    let mut cache_tick = Instant::now();
    let mut history_tick = Instant::now();
    while !shared.stop.load(Ordering::SeqCst) {
        for event in engine.events() {
            match event {
                Event::Transition {
                    old,
                    new,
                    listened,
                    position,
                } => {
                    let _ = record(&mut core, old, current, position, listened, position);
                    if let Some((id, next)) = prepared.take() {
                        if id == new {
                            if let Ok(data) = core.commit_prepared(next) {
                                current = new;
                                shared.merge(&data);
                                let _ = events.send(update(data));
                            }
                        }
                    }
                    prepare(&mut core, &shared, &engine, &mut serial, &mut prepared);
                }
                Event::End {
                    id,
                    listened,
                    position,
                } => {
                    let _ = record(&mut core, id, current, position, listened, position);
                    if let Ok(data) = core.execute(Command::Next { automatic: true }) {
                        let _ =
                            start_plan(&data, &shared, &engine, &mut serial, &mut current, false);
                        shared.merge(&data);
                        let _ = events.send(update(visible_plan(&data)));
                        prepare(&mut core, &shared, &engine, &mut serial, &mut prepared);
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
                    if id == current
                        && message == "Audio decoding or streaming failed"
                        && shared.model.lock().unwrap()["playbackSource"] != "cache"
                    {
                        if let Ok(mut data) = core.execute(Command::CachedPlayback) {
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
                                let _ = events.send(update(visible_plan(&data)));
                                prepare(&mut core, &shared, &engine, &mut serial, &mut prepared);
                                continue;
                            }
                        }
                    }
                    let _=events.send(json!({"request":{"op":"_audio_error"},"response":{"ok":false,"error":message}}));
                }
                Event::Seeked(position) => {
                    shared.seeks.lock().unwrap().push(position as i64 * 1000);
                    let _ = events.send(json!({"kind":"seeked","position":position}));
                }
            }
        }
        let input = match receiver.recv_timeout(Duration::from_millis(25)) {
            Ok(input) => Some(input),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(_) => break,
        };
        if let Some(input) = input {
            let op = input["op"].as_str().unwrap_or("");
            if op == "_shutdown" {
                break;
            }
            if op == "_audio_checkpoint" {
                if let Ok(state) =
                    serde_json::from_value::<audio::Snapshot>(input["snapshot"].clone())
                {
                    let _ = record(
                        &mut core,
                        state.id,
                        current,
                        if input["reset"] == true {
                            0
                        } else {
                            state.position
                        },
                        state.listened,
                        state.duration,
                    );
                }
                continue;
            }
            let transport = matches!(
                op,
                "play"
                    | "play_album"
                    | "select_track"
                    | "next"
                    | "previous"
                    | "radio"
                    | "mix"
                    | "stop_radio"
                    | "clear_cache"
                    | "connect"
                    | "select_server"
                    | "logout"
                    | "queue_remove"
            );
            if transport {
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
            let result = if op == "audio_config" {
                serde_json::from_value::<audio::dsp::Config>(input["config"].clone())
                    .map_err(Error::from)
                    .and_then(|config| {
                        core.set_audio_config(config.clone())?;
                        engine.handle.configure(config.clone())?;
                        Ok(json!({"audioConfig":config}))
                    })
            } else {
                serde_json::from_value::<Command>(input.clone())
                    .map_err(Error::from)
                    .and_then(|command| core.execute(command))
            };
            let mut response = match result {
                Ok(mut data) => {
                    let discarded = (is_listing(op) || op == "similar_artists")
                        && input["_view"].as_u64().unwrap_or(0)
                            != shared.view.load(Ordering::SeqCst);
                    if discarded {
                        data = json!({"_discarded":true});
                    } else {
                        if matches!(op, "connect" | "select_server" | "logout") {
                            let _ = engine.handle.stop();
                            current = 0;
                            prepared = None;
                            shared.plans.lock().unwrap().clear();
                            shared.shown.store(0, Ordering::SeqCst);
                            data["track"] = Value::Null;
                            data["items"] = json!([]);
                            data["detail"] = json!({});
                            data["playlistChoices"] = json!([]);
                            data["hasMore"] = json!(false);
                            data["queue"] = json!({"items":[],"repeat":"off","shuffled":false});
                            if op == "logout" {
                                data["servers"] = json!([]);
                                data["libraries"] = json!([]);
                            }
                        }
                        if data.get("stream").is_some() {
                            let _ = start_plan(
                                &data,
                                &shared,
                                &engine,
                                &mut serial,
                                &mut current,
                                input["_paused"] == true,
                            );
                            prepare(&mut core, &shared, &engine, &mut serial, &mut prepared);
                        } else if matches!(
                            op,
                            "enqueue"
                                | "enqueue_album"
                                | "queue_move"
                                | "queue_remove"
                                | "shuffle"
                                | "repeat"
                        ) {
                            prepare(&mut core, &shared, &engine, &mut serial, &mut prepared);
                        }
                        if op == "similar_artists" || op == "lyrics" {
                            data[if op == "lyrics" {
                                "lyricsKey"
                            } else {
                                "similarKey"
                            }] = input["key"].clone();
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
            // Correlate failures as well as successes; the presentation must be
            // able to leave its loading state without accepting a stale reply.
            if matches!(op, "lyrics" | "similar_artists") && response["data"]["_discarded"] != true
            {
                if !response["data"].is_object() {
                    response["data"] = json!({});
                }
                response["data"][if op == "lyrics" {
                    "lyricsKey"
                } else {
                    "similarKey"
                }] = input["key"].clone();
            }
            if response["data"].is_object() {
                response["data"]["_listAction"] = json!(if is_listing(op) {
                    if is_page(op, &input) && response["data"]["replaceItems"] != true {
                        "append"
                    } else {
                        "replace"
                    }
                } else {
                    "none"
                });
            }
            if input["_foreground"] == true {
                shared.foreground.fetch_sub(1, Ordering::SeqCst);
            }
            if input["_page"] == true {
                shared.pages.fetch_sub(1, Ordering::SeqCst);
            }
            let _ = events.send(json!({"request":input,"response":response}));
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
            if let Ok(data) = core.execute(Command::CacheStatus) {
                let mut data = data;
                artwork_delta(&shared, &mut data);
                data["history"] =
                    core.execute(Command::Status).unwrap_or_default()["history"].clone();
                let _ = events.send(update(data));
            }
            cache_tick = Instant::now();
        }
        if history_tick.elapsed() >= Duration::from_secs(30) {
            if shared.model.lock().unwrap()["networkOnline"] == true {
                if let Ok(data) = core.execute(Command::SyncHistory) {
                    let _ = events.send(update(data));
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
    shared.stop.store(true, Ordering::SeqCst);
    drop(platform);
    *shared.control.lock().unwrap() = None;
    *shared.audio.lock().unwrap() = None;
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
    *current = *serial;
    let input = source(*current, data)?;
    shared.plans.lock().unwrap().clear();
    shared
        .plans
        .lock()
        .unwrap()
        .insert(*current, visible_plan(data));
    shared.shown.store(*current, Ordering::SeqCst);
    engine.handle.load(input, paused)
}
fn prepare(
    core: &mut Core,
    shared: &Shared,
    engine: &Engine,
    serial: &mut u64,
    prepared: &mut Option<(u64, Prepared)>,
) {
    *prepared = None;
    let _ = engine.handle.prepare(None);
    if let Ok(Some(next)) = core.prepare_next() {
        *serial += 1;
        if let Ok(source) = source(*serial, &next.plan) {
            shared
                .plans
                .lock()
                .unwrap()
                .insert(*serial, visible_plan(&next.plan));
            let _ = engine.handle.prepare(Some(source));
            *prepared = Some((*serial, next));
        }
    }
}
