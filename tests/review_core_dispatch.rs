//! Core dispatcher review: failing regressions for logic defects in
//! `src/lib.rs` (`Core::execute`) and its offline/refresh helpers.
//! Each test asserts the correct behaviour and FAILS until the defect is fixed.
use plexfreq_core::{Command, Core};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

struct Server {
    url: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn new(handler: impl Fn(&str) -> Value + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let worker = thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut bytes = Vec::new();
                        let mut buffer = [0; 4096];
                        while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                            let n = stream.read(&mut buffer).unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            bytes.extend_from_slice(&buffer[..n]);
                        }
                        if bytes.is_empty() {
                            continue;
                        }
                        let request = String::from_utf8(bytes).unwrap();
                        let body = handler(&request).to_string().into_bytes();
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(&body);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(e) => panic!("{e}"),
                }
            }
        });
        Self {
            url,
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn call(core: &mut Core, input: Value) -> plexfreq_core::Result<Value> {
    core.execute(serde_json::from_value::<Command>(input).unwrap())
}

fn track(id: &str) -> Value {
    json!({"ratingKey":id,"key":format!("/library/metadata/{id}"),"type":"track","title":format!("Track {id}"),"duration":180000,"parentRatingKey":"20","parentTitle":"Album","grandparentRatingKey":"10","grandparentTitle":"Artist","Media":[{"audioCodec":"flac","Part":[{"key":format!("/library/parts/{id}/audio.flac")}]}]})
}

fn empty() -> Value {
    json!({"MediaContainer":{"Directory":[],"Metadata":[]}})
}

fn connect(core: &mut Core, server: &Server) {
    call(
        core,
        json!({"op":"download_policy","wifi_only":false,"paused":true}),
    )
    .unwrap();
    call(
        core,
        json!({"op":"connect","url":server.url,"token":"fixture"}),
    )
    .unwrap();
}

/// Query parameter value from a raw HTTP request line.
fn param(request: &str, name: &str) -> Option<usize> {
    let line = request.lines().next()?;
    let query = line.split_whitespace().nth(1)?.split_once('?')?.1;
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
        .and_then(|v| v.parse().ok())
}

/// Plex-like playlist with `count` tracks. `total` controls whether pages carry
/// `totalSize` (Plex may omit it; callers must then stop on a short/empty page).
fn playlist_server(count: usize, total: bool) -> Server {
    Server::new(move |r| {
        if r.starts_with("GET /playlists/5/items?") {
            let start = param(r, "X-Plex-Container-Start").unwrap_or(0);
            let size = param(r, "X-Plex-Container-Size").unwrap_or(100);
            let items: Vec<_> = (start..count.min(start + size))
                .map(|i| track(&(1000 + i).to_string()))
                .collect();
            let mut container = json!({"size":items.len(),"Metadata":items});
            if total {
                container["totalSize"] = json!(count);
            }
            return json!({ "MediaContainer": container });
        }
        if r.starts_with("GET /playlists/5") {
            return json!({"MediaContainer":{"Metadata":[{"ratingKey":"5","type":"playlist","title":"Road trip","playlistType":"audio"}]}});
        }
        empty()
    })
}

// lib.rs `Command::ClearCache` clears `download_groups` but leaves
// `download_plans` (and `download_titles`) behind. The orphaned plan is still
// iterated by `maintain_refresh`, which re-collects and re-installs the
// playlist: downloads the user just cleared silently come back.
#[test]
fn clear_cache_must_not_leave_refreshable_download_plans() {
    let server = playlist_server(3, true);
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    call(
        &mut core,
        json!({"op":"pin","key":"5","kind":"playlist","enabled":true}),
    )
    .unwrap();
    call(&mut core, json!({"op":"clear_cache"})).unwrap();
    call(
        &mut core,
        json!({"op":"refresh_policy","enabled":true,"interval_hours":1}),
    )
    .unwrap();
    call(
        &mut core,
        json!({"op":"network_state","wifi":true,"online":true}),
    )
    .unwrap();
    call(
        &mut core,
        json!({"op":"download_policy","wifi_only":false,"paused":false}),
    )
    .unwrap();
    let status = call(&mut core, json!({"op":"refresh_downloads"})).unwrap();
    assert_eq!(
        status["cache"]["pinnedGroups"],
        json!([]),
        "clear_cache must remove every download group"
    );
    assert_eq!(
        status["cache"]["refreshingGroup"],
        Value::Null,
        "a cleared download must not be scheduled for background re-download"
    );
}

// lib.rs `Command::QueueMove` no-op branch (`from == to`) serializes the raw
// internal `Radio` struct (`"radio": self.radio`) while every other command
// emits `Radio::summary()`. The DTO contract changes shape (`source` becomes
// an object; sonic radio leaks the full seed item) depending on the indices.
#[test]
fn queue_move_noop_reports_radio_summary_like_every_other_command() {
    let server = Server::new(|r| {
        if r.starts_with("GET /identity") {
            return json!({"MediaContainer":{"machineIdentifier":"fixture-server"}});
        }
        if r.starts_with("POST /playQueues?") {
            let mut item = track("1");
            item["playQueueItemID"] = json!(1);
            return json!({"MediaContainer":{"playQueueID":7,"playQueueSelectedItemID":1,"Metadata":[item]}});
        }
        empty()
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    let started = call(
        &mut core,
        json!({"op":"station","key":"/library/sections/1/station/1","title":"Library radio"}),
    )
    .unwrap();
    assert_eq!(started["radio"]["source"], "station");
    let moved = call(&mut core, json!({"op":"queue_move","from":0,"to":0})).unwrap();
    assert_eq!(
        moved["radio"], started["radio"],
        "no-op queue_move must report the same radio summary DTO"
    );
}

// lib.rs `Core::alphabet` calls `self.plex.container` directly instead of
// `container_cached`, so it ignores `offline_mode` (contacts the server while
// the user chose offline mode) and never saves the index for offline use.
#[test]
fn alphabet_respects_offline_mode() {
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    let server = Server::new(move |r| {
        if r.starts_with("GET /library/sections/1/firstCharacter") {
            seen.fetch_add(1, Ordering::SeqCst);
            return json!({"MediaContainer":{"Directory":[{"title":"A","size":3}]}});
        }
        empty()
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    call(&mut core, json!({"op":"offline_mode","enabled":true})).unwrap();
    let _ = call(&mut core, json!({"op":"alphabet","section":"1"}));
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "offline mode must not contact the server for the artist alphabet"
    );
}

// lib.rs `Command::Pin` playlist loop checks `tracks.len() >= 1000` *before*
// fetching the next page, so a playlist of exactly 1000 tracks without
// `totalSize` is rejected as "exceeds 1000 tracks" although the follow-up page
// is empty and the 1000-track plan limit is otherwise accepted.
#[test]
fn pin_playlist_with_exactly_1000_tracks_and_unknown_total_succeeds() {
    let server = playlist_server(1000, false);
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    let result = call(
        &mut core,
        json!({"op":"pin","key":"5","kind":"playlist","enabled":true}),
    );
    assert!(
        result.is_ok(),
        "1000-track playlist must be pinnable: {:?}",
        result.err().map(|e| e.to_string())
    );
}

// lib.rs `Core::build` validates persisted cache/audio/quality configs but not
// `refresh_policy`. A persisted `intervalHours: 0` makes `Scheduler::due`
// treat every plan as stale (`elapsed >= 0`) so plans re-download on every
// 15-minute attempt window, and the UI shows an impossible interval.
#[test]
fn corrupt_refresh_policy_falls_back_to_a_valid_interval() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("session.json"),
        json!({"refreshPolicy":{"enabled":true,"intervalHours":0}}).to_string(),
    )
    .unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    let status = call(&mut core, json!({"op":"status"})).unwrap();
    let hours = status["cache"]["refreshHours"].as_u64().unwrap();
    assert!(
        [1, 6, 12, 24].contains(&hours),
        "persisted refresh interval {hours} must be validated on startup"
    );
}

// lib.rs `Core::connect` unconditionally clears download groups, plans,
// titles and the manual cache. Re-selecting the *same* server (same URL and
// token, hence the same cache namespace whose audio files remain valid) wipes
// every pinned download, losing the user's offline selections.
#[test]
fn reconnecting_to_the_same_server_keeps_pinned_downloads() {
    let server = Server::new(|_| empty());
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    call(&mut core, json!({"op":"cache_tracks","items":[track("1")]})).unwrap();
    connect(&mut core, &server);
    let status = call(&mut core, json!({"op":"status"})).unwrap();
    assert_eq!(
        status["cache"]["pinnedGroups"],
        json!(["track:1"]),
        "reconnecting to the same server must not discard pinned downloads"
    );
}
