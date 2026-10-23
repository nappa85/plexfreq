//! Runtime fixtures driving the real Core worker with a simulated audio sink,
//! no platform observers (no D-Bus or host PulseAudio) and sessions in
//! temporary directories with dummy tokens.
use super::*;
use crate::model::{Item, ItemKind, Media, Part};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
};

fn track(key: usize, duration: u64) -> Item {
    Item {
        rating_key: key.to_string(),
        kind: ItemKind::Track,
        title: format!("Track {key}"),
        duration,
        parent_rating_key: "900".into(),
        media: vec![Media {
            audio_codec: "pcm".into(),
            container: "wav".into(),
            parts: vec![Part {
                key: format!("/library/parts/{key}/file.wav"),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn wav(milliseconds: u32) -> Vec<u8> {
    let frames = 48 * milliseconds;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + frames * 2).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&48000u32.to_le_bytes());
    bytes.extend_from_slice(&96000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(frames * 2).to_le_bytes());
    bytes.resize(bytes.len() + frames as usize * 2, 0x10);
    bytes
}

/// Serves the same short WAV for every media part and an empty container for
/// every other Plex request (timeline, scrobble, metadata).
fn serve(mut stream: TcpStream, audio: Arc<Vec<u8>>) {
    let mut request = Vec::new();
    let mut byte = [0];
    while !request.ends_with(b"\r\n\r\n") {
        if !matches!(stream.read(&mut byte), Ok(1)) {
            return;
        }
        request.push(byte[0]);
    }
    let line = String::from_utf8_lossy(&request)
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned();
    let (kind, body) = if line.contains("/library/parts/") {
        ("audio/wav", audio.to_vec())
    } else {
        (
            "application/json",
            json!({"MediaContainer":{"size":0}})
                .to_string()
                .into_bytes(),
        )
    };
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(&body);
}

fn fixture_server(milliseconds: u32) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let audio = Arc::new(wav(milliseconds));
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let audio = audio.clone();
            thread::spawn(move || serve(stream, audio));
        }
    });
    origin
}

