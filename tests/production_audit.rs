use plexfreq_core::{model::Item, queue::Queue};

fn track(id: &str) -> Item {
    Item {
        rating_key: id.into(),
        kind: "track".into(),
        duration: 120000,
        ..Default::default()
    }
}

// Production audit: DSP must never emit NaN even for corrupt persisted config.
// Processor::new trusts headroom_db without validation; Handle::configure
// validates but Actor::new / restored session bypass it. NaN headroom poisons
// every sample (NaN.clamp returns NaN) and is pushed to appsrc.
#[test]
fn dsp_nan_headroom_must_not_poison_output() {
    use plexfreq_core::audio::dsp::{Config, Processor};
    let config = Config {
        headroom_db: f32::NAN,
        ..Default::default()
    };
    let mut processor = Processor::new(48000, &config);
    let mut pcm = [0.5f32, -0.25, 0.0, 1.0];
    processor.process(&mut pcm, 0.0, 1.0);
    assert!(
        pcm.iter().all(|v| v.is_finite()),
        "NaN headroom poisoned output to {pcm:?}; Processor::new must clamp/validate"
    );
    assert!(
        pcm.iter().all(|v| (-1.0..=1.0).contains(v)),
        "output out of [-1,1]: {pcm:?}"
    );
}

// Production audit: LRC bracket-only lines are content ([Chorus], [hello]),
// not metadata. parse() currently drops any line starting with '[' and ending
// with ']', losing legitimate lyric content.
#[test]
fn lyrics_bracket_only_line_is_content() {
    let lines = plexfreq_core::lyrics::parse("[hello]");
    assert_eq!(
        lines.len(),
        1,
        "bracket-only lyric '[hello]' was dropped; got {lines:?}"
    );
    assert_eq!(lines[0].text, "[hello]");
}

// Production audit: a single explicit null must not poison a whole container.
// Item::title/thumb/kind/key/rating_key lack null_default, so one track with
// "title":null fails the entire browse page instead of defaulting that field.
#[test]
fn model_null_title_must_not_poison_container() {
    let payload = serde_json::json!({
        "MediaContainer": {
            "size": 2,
            "Metadata": [
                {"ratingKey": "1", "key": "/a", "title": null, "type": "track"},
                {"ratingKey": "2", "key": "/b", "title": "Good", "type": "track"}
            ]
        }
    });
    let envelope: Result<plexfreq_core::model::Envelope, _> = serde_json::from_value(payload);
    let envelope = envelope.expect("one null title must not fail whole container");
    assert_eq!(envelope.container.items.len(), 2);
    assert_eq!(envelope.container.items[1].title, "Good");
}

// Production audit: Plex numeric fields vary (numbers vs numeric strings,
// floats). Only gain is tolerant; year as "2020" currently fails the whole
// item. Must default or parse like optional_gain.
#[test]
fn model_numeric_string_year_must_not_poison_item() {
    let payload = serde_json::json!({
        "MediaContainer": {
            "size": 1,
            "Metadata": [
                {"ratingKey": "1", "key": "/a", "title": "T", "type": "track", "year": "2020"}
            ]
        }
    });
    let envelope: Result<plexfreq_core::model::Envelope, _> = serde_json::from_value(payload);
    assert!(
        envelope.is_ok(),
        "numeric-string year poisoned item; should default/parse"
    );
}

// Production audit: presentation() invents a current selection for corrupt
// state. With order [0,99] (99 OOB) and current Some(99), it maps to
// Some(cursor) pointing at the wrong track instead of None.
#[test]
fn queue_presentation_corrupt_current_must_be_none() {
    let mut q = Queue::default();
    q.replace(vec![track("0"), track("1")], 0).unwrap();
    let mut value = serde_json::to_value(&q).unwrap();
    value["order"] = serde_json::json!([0, 99]);
    value["current"] = serde_json::json!(99);
    value["cursor"] = serde_json::json!(1);
    let corrupt: Queue = serde_json::from_value(value).unwrap();
    let presented = corrupt.presentation();
    assert_eq!(
        presented.current, None,
        "corrupt current 99 mapped to {:?}, pointing at wrong track",
        presented.current
    );
}

// Production audit: enqueue after natural end leaves new tracks unreachable.
// upcoming() returns [] when current is None, so freshly enqueued tracks after
// queue end require manual previous() gymnastics instead of being listed.
#[test]
fn queue_enqueue_after_natural_end_is_reachable() {
    let mut q = Queue::default();
    q.replace(vec![track("0"), track("1")], 0).unwrap();
    assert!(q.advance(false).is_some());
    assert!(q.advance(false).is_none());
    assert!(q.track().is_none());
    q.insert(vec![track("2")], false).unwrap();
    let upcoming = q.upcoming(5);
    assert!(
        upcoming.iter().any(|i| i.rating_key == "2"),
        "enqueued track after natural end unreachable; upcoming={:?}",
        upcoming.iter().map(|i| &i.rating_key).collect::<Vec<_>>()
    );
}

