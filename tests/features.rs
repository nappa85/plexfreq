use plexfreq_core::{Command, Core};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

struct Fixture {
    url: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Fixture {
    fn new(handler: impl Fn(&str) -> (&'static str, Vec<u8>) + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let worker = thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        socket
                            .set_read_timeout(Some(Duration::from_secs(3)))
                            .unwrap();
                        let mut bytes = Vec::new();
                        let mut buf = [0; 4096];
                        while !bytes.windows(4).any(|b| b == b"\r\n\r\n") {
                            let n = socket.read(&mut buf).unwrap();
                            if n == 0 {
                                break;
                            }
                            bytes.extend_from_slice(&buf[..n]);
                        }
                        if !bytes.windows(4).any(|b| b == b"\r\n\r\n") {
                            continue;
                        }
                        let request = String::from_utf8(bytes).unwrap();
                        assert!(request.to_lowercase().contains("x-plex-token: fixture"));
                        let (kind, body) = handler(&request);
                        // Download cancellation can legitimately close the peer.
                        let _=write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len());
                        let _ = socket.write_all(&body);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("fixture socket failure: {e}"),
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
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn response(value: Value) -> (&'static str, Vec<u8>) {
    ("application/json", value.to_string().into_bytes())
}
fn sections() -> Value {
    json!({"MediaContainer":{"Directory":[{"key":"1","type":"artist","title":"Fixture"}]}})
}
fn connect(core: &mut Core, url: &str) {
    core.execute(Command::Connect {
        url: url.into(),
        token: "fixture".into(),
    })
    .unwrap();
}
fn pause_cache(core: &mut Core) {
    core.execute(Command::CacheConfig {
        enabled: false,
        limit_mb: 64,
        ahead: 0,
    })
    .unwrap();
}

#[test]
fn global_music_search_collections_and_favorites_use_documented_requests() {
    let fixture = Fixture::new(|request| {
        if request.starts_with("GET /library/sections ")
            || request.starts_with("GET /library/sections? ")
        {
            return response(sections());
        }
        if request.starts_with("PUT /:/rate?") {
            assert!(request.contains("key=3&identifier=com.plexapp.plugins.library&rating=10"));
            return ("text/plain", vec![]);
        }
        if request.starts_with("GET /hubs/search?") {
            assert!(request.contains("query=Fixture+%26+song&limit=100"));
            return response(
                json!({"MediaContainer":{"Hub":[{"Metadata":[{"type":"artist","ratingKey":"1","title":"Fixture artist"},{"type":"album","ratingKey":"2","title":"Fixture album"},{"type":"track","ratingKey":"3","title":"Fixture song"},{"type":"movie","ratingKey":"4","title":"Ignored"}]}]}}),
            );
        }
        assert!(request.starts_with("GET /library/sections/1/all?"));
        assert!(
            request.contains("userRating=10")
                || request.contains("addedAt%3Adesc")
                || request.contains("lastViewedAt%3Adesc&viewCount%3E%3E=0")
        );
        response(json!({"MediaContainer":{"size":0,"totalSize":0,"Metadata":[]}}))
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &fixture.url);
    pause_cache(&mut core);
    let results = core
        .execute(Command::Search {
            query: "Fixture & song".into(),
        })
        .unwrap();
    assert_eq!(results["items"].as_array().unwrap().len(), 3);
    core.execute(Command::Favorite {
        key: "3".into(),
        enabled: true,
    })
    .unwrap();
    for view in ["favorites", "added", "played"] {
        core.execute(Command::Collection {
            section: "1".into(),
            view: view.into(),
            start: 0,
        })
        .unwrap();
    }
    core.execute(Command::OfflineMode { enabled: true })
        .unwrap();
    let saved = core
        .execute(Command::Search {
            query: "song".into(),
        })
        .unwrap();
    assert_eq!(saved["items"][0]["userRating"], 10.0);
    assert_eq!(saved["offline"], true);
}

#[test]
fn lyrics_follow_advertised_stream_and_survive_offline_restart() {
    let fixture = Fixture::new(|request| {
        if request.starts_with("GET /library/sections ")
            || request.starts_with("GET /library/sections? ")
        {
            return response(sections());
        }
        if request.starts_with("GET /library/streams/7") {
            return (
                "text/plain",
                b"[00:01.00]First line\n[00:02]Second line".to_vec(),
            );
        }
        assert!(request.starts_with("GET /library/metadata/3?"));
        response(
            json!({"MediaContainer":{"Metadata":[{"ratingKey":"3","type":"track","Media":[{"Part":[{"Stream":[{"streamType":4,"format":"lrc","key":"/library/streams/7"}]}]}]}]}}),
        )
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &fixture.url);
    pause_cache(&mut core);
    let lyrics = core.execute(Command::Lyrics { key: "3".into() }).unwrap();
    assert_eq!(lyrics["lyrics"][0]["time"], 1000);
    drop(core);
    drop(fixture);
    let mut core = Core::new(dir.path().into()).unwrap();
    core.execute(Command::OfflineMode { enabled: true })
        .unwrap();
    assert_eq!(
        core.execute(Command::Lyrics { key: "3".into() }).unwrap(),
        lyrics
    );
}

#[test]
fn plex_json_lyrics_are_decoded_before_offline_storage() {
    let fixture = Fixture::new(|request| {
        if request.starts_with("GET /library/sections ")
            || request.starts_with("GET /library/sections? ")
        {
            return response(sections());
        }
        if request.starts_with("GET /library/streams/7") {
            return response(
                json!({"MediaContainer":{"size":1,"Lyrics":[{"timed":false,"Line":[{"Span":[{"text":"First "},{"text":"line"}]},{"Span":[{"text":"Second line"}]}]}]}}),
            );
        }
        assert!(request.starts_with("GET /library/metadata/3?"));
        response(
            json!({"MediaContainer":{"Metadata":[{"ratingKey":"3","type":"track","Media":[{"Part":[{"Stream":[{"streamType":4,"key":"/library/streams/7"}]}]}]}]}}),
        )
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &fixture.url);
    pause_cache(&mut core);
    let lyrics = core.execute(Command::Lyrics { key: "3".into() }).unwrap();
    assert_eq!(lyrics["lyrics"][0]["text"], "First line");
    assert_eq!(lyrics["lyrics"][0]["time"], Value::Null);
    drop(core);
    drop(fixture);
    let mut core = Core::new(dir.path().into()).unwrap();
    core.execute(Command::OfflineMode { enabled: true })
        .unwrap();
    assert_eq!(
        core.execute(Command::Lyrics { key: "3".into() }).unwrap(),
        lyrics
    );
}

#[test]
fn playlist_totals_are_server_metadata_not_loaded_page_counts() {
    let fixture = Fixture::new(|request| {
        if request.starts_with("GET /library/sections ")
            || request.starts_with("GET /library/sections? ")
        {
            return response(sections());
        }
        assert!(request.starts_with("GET /playlists?playlistType=audio"));
        response(json!({"MediaContainer":{"size":4,"totalSize":4,"Metadata":[
            {"ratingKey":"1","type":"playlist","title":"Long","leafCount":150,"duration":7265000},
            {"ratingKey":"2","type":"playlist","title":"Empty","leafCount":0},
            {"ratingKey":"3","type":"playlist","title":"Seconds","leafCount":1,"durationInSeconds":90},
            {"ratingKey":"4","type":"playlist","title":"Unknown","leafCount":null,"duration":null}
        ]}}))
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &fixture.url);
    let page = core.execute(Command::Playlists { start: 0 }).unwrap();
    let items = page["items"].as_array().unwrap();
    assert_eq!(items[0]["leafCount"], 150);
    assert_eq!(items[0]["totalDuration"], 7265000);
    assert_eq!(items[1]["totalDuration"], 0);
    assert_eq!(items[2]["totalDuration"], 90000);
    assert!(items[3]["totalDuration"].is_null());
}

#[test]
fn cover_album_artwork_prefers_album_thumb_and_confines_its_origin() {
    let fixture = Fixture::new(|_| response(sections()));
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &fixture.url);
    pause_cache(&mut core);
    let item = plexfreq_core::model::Item {
        rating_key: "3".into(),
        kind: "track".into(),
        thumb: "/track/thumb".into(),
        parent_thumb: "/album/thumb".into(),
        ..Default::default()
    };
    let queue = core
        .execute(Command::Enqueue {
            items: vec![item.clone()],
            next: false,
        })
        .unwrap();
    assert!(queue["queue"]["items"][0]["albumArtwork"]
        .as_str()
        .unwrap()
        .contains("/album/thumb?"));
    let unsafe_item = plexfreq_core::model::Item {
        rating_key: "4".into(),
        parent_thumb: "//outside.invalid/thumb".into(),
        ..item
    };
    let queue = core
        .execute(Command::Enqueue {
            items: vec![unsafe_item],
            next: false,
        })
        .unwrap();
    assert!(queue["queue"]["items"][1]["albumArtwork"]
        .as_str()
        .unwrap()
        .contains("/track/thumb?"));
}

fn song(key: &str) -> plexfreq_core::model::Item {
    serde_json::from_value(json!({"ratingKey":key,"type":"track","title":"Fixture song","duration":10000,"Media":[{"Part":[{"key":format!("/library/parts/{key}.wav")}]}]})).unwrap()
}
fn playlist_edit(
    action: &str,
    items: Vec<plexfreq_core::model::Item>,
    id: Option<u64>,
    after: Option<u64>,
) -> Command {
    Command::PlaylistEdit {
        action: action.into(),
        key: if action == "create" {
            "".into()
        } else {
            "9".into()
        },
        title: "Fixture & mix".into(),
        items,
        item_id: id,
        after,
    }
}

#[test]
fn playlist_edits_use_occurrence_ids_and_encoded_server_source() {
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = log.clone();
    let fixture = Fixture::new(move |request| {
        let mut parts = request.lines().next().unwrap().split_whitespace();
        let method = parts.next().unwrap();
        let url = url::Url::parse(&format!("http://fixture{}", parts.next().unwrap())).unwrap();
        seen.lock()
            .unwrap()
            .push((method.to_string(), url.path().to_string()));
        match (method, url.path()) {
            ("GET", "/library/sections") => response(sections()),
            ("GET", "/identity") => {
                response(json!({"MediaContainer":{"machineIdentifier":"fixture-server"}}))
            }
            ("POST", "/playlists") => {
                assert!(url
                    .query_pairs()
                    .any(|(k, v)| k == "title" && v == "Fixture & mix"));
                assert!(url.query_pairs().any(|(k,v)|k=="uri" && v=="server://fixture-server/com.plexapp.plugins.library/library/metadata/1,2"));
                response(
                    json!({"MediaContainer":{"Metadata":[{"ratingKey":"9","type":"playlist","title":"Fixture"}]}}),
                )
            }
            ("GET", "/playlists/9") => response(
                json!({"MediaContainer":{"Metadata":[{"ratingKey":"9","type":"playlist","playlistType":"audio","title":"Fixture","smart":false}]}}),
            ),
            ("GET", "/playlists/9/items") => response(
                json!({"MediaContainer":{"size":2,"totalSize":2,"Metadata":[{"ratingKey":"1","type":"track","playlistItemID":101},{"ratingKey":"1","type":"track","playlistItemID":102}]}}),
            ),
            ("PUT", "/playlists/9/items/102/move") => {
                assert!(url.query_pairs().any(|(k, v)| k == "after" && v == "101"));
                ("text/plain", vec![])
            }
            ("PUT", "/playlists/9/items")
            | ("DELETE", "/playlists/9/items/101")
            | ("PUT", "/playlists/9")
            | ("DELETE", "/playlists/9") => ("text/plain", vec![]),
            _ => panic!("Unexpected fixture request path"),
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &fixture.url);
    pause_cache(&mut core);
    core.execute(playlist_edit(
        "create",
        vec![song("1"), song("2")],
        None,
        None,
    ))
    .unwrap();
    let page = core
        .execute(Command::PlaylistItems {
            key: "9".into(),
            start: 0,
        })
        .unwrap();
    assert_eq!(page["items"][0]["playlistItemId"], 101);
    assert_eq!(page["items"][1]["playlistItemId"], 102);
    core.execute(playlist_edit("add", vec![song("1")], None, None))
        .unwrap();
    core.execute(playlist_edit("move", vec![], Some(102), Some(101)))
        .unwrap();
    core.execute(playlist_edit("remove", vec![], Some(101), None))
        .unwrap();
    core.execute(playlist_edit("rename", vec![], None, None))
        .unwrap();
    core.execute(playlist_edit("delete", vec![], None, None))
        .unwrap();
    assert!(log
        .lock()
        .unwrap()
        .iter()
        .any(|(m, p)| m == "DELETE" && p == "/playlists/9/items/101"));
}

#[test]
fn smart_playlist_entries_cannot_be_manually_changed() {
    let fixture = Fixture::new(|request| {
        if request.starts_with("GET /library/sections ")
            || request.starts_with("GET /library/sections? ")
        {
            response(sections())
        } else {
            assert!(request.starts_with("GET /playlists/9?"));
            response(
                json!({"MediaContainer":{"Metadata":[{"ratingKey":"9","type":"playlist","smart":true}]}}),
            )
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &fixture.url);
    assert!(core
        .execute(playlist_edit("add", vec![song("1")], None, None))
        .is_err());
}

#[test]
fn history_outbox_is_durable_deduplicated_and_does_not_count_seeks() {
    let scrobbles = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = scrobbles.clone();
    let fixture = Fixture::new(move |request| {
        if request.starts_with("GET /library/sections ")
            || request.starts_with("GET /library/sections? ")
        {
            response(sections())
        } else {
            assert!(request.starts_with("GET /:/scrobble?key=1&identifier="));
            count.fetch_add(1, Ordering::SeqCst);
            ("text/plain", vec![])
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &fixture.url);
    pause_cache(&mut core);
    let plan = core
        .execute(Command::Play {
            items: vec![song("1")],
            index: 0,
        })
        .unwrap();
    let occurrence = plan["playbackOccurrence"].as_str().unwrap().to_string();
    core.execute(Command::OfflineMode { enabled: true })
        .unwrap();
    core.execute(Command::PlaybackEvent {
        key: "1".into(),
        occurrence: occurrence.clone(),
        listened: 1000,
        duration: 10000,
    })
    .unwrap();
    assert_eq!(
        core.execute(Command::Status).unwrap()["history"]["pending"],
        0
    );
    for listened in [6000, 7000] {
        core.execute(Command::PlaybackEvent {
            key: "1".into(),
            occurrence: occurrence.clone(),
            listened,
            duration: 10000,
        })
        .unwrap();
    }
    assert_eq!(
        core.execute(Command::Status).unwrap()["history"]["pending"],
        1
    );
    drop(core);
    let mut core = Core::new(dir.path().into()).unwrap();
    assert_eq!(
        core.execute(Command::Status).unwrap()["history"]["pending"],
        1
    );
    core.execute(Command::SyncHistory).unwrap();
    core.execute(Command::SyncHistory).unwrap();
    assert_eq!(scrobbles.load(Ordering::SeqCst), 1);
    assert_eq!(
        core.execute(Command::Status).unwrap()["history"]["pending"],
        0
    );
}

#[test]
fn ordinary_timeline_acknowledges_history_without_second_scrobble() {
    let fixture = Fixture::new(|request| {
        if request.starts_with("GET /library/sections ")
            || request.starts_with("GET /library/sections? ")
        {
            response(sections())
        } else {
            assert!(request.starts_with("POST /:/timeline?"));
            assert!(request.contains("ratingKey=1"));
            ("text/plain", vec![])
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &fixture.url);
    pause_cache(&mut core);
    let plan = core
        .execute(Command::Play {
            items: vec![song("1")],
            index: 0,
        })
        .unwrap();
    core.execute(Command::PlaybackEvent {
        key: "1".into(),
        occurrence: plan["playbackOccurrence"].as_str().unwrap().into(),
        listened: 6000,
        duration: 10000,
    })
    .unwrap();
    core.execute(Command::Timeline {
        state: "playing".into(),
        position: 6000,
        duration: 10000,
        continuing: false,
    })
    .unwrap();
    assert_eq!(
        core.execute(Command::SyncHistory).unwrap()["history"]["pending"],
        0
    );
}

#[test]
fn wifi_policy_defers_jobs_until_confirmed_and_manager_removes_files() {
    let media = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = media.clone();
    let fixture = Fixture::new(move |request| {
        if request.starts_with("GET /library/sections ")
            || request.starts_with("GET /library/sections? ")
        {
            response(sections())
        } else {
            assert!(request.starts_with("GET /library/parts/1.wav"));
            count.fetch_add(1, Ordering::SeqCst);
            ("audio/wav", b"fixture-audio".to_vec())
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &fixture.url);
    core.execute(Command::DownloadPolicy {
        wifi_only: true,
        paused: false,
    })
    .unwrap();
    core.execute(Command::CacheTracks {
        items: vec![song("1")],
    })
    .unwrap();
    thread::sleep(Duration::from_millis(100));
    assert_eq!(media.load(Ordering::SeqCst), 0);
    assert_eq!(
        core.execute(Command::CacheStatus).unwrap()["cache"]["waitingForWifi"],
        true
    );
    core.execute(Command::NetworkState {
        wifi: true,
        online: true,
        live_hint: false,
    })
    .unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while core.execute(Command::Downloads).unwrap()["downloads"][0]["ready"] != 1 {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        core.execute(Command::Downloads).unwrap()["downloads"][0]["total"],
        1
    );
    core.execute(Command::DownloadAction {
        group: "track:1".into(),
        action: "remove".into(),
    })
    .unwrap();
    assert_eq!(
        core.execute(Command::CacheStatus).unwrap()["cache"]["tracks"],
        0
    );
}

#[test]
fn queued_old_network_observations_cannot_reopen_a_live_closed_gate() {
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = requests.clone();
    let fixture = Fixture::new(move |request| {
        if request.starts_with("GET /library/sections ")
            || request.starts_with("GET /library/sections? ")
        {
            response(sections())
        } else {
            seen.fetch_add(1, Ordering::SeqCst);
            ("audio/wav", b"fixture".to_vec())
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &fixture.url);
    core.execute(Command::DownloadPolicy {
        wifi_only: true,
        paused: false,
    })
    .unwrap();
    core.execute(Command::CacheTracks {
        items: vec![song("1")],
    })
    .unwrap();
    core.download_control().network(false);
    core.execute(Command::NetworkState {
        wifi: true,
        online: true,
        live_hint: true,
    })
    .unwrap();
    thread::sleep(Duration::from_millis(100));
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    assert_eq!(
        core.execute(Command::CacheStatus).unwrap()["cache"]["waitingForWifi"],
        true
    );
}

#[test]
fn multi_seed_mix_deduplicates_tracks_and_autoplay_uses_real_neighbors() {
    let fixture = Fixture::new(|request| {
        if request.starts_with("GET /library/sections ")
            || request.starts_with("GET /library/sections? ")
        {
            return response(sections());
        }
        let keys = if request.starts_with("GET /library/metadata/10/allLeaves?") {
            vec!["1", "2", "3"]
        } else if request.starts_with("GET /library/metadata/20/children?") {
            vec!["1", "4"]
        } else {
            assert!(request.contains("/nearest?limit=50"));
            vec!["5", "6"]
        };
        response(
            json!({"MediaContainer":{"size":keys.len(),"totalSize":keys.len(),"Metadata":keys.iter().map(|k|serde_json::to_value(song(k)).unwrap()).collect::<Vec<_>>()}}),
        )
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &fixture.url);
    pause_cache(&mut core);
    let seeds = vec![
        plexfreq_core::model::Item {
            rating_key: "10".into(),
            kind: "artist".into(),
            ..Default::default()
        },
        plexfreq_core::model::Item {
            rating_key: "20".into(),
            kind: "album".into(),
            ..Default::default()
        },
    ];
    let mix = core.execute(Command::Mix { seeds }).unwrap();
    assert_eq!(mix["queue"]["items"].as_array().unwrap().len(), 4);
    core.execute(Command::Play {
        items: vec![song("1")],
        index: 0,
    })
    .unwrap();
    core.execute(Command::Autoplay { enabled: true }).unwrap();
    let next = core.execute(Command::Next { automatic: true }).unwrap();
    assert_eq!(next["track"]["ratingKey"], "5");
    core.execute(Command::Next { automatic: false }).unwrap();
    let ended = core.execute(Command::Next { automatic: false }).unwrap();
    assert!(ended["track"].is_null());
}

#[test]
fn album_track_browsing_and_artist_grouping_use_metadata_types() {
    let fixture = Fixture::new(|request| {
        if request.starts_with("GET /library/sections ")
            || request.starts_with("GET /library/sections? ")
        {
            return response(sections());
        }
        if request.starts_with("GET /library/sections/1/all?") {
            assert!(request.contains("sort=year%3Adesc"));
            return response(json!({"MediaContainer":{"size":0,"totalSize":0}}));
        }
        if request.starts_with("GET /library/metadata/10?") {
            return response(
                json!({"MediaContainer":{"Metadata":[{"ratingKey":"10","type":"artist","title":"Fixture"}]}}),
            );
        }
        assert!(request.starts_with("GET /library/metadata/10/children?"));
        response(json!({"MediaContainer":{"size":4,"totalSize":4,"Metadata":[
            {"ratingKey":"1","type":"album","Format":[{"tag":"EP"}]},{"ratingKey":"2","type":"album","Format":[{"tag":"Single"}]},{"ratingKey":"3","type":"album","Subformat":[{"tag":"Live"}]},{"ratingKey":"4","type":"album"}
        ]}}))
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &fixture.url);
    for kind in ["album", "track"] {
        core.execute(Command::LibraryBrowse {
            section: "1".into(),
            kind: kind.into(),
            sort: "year".into(),
            start: 0,
        })
        .unwrap();
    }
    let grouped = core
        .execute(Command::ArtistAlbums { key: "10".into() })
        .unwrap();
    let kinds: Vec<_> = grouped["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["albumType"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["Albums", "EPs", "Live", "Singles"]);
}

#[test]
fn saved_artist_album_pages_and_private_artwork_are_available_without_server() {
    let fixture = Fixture::new(|request| {
        if request.starts_with("GET /library/sections ")
            || request.starts_with("GET /library/sections? ")
        {
            return response(sections());
        }
        if request.starts_with("GET /art/1") {
            return ("image/png", b"synthetic-image-fixture".to_vec());
        }
        if request.starts_with("GET /library/sections/1/all?") {
            return response(
                json!({"MediaContainer":{"size":1,"totalSize":1,"Metadata":[{"ratingKey":"1","type":"artist","title":"Fixture artist","thumb":"/art/1"}]}}),
            );
        }
        if request.starts_with("GET /library/metadata/1/children?") {
            return response(
                json!({"MediaContainer":{"size":1,"totalSize":1,"Metadata":[{"ratingKey":"2","type":"album","title":"Fixture album"}]}}),
            );
        }
        assert!(request.starts_with("GET /library/metadata/1?"));
        response(
            json!({"MediaContainer":{"Metadata":[{"ratingKey":"1","type":"artist","title":"Fixture artist","thumb":"/art/1","summary":"Biography"}]}}),
        )
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &fixture.url);
    pause_cache(&mut core);
    let page = core
        .execute(Command::Browse {
            section: "1".into(),
            kind: "artist".into(),
            query: String::new(),
            start: 0,
        })
        .unwrap();
    core.execute(Command::Detail {
        key: "1".into(),
        start: 0,
    })
    .unwrap();
    core.execute(Command::CacheArtwork {
        items: serde_json::from_value(page["items"].clone()).unwrap(),
    })
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = core
            .execute(Command::OfflineBrowse {
                kind: "artist".into(),
            })
            .unwrap();
        if status["items"][0]["artwork"]
            .as_str()
            .unwrap()
            .starts_with("file:")
        {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    drop(core);
    drop(fixture);
    let mut core = Core::new(dir.path().into()).unwrap();
    core.execute(Command::OfflineMode { enabled: true })
        .unwrap();
    assert_eq!(core.execute(Command::Libraries).unwrap()["offline"], true);
    let detail = core
        .execute(Command::Detail {
            key: "1".into(),
            start: 0,
        })
        .unwrap();
    assert_eq!(detail["detail"]["summary"], "Biography");
    assert_eq!(detail["items"][0]["ratingKey"], "2");
    let art = url::Url::parse(detail["detail"]["artwork"].as_str().unwrap())
        .unwrap()
        .to_file_path()
        .unwrap();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(art).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn pinned_album_plan_survives_restart_and_unpin_releases_it() {
    let fixture = Fixture::new(|request| {
        if request.starts_with("GET /library/sections ")
            || request.starts_with("GET /library/sections? ")
        {
            return response(sections());
        }
        if request.starts_with("GET /library/parts/3") {
            return ("audio/wav", b"fixture-audio".to_vec());
        }
        assert!(request.starts_with("GET /library/metadata/2/children?"));
        response(
            json!({"MediaContainer":{"size":1,"totalSize":1,"Metadata":[{"ratingKey":"3","type":"track","title":"Fixture","Media":[{"Part":[{"key":"/library/parts/3"}]}]}]}}),
        )
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &fixture.url);
    core.execute(Command::Pin {
        key: "2".into(),
        kind: "album".into(),
        enabled: true,
    })
    .unwrap();
    pause_cache(&mut core);
    drop(core);
    let mut core = Core::new(dir.path().into()).unwrap();
    let status = core.execute(Command::CacheStatus).unwrap();
    assert_eq!(status["cache"]["pinnedGroups"][0], "album:2");
    assert_eq!(status["cache"]["pinnedKeys"][0], "3");
    core.execute(Command::Pin {
        key: "2".into(),
        kind: "album".into(),
        enabled: false,
    })
    .unwrap();
    assert_eq!(
        core.execute(Command::CacheStatus).unwrap()["cache"]["pinnedKeys"],
        json!([])
    );
}
