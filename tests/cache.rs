use plexfreq_core::{
    cache::{namespace, Cache, Config},
    model::{Item, Media, Part},
    Command, Core,
};
use serde_json::json;
use std::{
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    thread,
    time::{Duration, Instant},
};

fn item(id: &str) -> Item {
    Item {
        rating_key: id.into(),
        kind: "track".into(),
        title: "Fixture audio".into(),
        duration: 10000,
        media: vec![Media {
            parts: vec![Part {
                key: format!("/library/parts/{id}/file.wav"),
                size: None,
                ..Default::default()
            }],
        }],
        ..Default::default()
    }
}
fn wait(mut condition: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(8);
    while !condition() {
        assert!(
            Instant::now() < until,
            "cache did not reach the expected state"
        );
        thread::sleep(Duration::from_millis(20));
    }
}
fn request(socket: &mut std::net::TcpStream) -> String {
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buf = [0; 4096];
    while !bytes.windows(4).any(|b| b == b"\r\n\r\n") {
        let n = socket.read(&mut buf).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buf[..n]);
    }
    String::from_utf8(bytes).unwrap()
}
fn origin(listener: &TcpListener) -> String {
    format!("http://{}/", listener.local_addr().unwrap())
}

#[test]
fn completed_files_survive_restart_are_private_and_scoped_to_credentials() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = origin(&listener);
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let req = request(&mut socket);
        assert!(req.to_lowercase().contains("x-plex-token: fixture-secret"));
        assert!(!req.contains("X-Plex-Token="));
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: 10\r\nETag: \"one\"\r\nConnection: close\r\n\r\nabcdefghij").unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let ns = namespace(&url, "fixture-secret");
    let song = item("1");
    let cache = Cache::new(dir.path().into(), Config::default(), "fixture-client").unwrap();
    cache
        .schedule(&url, "fixture-secret", vec![song.clone()])
        .unwrap();
    wait(|| cache.lookup(&ns, &song, false).is_some());
    let (path, _) = cache.lookup(&ns, &song, true).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"abcdefghij");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(cache
        .lookup(&namespace(&url, "different-user"), &song, false)
        .is_none());
    assert!(!std::fs::read_to_string(dir.path().join("index.json"))
        .unwrap()
        .contains("fixture-secret"));
    cache.cancel();
    drop(cache);
    server.join().unwrap();
    let cache = Cache::new(dir.path().into(), Config::default(), "fixture-client").unwrap();
    assert!(cache.lookup(&ns, &song, false).is_some());
    std::fs::write(path, b"truncated").unwrap();
    assert!(cache.lookup(&ns, &song, false).is_none());
}

#[test]
fn current_http_representation_can_differ_from_persistent_plex_part_size() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = origin(&listener);
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        request(&mut socket);
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: 10\r\nETag: \"current\"\r\nConnection: close\r\n\r\nabcdefghij").unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::new(dir.path().into(), Config::default(), "fixture").unwrap();
    let ns = namespace(&url, "token");
    let mut song = item("1");
    song.media[0].parts[0].size = Some(12);
    cache.schedule(&url, "token", vec![song.clone()]).unwrap();
    wait(|| cache.lookup(&ns, &song, false).is_some());
    let (path, metadata) = cache.lookup(&ns, &song, false).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"abcdefghij");
    assert_eq!(metadata.media[0].parts[0].size, Some(10));
    server.join().unwrap();
}

#[test]
fn interrupted_audio_resumes_with_validated_range_instead_of_playing_partial_data() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = origin(&listener);
    let server = thread::spawn(move || {
        {
            let (mut socket, _) = listener.accept().unwrap();
            request(&mut socket);
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: 10\r\nETag: \"one\"\r\nConnection: close\r\n\r\nabcd").unwrap();
        }
        let (mut socket, _) = listener.accept().unwrap();
        let req = request(&mut socket).to_lowercase();
        assert!(req.contains("range: bytes=4-"));
        assert!(req.contains("if-range: \"one\""));
        socket.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Type: audio/wav\r\nContent-Length: 6\r\nContent-Range: bytes 4-9/10\r\nETag: \"one\"\r\nConnection: close\r\n\r\nefghij").unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::new(dir.path().into(), Config::default(), "fixture").unwrap();
    let song = item("1");
    let ns = namespace(&url, "token");
    cache.schedule(&url, "token", vec![song.clone()]).unwrap();
    wait(|| !cache.status(&ns)["error"].as_str().unwrap().is_empty());
    assert!(cache.lookup(&ns, &song, false).is_none());
    cache.schedule(&url, "token", vec![song.clone()]).unwrap();
    wait(|| cache.lookup(&ns, &song, false).is_some());
    assert_eq!(
        std::fs::read(cache.lookup(&ns, &song, false).unwrap().0).unwrap(),
        b"abcdefghij"
    );
    server.join().unwrap();
}

