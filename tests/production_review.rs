use plexfreq_core::{model::Item, queue::Queue};

fn track(id: &str) -> Item {
    Item {
        rating_key: id.into(),
        kind: "track".into(),
        duration: 120000,
        ..Default::default()
    }
}

#[test]
fn previous_after_natural_end_returns_last_track() {
    let mut q = Queue::default();
    q.replace(vec![track("0"), track("1"), track("2")], 0)
        .unwrap();
    assert_eq!(q.advance(false).unwrap().rating_key, "1");
    assert_eq!(q.advance(false).unwrap().rating_key, "2");
    assert!(q.at_end());
    // Consume the final track: natural end of queue.
    assert!(q.advance(false).is_none());
    assert!(q.track().is_none());
    // Going back must resume the last audible track, not skip it.
    assert_eq!(q.previous().unwrap().rating_key, "2");
}

#[test]
fn trim_history_bounds_shuffled_radio_growth() {
    let items: Vec<Item> = (0..150).map(|i| track(&i.to_string())).collect();
    let mut q = Queue::default();
    q.replace(items, 0).unwrap();
    q.shuffle(true);
    // Play through the shuffled order to build history behind current.
    for _ in 0..149 {
        q.advance(false);
    }
    assert!(q.at_end() || q.track().is_some());
    // Radio trims to the last 100 tracks behind current.
    q.trim_history(100);
    assert!(
        q.items.len() <= 101,
        "shuffled history must stay bounded, kept {} items",
        q.items.len()
    );
    q.validate().unwrap();
}

#[test]
fn crossfade_validates_buffer_lengths_without_panicking() {
    let old = [1.0f32, 1.0, 1.0, 1.0];
    let new = [0.0f32, 0.0];
    let mut out = [0.0f32, 0.0, 0.0, 0.0];
    // Mismatched caller buffers must not panic the audio thread;
    // the shortest shared prefix is the only defined mix.
    plexfreq_core::audio::dsp::crossfade(&old, &new, &mut out, 0, 2);
    assert_eq!(out.len(), 4);
}

#[test]
fn short_next_track_joins_despite_crossfade_tail() {
    use plexfreq_core::audio::{Engine, Event, Sink, Source, RATE};
    use std::{
        io::Write,
        time::{Duration, Instant},
    };
    fn wav(path: &std::path::Path, frames: u32, sample: i16) {
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(36 + frames * 2).to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16u32.to_le_bytes()).unwrap();
        file.write_all(&1u16.to_le_bytes()).unwrap();
        file.write_all(&1u16.to_le_bytes()).unwrap();
        file.write_all(&RATE.to_le_bytes()).unwrap();
        file.write_all(&(RATE * 2).to_le_bytes()).unwrap();
        file.write_all(&2u16.to_le_bytes()).unwrap();
        file.write_all(&16u16.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&(frames * 2).to_le_bytes()).unwrap();
        for _ in 0..frames {
            file.write_all(&sample.to_le_bytes()).unwrap();
        }
    }
    fn source(id: u64, path: &std::path::Path, album: &str) -> Source {
        Source {
            id,
            url: url::Url::from_file_path(path).unwrap().to_string(),
            duration: 100,
            resume: 0,
            listened: 0,
            gain: 0.,
            album: album.into(),
            ..Default::default()
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let one = dir.path().join("one.wav");
    let two = dir.path().join("two.wav");
    wav(&one, 4800, 4096);
    wav(&two, 800, 8192);
    let engine = Engine::new(Sink::Capture).unwrap();
    engine.handle.volume(1.).unwrap();
    engine
        .handle
        .configure(plexfreq_core::audio::dsp::Config {
            crossfade_ms: 50,
            ..Default::default()
        })
        .unwrap();
    engine
        .handle
        .load(source(1, &one, "album-a"), false)
        .unwrap();
    engine
        .handle
        .prepare(Some(source(2, &two, "album-b")))
        .unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if engine
            .events()
            .iter()
            .any(|e| matches!(e, Event::End { id: 2, .. }))
        {
            break;
        }
        assert!(Instant::now() < until, "short join stalled");
        std::thread::sleep(Duration::from_millis(10));
    }
    let pcm = engine.handle.captured();
    // Gapless join must complete: no stall, no lost/duplicated tail beyond
    // one crossfade window. Exact overlap policy is an implementation choice,
    // but it can never exceed the short track itself.
    assert!(
        (4800 * 2..=5600 * 2).contains(&pcm.len()),
        "unexpected join length {}",
        pcm.len()
    );
    assert!(pcm.iter().all(|v| v.is_finite()));
}

