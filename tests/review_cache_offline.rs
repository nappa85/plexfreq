//! Review findings for the audio cache, offline snapshots and session storage.
//! Each test fails against the current implementation and documents the defect.
use plexfreq_core::{
    cache::{namespace, Cache, Config},
    lyrics::Line,
    model::{Container, Item, Media, Part},
    offline::Library,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
    time::{Duration, Instant, SystemTime},
};

fn item(id: &str) -> Item {
    Item {
        rating_key: id.into(),
        kind: plexfreq_core::model::ItemKind::Track,
        title: "Fixture audio".into(),
        duration: 10000,
        media: vec![Media {
            parts: vec![Part {
                key: format!("/library/parts/{id}/file.wav"),
                size: None,
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn wait(mut condition: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(8);
    while !condition() {
        assert!(Instant::now() < until, "cache did not reach expected state");
        thread::sleep(Duration::from_millis(20));
    }
}

/// Same derivation as the private `cache::key` (namespace, ratingKey, part).
fn cache_id(ns: &str, item: &Item) -> String {
    let part = &item.media[0].parts[0].key;
    format!(
        "{:x}",
        Sha256::digest(format!("{ns}\0{}\0{part}", item.rating_key).as_bytes())
    )
}

fn seed_partial(dir: &std::path::Path, ns: &str, song: &Item, body: &[u8]) {
    let id = cache_id(ns, song);
    std::fs::write(dir.join(format!("{id}.part")), body).unwrap();
    std::fs::write(
        dir.join(format!("{id}.resume.json")),
        serde_json::to_vec(&json!({
            "quality_kbps": 0,
            "total": null,
            "validator": "\"v\"",
            "extension": "wav",
            "namespace": ns,
            "item": song,
        }))
        .unwrap(),
    )
    .unwrap();
}

// Cache::remove() (download group "remove") deletes completed entries but
// leaves `<id>.part` + `<id>.resume.json`. pending_items() then still reports
// the removed track, and Core startup / CacheConfig re-schedule it (or even
// copy it into manual_cache), so a download the user removed silently comes
// back after restart.
#[test]
fn removing_a_download_also_discards_its_resumable_partial() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::new(dir.path().into(), Config::default(), "fixture").unwrap();
    let ns = namespace("http://127.0.0.1:9/", "token");
    let song = item("1");
    seed_partial(dir.path(), &ns, &song, b"abcd");
    assert_eq!(cache.pending_items(&ns).len(), 1);
    cache.remove(&ns, "1").unwrap();
    assert!(
        cache.pending_items(&ns).is_empty(),
        "removed track is still resumable and will be downloaded again"
    );
}

// A chunked (unknown-length) original download that received every byte but
// was killed before the rename leaves a `.part` whose length equals the full
// file and a resume record with `total: null`. The next attempt sends
// `Range: bytes=<len>-`, a conforming server answers 416, and download()
// treats Http(416) as a terminal error without discarding the partial, so
// every later schedule repeats the same unsatisfiable range forever.
#[test]
fn unsatisfiable_resume_range_restarts_instead_of_failing_forever() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let server_stop = stop.clone();
    let server = thread::spawn(move || {
        while !server_stop.load(std::sync::atomic::Ordering::SeqCst) {
            let Ok((mut socket, _)) = listener.accept() else {
                thread::sleep(Duration::from_millis(10));
                continue;
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0; 4096];
            while !bytes.windows(4).any(|b| b == b"\r\n\r\n") {
                match socket.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => bytes.extend_from_slice(&buf[..n]),
                }
            }
            let req = String::from_utf8_lossy(&bytes).to_lowercase();
            let response: &[u8] = if req.contains("range: bytes=10-") {
                b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */10\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            } else {
                b"HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: 10\r\nETag: \"v\"\r\nConnection: close\r\n\r\nabcdefghij"
            };
            let _ = socket.write_all(response);
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::new(dir.path().into(), Config::default(), "fixture").unwrap();
    let ns = namespace(&url, "token");
    let song = item("1");
    seed_partial(dir.path(), &ns, &song, b"abcdefghij");
    let settled = |cache: &Cache| {
        cache.lookup(&ns, &song, false).is_some()
            || cache.status(&ns)["errors"]
                .as_object()
                .is_some_and(|e| e.contains_key("1"))
    };
    cache.schedule(&url, "token", vec![song.clone()]).unwrap();
    wait(|| settled(&cache));
    if cache.lookup(&ns, &song, false).is_none() {
        // The user (or the next queue change) retries the download.
        cache.schedule(&url, "token", vec![song.clone()]).unwrap();
        wait(|| settled(&cache));
    }
    let cached = cache.lookup(&ns, &song, false).is_some();
    let error = cache.status(&ns)["error"].clone();
    cache.cancel();
    drop(cache);
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    server.join().unwrap();
    assert!(
        cached,
        "stale full-length partial keeps producing HTTP 416; track can never be cached ({error})"
    );
}

// Library::rate() parses *every* `<namespace>-*` file as a Container and
// rewrites it. Lyrics files share that prefix and a short JSON array of
// lines deserializes as a Container (serde sequence form + lenient fields),
// so rating any track rewrites saved lyrics into a MediaContainer object and
// they can no longer be loaded offline.
#[test]
fn rating_a_track_does_not_destroy_saved_offline_lyrics() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::new(dir.path().into(), true).unwrap();
    let lines = vec![
        Line {
            time: Some(0),
            text: "first".into(),
        },
        Line {
            time: Some(1000),
            text: "second".into(),
        },
    ];
    library.save_lyrics("ns", "5", &lines);
    assert_eq!(library.lyrics("ns", "5").as_ref(), Some(&lines));
    library.rate("ns", "7", 8.0);
    assert_eq!(
        library.lyrics("ns", "5"),
        Some(lines),
        "rate() rewrote the lyrics file as a Container: {}",
        std::fs::read_to_string(dir.path().join("ns-lyrics-5.json")).unwrap()
    );
}

// Library::rate() rewrites every snapshot of the namespace even when it does
// not contain the rated key. Each rewrite resets the file mtime, which is the
// only age signal prune_excluding() uses, so one rating makes all snapshots
// "newest" (LRU order destroyed) and rewrites up to 64 MiB of flash.
#[test]
fn rating_a_track_leaves_unrelated_snapshots_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::new(dir.path().into(), true).unwrap();
    let container = Container {
        items: vec![item("1")],
        ..Default::default()
    };
    library.save("ns", "/library/metadata/1", &[], &container);
    let file = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with("ns-"))
        .unwrap()
        .path();
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(old)
        .unwrap();
    library.rate("ns", "2", 6.0);
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        old,
        "snapshot without the rated track was rewritten"
    );
}