#[test]
fn a_server_ignoring_range_restarts_the_file_without_appending_duplicate_bytes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = origin(&listener);
    let server = thread::spawn(move || {
        {
            let (mut socket, _) = listener.accept().unwrap();
            request(&mut socket);
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: 10\r\nETag: \"old\"\r\nConnection: close\r\n\r\nabcd").unwrap();
        }
        let (mut socket, _) = listener.accept().unwrap();
        assert!(request(&mut socket)
            .to_lowercase()
            .contains("range: bytes=4-"));
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: 10\r\nETag: \"new\"\r\nConnection: close\r\n\r\n0123456789").unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::new(dir.path().into(), Config::default(), "fixture").unwrap();
    let song = item("1");
    let ns = namespace(&url, "token");
    cache.schedule(&url, "token", vec![song.clone()]).unwrap();
    wait(|| !cache.status(&ns)["error"].as_str().unwrap().is_empty());
    cache.schedule(&url, "token", vec![song.clone()]).unwrap();
    wait(|| cache.lookup(&ns, &song, false).is_some());
    assert_eq!(
        std::fs::read(cache.lookup(&ns, &song, false).unwrap().0).unwrap(),
        b"0123456789"
    );
    server.join().unwrap();
}

#[test]
fn malformed_resume_and_non_audio_responses_never_become_ready() {
    for response in [
        b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 5\r\nConnection: close\r\n\r\nlogin".as_slice(),
        b"HTTP/1.1 206 Partial Content\r\nContent-Type: audio/wav\r\nContent-Length: 5\r\nContent-Range: bytes 5-9/10\r\nConnection: close\r\n\r\n12345".as_slice(),
        b"HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: 70000000\r\nConnection: close\r\n\r\n".as_slice(),
    ] {
        let listener=TcpListener::bind("127.0.0.1:0").unwrap(); let url=origin(&listener); let bytes=response.to_vec();
        let server=thread::spawn(move || { let (mut socket,_)=listener.accept().unwrap(); request(&mut socket); socket.write_all(&bytes).unwrap(); });
        let dir=tempfile::tempdir().unwrap(); let cache=Cache::new(dir.path().into(),Config{enabled:true,limit_mb:64,ahead:5},"fixture").unwrap(); let ns=namespace(&url,"token"); let song=item("1");
        cache.schedule(&url,"token",vec![song.clone()]).unwrap(); wait(||!cache.status(&ns)["error"].as_str().unwrap().is_empty());
        assert!(cache.lookup(&ns,&song,false).is_none()); assert_eq!(cache.status(&ns)["tracks"],0); cache.cancel(); server.join().unwrap();
    }
}

#[test]
fn cached_catalog_and_playback_work_after_server_disappears() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = origin(&listener);
    let server = thread::spawn(move || {
        {
            let (mut socket, _) = listener.accept().unwrap();
            assert!(request(&mut socket).contains("/library/sections"));
            let body = json!({"MediaContainer":{"Directory":[]}}).to_string();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
        let (mut socket, _) = listener.accept().unwrap();
        assert!(request(&mut socket).contains("/library/parts/1/file.wav"));
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: 10\r\nConnection: close\r\n\r\nabcdefghij").unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    core.execute(Command::Connect {
        url,
        token: "fixture-secret".into(),
    })
    .unwrap();
    core.execute(Command::CacheTracks {
        items: vec![item("1")],
    })
    .unwrap();
    wait(|| core.execute(Command::CacheStatus).unwrap()["cache"]["tracks"] == 1);
    server.join().unwrap();
    let catalog = core.execute(Command::CachedTracks).unwrap();
    assert_eq!(catalog["items"].as_array().unwrap().len(), 1);
    let play = core
        .execute(Command::Play {
            items: vec![item("1")],
            index: 0,
        })
        .unwrap();
    assert_eq!(play["playbackSource"], "cache");
    assert!(play["stream"].as_str().unwrap().starts_with("file:///"));
    core.execute(Command::SavePlayback {
        key: "1".into(),
        position: 2300,
        generation: None,
    })
    .unwrap();
    drop(core);
    let mut core = Core::new(dir.path().into()).unwrap();
    assert_eq!(
        core.execute(Command::Status).unwrap()["resumePosition"],
        2300
    );
    let resumed = core.execute(Command::Resume).unwrap();
    assert_eq!(resumed["resumePosition"], 2300);
    assert_eq!(resumed["playbackSource"], "cache");
    core.execute(Command::SavePlayback {
        key: "1".into(),
        position: 8000,
        generation: play["playbackGeneration"].as_u64(),
    })
    .unwrap();
    assert_eq!(
        core.execute(Command::Status).unwrap()["resumePosition"],
        2300
    );
    let cached = core.execute(Command::CachedTracks).unwrap();
    let items = serde_json::from_value(cached["items"].clone()).unwrap();
    assert_eq!(
        core.execute(Command::Play { items, index: 0 }).unwrap()["playbackSource"],
        "cache"
    );
    core.execute(Command::ClearCache).unwrap();
    assert_eq!(
        core.execute(Command::CacheStatus).unwrap()["cache"]["tracks"],
        0
    );
}