fn reply(runtime: &Runtime, op: &str, seen: &mut Vec<Value>) -> Value {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if let Some(events) = runtime.poll()["events"].as_array() {
            let mut found = None;
            for event in events {
                if found.is_none() && event["request"]["op"] == op {
                    found = Some(event.clone());
                } else {
                    seen.push(event.clone());
                }
            }
            if let Some(event) = found {
                return event;
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("no reply for {op}");
}

#[test]
fn a_radio_refresh_warning_reaches_poll_without_becoming_a_playback_plan() {
    let server =
        crate::radio_health_tests::server(|_| ("500 Internal Server Error", String::new()));
    let dir = tempfile::tempdir().unwrap();
    drop(crate::radio_health_tests::station(
        dir.path(),
        &server,
        3,
        2,
        &[1, 2, 3],
    ));
    let runtime = Runtime::spawn(dir.path().into(), audio::Sink::Fake, false).unwrap();
    runtime
        .submit(json!({"op":"next","automatic":false}))
        .unwrap();
    let mut seen = Vec::new();
    reply(&runtime, "next", &mut seen);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        seen.extend(runtime.poll()["events"].as_array().unwrap().iter().cloned());
        if let Some(warning) = seen.iter().find(|event| event["kind"] == "radio_warning") {
            assert!(warning.get("request").is_none());
            assert!(warning.get("response").is_none());
            break;
        }
        assert!(Instant::now() < deadline, "radio warning was not drained");
        thread::sleep(Duration::from_millis(5));
    }
    assert!(crate::mutex_lock(&runtime.shared.model)
        .get("radio_warning")
        .is_none());
}

#[test]
fn failed_refill_waits_for_presentation_and_preserves_published_successor_debt() {
    let server = crate::radio_health_tests::server(|_| {
        ("200 OK", crate::radio_health_tests::station_json(&[4]))
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = crate::radio_health_tests::station(dir.path(), &server, 3, 0, &[2, 3]);
    core.execute(Command::Next { automatic: false }).unwrap();
    let identity = core.refill_identity().unwrap();
    let mut prepared = Some((42, core.prepare_next().unwrap().unwrap()));
    let (requests, _) = mpsc::sync_channel(1);
    let shared = Shared::new(requests, json!({}));
    // Model poll holding the presentation lock before the failure handler can
    // inspect it; the handler must wait and see publication before mutating.
    let mut presented = crate::mutex_lock(&shared.presented);
    let (started, ready) = mpsc::channel();
    thread::scope(|scope| {
        let shared = &shared;
        let core = &mut core;
        let prepared = &mut prepared;
        let identity = &identity;
        let failed = scope.spawn(move || {
            started.send(()).unwrap();
            failed_refill(core, shared, identity, prepared);
        });
        ready.recv().unwrap();
        presented.shown = 42;
        drop(presented);
        failed.join().unwrap();
    });
    assert!(!core.take_radio_warning());
    assert_eq!(core.radio.as_ref().unwrap().refill.pending, 1);
    core.commit_prepared(prepared.take().unwrap().1).unwrap();
    assert_eq!(core.radio.as_ref().unwrap().refill.pending, 2);
}

/// Session facts that must match what a fresh startup loads from disk.
fn persisted_view(mut status: Value) -> Value {
    if let Some(object) = status.as_object_mut() {
        object.remove("cache");
        object.remove("_listAction");
    }
    status
}

#[test]
fn panicking_request_rebuilds_core_from_persisted_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = crate::store::Settings {
        client_identifier: "fixture-client".into(),
        ..Default::default()
    };
    settings
        .playback
        .queue
        .replace(vec![track(1, 1000)], 0)
        .unwrap();
    settings.playback.position = 400;
    settings.playback.occurrence = "fixture-occurrence".into();
    crate::store::save(dir.path(), &settings).unwrap();
    let runtime = Runtime::spawn(dir.path().into(), audio::Sink::Fake, false).unwrap();
    let mut seen = Vec::new();

    // Enqueue changes the in-memory queue, then the injected panic fires
    // before the session is saved.
    crate::test_hooks::panic_after(dir.path(), "enqueue");
    let items = serde_json::to_value(vec![track(2, 1000)]).unwrap();
    runtime
        .submit(json!({"op":"enqueue","items":items}))
        .unwrap();
    let failed = reply(&runtime, "enqueue", &mut seen);
    assert_eq!(failed["response"]["ok"], false, "{failed}");
    assert_eq!(failed["response"]["error"], "Backend panic", "{failed}");

    let fresh = persisted_view(
        Core::inspect(dir.path().into())
            .unwrap()
            .execute(Command::Status)
            .unwrap(),
    );
    assert_eq!(fresh["queue"]["items"].as_array().unwrap().len(), 1);
    runtime.submit(json!({"op":"status"})).unwrap();
    let status = reply(&runtime, "status", &mut seen);
    assert_eq!(
        persisted_view(status["response"]["data"].clone()),
        fresh,
        "the worker kept a partially mutated Core"
    );
    // The rebuilt presentation reached the UI as well as the error reply.
    let rebuilt = seen
        .iter()
        .find(|event| {
            event["request"]["op"] == "_update" && event["response"]["data"]["queue"].is_object()
        })
        .unwrap_or_else(|| panic!("no rebuilt-state update in {seen:?}"));
    assert_eq!(
        rebuilt["response"]["data"]["queue"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        crate::mutex_lock(&runtime.shared.model)["queue"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Later requests are served by the rebuilt Core and persist normally.
    runtime
        .submit(json!({"op":"enqueue","items":items}))
        .unwrap();
    let enqueued = reply(&runtime, "enqueue", &mut seen);
    assert_eq!(enqueued["response"]["ok"], true, "{enqueued}");
    assert_eq!(
        crate::store::load(dir.path())
            .unwrap()
            .playback
            .queue
            .items()
            .len(),
        2
    );
    assert!(!runtime.shared.busy());
    assert!(!runtime.shared.loading_more());
}

#[test]
fn saturated_queue_keeps_every_occurrences_newest_checkpoint() {
    let (requests, _receiver) = mpsc::sync_channel(1);
    requests.try_send(WorkerMsg::Shutdown).unwrap();
    let shared = Shared::new(requests, json!({}));
    // A long worker stall while many occurrences pass: each occurrence gets
    // several checkpoints, all of which spill past the full request queue.
    for id in 1..=100u64 {
        for step in 1..=3u64 {
            shared.checkpoint(
                audio::Snapshot {
                    id,
                    position: step * 1000,
                    listened: id * 10_000 + step,
                    duration: 600_000,
                    ..Default::default()
                },
                false,
            );
        }
    }
    let drained = shared.take_checkpoints();
    for id in 1..=100u64 {
        let newest = drained
            .iter()
            .filter(|checkpoint| checkpoint.snapshot.id == id)
            .max_by_key(|checkpoint| checkpoint.seq)
            .unwrap_or_else(|| panic!("occurrence {id} lost its checkpoints"));
        assert_eq!(newest.snapshot.listened, id * 10_000 + 3);
        assert_eq!(newest.snapshot.position, 3000);
    }
    // Coalesced: one checkpoint per occurrence, applied oldest first.
    assert_eq!(drained.len(), 100);
    assert!(drained.windows(2).all(|pair| pair[0].seq < pair[1].seq));
    assert!(shared.take_checkpoints().is_empty());
}

#[test]
fn long_gapless_queue_updates_shown_track_once_per_transition() {
    const TRACKS: usize = 200;
    // The capture sink consumes PCM as fast as the engine produces it (about
    // 17x real time), so a one-second track plays in tens of milliseconds while
    // the worker still has time to prepare each successor.
    const LENGTH_MS: u32 = 1000;
    let origin = fixture_server(LENGTH_MS);
    let dir = tempfile::tempdir().unwrap();
    crate::store::save(
        dir.path(),
        &crate::store::Settings {
            client_identifier: "fixture-client".into(),
            server_url: origin,
            server_token: "fixture-token".into(),
            autoplay: false,
            // Streams only: no look-ahead downloads competing for the shared
            // I/O runtime with other fixtures in this process.
            cache: crate::cache::Config {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let runtime = Runtime::spawn(dir.path().into(), audio::Sink::Capture, false).unwrap();
    let items: Vec<_> = (1..=TRACKS)
        .map(|key| track(key, u64::from(LENGTH_MS)))
        .collect();
    runtime
        .submit(json!({"op":"play","items":serde_json::to_value(items).unwrap(),"index":0}))
        .unwrap();
    let mut shown = Vec::new();
    let mut most_plans = 0;
    let end = Instant::now() + Duration::from_secs(120);
    let mut finished = None;
    loop {
        assert!(Instant::now() < end, "queue did not finish: {shown:?}");
        let polled = runtime.poll();
        for event in polled["events"].as_array().unwrap() {
            let op = &event["request"]["op"];
            let data = &event["response"]["data"];
            if op == "play" {
                assert_eq!(event["response"]["ok"], true, "{event}");
            }
            if (op == "play" || op == "_update") && data["track"].is_object() {
                shown.push(data["track"]["ratingKey"].as_str().unwrap().to_owned());
            }
            assert_ne!(op, "_audio_error", "{event}");
        }
        most_plans = most_plans.max(runtime.shared.plan_count());
        if shown.last().map(String::as_str) == Some("200") && !runtime.shared.playback().playing {
            let since = *finished.get_or_insert_with(Instant::now);
            if since.elapsed() > Duration::from_millis(500) {
                break;
            }
        } else {
            finished = None;
        }
        thread::sleep(Duration::from_millis(3));
    }
    let expected: Vec<_> = (1..=TRACKS).map(|key| key.to_string()).collect();
    assert_eq!(
        shown, expected,
        "each track must be shown exactly once, in order"
    );
    // Only the audible occurrence and its prepared successor are kept.
    assert!(most_plans <= 2, "{most_plans} plans retained");
    // Short tracks exercise both paths: gapless Transitions committed by the
    // worker and natural Ends restarted by it.
    let transitions = runtime.shared.transitions.load(Ordering::SeqCst);
    eprintln!("gapless transitions: {transitions} of {}", TRACKS - 1);
    assert!(
        transitions >= TRACKS / 4,
        "only {transitions} gapless transitions"
    );
}

/// A server where the keys in `gone` were deleted: their parts and metadata
/// answer as PMS does for a missing item (404 + `X-Plex-Protocol`), and a
/// transcode of them is refused (400). `hold` delays metadata replies.
fn stale_server(milliseconds: u32, gone: &'static [&'static str], hold: Duration) -> String {
    scripted_server(Script {
        milliseconds,
        gone,
        hold,
        ..Default::default()
    })
}

#[derive(Default)]
struct Script {
    milliseconds: u32,
    gone: &'static [&'static str],
    hold: Duration,
    /// Replaces every metadata reply status (e.g. "401 Unauthorized").
    metadata: Option<&'static str>,
    /// Request lines received, for counting fetches.
    log: Option<Arc<Mutex<Vec<String>>>>,
    /// Metadata replies wait until this opens (counting the waiting ones).
    gate: Option<Arc<Gate>>,
    /// Station 50 continuations answer after this delay with `station_next`.
    station: Option<(Duration, usize)>,
}

/// A barrier the test opens: requests block on it, counted as they arrive.
#[derive(Default)]
struct Gate {
    state: Mutex<(bool, usize)>,
    changed: std::sync::Condvar,
}
impl Gate {
    fn pass(&self) {
        let mut state = crate::mutex_lock(&self.state);
        state.1 += 1;
        self.changed.notify_all();
        while !state.0 {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(|poison| poison.into_inner());
        }
    }
    fn waiting(&self) -> usize {
        crate::mutex_lock(&self.state).1
    }
    fn open(&self) {
        crate::mutex_lock(&self.state).0 = true;
        self.changed.notify_all();
    }
}

fn scripted_server(script: Script) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let audio = Arc::new(wav(script.milliseconds));
    let script = Arc::new(script);
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let audio = audio.clone();
            let script = script.clone();
            thread::spawn(move || {
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    if !matches!(stream.read(&mut byte), Ok(1)) {
                        return;
                    }
                    request.push(byte[0]);
                }
                let line = String::from_utf8_lossy(&request)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                if let Some(log) = &script.log {
                    crate::mutex_lock(log).push(line.clone());
                }
                let gone = script.gone;
                let dead = |prefix: &str| {
                    gone.iter().any(|key| {
                        line.contains(&format!("{prefix}{key}/"))
                            || line.contains(&format!("{prefix}{key} "))
                    })
                };
                let empty = || {
                    json!({"MediaContainer":{"size":0}})
                        .to_string()
                        .into_bytes()
                };
                let (status, kind, body) = if line.contains("/transcode/") {
                    if gone
                        .iter()
                        .any(|key| line.contains(&format!("%2Flibrary%2Fmetadata%2F{key}&")))
                    {
                        ("400 Bad Request", "text/html", Vec::new())
                    } else {
                        ("200 OK", "audio/wav", audio.to_vec())
                    }
                } else if line.contains("/library/parts/") {
                    if dead("/library/parts/") {
                        ("404 Not Found", "text/html", Vec::new())
                    } else {
                        ("200 OK", "audio/wav", audio.to_vec())
                    }
                } else if let (true, Some((delay, key))) =
                    (line.contains("/playQueues/50"), script.station)
                {
                    thread::sleep(delay);
                    let item = Item {
                        play_queue_item_id: Some(100 + key as u64),
                        ..track(key, script.milliseconds.into())
                    };
                    let body = json!({"MediaContainer":{"playQueueID":50,"Metadata":[item]}});
                    ("200 OK", "application/json", body.to_string().into_bytes())
                } else if line.contains("/library/metadata/") {
                    if let Some(gate) = &script.gate {
                        gate.pass();
                    }
                    thread::sleep(script.hold);
                    if let Some(status) = script.metadata {
                        (status, "text/html", Vec::new())
                    } else if dead("/library/metadata/") {
                        ("404 Not Found", "text/html", Vec::new())
                    } else {
                        ("200 OK", "application/json", empty())
                    }
                } else {
                    ("200 OK", "application/json", empty())
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nX-Plex-Protocol: 1.0\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            });
        }
    });
    origin
}

fn stale_runtime(origin: String) -> (tempfile::TempDir, Runtime) {
    let dir = tempfile::tempdir().unwrap();
    crate::store::save(
        dir.path(),
        &crate::store::Settings {
            client_identifier: "fixture-client".into(),
            server_url: origin,
            server_token: "fixture-token".into(),
            autoplay: false,
            cache: crate::cache::Config {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let runtime = Runtime::spawn(dir.path().into(), audio::Sink::Fake, false).unwrap();
    (dir, runtime)
}

/// Poll until `done` accepts the collected events, or fail after `limit`.
fn collect(runtime: &Runtime, limit: Duration, done: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    let end = Instant::now() + limit;
    let mut seen = Vec::new();
    while Instant::now() < end {
        if let Some(events) = runtime.poll()["events"].as_array() {
            seen.extend(events.iter().cloned());
        }
        if done(&seen) {
            return seen;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out; events: {seen:?}");
}

fn shown(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .filter(|event| {
            matches!(
                event["request"]["op"].as_str(),
                Some("play" | "_update" | "previous" | "next")
            )
        })
        .filter_map(|event| event["response"]["data"]["track"]["ratingKey"].as_str())
        .map(str::to_owned)
        .collect()
}

fn banners(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .filter(|event| event["request"]["op"] == "_audio_error")
        .map(|event| event["response"]["error"].as_str().unwrap_or("").to_owned())
        .collect()
}

#[test]
fn a_deleted_current_track_is_skipped_forward() {
    let (_dir, runtime) = stale_runtime(stale_server(60_000, &["2"], Duration::ZERO));
    let items: Vec<_> = (1..=3).map(|key| track(key, 60_000)).collect();
    runtime
        .submit(json!({"op":"play","items":serde_json::to_value(items).unwrap(),"index":1}))
        .unwrap();
    let events = collect(&runtime, Duration::from_secs(15), |events| {
        shown(events).last().map(String::as_str) == Some("3")
    });
    assert_eq!(banners(&events), Vec::<String>::new(), "{events:?}");
    assert_eq!(
        crate::mutex_lock(&runtime.shared.model)["track"]["ratingKey"],
        "3"
    );
}

#[test]
fn a_deleted_last_track_ends_with_a_banner() {
    let (dir, runtime) = stale_runtime(stale_server(60_000, &["2"], Duration::ZERO));
    let items: Vec<_> = (1..=2).map(|key| track(key, 60_000)).collect();
    runtime
        .submit(json!({"op":"play","items":serde_json::to_value(items).unwrap(),"index":1}))
        .unwrap();
    let events = collect(&runtime, Duration::from_secs(15), |events| {
        !banners(events).is_empty()
    });
    assert_eq!(
        banners(&events),
        vec![crate::GONE_TRACK.to_owned()],
        "{events:?}"
    );
    drop(runtime);
    let status = Core::inspect(dir.path().into())
        .unwrap()
        .execute(Command::Status)
        .unwrap();
    assert!(status["track"].is_null(), "{status}");
}

#[test]
fn previous_onto_a_deleted_track_keeps_going_back() {
    let (_dir, runtime) = stale_runtime(stale_server(60_000, &["2"], Duration::ZERO));
    let items: Vec<_> = (1..=3).map(|key| track(key, 60_000)).collect();
    runtime
        .submit(json!({"op":"play","items":serde_json::to_value(items).unwrap(),"index":2}))
        .unwrap();
    collect(&runtime, Duration::from_secs(10), |events| {
        shown(events).last().map(String::as_str) == Some("3")
    });
    runtime.submit(json!({"op":"previous"})).unwrap();
    let events = collect(&runtime, Duration::from_secs(15), |events| {
        shown(events).last().map(String::as_str) == Some("1")
    });
    assert_eq!(banners(&events), Vec::<String>::new(), "{events:?}");
}

#[test]
fn a_pause_during_the_check_is_kept_by_the_automatic_skip() {
    let (_dir, runtime) = stale_runtime(stale_server(60_000, &["2"], Duration::from_millis(1500)));
    let items: Vec<_> = (1..=3).map(|key| track(key, 60_000)).collect();
    runtime
        .submit(json!({"op":"play","items":serde_json::to_value(items).unwrap(),"index":1}))
        .unwrap();
    // Stream and transcode fail quickly; the metadata check is held. The
    // engine holds the failure, so the Pause lands on it.
    thread::sleep(Duration::from_millis(700));
    runtime.submit(json!({"op":"audio_pause"})).unwrap();
    let events = collect(&runtime, Duration::from_secs(15), |events| {
        shown(events).last().map(String::as_str) == Some("3")
    });
    assert_eq!(banners(&events), Vec::<String>::new(), "{events:?}");
    // The skip loaded the next track with the user's (paused) intent.
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let state = runtime.shared.playback();
        if state.loaded && !state.failed {
            assert!(state.paused && !state.intent, "{state:?}");
            break;
        }
        assert!(Instant::now() < end, "never loaded: {state:?}");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_live_track_that_fails_reports_its_original_error() {
    // Every stream fails, but the metadata says the item exists.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            thread::spawn(move || {
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    if !matches!(stream.read(&mut byte), Ok(1)) {
                        return;
                    }
                    request.push(byte[0]);
                }
                let line = String::from_utf8_lossy(&request).to_string();
                let media = line.contains("/library/parts/") || line.contains("/transcode/");
                let (status, body) = if media {
                    ("500 Internal Server Error", String::new())
                } else {
                    ("200 OK", json!({"MediaContainer":{"size":0}}).to_string())
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nX-Plex-Protocol: 1.0\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    let (_dir, runtime) = stale_runtime(origin);
    let items: Vec<_> = (1..=2).map(|key| track(key, 60_000)).collect();
    runtime
        .submit(json!({"op":"play","items":serde_json::to_value(items).unwrap(),"index":0}))
        .unwrap();
    let events = collect(&runtime, Duration::from_secs(60), |events| {
        !banners(events).is_empty()
    });
    assert_eq!(
        banners(&events),
        vec!["Audio decoding or streaming failed".to_owned()],
        "{events:?}"
    );
    assert!(!shown(&events).contains(&"2".to_owned()), "{events:?}");
}

fn never_shows(events: &[Value], key: &str) -> bool {
    !shown(events).iter().any(|shown| shown == key)
}

#[test]
fn a_deleted_successor_found_by_pre_loading_is_never_played() {
    // The incident: the next track was deleted. Its gapless pre-load fails,
    // the check confirms it, and playback moves past it at the transition.
    let (_dir, runtime) = stale_runtime(stale_server(2500, &["2"], Duration::ZERO));
    let items = vec![track(1, 2500), track(2, 2500), track(3, 2500)];
    runtime
        .submit(json!({"op":"play","items":serde_json::to_value(items).unwrap(),"index":0}))
        .unwrap();
    let events = collect(&runtime, Duration::from_secs(20), |events| {
        shown(events).last().map(String::as_str) == Some("3")
    });
    assert!(never_shows(&events, "2"), "{events:?}");
    assert_eq!(banners(&events), Vec::<String>::new(), "{events:?}");
}

#[test]
fn a_deleted_track_found_by_cache_ahead_is_never_played() {
    let origin = stale_server(2500, &["2"], Duration::ZERO);
    let dir = tempfile::tempdir().unwrap();
    crate::store::save(
        dir.path(),
        &crate::store::Settings {
            client_identifier: "fixture-client".into(),
            server_url: origin,
            server_token: "fixture-token".into(),
            autoplay: false,
            ..Default::default()
        },
    )
    .unwrap();
    let runtime = Runtime::spawn(dir.path().into(), audio::Sink::Fake, false).unwrap();
    let items = vec![track(1, 2500), track(2, 2500), track(3, 2500)];
    runtime
        .submit(json!({"op":"play","items":serde_json::to_value(items).unwrap(),"index":0}))
        .unwrap();
    let events = collect(&runtime, Duration::from_secs(20), |events| {
        shown(events).last().map(String::as_str) == Some("3")
    });
    assert!(never_shows(&events, "2"), "{events:?}");
    assert_eq!(banners(&events), Vec::<String>::new(), "{events:?}");
}

#[test]
fn a_hung_check_reports_the_original_error_after_its_timeout() {
    let (_dir, runtime) = stale_runtime(stale_server(60_000, &["2"], Duration::from_secs(12)));
    let items: Vec<_> = (1..=3).map(|key| track(key, 60_000)).collect();
    let started = Instant::now();
    runtime
        .submit(json!({"op":"play","items":serde_json::to_value(items).unwrap(),"index":1}))
        .unwrap();
    let events = collect(&runtime, Duration::from_secs(20), |events| {
        !banners(events).is_empty()
    });
    assert_eq!(
        banners(&events),
        vec!["Audio decoding or streaming failed".to_owned()],
        "{events:?}"
    );
    // The probe's own timeout ends the wait; the verdict deadline is only a
    // backstop for a lost result.
    assert!(started.elapsed() >= Duration::from_secs(5));
    assert!(started.elapsed() < recovery::VERDICT_DEADLINE + Duration::from_secs(2));
    // The worker kept serving requests meanwhile.
    runtime.submit(json!({"op":"status"})).unwrap();
    let mut seen = Vec::new();
    assert_eq!(reply(&runtime, "status", &mut seen)["response"]["ok"], true);
}

#[test]
fn a_refused_check_reports_the_original_error_without_skipping() {
    let (_dir, runtime) = stale_runtime(scripted_server(Script {
        milliseconds: 60_000,
        gone: &["2"],
        metadata: Some("401 Unauthorized"),
        ..Default::default()
    }));
    let items: Vec<_> = (1..=3).map(|key| track(key, 60_000)).collect();
    runtime
        .submit(json!({"op":"play","items":serde_json::to_value(items).unwrap(),"index":1}))
        .unwrap();
    let events = collect(&runtime, Duration::from_secs(15), |events| {
        !banners(events).is_empty()
    });
    assert_eq!(
        banners(&events),
        vec!["Audio decoding or streaming failed".to_owned()],
        "{events:?}"
    );
    assert!(never_shows(&events, "3"), "{events:?}");
}

#[test]
fn a_skip_target_without_file_details_is_not_fetched_automatically() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let (_dir, runtime) = stale_runtime(scripted_server(Script {
        milliseconds: 60_000,
        gone: &["2"],
        log: Some(log.clone()),
        ..Default::default()
    }));
    let mut bare = track(3, 60_000);
    bare.media.clear();
    let items = vec![track(1, 60_000), track(2, 60_000), bare];
    runtime
        .submit(json!({"op":"play","items":serde_json::to_value(items).unwrap(),"index":1}))
        .unwrap();
    let events = collect(&runtime, Duration::from_secs(15), |events| {
        !banners(events).is_empty()
    });
    assert_eq!(
        banners(&events),
        vec![crate::GONE_TRACK.to_owned()],
        "{events:?}"
    );
    assert!(never_shows(&events, "3"), "{events:?}");
    // Gapless preparation may fetch it before the failure is diagnosed
    // (existing behaviour); after the gone check, nothing fetches it.
    let log = crate::mutex_lock(&log);
    let checked = log
        .iter()
        .position(|line| line.starts_with("GET /library/metadata/2 "))
        .expect("the failed track was checked");
    assert!(
        !log[checked..]
            .iter()
            .any(|line| line.contains("/library/metadata/3")),
        "{log:?}"
    );
}

#[test]
fn a_failed_successor_is_not_fetched_again_on_queue_edits() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let (_dir, runtime) = stale_runtime(scripted_server(Script {
        milliseconds: 60_000,
        gone: &["2"],
        log: Some(log.clone()),
        ..Default::default()
    }));
    // The platform reports no network: checks are refused outright (the
    // local fixture still streams), so the successor is never confirmed.
    runtime
        .submit(json!({"op":"network_state","wifi":false,"online":false}))
        .unwrap();
    let items = vec![track(1, 60_000), track(2, 60_000)];
    runtime
        .submit(json!({"op":"play","items":serde_json::to_value(items).unwrap(),"index":0}))
        .unwrap();
    let mut seen = Vec::new();
    reply(&runtime, "play", &mut seen);
    let successor = || {
        crate::mutex_lock(&log)
            .iter()
            .filter(|line| line.contains("/library/parts/2/"))
            .count()
    };
    let end = Instant::now() + Duration::from_secs(10);
    while successor() == 0 && Instant::now() < end {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(successor(), 1);
    thread::sleep(Duration::from_millis(300));
    for key in 10..13 {
        let items = serde_json::to_value(vec![track(key, 60_000)]).unwrap();
        runtime
            .submit(json!({"op":"enqueue","items":items}))
            .unwrap();
        reply(&runtime, "enqueue", &mut seen);
    }
    thread::sleep(Duration::from_millis(500));
    assert_eq!(successor(), 1, "{:?}", crate::mutex_lock(&log));
    assert!(banners(&seen).is_empty(), "{seen:?}");
    assert!(
        !crate::mutex_lock(&log)
            .iter()
            .any(|line| line.contains("/library/metadata/2")),
        "the check was refused"
    );
}

/// A station whose server queue was emptied by a library recreation, with
/// `current` failing and `cached` fully downloaded (stage 1b).
fn stale_station(
    origin: String,
    milliseconds: u32,
    current: usize,
    cached: &[usize],
) -> (tempfile::TempDir, Runtime) {
    let dir = tempfile::tempdir().unwrap();
    let items: Vec<_> = (1..=3)
        .map(|key| Item {
            play_queue_item_id: Some(100 + key as u64),
            ..track(key, milliseconds.into())
        })
        .collect();
    let namespace = crate::cache::namespace(&origin, "fixture-token");
    for key in cached {
        crate::cache::seed_completed(
            &dir.path().join("audio-cache"),
            &namespace,
            &items[key - 1],
            "wav",
            &wav(milliseconds),
        );
    }
    let mut settings = crate::store::Settings {
        client_identifier: "fixture-client".into(),
        server_url: origin,
        server_token: "fixture-token".into(),
        autoplay: false,
        cache: crate::cache::Config {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };
    settings.playback.queue.replace(items, current).unwrap();
    settings.playback.radio = Some(crate::radio::Radio::new(
        crate::radio::RadioKind::Artist,
        "Fixture".into(),
        crate::radio::Source::Station { queue_id: 50 },
    ));
    crate::store::save(dir.path(), &settings).unwrap();
    let runtime = Runtime::spawn(dir.path().into(), audio::Sink::Fake, false).unwrap();
    (dir, runtime)
}

#[test]
fn a_deleted_track_on_an_emptied_station_continues_with_cached_tracks() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let (_dir, runtime) = stale_station(
        scripted_server(Script {
            milliseconds: 60_000,
            gone: &["2", "3"],
            log: Some(log.clone()),
            ..Default::default()
        }),
        60_000,
        2,
        &[1],
    );
    runtime.submit(json!({"op":"resume"})).unwrap();
    let events = collect(&runtime, Duration::from_secs(15), |events| {
        shown(events).last().map(String::as_str) == Some("1")
    });
    assert_eq!(banners(&events), Vec::<String>::new(), "{events:?}");
    let log = crate::mutex_lock(&log).clone();
    let checks = log
        .iter()
        .filter(|line| line.contains("/library/metadata/3 "))
        .count();
    assert_eq!(checks, 1, "{log:?}");
    let refills = log
        .iter()
        .filter(|line| line.contains("/playQueues/50"))
        .count();
    // The skip's refill plus speculative preparation: bounded, no loop.
    assert!((1..=4).contains(&refills), "{refills}: {log:?}");
}

#[test]
fn a_deleted_track_on_an_emptied_station_with_nothing_cached_ends_with_a_banner() {
    let (_dir, runtime) =
        stale_station(stale_server(60_000, &["3"], Duration::ZERO), 60_000, 2, &[]);
    runtime.submit(json!({"op":"resume"})).unwrap();
    let events = collect(&runtime, Duration::from_secs(15), |events| {
        !banners(events).is_empty()
    });
    assert_eq!(
        banners(&events),
        vec![crate::GONE_TRACK.to_owned()],
        "{events:?}"
    );
}

#[test]
fn a_dead_station_found_by_pre_loading_cycles_its_cached_track() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let (_dir, runtime) = stale_station(
        scripted_server(Script {
            milliseconds: 2500,
            gone: &["2", "3"],
            log: Some(log.clone()),
            ..Default::default()
        }),
        2500,
        0,
        &[1],
    );
    runtime.submit(json!({"op":"resume"})).unwrap();
    // Track 1 plays from the cache; its successors fail to pre-load, are
    // checked in the background and the cached track comes round again.
    let events = collect(&runtime, Duration::from_secs(20), |events| {
        shown(events).iter().filter(|key| *key == "1").count() >= 2
    });
    assert!(never_shows(&events, "2"), "{events:?}");
    assert!(never_shows(&events, "3"), "{events:?}");
    assert_eq!(banners(&events), Vec::<String>::new(), "{events:?}");
    let log = crate::mutex_lock(&log).clone();
    for key in ["2", "3"] {
        let checks = log
            .iter()
            .filter(|line| line.contains(&format!("/library/metadata/{key} ")))
            .count();
        assert!(checks <= 1, "{key}: {log:?}");
    }
}

/// Transcode requests: the first `drops` bodies end after `partial_ms` of
/// audio although their length promises the whole track, and the connection
/// is cut 1.5 s later (a mobile drop mid-track); later ones are complete. Other requests get an empty
/// container. Request lines are logged.
fn dropping_server(
    milliseconds: u32,
    partial_ms: u32,
    drops: usize,
    log: Arc<Mutex<Vec<String>>>,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let audio = Arc::new(wav(milliseconds));
    let cut = wav(partial_ms).len();
    let served = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let (audio, served, log) = (audio.clone(), served.clone(), log.clone());
            thread::spawn(move || {
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    if !matches!(stream.read(&mut byte), Ok(1)) {
                        return;
                    }
                    request.push(byte[0]);
                }
                let line = String::from_utf8_lossy(&request)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                crate::mutex_lock(&log).push(line.clone());
                if line.contains("/transcode/") {
                    let n = served.fetch_add(1, Ordering::SeqCst);
                    let body: &[u8] = if n < drops { &audio[..cut] } else { &audio };
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        audio.len()
                    );
                    let _ = stream.write_all(body);
                    if n < drops {
                        // Keep the connection open while the start plays,
                        // as a real drop happens mid-track, then cut it.
                        let _ = stream.flush();
                        thread::sleep(Duration::from_millis(1500));
                    }
                } else {
                    let body = json!({"MediaContainer":{"size":0}}).to_string();
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nX-Plex-Protocol: 1.0\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                }
            });
        }
    });
    origin
}

#[test]
fn a_dropped_transcode_restarts_where_it_stopped() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let origin = dropping_server(60_000, 6000, 1, log.clone());
    let dir = tempfile::tempdir().unwrap();
    let mut settings = crate::store::Settings {
        client_identifier: "fixture-client".into(),
        server_url: origin,
        server_token: "fixture-token".into(),
        autoplay: false,
        cache: crate::cache::Config {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };
    // Every stream is a 320 kbps transcode.
    settings.quality.wifi_kbps = 320;
    crate::store::save(dir.path(), &settings).unwrap();
    let runtime = Runtime::spawn(dir.path().into(), audio::Sink::Fake, false).unwrap();
    let items: Vec<_> = (1..=2).map(|key| track(key, 60_000)).collect();
    runtime
        .submit(json!({"op":"play","items":serde_json::to_value(items).unwrap(),"index":0}))
        .unwrap();
    let transcodes = || {
        crate::mutex_lock(&log)
            .iter()
            // Track 1 only: track 2's are gapless preparation.
            .filter(|line| line.contains("/transcode/") && line.contains("metadata%2F1&"))
            .cloned()
            .collect::<Vec<_>>()
    };
    let events = collect(&runtime, Duration::from_secs(20), |_| {
        transcodes().len() >= 2 && runtime.shared.playback().playing
    });
    assert_eq!(banners(&events), Vec::<String>::new(), "{events:?}");
    let requests = transcodes();
    // The restart resumes exactly where the output stopped. (The HTTP
    // reader reports the broken transfer before its buffered bytes, so
    // that is soon after the start here; nothing heard is skipped.)
    let offset = requests[1]
        .split("offset=")
        .nth(1)
        .and_then(|rest| rest.split(['&', ' ']).next())
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.);
    assert!(offset > 0. && offset < 6., "{offset}: {requests:?}");
    assert!(requests[0].contains("offset=0"), "{requests:?}");
    assert_eq!(
        crate::mutex_lock(&runtime.shared.model)["track"]["ratingKey"],
        "1"
    );
}

