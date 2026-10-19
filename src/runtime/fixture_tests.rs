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
                } else if line.contains("/library/metadata/") {
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
fn a_user_action_during_the_check_prevents_the_automatic_skip() {
    let (_dir, runtime) = stale_runtime(stale_server(60_000, &["2"], Duration::from_millis(1500)));
    let items: Vec<_> = (1..=3).map(|key| track(key, 60_000)).collect();
    runtime
        .submit(json!({"op":"play","items":serde_json::to_value(items).unwrap(),"index":1}))
        .unwrap();
    // Stream and transcode fail quickly; the metadata check is held.
    thread::sleep(Duration::from_millis(700));
    runtime.submit(json!({"op":"audio_pause"})).unwrap();
    let events = collect(&runtime, Duration::from_secs(15), |events| {
        !banners(events).is_empty()
    });
    assert_eq!(
        banners(&events),
        vec![crate::GONE_TRACK.to_owned()],
        "{events:?}"
    );
    assert!(!shown(&events).contains(&"3".to_owned()), "{events:?}");
    assert_eq!(
        crate::mutex_lock(&runtime.shared.model)["track"]["ratingKey"],
        "2"
    );
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
    settings.playback.radio = Some(crate::radio::Radio {
        kind: crate::radio::RadioKind::Artist,
        title: "Fixture".into(),
        source: crate::radio::Source::Station { queue_id: 50 },
    });
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