// Production review: Queue is capped at 10000 tracks in replace/insert/validate.
// Append must enforce the same bound, otherwise radio/autoplay extensions
// silently create an invalid queue that is wiped on next restart
// (settings.playback.queue.validate() fails -> default).
#[test]
fn queue_append_enforces_maximum_without_corrupting_persisted_state() {
    let items: Vec<Item> = (0..10000).map(|i| track(&i.to_string())).collect();
    let mut q = Queue::default();
    q.replace(items, 0).unwrap();
    q.validate().unwrap();
    let result = q.append(vec![track("10000")]);
    assert!(
        result.is_err(),
        "append beyond 10000 must be rejected, got Ok"
    );
    // Persisted state must stay valid.
    q.validate().unwrap();
    assert_eq!(q.items.len(), 10000);
}

// Production review: Cache::schedule allows 1021 tracks while its own error
// message and every other download entry point (CacheTracks/Pin) promise
// "up to 1000". The outlier lets oversized plans through and breaks the
// documented contract.
#[test]
fn cache_schedule_rejects_over_1000_tracks_consistently() {
    use plexfreq_core::cache::{Cache, Config};
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::new(
        dir.path().into(),
        Config {
            enabled: true,
            limit_mb: 512,
            ahead: 5,
        },
        "fixture",
    )
    .unwrap();
    let items: Vec<Item> = (0..1001).map(|i| track(&i.to_string())).collect();
    let result = cache.schedule("http://127.0.0.1:32400/", "token", items);
    assert!(
        result.is_err(),
        "schedule of 1001 tracks must be rejected to match documented 1000-track limit"
    );
}

// Production review (unfixed): Queue::validate must reject a corrupt cursor.
// A persisted queue with cursor far beyond order currently passes validation,
// so corrupt session state survives restarts instead of resetting to default.
#[test]
fn queue_validate_rejects_corrupt_cursor() {
    let mut q = Queue::default();
    q.replace(vec![track("0"), track("1")], 0).unwrap();
    let mut value = serde_json::to_value(&q).unwrap();
    value["cursor"] = serde_json::json!(99);
    value["current"] = serde_json::Value::Null;
    let corrupt: Queue = serde_json::from_value(value).unwrap();
    assert!(
        corrupt.validate().is_err(),
        "cursor 99 with 2 items must be rejected as invalid persisted state"
    );
}

// Production review (unfixed): Queue::insert must never panic on corrupt state.
// With current == Some and a stale cursor (e.g. restored from a corrupt
// session), `order.splice(at..at, …)` uses at = cursor+1 which is out of bounds
// and panics the worker thread instead of returning an error.
#[test]
fn queue_insert_with_corrupt_cursor_must_not_panic() {
    let mut q = Queue::default();
    q.replace(vec![track("0"), track("1")], 0).unwrap();
    let mut value = serde_json::to_value(&q).unwrap();
    value["cursor"] = serde_json::json!(99);
    let mut corrupt: Queue = serde_json::from_value(value).unwrap();
    // Must return Ok/Err, never panic the caller (worker thread).
    let _ = corrupt.insert(vec![track("2")], true);
    // If it did not panic, the resulting queue must at least be internally
    // consistent or rejected by validate, never silently corrupt.
    let _ = corrupt.validate();
}

// Production review (unfixed): Queue::presentation must never panic.
// It indexes items directly (`items[*i]`), so a single corrupt order entry
// (e.g. 99 with 2 items) panics the worker thread during next/prepare.
// Presentation is called on every playback plan; it must be infallible.
#[test]
fn queue_presentation_must_not_panic_on_corrupt_order() {
    let mut q = Queue::default();
    q.replace(vec![track("0"), track("1")], 0).unwrap();
    let mut value = serde_json::to_value(&q).unwrap();
    value["order"] = serde_json::json!([0, 99]);
    let corrupt: Queue = serde_json::from_value(value).unwrap();
    assert!(
        corrupt.validate().is_err(),
        "corrupt order must be rejected by validate"
    );
    // Must not panic even on corrupt input; worker threads must survive.
    let _ = corrupt.presentation();
}