#[test]
fn queue_prefetch_follows_shuffle_order_instead_of_library_order() {
    let mut queue = plexfreq_core::queue::Queue::default();
    queue
        .replace((0..10).map(|i| item(&i.to_string())).collect(), 4)
        .unwrap();
    queue.shuffle(true);
    let ready = queue.upcoming(6);
    assert_eq!(ready[0].rating_key, "4");
    for expected in ready.iter().skip(1) {
        assert_eq!(
            queue.advance(false).unwrap().rating_key,
            expected.rating_key
        );
    }
}

#[test]
fn partial_download_recovers_automatically_after_core_restart() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = origin(&listener);
    let server = thread::spawn(move || {
        {
            let (mut socket, _) = listener.accept().unwrap();
            request(&mut socket);
            let body = json!({"MediaContainer":{"Directory":[]}}).to_string();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
        {
            let (mut socket, _) = listener.accept().unwrap();
            request(&mut socket);
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: 10\r\nETag: \"one\"\r\nConnection: close\r\n\r\nabcd").unwrap();
        }
        let (mut socket, _) = listener.accept().unwrap();
        assert!(request(&mut socket)
            .to_lowercase()
            .contains("range: bytes=4-"));
        socket.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Type: audio/wav\r\nContent-Length: 6\r\nContent-Range: bytes 4-9/10\r\nETag: \"one\"\r\nConnection: close\r\n\r\nefghij").unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    core.execute(Command::Connect {
        url,
        token: "token".into(),
    })
    .unwrap();
    core.execute(Command::CacheTracks {
        items: vec![item("1")],
    })
    .unwrap();
    wait(|| {
        !core.execute(Command::CacheStatus).unwrap()["cache"]["error"]
            .as_str()
            .unwrap()
            .is_empty()
    });
    drop(core);
    let mut core = Core::new(dir.path().into()).unwrap();
    wait(|| core.execute(Command::CacheStatus).unwrap()["cache"]["tracks"] == 1);
    assert_eq!(
        core.execute(Command::Play {
            items: vec![item("1")],
            index: 0
        })
        .unwrap()["playbackSource"],
        "cache"
    );
    server.join().unwrap();
}

#[test]
fn clearing_cache_cancels_an_inflight_transfer_without_republishing_it() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = origin(&listener);
    let (release, wait_release) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        request(&mut socket);
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: 10\r\nETag: \"one\"\r\nConnection: close\r\n\r\nabcd").unwrap();
        wait_release.recv_timeout(Duration::from_secs(5)).unwrap();
        let _ = socket.write_all(b"efghij");
    });
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::new(dir.path().into(), Config::default(), "fixture").unwrap();
    let ns = namespace(&url, "token");
    let song = item("1");
    cache.schedule(&url, "token", vec![song.clone()]).unwrap();
    wait(|| cache.status(&ns)["received"] == 4);
    cache.clear().unwrap();
    release.send(()).unwrap();
    server.join().unwrap();
    thread::sleep(Duration::from_millis(100));
    assert!(cache.lookup(&ns, &song, false).is_none());
    assert_eq!(cache.status(&ns)["tracks"], 0);
    assert_eq!(cache.status(&ns)["bytes"], 0);
}

#[test]
fn shared_network_hint_cancels_inflight_audio_without_the_core_command_queue() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = origin(&listener);
    let (release, waiting) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        request(&mut socket);
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: 10\r\nETag: \"one\"\r\nConnection: close\r\n\r\nabcd").unwrap();
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        let _ = socket.write_all(b"efghij");
    });
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::new(dir.path().into(), Config::default(), "fixture").unwrap();
    let art_epoch = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let control = cache.control(art_epoch.clone());
    control.policy(true, false);
    control.network(true);
    let ns = namespace(&url, "token");
    let song = item("1");
    cache.schedule(&url, "token", vec![song.clone()]).unwrap();
    wait(|| cache.status(&ns)["received"] == 4);
    thread::spawn(move || control.network(false))
        .join()
        .unwrap();
    assert!(!cache.gate().allowed());
    assert!(art_epoch.load(std::sync::atomic::Ordering::SeqCst) > 0);
    release.send(()).unwrap();
    server.join().unwrap();
    thread::sleep(Duration::from_millis(100));
    assert!(cache.lookup(&ns, &song, false).is_none());
}
