//! Regression coverage for production-review follow-ups.

use plexfreq_core::{
    cache::{namespace, Cache, Config},
    model::{Container, Item},
    queue::Queue,
    Command, Core,
};
use serde_json::json;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

fn track(id: &str) -> Item {
    Item {
        rating_key: id.into(),
        kind: "track".into(),
        duration: 120_000,
        ..Default::default()
    }
}

fn album(id: &str, parent: &str, title: &str) -> Item {
    Item {
        rating_key: id.into(),
        kind: "album".into(),
        title: title.into(),
        parent_rating_key: parent.into(),
        ..Default::default()
    }
}

fn artist(id: &str, title: &str) -> Item {
    Item {
        rating_key: id.into(),
        kind: "artist".into(),
        title: title.into(),
        ..Default::default()
    }
}

// Queue::reorder() resets cursor to 0 when current is None (natural end).
// Moving unrelated rows must not change which track previous() resumes.
#[test]
fn queue_move_after_natural_end_preserves_last_audible_track() {
    let mut q = Queue::default();
    q.replace(vec![track("0"), track("1"), track("2")], 0)
        .unwrap();
    assert_eq!(q.advance(false).unwrap().rating_key, "1");
    assert_eq!(q.advance(false).unwrap().rating_key, "2");
    assert!(q.advance(false).is_none());
    assert!(q.track().is_none());

    // Reorder unrelated entries while at natural end.
    q.move_item(0, 1).unwrap();

    // Must still resume the last audible track ("2"), not the new first row.
    assert_eq!(
        q.previous().unwrap().rating_key,
        "2",
        "move at natural end must not lose the last playback slot"
    );
}

#[test]
fn queue_move_last_audible_after_natural_end_preserves_occurrence() {
    let mut q = Queue::default();
    q.replace(vec![track("0"), track("1"), track("2")], 0)
        .unwrap();
    q.advance(false);
    q.advance(false);
    q.advance(false);
    q.move_item(2, 0).unwrap();
    q.validate().unwrap();
    assert_eq!(q.previous().unwrap().rating_key, "2");
}

#[test]
fn queue_shuffle_after_natural_end_preserves_occurrence() {
    let mut q = Queue::default();
    q.replace(vec![track("0"), track("1"), track("2")], 0)
        .unwrap();
    q.advance(false);
    q.advance(false);
    q.advance(false);
    q.shuffle(true);
    q.validate().unwrap();
    assert_eq!(q.previous().unwrap().rating_key, "2");
    assert!(q.advance(false).is_none());
}

#[test]
fn queue_remove_final_slot_after_natural_end_keeps_valid_cursor() {
    let mut q = Queue::default();
    q.replace(vec![track("0"), track("1"), track("2")], 0)
        .unwrap();
    q.advance(false);
    q.advance(false);
    q.advance(false);
    q.remove(2).unwrap();
    q.validate().unwrap();
    assert_eq!(q.previous().unwrap().rating_key, "1");
}

// Same root cause via shuffle(): toggling shuffle at natural end loses cursor.
#[test]
fn queue_shuffle_after_natural_end_preserves_last_slot() {
    let mut q = Queue::default();
    q.replace(vec![track("0"), track("1"), track("2")], 0)
        .unwrap();
    q.advance(false);
    q.advance(false);
    assert!(q.advance(false).is_none());
    assert!(q.track().is_none());

    q.shuffle(true);

    // previous() must resume *some* previously audible track (the last slot),
    // and the queue must still validate. Resetting cursor to 0 makes it
    // resume the shuffled first slot instead of the last.
    let resumed = q.previous().unwrap().rating_key.clone();
    assert!(
        ["0", "1", "2"].contains(&resumed.as_str()),
        "unexpected resumed track {resumed}"
    );
    // The strong assertion: after natural end cursor pointed at the final
    // playback slot (2 slots behind start in a 3-track queue). After shuffle
    // the cursor must point at the last slot, not the first.
    // With the current reorder() bug cursor is 0, so at_end() is false and the
    // traversal restarts from the front instead of the back.
    // We assert the cursor is at the end of the shuffled order.
    let mut probe = q.clone();
    // Consuming previous() again must hit the start quickly if we resumed the
    // last element, otherwise we walk the whole queue.
    let mut steps = 0;
    while probe.advance(false).is_some() {
        steps += 1;
        if steps > 5 {
            break;
        }
    }
    assert!(
        steps <= 1,
        "shuffle at natural end reset cursor to front (walked {steps} extra tracks)"
    );
}