#[test]
fn transcode_restarts_need_new_audio_and_are_bounded() {
    let mut failures = Failures::default();
    // A transcode that never played: no restart (the gone check decides).
    assert_eq!(failures.admit_restart("a", 0), None);
    assert_eq!(failures.admit_restart("a", 500), Some(1));
    // Within an episode, attempts without new audio count up to four.
    assert_eq!(failures.admit_restart("a", 0), Some(2));
    assert_eq!(failures.admit_restart("a", 3_000), Some(3));
    assert_eq!(failures.admit_restart("a", 0), Some(4));
    assert_eq!(failures.admit_restart("a", 0), None);
    assert_eq!(failures.admit_restart("a", 9_000), None, "exhausted holds");
    // Ten seconds of new audio proves the stream works again.
    assert_eq!(failures.admit_restart("a", 10_000), Some(1));
    // Another occurrence starts its own episode.
    assert_eq!(failures.admit_restart("b", 0), None);
    assert_eq!(failures.admit_restart("b", 1), Some(1));
    assert_eq!(
        (1..=4).map(restart_backoff).collect::<Vec<_>>(),
        [0, 2, 4, 8].map(Duration::from_secs)
    );
}

/// Stop sends a reset checkpoint, then asks the engine to stop
/// asynchronously. Until the engine applies it, its snapshot still carries
/// the old position: neither the maintenance record nor the shutdown record
/// may overwrite the reset with it.
#[test]
fn a_stop_is_not_undone_by_the_engines_stale_position() {
    let origin = fixture_server(20_000);
    let dir = tempfile::tempdir().unwrap();
    crate::store::save(
        dir.path(),
        &crate::store::Settings {
            client_identifier: "fixture-client".into(),
            server_url: origin,
            server_token: "fixture-token".into(),
            autoplay: false,
            cache: crate::cache::Config {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let runtime = Runtime::spawn(dir.path().into(), audio::Sink::Fake, false).unwrap();
    let mut seen = Vec::new();
    runtime
        .submit(json!({"op":"play","items":serde_json::to_value(vec![track(1, 20_000)]).unwrap(),"index":0}))
        .unwrap();
    assert_eq!(reply(&runtime, "play", &mut seen)["response"]["ok"], true);
    let end = Instant::now() + Duration::from_secs(10);
    while runtime.shared.playback().position < 1500 {
        assert!(Instant::now() < end, "fixture did not play");
        thread::sleep(Duration::from_millis(20));
    }
    // The reset checkpoint of a Stop whose engine command has not run yet:
    // the engine keeps playing and reporting its position.
    runtime.shared.checkpoint(runtime.shared.playback(), true);
    thread::sleep(Duration::from_millis(300));
    assert!(runtime.shared.playback().position >= 1500);
    drop(runtime);
    let saved = crate::store::load(dir.path()).unwrap();
    assert_eq!(
        saved.playback.position, 0,
        "the stop was undone at shutdown"
    );
}

/// A Plex transcode that always ends cleanly after `milliseconds` of audio
/// from its start, and after 100 ms when asked to start at an offset (the
/// media's real end lies before the duration in the metadata).
fn short_transcode_server(milliseconds: u32) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            thread::spawn(move || {
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    if !matches!(stream.read(&mut byte), Ok(1)) {
                        return;
                    }
                    request.push(byte[0]);
                }
                let line = String::from_utf8_lossy(&request)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                if line.contains("/transcode/universal/start") {
                    let resumed = line.contains("offset=") && !line.contains("offset=0.000");
                    let audio = wav(if resumed { 100 } else { milliseconds });
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nConnection: close\r\n\r\n",
                    );
                    let _ = stream.write_all(&audio);
                } else {
                    let body = json!({"MediaContainer":{"size":0}}).to_string();
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                }
            });
        }
    });
    origin
}