// Production review (unfixed): SavePlayback must not clamp to zero when the
// track duration is still unknown (0). Audio seek explicitly avoids clamping
// in that case, but SavePlayback does `position.min(duration)`, so resuming a
// track with unknown duration always loses the saved position.
#[test]
fn saveplayback_preserves_position_when_duration_unknown() {
    use plexfreq_core::{
        model::{Media, Part},
        Command, Core,
    };
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let task = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buf = [0; 4096];
        loop {
            let n = socket.read(&mut buf).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buf[..n]);
            if request.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let body = serde_json::json!({"MediaContainer":{"Directory":[]}}).to_string();
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    core.execute(Command::Connect {
        url: origin,
        token: "secret".into(),
    })
    .unwrap();
    let item = Item {
        rating_key: "0".into(),
        kind: "track".into(),
        title: "Zero".into(),
        duration: 0,
        media: vec![Media {
            parts: vec![Part {
                key: "/library/parts/0/file.mp3".into(),
                size: None,
                streams: vec![],
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    core.execute(Command::Play {
        items: vec![item],
        index: 0,
    })
    .unwrap();
    core.execute(Command::SavePlayback {
        key: "0".into(),
        position: 5000,
        generation: None,
    })
    .unwrap();
    let status = core.execute(Command::Status).unwrap();
    task.join().unwrap();
    assert_eq!(
        status["resumePosition"], 5000,
        "unknown duration must not clamp saved position to zero"
    );
}

// Production review (unfixed): preparing the next track must never kill the
// currently playing track. Actor::control uses `?` for Prepare, so a single
// invalid next URL (e.g. rejected by safe_uri) returns Err and run() calls
// error(), which clears current/output and stops playback.
#[test]
fn prepare_failure_keeps_current_playback_alive() {
    use plexfreq_core::audio::{Engine, Sink, Source, RATE};
    use std::{
        io::Write,
        time::{Duration, Instant},
    };
    fn wav(path: &std::path::Path, frames: u32) {
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(36 + frames * 2).to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16u32.to_le_bytes()).unwrap();
        file.write_all(&1u16.to_le_bytes()).unwrap();
        file.write_all(&1u16.to_le_bytes()).unwrap();
        file.write_all(&RATE.to_le_bytes()).unwrap();
        file.write_all(&(RATE * 2).to_le_bytes()).unwrap();
        file.write_all(&2u16.to_le_bytes()).unwrap();
        file.write_all(&16u16.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&(frames * 2).to_le_bytes()).unwrap();
        for _ in 0..frames {
            file.write_all(&1000i16.to_le_bytes()).unwrap();
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.wav");
    wav(&good, 48000);
    let engine = Engine::new(Sink::Capture).unwrap();
    engine
        .handle
        .load(
            Source {
                id: 1,
                url: url::Url::from_file_path(&good).unwrap().to_string(),
                duration: 1000,
                resume: 0,
                listened: 0,
                gain: 0.,
                album: String::new(),
                ..Default::default()
            },
            false,
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let before = engine.handle.snapshot();
    assert!(
        before.loaded,
        "current track must be loaded before bad prepare"
    );
    engine
        .handle
        .prepare(Some(Source {
            id: 2,
            url: "ftp://invalid/audio.mp3".into(),
            duration: 1000,
            resume: 0,
            listened: 0,
            gain: 0.,
            album: String::new(),
            ..Default::default()
        }))
        .unwrap();
    let until = Instant::now() + Duration::from_secs(2);
    let mut after = engine.handle.snapshot();
    while Instant::now() < until && after.loaded {
        std::thread::sleep(Duration::from_millis(20));
        after = engine.handle.snapshot();
        if !after.loaded {
            break;
        }
    }
    assert!(
        after.loaded,
        "bad next-track prepare must not unload current playback, got loaded=false error={}",
        after.error
    );
    assert_eq!(
        after.id, before.id,
        "current track id must survive bad prepare"
    );
}

// Production review (unfixed): an empty station key means "no station", not a
// protocol error. station_key currently forwards "" to server_path, which
// rejects it, so start_radio fails instead of falling back to sonic radio.
#[test]
fn station_empty_key_means_no_station_not_error() {
    use plexfreq_core::{model::Item, radio::station_key};
    let base = url::Url::parse("https://music.example/").unwrap();
    let item = Item {
        stations: serde_json::json!([{"key": ""}]),
        ..Default::default()
    };
    let result = station_key(&item, &base);
    assert!(
        matches!(result, Ok(None)),
        "empty station key must decode as no station, got {result:?}"
    );
}

// Production review: a single malformed server entry must not poison discovery
// of all other servers. Current resources() collects with `?`, so one entry
// with e.g. null accessToken fails the whole call instead of being skipped.
#[test]
fn server_discovery_skips_malformed_entries_instead_of_failing_all() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let task = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buf = [0; 4096];
        loop {
            let n = socket.read(&mut buf).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buf[..n]);
            if request.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let body = serde_json::json!([
            {"name":"Good server","clientIdentifier":"good","provides":"server","accessToken":"good-secret","connections":[]},
            {"name":"Broken server","clientIdentifier":"broken","provides":"server","accessToken":null,"connections":[]}
        ])
        .to_string();
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    let plex =
        plexfreq_core::plex::Plex::new("test-client", url::Url::parse(&origin).unwrap()).unwrap();
    let resources = plex.resources("account-secret");
    task.join().unwrap();
    let resources =
        resources.expect("mixed valid+malformed discovery must succeed with the good server");
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].client_identifier, "good");
}