// Offline detail() fallback ignores `start`: it returns every known child
// with "start":<requested> and hasMore:false, so paged offline browsing
// duplicates rows.
#[test]
fn offline_detail_fallback_respects_pagination_start() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let task = thread::spawn(move || {
        // Only Connect is served over HTTP; everything else is seeded directly
        // into library-cache below.
        let (mut socket, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buf = [0; 4096];
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        loop {
            let n = socket.read(&mut buf).unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buf[..n]);
            if request.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let body = json!({"MediaContainer":{"Directory":[]}}).to_string();
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
        url: origin.clone(),
        token: "fixture".into(),
    })
    .unwrap();
    task.join().unwrap();

    let ns = namespace(&origin, "fixture");

    // Seed artist metadata plus 150 albums via a *different* saved endpoint so
    // the children page for start=100 is NOT cached and the fallback runs.
    let lib = plexfreq_core::offline::Library::new(dir.path().join("library-cache"), true).unwrap();
    lib.save(
        &ns,
        "/library/metadata/10",
        &[("includeStations", "1".into())],
        &Container {
            items: vec![artist("10", "Fixture artist")],
            ..Default::default()
        },
    );
    let albums: Vec<Item> = (0..150)
        .map(|i| album(&format!("{i}"), "10", &format!("Album {i:03}")))
        .collect();
    lib.save(
        &ns,
        "/library/sections/1/all",
        &[],
        &Container {
            items: albums,
            size: 150,
            total_size: Some(150),
            ..Default::default()
        },
    );

    core.execute(Command::OfflineMode { enabled: true })
        .unwrap();
    let page = core
        .execute(Command::Detail {
            key: "10".into(),
            start: 100,
        })
        .unwrap();
    let items = page["items"].as_array().unwrap();
    assert_eq!(
        items.len(),
        50,
        "offline fallback must paginate from start=100, got {} items",
        items.len()
    );
    assert_eq!(page["start"], 100);
}

// offline::Library::prune() double-counts an overwrite: total includes the old
// file while extra is the new size, so saving the same key at the quota edge
// evicts an unrelated file.
#[test]
fn offline_library_overwrite_does_not_evict_unrelated_snapshot() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let lib = plexfreq_core::offline::Library::new(dir.path().into(), true).unwrap();
    let ns = "fixture-namespace";

    // Two snapshots inflated to 32 MiB each (sparse, fast) sit exactly at the
    // 64 MiB cap.
    let victim_container = Container {
        items: vec![artist("1", "Victim")],
        ..Default::default()
    };
    lib.save(ns, "/library/sections/1/all", &[], &victim_container);
    let other = Container {
        items: vec![artist("2", "Other")],
        ..Default::default()
    };
    lib.save(ns, "/library/metadata/2", &[], &other);
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    paths.sort();
    assert_eq!(paths.len(), 2);
    // Identify victim by content, then inflate victim first so it is oldest.
    let mut victim_path = paths[0].clone();
    for path in &paths {
        if std::fs::read(path)
            .map(|b| String::from_utf8_lossy(&b).contains("Victim"))
            .unwrap_or(false)
        {
            victim_path = path.clone();
            break;
        }
    }
    let other_path = paths.iter().find(|p| *p != &victim_path).unwrap().clone();
    {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&victim_path)
            .unwrap();
        file.set_len(32 * 1024 * 1024).unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(50));
    {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&other_path)
            .unwrap();
        file.set_len(32 * 1024 * 1024).unwrap();
    }
    // Touch other to be newer (victim already older by creation order, but
    // enforce via sleep-free mtime bump using set_len again would update mtime;
    // instead just record names).
    let before: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(before.len(), 2);

    // Overwrite the *same* metadata path with a small payload. Correct
    // accounting is total - old + new = ~32 MiB, well under cap, so nothing
    // should be evicted. Buggy accounting is total + new = ~64 MiB + small >
    // cap, so it evicts the oldest (victim).
    let other_again = Container {
        items: vec![artist("2", "Other updated")],
        ..Default::default()
    };
    lib.save(ns, "/library/metadata/2", &[], &other_again);

    assert!(
        victim_path.exists(),
        "overwrite double-counted old+new and evicted an unrelated snapshot"
    );
    assert!(other_path.exists() || std::fs::read_dir(dir.path()).unwrap().count() == 2);
    let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
}