/// Metadata longer than the transcoded media: the first clean early end is
/// restarted, and a restart ending again where the media ends is the track's
/// end, so the queue continues instead of failing or looping.
#[test]
fn a_transcode_ending_early_again_after_restart_ends_the_track() {
    let origin = short_transcode_server(1000);
    let dir = tempfile::tempdir().unwrap();
    crate::store::save(
        dir.path(),
        &crate::store::Settings {
            client_identifier: "fixture-client".into(),
            server_url: origin,
            server_token: "fixture-token".into(),
            autoplay: false,
            quality: crate::quality::Config {
                wifi_kbps: 320,
                ..Default::default()
            },
            cache: crate::cache::Config {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let runtime = Runtime::spawn(dir.path().into(), audio::Sink::Fake, false).unwrap();
    let items = serde_json::to_value(vec![track(1, 20_000), track(2, 20_000)]).unwrap();
    runtime
        .submit(json!({"op":"play","items":items,"index":0}))
        .unwrap();
    let end = Instant::now() + Duration::from_secs(20);
    let mut errors = Vec::new();
    loop {
        assert!(
            Instant::now() < end,
            "the queue did not advance; errors {errors:?}"
        );
        for event in runtime.poll()["events"].as_array().unwrap() {
            if event["request"]["op"] == "_audio_error" || event["response"]["ok"] == false {
                errors.push(event.clone());
            }
        }
        let model = crate::mutex_lock(&runtime.shared.model);
        let state = runtime.shared.playback();
        if model["track"]["ratingKey"] == "2" && state.loaded && !state.failed && state.position > 0
        {
            break;
        }
        drop(model);
        thread::sleep(Duration::from_millis(20));
    }
    assert!(errors.is_empty(), "{errors:?}");
}

fn idle_runtime() -> (tempfile::TempDir, Runtime) {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = crate::store::Settings {
        client_identifier: "fixture-client".into(),
        ..Default::default()
    };
    settings
        .playback
        .queue
        .replace(vec![track(1, 1000)], 0)
        .unwrap();
    crate::store::save(dir.path(), &settings).unwrap();
    let runtime = Runtime::spawn(dir.path().into(), audio::Sink::Fake, false).unwrap();
    (dir, runtime)
}

/// A panic outside any request (event handling, a tick) costs one iteration:
/// Core is rebuilt from persisted state and the worker keeps serving.
#[test]
fn a_panicking_iteration_rebuilds_core_and_keeps_serving() {
    let (dir, runtime) = idle_runtime();
    let mut seen = Vec::new();
    crate::test_hooks::panic_after(dir.path(), "iteration");
    // Rebuilding Core shares the I/O runtime with every other fixture.
    let end = Instant::now() + Duration::from_secs(20);
    while !seen.iter().any(|event: &Value| {
        event["request"]["op"] == "_update" && event["response"]["data"]["queue"].is_object()
    }) {
        assert!(Instant::now() < end, "no rebuilt state published: {seen:?}");
        if let Some(events) = runtime.poll()["events"].as_array() {
            seen.extend(events.iter().cloned());
        }
        thread::sleep(Duration::from_millis(10));
    }
    runtime.submit(json!({"op":"status"})).unwrap();
    let status = reply(&runtime, "status", &mut seen);
    assert_eq!(status["response"]["ok"], true, "{status}");
    assert_eq!(
        status["response"]["data"]["queue"]["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

/// A panic after a request was admitted but outside `Core::execute` still
/// answers it and releases its busy state.
#[test]
fn a_panic_after_admission_answers_the_request_and_clears_busy() {
    let (dir, runtime) = idle_runtime();
    let mut seen = Vec::new();
    crate::test_hooks::panic_after(dir.path(), "admitted");
    runtime.submit(json!({"op":"status"})).unwrap();
    let failed = reply(&runtime, "status", &mut seen);
    assert_eq!(failed["response"]["ok"], false, "{failed}");
    assert_eq!(failed["response"]["error"], "Backend panic");
    let end = Instant::now() + Duration::from_secs(5);
    while runtime.shared.busy() {
        assert!(Instant::now() < end, "busy stayed raised");
        thread::sleep(Duration::from_millis(10));
    }
    runtime.submit(json!({"op":"status"})).unwrap();
    assert_eq!(reply(&runtime, "status", &mut seen)["response"]["ok"], true);
}

/// A worker that keeps panicking stops instead of rebuilding Core forever,
/// and still answers the requests queued for it.
#[test]
fn a_worker_that_keeps_panicking_stops_and_answers_queued_requests() {
    let (dir, runtime) = idle_runtime();
    let mut seen = Vec::new();
    for _ in 0..PANIC_LIMIT {
        crate::test_hooks::panic_after(dir.path(), "iteration");
    }
    // Queued while the worker panics: each must still be answered.
    let mut queued = 0;
    for _ in 0..3 {
        if runtime.submit(json!({"op":"status"})).is_ok() {
            queued += 1;
        }
    }
    assert!(
        queued > 0,
        "no request was queued before the worker stopped"
    );
    let end = Instant::now() + Duration::from_secs(5);
    while !runtime.shared.stop.load(Ordering::SeqCst) {
        assert!(Instant::now() < end, "the worker kept running");
        thread::sleep(Duration::from_millis(10));
    }
    let end = Instant::now() + Duration::from_secs(10);
    let mut answered = 0;
    while answered < queued {
        assert!(
            Instant::now() < end,
            "{answered} of {queued} queued requests answered"
        );
        for event in runtime.poll()["events"].as_array().unwrap() {
            if event["request"]["op"] == "status" {
                answered += 1;
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    // Later requests fail at admission or are answered: never left pending.
    match runtime.submit(json!({"op":"status"})) {
        Err(_) => {}
        Ok(_) => {
            let answer = reply(&runtime, "status", &mut seen);
            assert_eq!(answer["response"]["ok"], false, "{answer}");
        }
    }
    assert!(!runtime.shared.busy());
}

/// A panic while a held failure is being handled must not leave the engine
/// holding it (silent, until its own expiry): the rebuild releases it.
#[test]
fn a_panic_while_handling_a_held_failure_releases_it() {
    let origin = short_transcode_server(1000);
    let dir = tempfile::tempdir().unwrap();
    crate::store::save(
        dir.path(),
        &crate::store::Settings {
            client_identifier: "fixture-client".into(),
            server_url: origin,
            server_token: "fixture-token".into(),
            autoplay: false,
            quality: crate::quality::Config {
                wifi_kbps: 320,
                ..Default::default()
            },
            cache: crate::cache::Config {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let runtime = Runtime::spawn(dir.path().into(), audio::Sink::Fake, false).unwrap();
    crate::test_hooks::panic_after(dir.path(), "held_failure");
    let items = serde_json::to_value(vec![track(1, 20_000)]).unwrap();
    runtime
        .submit(json!({"op":"play","items":items,"index":0}))
        .unwrap();
    let end = Instant::now() + Duration::from_secs(20);
    while crate::test_hooks::armed(dir.path(), "held_failure") {
        assert!(Instant::now() < end, "no held failure was handled");
        runtime.poll();
        thread::sleep(Duration::from_millis(20));
    }
    let end = Instant::now() + Duration::from_secs(10);
    while runtime.shared.playback().failed {
        assert!(Instant::now() < end, "the held failure was never released");
        runtime.poll();
        thread::sleep(Duration::from_millis(20));
    }
}

/// A run of deleted tracks ahead, with cache-ahead off: each is found before
/// playback reaches it (the successor pre-load checks them one after another),
/// so the run is stepped over without a failure.
#[test]
fn a_run_of_gone_tracks_ahead_is_found_before_it_is_reached() {
    let (_dir, runtime) = stale_runtime(stale_server(60_000, &["2", "3", "4"], Duration::ZERO));
    let items =
        serde_json::to_value((1..=6).map(|key| track(key, 60_000)).collect::<Vec<_>>()).unwrap();
    runtime
        .submit(json!({"op":"play","items":items,"index":0}))
        .unwrap();
    let end = Instant::now() + Duration::from_secs(20);
    let mut gone = Value::Null;
    loop {
        assert!(
            Instant::now() < end,
            "the gone run was not found ahead: {gone}"
        );
        for event in runtime.poll()["events"].as_array().unwrap() {
            let keys = &event["response"]["data"]["cache"]["goneKeys"];
            if keys.is_array() {
                gone = keys.clone();
            }
        }
        let state = runtime.shared.playback();
        let model = crate::mutex_lock(&runtime.shared.model);
        if state.loaded {
            assert_eq!(model["track"]["ratingKey"], "1", "playback left track 1");
        }
        drop(model);
        if gone == json!(["2", "3", "4"]) {
            // Found while track 1 is still playing.
            assert!(state.loaded && state.playing && !state.failed, "{state:?}");
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// A Next the user queues while a gone check is pending is the one that
/// counts: the verdict's automatic skip must not advance a second time.
#[test]
fn a_next_queued_during_the_gone_check_is_not_advanced_twice() {
    let gate = Arc::new(Gate::default());
    let origin = scripted_server(Script {
        milliseconds: 60_000,
        gone: &["2"],
        gate: Some(gate.clone()),
        ..Default::default()
    });
    let (_dir, runtime) = stale_runtime(origin);
    let items: Vec<_> = (1..=4).map(|key| track(key, 60_000)).collect();
    runtime
        .submit(json!({"op":"play","items":serde_json::to_value(items).unwrap(),"index":1}))
        .unwrap();
    // The stream failed and is held; its metadata check is in flight.
    let end = Instant::now() + Duration::from_secs(10);
    while gate.waiting() == 0 || !runtime.shared.playback().failed {
        assert!(Instant::now() < end, "the check never started");
        runtime.poll();
        thread::sleep(Duration::from_millis(10));
    }
    runtime
        .submit(json!({"op":"next","automatic":false}))
        .unwrap();
    let mut seen = Vec::new();
    assert_eq!(reply(&runtime, "next", &mut seen)["response"]["ok"], true);
    // Only now the verdict (gone) arrives.
    gate.open();
    let events = collect(&runtime, Duration::from_secs(10), |events| {
        events.iter().any(|event| {
            event["response"]["data"]["cache"]["goneKeys"]
                .as_array()
                .is_some_and(|keys| keys.iter().any(|key| key == "2"))
        })
    });
    thread::sleep(Duration::from_millis(500));
    let later: Vec<Value> = runtime.poll()["events"].as_array().unwrap().clone();
    let all: Vec<Value> = seen.into_iter().chain(events).chain(later).collect();
    assert!(never_shows(&all, "4"), "{all:?}");
    assert_eq!(
        crate::mutex_lock(&runtime.shared.model)["track"]["ratingKey"],
        "3"
    );
}

/// A burst of Next presses goes through the worker in order: each counts once.
#[test]
fn a_burst_of_next_presses_advances_once_per_press() {
    let (_dir, runtime) = stale_runtime(stale_server(60_000, &[], Duration::ZERO));
    let items: Vec<_> = (1..=30).map(|key| track(key, 60_000)).collect();
    runtime
        .submit(json!({"op":"play","items":serde_json::to_value(items).unwrap(),"index":0}))
        .unwrap();
    let mut seen = Vec::new();
    assert_eq!(reply(&runtime, "play", &mut seen)["response"]["ok"], true);
    for _ in 0..20 {
        runtime
            .submit(json!({"op":"next","automatic":false}))
            .unwrap();
    }
    // Every press is answered and moves exactly one track on, in order.
    let mut keys = Vec::new();
    let end = Instant::now() + Duration::from_secs(20);
    while keys.len() < 20 {
        assert!(Instant::now() < end, "answered {keys:?}");
        for event in runtime.poll()["events"].as_array().unwrap() {
            if event["request"]["op"] == "next" {
                assert_eq!(event["response"]["ok"], true, "{event}");
                keys.push(event["response"]["data"]["track"]["ratingKey"].clone());
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    let expected: Vec<Value> = (2..=21).map(|key| json!(key.to_string())).collect();
    assert_eq!(keys, expected);
}

/// The engine's control queue is full when the gone skip's resolution is
/// sent: the worker keeps it and sends it again instead of losing it.
#[test]
fn a_resolution_refused_by_a_full_engine_queue_is_sent_again() {
    let (_dir, runtime) = stale_runtime(stale_server(60_000, &["2"], Duration::ZERO));
    let end = Instant::now() + Duration::from_secs(5);
    let handle = loop {
        if let Some(handle) = crate::mutex_lock(&runtime.shared.audio).clone() {
            break handle;
        }
        assert!(Instant::now() < end, "the engine never started");
        thread::sleep(Duration::from_millis(10));
    };
    handle.refuse_resolves(2);
    let items: Vec<_> = (1..=3).map(|key| track(key, 60_000)).collect();
    runtime
        .submit(json!({"op":"play","items":serde_json::to_value(items).unwrap(),"index":1}))
        .unwrap();
    let events = collect(&runtime, Duration::from_secs(15), |events| {
        shown(events).last().map(String::as_str) == Some("3")
    });
    assert_eq!(handle.refused_resolves_left(), 0, "the hook never refused");
    assert_eq!(banners(&events), Vec::<String>::new(), "{events:?}");
    let end = Instant::now() + Duration::from_secs(5);
    while !(runtime.shared.playback().loaded && !runtime.shared.playback().failed) {
        assert!(Instant::now() < end, "the skip never loaded");
        thread::sleep(Duration::from_millis(20));
    }
}

/// A station radio of five fully cached tracks (`durations` in ms) whose
/// continuation answers track 6 after 3 s.
fn slow_station(durations: [u32; 5]) -> (tempfile::TempDir, Runtime) {
    let origin = scripted_server(Script {
        milliseconds: 60_000,
        station: Some((Duration::from_secs(3), 6)),
        ..Default::default()
    });
    let dir = tempfile::tempdir().unwrap();
    let items: Vec<_> = (1..=5)
        .map(|key| Item {
            play_queue_item_id: Some(100 + key as u64),
            ..track(key, durations[key - 1].into())
        })
        .collect();
    let namespace = crate::cache::namespace(&origin, "fixture-token");
    for item in &items {
        crate::cache::seed_completed(
            &dir.path().join("audio-cache"),
            &namespace,
            item,
            "wav",
            &wav(item.duration as u32),
        );
    }
    let mut settings = crate::store::Settings {
        client_identifier: "fixture-client".into(),
        server_url: origin,
        server_token: "fixture-token".into(),
        autoplay: false,
        cache: crate::cache::Config {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };
    settings.playback.queue.replace(items, 0).unwrap();
    settings.playback.radio = Some(crate::radio::Radio::new(
        crate::radio::RadioKind::Artist,
        "Fixture".into(),
        crate::radio::Source::Station { queue_id: 50 },
    ));
    crate::store::save(dir.path(), &settings).unwrap();
    let runtime = Runtime::spawn(dir.path().into(), audio::Sink::Fake, false).unwrap();
    (dir, runtime)
}

fn has_track(queue: &Value, key: &str) -> bool {
    queue["items"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item["ratingKey"] == key))
}

/// Moves onto cached successors are answered at once, one after the other,
/// while the station takes 3 s; the refill lands afterwards.
#[test]
fn next_on_a_radio_does_not_wait_for_a_slow_station() {
    let (dir, runtime) = slow_station([60_000; 5]);
    let mut seen = Vec::new();
    runtime.submit(json!({"op":"resume"})).unwrap();
    assert_eq!(reply(&runtime, "resume", &mut seen)["response"]["ok"], true);
    for expected in ["2", "3"] {
        let pressed = Instant::now();
        runtime
            .submit(json!({"op":"next","automatic":false}))
            .unwrap();
        let next = reply(&runtime, "next", &mut seen);
        assert_eq!(next["response"]["data"]["track"]["ratingKey"], expected);
        assert!(
            pressed.elapsed() < Duration::from_secs(1),
            "Next took {:?}",
            pressed.elapsed()
        );
    }
    collect(&runtime, Duration::from_secs(20), |events| {
        events
            .iter()
            .any(|event| has_track(&event["response"]["data"]["queue"], "6"))
    });
    drop(runtime);
    let saved = crate::Core::inspect(dir.path().into())
        .unwrap()
        .execute(Command::Status)
        .unwrap();
    assert!(has_track(&saved["queue"], "6"), "{saved}");
}

/// Track 2 ends while its refill is in flight: the gapless successor becomes
/// audible and is committed, and the refill lands on the committed queue,
/// presented with track 3 current.
#[test]
fn a_transition_during_a_slow_refill_keeps_both() {
    let (dir, runtime) = slow_station([60_000, 1500, 60_000, 60_000, 60_000]);
    let mut seen = Vec::new();
    runtime.submit(json!({"op":"resume"})).unwrap();
    assert_eq!(reply(&runtime, "resume", &mut seen)["response"]["ok"], true);
    runtime
        .submit(json!({"op":"next","automatic":false}))
        .unwrap();
    assert_eq!(
        reply(&runtime, "next", &mut seen)["response"]["data"]["track"]["ratingKey"],
        "2"
    );
    let events = collect(&runtime, Duration::from_secs(20), |events| {
        events
            .iter()
            .any(|event| has_track(&event["response"]["data"]["queue"], "6"))
    });
    assert_eq!(
        shown(&events).last().map(String::as_str),
        Some("3"),
        "{events:?}"
    );
    // Every queue presented after track 3 became audible selects it.
    let after = events
        .iter()
        .position(|event| event["response"]["data"]["track"]["ratingKey"] == "3")
        .expect("track 3 shown");
    for event in &events[after..] {
        let queue = &event["response"]["data"]["queue"];
        if let Some(current) = queue["current"].as_u64() {
            assert_eq!(
                queue["items"][current as usize]["ratingKey"], "3",
                "{event}"
            );
        }
    }
    drop(runtime);
    let saved = crate::Core::inspect(dir.path().into())
        .unwrap()
        .execute(Command::Status)
        .unwrap();
    assert!(has_track(&saved["queue"], "6"), "{saved}");
    assert_eq!(saved["track"]["ratingKey"], "3", "{saved}");
}