// Production audit: corrupt persisted cache config must not prevent startup.
// store::load returns Settings without validation; Cache::new then fails and
// Core::build propagates Err, so one bad session.json bricks the app.
#[test]
fn corrupt_cache_config_must_not_prevent_startup() {
    use plexfreq_core::Core;
    let dir = tempfile::tempdir().unwrap();
    let mut settings = plexfreq_core::store::Settings::default();
    settings.cache.limit_mb = u64::MAX;
    settings.cache.ahead = usize::MAX;
    plexfreq_core::store::save(dir.path(), &settings).unwrap();
    let core = Core::new(dir.path().into());
    assert!(
        core.is_ok(),
        "corrupt cache config bricks startup; should fall back to defaults, got {:?}",
        core.err()
    );
}

// Production audit: storage keys containing path separators must be rejected
// or hashed. Library::save_lyrics interpolates the key into a filename; a key
// with '/' currently creates a nested path that fails silently (parent dir
// missing) and loses lyrics instead of returning an error or sanitizing.
#[test]
fn offline_lyrics_slash_key_must_be_rejected_or_hashed() {
    use plexfreq_core::offline::Library;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("library-cache");
    let library = Library::new(root.clone(), true).unwrap();
    // Numeric keys are the only valid production input (enforced by Command).
    // A slash key must not silently succeed while writing nowhere.
    library.save_lyrics(
        "ns",
        "a/b",
        &[plexfreq_core::lyrics::Line {
            time: None,
            text: "x".into(),
        }],
    );
    // Current code joins root with "ns-lyrics-a/b.json" whose parent does not
    // exist, so the write fails silently and the read returns None. A fixed
    // implementation must either reject invalid keys or hash them so the
    // round-trip works. Either way, silent loss is the bug.
    let roundtrip = {
        let lib2 = Library::new(root.clone(), true).unwrap();
        lib2.lyrics("ns", "a/b")
    };
    // If keys are validated, save should have failed loudly; since save
    // returns (), the only observable correct behavior is a successful
    // round-trip (hashed/sanitized) or an explicit rejection path. Today it
    // silently loses data.
    assert!(
        roundtrip.is_some(),
        "slash key silently lost lyrics; Library must hash/sanitize keys or reject them loudly"
    );
}

// Production audit: station_key returns any nested "key", so an outer
// container key shadows the real Playlist station key. Must prefer the
// Playlist/Stations child shape explicitly.
#[test]
fn radio_station_key_prefers_playlist_over_outer_key() {
    use plexfreq_core::radio::station_key;
    let base = url::Url::parse("https://music.example/").unwrap();
    let good = "/library/metadata/42/station?type=10";
    let item = Item {
        stations: serde_json::json!({"key": "/bad", "Playlist": [{"key": good}]}),
        ..Default::default()
    };
    let key = station_key(&item, &base).unwrap();
    assert_eq!(
        key.as_deref(),
        Some(good),
        "outer container key shadowed real station key; got {key:?}"
    );
}

// Production audit: server_path allows ".." segments that normalize to a
// different endpoint under the same origin. Must reject to prevent query
// smuggling / endpoint confusion from server-supplied hub keys.
#[test]
fn server_path_rejects_dotdot_segments() {
    let base = url::Url::parse("https://music.example/").unwrap();
    let result = plexfreq_core::plex::server_path(&base, "/library/../admin");
    assert!(
        result.is_err(),
        "dotdot path normalized to {:?}; must be rejected",
        result.map(|u| u.to_string())
    );
}

// Production audit: artwork MIME check is case-sensitive without trim.
// Real servers/proxies vary case ("Image/JPEG"); must accept
// case-insensitively. Exercises the public Artwork worker end-to-end.
#[test]
fn artwork_accepts_case_insensitive_image_mime() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
        time::{Duration, Instant},
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let task = thread::spawn(move || {
        // Serve a single request then exit so task.join() cannot block.
        // Use a short accept timeout: if no job arrives, exit anyway.
        listener.set_nonblocking(false).unwrap();
        // Bound the whole server lifetime: accept with timeout via thread scope.
        // Simplest: single blocking accept with 6s OS-level behavior achieved
        // by spawning and joining with overall test timeout (5s poll + join).
        // Here we accept exactly once; Artwork sends exactly one job.
        let Ok((mut socket, _)) = listener.accept() else {
            return;
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buf = [0; 4096];
        loop {
            match socket.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    request.extend_from_slice(&buf[..n]);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let body = vec![0xFFu8; 1024];
        let _ = write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: Image/JPEG\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len(),
            );
        let _ = socket.write_all(&body);
    });
    let dir = tempfile::tempdir().unwrap();
    let artwork = plexfreq_core::offline::Artwork::new(dir.path().into(), true).unwrap();
    let server = format!("http://{addr}/");
    // Valid Plex image path under the mock origin.
    artwork.schedule(&server, "token", "/photo/:/transcode?url=test");
    let namespace = plexfreq_core::cache::namespace(&server, "token");
    let until = Instant::now() + Duration::from_secs(5);
    let mut found = None;
    while Instant::now() < until {
        found = artwork.local(&namespace, "/photo/:/transcode?url=test");
        if found.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = task.join();
    assert!(
        found.is_some(),
        "case-variant Image/JPEG rejected; must be accepted case-insensitively"
    );
}
