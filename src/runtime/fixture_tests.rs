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