// atomic_write_bytes() names temps via with_extension("<uuid>.tmp"), so
// index.json orphans look like "index.<uuid>.tmp" whose stem is "index".
// Cache::new() only reclaims 64-hex stems, so crash orphans leak forever and
// are invisible to disk_bytes() quota accounting.
#[test]
fn cache_reclaims_crashed_index_tmp_instead_of_leaking_it() {
    let dir = tempfile::tempdir().unwrap();
    let cache_dir = dir.path().join("audio-cache");
    let cache = Cache::new(
        cache_dir.clone(),
        Config {
            enabled: true,
            limit_mb: 64,
            ahead: 5,
        },
        "fixture",
    )
    .unwrap();
    drop(cache);

    // Simulate a crash between temp write and rename for index.json.
    std::fs::write(
        cache_dir.join("index.12345678-1234-1234-1234-123456789abc.tmp"),
        b"orphan",
    )
    .unwrap();
    std::fs::write(
        cache_dir.join("index.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.tmp"),
        b"x",
    )
    .unwrap();

    let _reopened = Cache::new(
        cache_dir.clone(),
        Config {
            enabled: true,
            limit_mb: 64,
            ahead: 5,
        },
        "fixture",
    )
    .unwrap();

    let leftovers: Vec<String> = std::fs::read_dir(&cache_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "crashed atomic-write temps leaked: {leftovers:?}"
    );
}

#[test]
fn cache_counts_unreclaimed_temporary_bytes_in_quota() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Cache::new(dir.path().into(), Config::default(), "fixture").unwrap();
    std::fs::write(dir.path().join("index.json.fixture.tmp"), b"orphan").unwrap();
    assert_eq!(cache.status("fixture")["bytes"], 6);
}

// album_tracks() rejects exactly 1000 tracks when totalSize is unknown
// (uses >=1000 instead of >1000). Every other 1000-track limit in the tree
// allows exactly 1000.
#[test]
fn album_with_exactly_1000_tracks_and_unknown_total_is_playable() {
    use std::time::{Duration, Instant};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let task = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut served_sections = false;
        let mut idle_since = Instant::now();
        let mut served_children = 0;
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    idle_since = Instant::now();
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
                    let request = String::from_utf8_lossy(&request).into_owned();
                    let body = if request.contains("/library/sections") && !served_sections {
                        served_sections = true;
                        json!({"MediaContainer":{"Directory":[]}}).to_string()
                    } else if request.contains("/library/metadata/10/children") {
                        served_children += 1;
                        // 10 full pages of 100, then an empty page; no totalSize.
                        let start = request
                            .split("X-Plex-Container-Start=")
                            .nth(1)
                            .and_then(|s| {
                                s.split(|c: char| !c.is_ascii_digit())
                                    .next()
                                    .and_then(|n| n.parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if start >= 1000 {
                            json!({"MediaContainer":{"size":0,"Metadata":[]}}).to_string()
                        } else {
                            let items: Vec<_> = (start..start + 100)
                                .map(|i| {
                                    json!({"ratingKey":i.to_string(),"type":"track","title":format!("T{i}"),"Media":[{"Part":[{"key":format!("/library/parts/{i}/file.mp3")}]}]})
                                })
                                .collect();
                            json!({"MediaContainer":{"size":100,"Metadata":items}}).to_string()
                        }
                    } else {
                        json!({"MediaContainer":{"size":0,"Metadata":[]}}).to_string()
                    };
                    let _ = write!(
                        socket,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    // Buggy code stops after 10 pages (no empty-page fetch);
                    // fixed code fetches the 11th empty page. Exit promptly in
                    // both cases after 2s idle with at least the sections call.
                    if served_children >= 11 {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if served_sections
                        && served_children >= 10
                        && idle_since.elapsed() > Duration::from_secs(2)
                    {
                        break;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    core.execute(Command::CacheConfig {
        enabled: false,
        limit_mb: 64,
        ahead: 0,
    })
    .unwrap();
    core.execute(Command::Connect {
        url: origin,
        token: "fixture".into(),
    })
    .unwrap();
    let result = core.execute(Command::PlayAlbum { key: "10".into() });
    task.join().unwrap();
    assert!(
        result.is_ok(),
        "exactly-1000-track album with unknown total must play, got {result:?}"
    );
    assert_eq!(
        result.unwrap()["queue"]["items"].as_array().unwrap().len(),
        1000
    );
}
