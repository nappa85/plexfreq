//! Local protocol/state regressions for the daily-use parity expansion.
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
    time::Duration,
};

struct Server {
    url: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(handler: impl Fn(&str) -> Value + Send + 'static) -> Self {
        Self::bytes(move |r| ("application/json", handler(r).to_string().into_bytes()))
    }
    fn bytes(handler: impl Fn(&str) -> (&'static str, Vec<u8>) + Send + 'static) -> Self {
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
                        assert!(request.to_lowercase().contains("x-plex-token: fixture"));
                        let (mime, body) = handler(&request);
                        let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                        let _ = stream.write_all(&body);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
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
    core.execute(serde_json::from_value::<Command>(input)?)
}
fn track(id: &str) -> Value {
    json!({"ratingKey":id,"key":format!("/library/metadata/{id}"),"type":"track","title":format!("Track {id}"),"duration":180000,"parentRatingKey":"20","parentTitle":"Offline album","grandparentRatingKey":"10","grandparentTitle":"Offline artist","Media":[{"audioCodec":"flac","Part":[{"key":format!("/library/parts/{id}/audio.flac")}]}]})
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
fn empty() -> Value {
    json!({"MediaContainer":{"Directory":[],"Metadata":[]}})
}

#[test]
fn streaming_quality_is_network_aware_persistent_and_local_first() {
    let server = Server::new(|_| empty());
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    let quality = json!({"wifiKbps":0,"mobileKbps":160,"downloadKbps":128,"codecFallback":true});
    call(&mut core, json!({"op":"quality_config","config":quality})).unwrap();
    let plan = call(
        &mut core,
        json!({"op":"play","items":[track("1")],"index":0}),
    )
    .unwrap();
    let uri = url::Url::parse(plan["stream"].as_str().unwrap()).unwrap();
    assert_eq!(uri.path(), "/music/:/transcode/universal/start.m3u8");
    let params: std::collections::BTreeMap<_, _> = uri.query_pairs().into_owned().collect();
    assert_eq!(params["protocol"], "http");
    assert_eq!(params["musicBitrate"], "160");
    assert_eq!(params["path"], "/library/metadata/1");
    assert_eq!(plan["playbackSource"], "transcode");
    call(
        &mut core,
        json!({"op":"network_state","wifi":true,"online":true}),
    )
    .unwrap();
    let plan = call(&mut core, json!({"op":"resume"})).unwrap();
    assert!(plan["stream"]
        .as_str()
        .unwrap()
        .contains("/library/parts/1/"));
    drop(core);
    let mut core = Core::new(dir.path().into()).unwrap();
    assert_eq!(
        call(&mut core, json!({"op":"status"})).unwrap()["qualityConfig"],
        quality
    );
}

#[test]
fn invalid_quality_changes_do_not_replace_saved_policy() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    let old = call(&mut core, json!({"op":"status"})).unwrap()["qualityConfig"].clone();
    assert!(old.is_object());
    assert!(call(
        &mut core,
        json!({"op":"quality_config","config":{"mobileKbps":999999}})
    )
    .is_err());
    assert_eq!(
        call(&mut core, json!({"op":"status"})).unwrap()["qualityConfig"],
        old
    );
}

#[test]
fn playlist_download_refresh_is_ordered_persistent_and_transactional() {
    let changed = Arc::new(AtomicBool::new(false));
    let flag = changed.clone();
    let server = Server::new(move |r| {
        if r.starts_with("GET /playlists/5/items") {
            return json!({"MediaContainer":{"size":2,"totalSize":2,"Metadata":if flag.load(Ordering::SeqCst){vec![track("3"),track("1")]}else{vec![track("2"),track("1")]}}});
        }
        if r.starts_with("GET /playlists/5") {
            return json!({"MediaContainer":{"Metadata":[{"type":"playlist","ratingKey":"5","title":"Fixture mix","playlistType":"audio","smart":true}]}});
        }
        empty()
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    call(
        &mut core,
        json!({"op":"download_plan","kind":"playlist","key":"5","minutes":30}),
    )
    .unwrap();
    changed.store(true, Ordering::SeqCst);
    call(
        &mut core,
        json!({"op":"download_action","group":"playlist:5","action":"refresh"}),
    )
    .unwrap();
    let saved: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("session.json")).unwrap()).unwrap();
    assert_eq!(saved["downloadGroups"]["playlist:5"], json!(["3", "1"]));
    drop(core);
    let mut core = Core::new(dir.path().into()).unwrap();
    call(&mut core, json!({"op":"offline_mode","enabled":true})).unwrap();
    assert!(call(
        &mut core,
        json!({"op":"download_action","group":"playlist:5","action":"refresh"})
    )
    .is_err());
    assert_eq!(
        call(&mut core, json!({"op":"downloads"})).unwrap()["downloads"][0]["total"],
        2
    );
}

#[test]
fn offline_catalogue_search_includes_unvisited_download_parents() {
    let server = Server::new(|r| {
        if r.starts_with("GET /playlists/5/items") {
            json!({"MediaContainer":{"size":1,"Metadata":[track("1")]}})
        } else if r.starts_with("GET /playlists/5") {
            json!({"MediaContainer":{"Metadata":[{"type":"playlist","ratingKey":"5","title":"Fixture mix"}]}})
        } else {
            empty()
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    call(
        &mut core,
        json!({"op":"download_plan","kind":"playlist","key":"5","minutes":30}),
    )
    .unwrap();
    call(&mut core, json!({"op":"offline_mode","enabled":true})).unwrap();
    drop(server);
    let albums = call(
        &mut core,
        json!({"op":"offline_search","query":"OFFLINE","kind":"album","start":0}),
    )
    .unwrap();
    assert_eq!(albums["items"][0]["ratingKey"], "20");
    assert_eq!(albums["items"][0]["title"], "Offline album");
    let artists = call(
        &mut core,
        json!({"op":"offline_search","query":"artist","kind":"artist","start":0}),
    )
    .unwrap();
    assert_eq!(artists["items"][0]["ratingKey"], "10");
}

#[test]
fn discovery_home_retains_server_groups_and_filters_non_music() {
    let server = Server::new(|r| {
        if r.starts_with("GET /hubs/sections/1?") {
            assert!(r.contains("includeMyMixes=1"));
            assert!(r.contains("includeStations=1"));
            return json!({"MediaContainer":{"Hub":[{"title":"Heavy rotation","hubIdentifier":"music.rotation","Metadata":[track("1"),{"ratingKey":"9","type":"movie","title":"Wrong"}]},{"title":"On this day","Metadata":[{"ratingKey":"20","type":"album","title":"Anniversary"}]}]}});
        }
        empty()
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    let home = call(&mut core, json!({"op":"discovery_home","section":"1"})).unwrap();
    assert_eq!(home["items"].as_array().unwrap().len(), 2);
    assert_eq!(home["items"][0]["discoveryGroup"], "Heavy rotation");
    call(&mut core, json!({"op":"offline_mode","enabled":true})).unwrap();
    drop(server);
    assert_eq!(
        call(&mut core, json!({"op":"discovery_home","section":"1"})).unwrap()["items"],
        home["items"]
    );
}

#[test]
fn sonic_adventure_uses_server_path_and_keeps_queue_unchanged_until_play() {
    let server = Server::new(|r| {
        if r.starts_with("GET /library/sections/1/computePath?") {
            assert!(r.contains("startID=1"));
            assert!(r.contains("endID=3"));
            return json!({"MediaContainer":{"size":3,"Metadata":[track("1"),track("2"),track("3")]}});
        }
        empty()
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    let route = call(
        &mut core,
        json!({"op":"sonic_adventure","section":"1","start_key":"1","end_key":"3"}),
    )
    .unwrap();
    assert_eq!(route["items"].as_array().unwrap().len(), 3);
    assert!(
        call(&mut core, json!({"op":"status"})).unwrap()["queue"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn duration_bounded_radio_plan_survives_restart_without_changing_playback() {
    let server = Server::new(|r| {
        if r.starts_with("GET /library/metadata/10?") {
            return json!({"MediaContainer":{"Metadata":[{"ratingKey":"10","type":"artist","title":"Station seed","Stations":[{"key":"/library/metadata/10/station/1"}]}]}});
        }
        if r.starts_with("GET /identity") {
            return json!({"MediaContainer":{"machineIdentifier":"fixture-server"}});
        }
        if r.starts_with("POST /playQueues?") || r.starts_with("GET /playQueues/7?") {
            let range = if r.starts_with("POST") {
                1..6
            } else {
                assert!(r.contains("center=5"));
                6..11
            };
            let items: Vec<_> = range
                .map(|id| {
                    let mut item = track(&id.to_string());
                    item["playQueueItemID"] = json!(id);
                    item
                })
                .collect();
            return json!({"MediaContainer":{"playQueueID":7,"playQueueSelectedItemID":1,"Metadata":items}});
        }
        assert!(!r.contains("/:/timeline"));
        empty()
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    let result = call(
        &mut core,
        json!({"op":"download_plan","kind":"artist_radio","key":"10","minutes":30}),
    )
    .unwrap();
    assert_eq!(result["downloads"][0]["total"], 10);
    assert_eq!(result["downloads"][0]["duration"], 1800000);
    assert!(
        call(&mut core, json!({"op":"status"})).unwrap()["queue"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    drop(core);
    drop(server);
    let mut core = Core::new(dir.path().into()).unwrap();
    assert_eq!(
        call(&mut core, json!({"op":"downloads"})).unwrap()["downloads"][0]["group"],
        "artist_radio:10"
    );
}

#[test]
fn transcoded_downloads_are_quality_scoped_cleanup_sessions_and_play_locally() {
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = stopped.clone();
    let server = Server::bytes(move |r| {
        if r.starts_with("GET /music/:/transcode/universal/start.m3u8?") {
            assert!(r.contains("musicBitrate=128"));
            assert!(!r.split("\r\n").next().unwrap().contains("X-Plex-Token"));
            return ("audio/mpeg", b"encoded-fixture".to_vec());
        }
        if r.starts_with("GET /video/:/transcode/universal/stop?") {
            flag.store(true, Ordering::SeqCst);
            return ("text/plain", Vec::new());
        }
        if r.contains("/library/parts/") {
            return ("audio/flac", b"original-fixture".to_vec());
        }
        ("application/json", empty().to_string().into_bytes())
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    call(
        &mut core,
        json!({"op":"quality_config","config":{"downloadKbps":128}}),
    )
    .unwrap();
    call(
        &mut core,
        json!({"op":"download_policy","wifi_only":false,"paused":false}),
    )
    .unwrap();
    call(&mut core, json!({"op":"cache_tracks","items":[track("1")]})).unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(5);
    while call(&mut core, json!({"op":"cache_status"})).unwrap()["cache"]["tracks"] != 1
        || !stopped.load(Ordering::SeqCst)
    {
        assert!(std::time::Instant::now() < until);
        thread::sleep(Duration::from_millis(5));
    }
    call(&mut core, json!({"op":"offline_mode","enabled":true})).unwrap();
    drop(server);
    let play = call(
        &mut core,
        json!({"op":"play","items":[track("1")],"index":0}),
    )
    .unwrap();
    assert_eq!(play["playbackSource"], "cache");
    let file = url::Url::parse(play["stream"].as_str().unwrap())
        .unwrap()
        .to_file_path()
        .unwrap();
    assert_eq!(std::fs::read(file).unwrap(), b"encoded-fixture");
}

#[test]
fn progressive_transcode_resume_decodes_and_closes_its_server_session() {
    use plexfreq_core::audio::{Engine, Event, Sink, Source};
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = stopped.clone();
    let server = Server::bytes(move |r| {
        if r.starts_with("GET /video/:/transcode/universal/stop?") {
            flag.store(true, Ordering::SeqCst);
            return ("text/plain", Vec::new());
        }
        assert!(r.starts_with("GET /music/:/transcode/universal/start.m3u8?"));
        assert!(r.contains("offset=0.050"));
        assert!(!r.to_lowercase().contains("\r\nrange:"));
        let frames = 2400u32;
        let mut wave = Vec::new();
        wave.extend(b"RIFF");
        wave.extend((36 + frames * 2).to_le_bytes());
        wave.extend(b"WAVEfmt ");
        wave.extend(16u32.to_le_bytes());
        wave.extend(1u16.to_le_bytes());
        wave.extend(1u16.to_le_bytes());
        wave.extend(48000u32.to_le_bytes());
        wave.extend(96000u32.to_le_bytes());
        wave.extend(2u16.to_le_bytes());
        wave.extend(16u16.to_le_bytes());
        wave.extend(b"data");
        wave.extend((frames * 2).to_le_bytes());
        for _ in 0..frames {
            wave.extend(4096i16.to_le_bytes());
        }
        ("audio/wav", wave)
    });
    let mut uri = plexfreq_core::quality::transcode_url(
        &url::Url::parse(&server.url).unwrap(),
        "1",
        192,
        &uuid::Uuid::new_v4().to_string(),
    )
    .unwrap();
    uri.query_pairs_mut().append_pair("X-Plex-Token", "fixture");
    let engine = Engine::new(Sink::Capture).unwrap();
    engine.handle.volume(1.).unwrap();
    engine
        .handle
        .load(
            Source {
                id: 1,
                url: uri.into(),
                duration: 100,
                resume: 50,
                listened: 0,
                gain: 0.,
                album: String::new(),
                ..Default::default()
            },
            false,
        )
        .unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let events = engine.events();
        assert!(
            !events.iter().any(|e| matches!(e, Event::Error { .. })),
            "{events:?}"
        );
        if events.iter().any(|e| {
            matches!(
                e,
                Event::End {
                    id: 1,
                    position: 100,
                    ..
                }
            )
        }) {
            break;
        }
        assert!(std::time::Instant::now() < until);
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(engine.handle.captured().len(), 4800);
    drop(engine);
    assert!(stopped.load(Ordering::SeqCst));
}

#[test]
fn failed_refresh_keeps_previous_membership_and_saved_plan() {
    let malformed = Arc::new(AtomicBool::new(false));
    let flag = malformed.clone();
    let server = Server::new(move |r| {
        if r.starts_with("GET /playlists/5/items") {
            return json!({"MediaContainer":{"size":1,"Metadata":[if flag.load(Ordering::SeqCst){json!({"type":"movie","ratingKey":"9"})}else{track("1")}]}});
        }
        if r.starts_with("GET /playlists/5") {
            return json!({"MediaContainer":{"Metadata":[{"type":"playlist","ratingKey":"5","title":"Fixture"}]}});
        }
        empty()
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    call(
        &mut core,
        json!({"op":"download_plan","kind":"playlist","key":"5","minutes":30}),
    )
    .unwrap();
    let before = std::fs::read(dir.path().join("session.json")).unwrap();
    malformed.store(true, Ordering::SeqCst);
    assert!(call(
        &mut core,
        json!({"op":"download_action","group":"playlist:5","action":"refresh"})
    )
    .is_err());
    assert_eq!(
        std::fs::read(dir.path().join("session.json")).unwrap(),
        before
    );
}

#[test]
fn advertised_home_station_is_playable_and_downloadable_without_numeric_key() {
    let server = Server::new(|r| {
        if r.starts_with("GET /hubs/sections/1?") {
            return json!({"MediaContainer":{"Hub":[{"title":"Stations","Metadata":[{"type":"playlist","radio":"1","key":"/library/sections/1/station/1","title":"Library radio"},{"type":"playlist","radio":"1","key":"//other.invalid/library/station/1","title":"Invalid"}]}]}});
        }
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
    let home = call(&mut core, json!({"op":"discovery_home","section":"1"})).unwrap();
    assert_eq!(home["items"].as_array().unwrap().len(), 1);
    assert_eq!(home["items"][0]["station"], true);
    let plan=call(&mut core,json!({"op":"download_station","key":"/library/sections/1/station/1","title":"Library radio","minutes":30})).unwrap();
    assert_eq!(plan["downloads"][0]["title"], "Library radio");
    assert_eq!(plan["downloads"][0]["total"], 1);
    let play = call(
        &mut core,
        json!({"op":"station","key":"/library/sections/1/station/1","title":"Library radio"}),
    )
    .unwrap();
    assert_eq!(play["queue"]["items"].as_array().unwrap().len(), 1);
}

#[test]
fn bounded_transcode_probe_reads_at_most_256_kib_and_stops_the_session() {
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = stopped.clone();
    let server = Server::bytes(move |r| {
        if r.starts_with("GET /music/:/transcode/universal/start.m3u8?") {
            return ("audio/mpeg", vec![1; 1024 * 1024]);
        }
        if r.starts_with("GET /video/:/transcode/universal/stop?") {
            flag.store(true, Ordering::SeqCst);
            return ("text/plain", Vec::new());
        }
        if r.starts_with("GET /library/metadata/1?") {
            return (
                "application/json",
                json!({"MediaContainer":{"Metadata":[track("1")]}})
                    .to_string()
                    .into_bytes(),
            );
        }
        ("application/json", empty().to_string().into_bytes())
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    let probe = call(
        &mut core,
        json!({"op":"probe_quality","key":"1","kbps":160}),
    )
    .unwrap();
    assert_eq!(probe["transcodeProbe"]["bytes"], 256 * 1024);
    assert_eq!(probe["transcodeProbe"]["stopped"], true);
    assert!(stopped.load(Ordering::SeqCst));
}

#[test]
fn music_filters_and_smart_playlists_use_scoped_server_source_uris() {
    let server = Server::new(|r| {
        if r.starts_with("GET /identity") {
            return json!({"MediaContainer":{"machineIdentifier":"fixture-server"}});
        }
        if r.starts_with("GET /library/sections/1/all?") {
            if r.contains("includeMeta=1") {
                return json!({"MediaContainer":{"Meta":{"Type":[{"type":"track","Field":[{"key":"track.genre"},{"key":"track.year"},{"key":"track.viewCount"}]}]}}});
            }
            let path = r.split_whitespace().nth(1).unwrap();
            let uri = url::Url::parse(&format!("http://fixture{path}")).unwrap();
            let p: std::collections::BTreeMap<_, _> = uri.query_pairs().into_owned().collect();
            assert_eq!(p["track.genre"], "7");
            assert_eq!(p["track.year>="], "2000");
            assert_eq!(p["track.year<="], "2009");
            assert_eq!(p["track.viewCount"], "0");
            return json!({"MediaContainer":{"size":1,"Metadata":[track("1")]}});
        }
        if r.starts_with("POST /playlists?") {
            let path = r.split_whitespace().nth(1).unwrap();
            let uri = url::Url::parse(&format!("http://fixture{path}")).unwrap();
            let p: std::collections::BTreeMap<_, _> = uri.query_pairs().into_owned().collect();
            assert_eq!(p["smart"], "1");
            assert!(p["uri"].starts_with(
                "server://fixture-server/com.plexapp.plugins.library/library/sections/1/all?"
            ));
            assert!(p["uri"].contains("track.genre=7"));
            assert!(p["uri"].contains("track.year%3E%3D=2000"));
            return json!({"MediaContainer":{"Metadata":[{"type":"playlist","ratingKey":"50","title":"Filtered","smart":true,"playlistType":"audio"}]}});
        }
        empty()
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    let filters = json!({"genre":"7","yearFrom":2000,"yearTo":2009,"unplayed":true});
    let page=call(&mut core,json!({"op":"filtered_browse","section":"1","kind":"track","sort":"title","filters":filters,"start":0})).unwrap();
    assert_eq!(page["items"][0]["ratingKey"], "1");
    let created=call(&mut core,json!({"op":"smart_playlist","action":"create","section":"1","title":"Filtered","filters":filters,"sort":"title","limit":100})).unwrap();
    assert_eq!(created["createdPlaylist"]["smart"], true);
}

#[test]
fn discovery_layout_is_scoped_persistent_and_hub_drilldown_is_paged() {
    let server = Server::new(|r| {
        if r.starts_with("GET /hubs/sections/1/recent?") {
            return json!({"MediaContainer":{"size":1,"totalSize":101,"Metadata":[track("3")]}});
        }
        if r.starts_with("GET /hubs/sections/1?") {
            return json!({"MediaContainer":{"Hub":[{"title":"One","hubIdentifier":"one","key":"/hubs/sections/1/recent","more":true,"Metadata":[track("1")]},{"title":"Two","hubIdentifier":"two","Metadata":[track("2")]}]}});
        }
        empty()
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    call(&mut core, json!({"op":"discovery_home","section":"1"})).unwrap();
    call(
        &mut core,
        json!({"op":"discovery_config","section":"1","hidden":["two"],"order":["two","one"]}),
    )
    .unwrap();
    let home = call(&mut core, json!({"op":"discovery_home","section":"1"})).unwrap();
    assert_eq!(home["items"].as_array().unwrap().len(), 1);
    assert_eq!(home["items"][0]["hubKey"], "/hubs/sections/1/recent");
    let page = call(
        &mut core,
        json!({"op":"hub_items","key":"/hubs/sections/1/recent","start":100}),
    )
    .unwrap();
    assert_eq!(page["start"], 100);
    assert_eq!(page["items"][0]["ratingKey"], "3");
    drop(core);
    let mut core = Core::new(dir.path().into()).unwrap();
    call(&mut core, json!({"op":"offline_mode","enabled":true})).unwrap();
    drop(server);
    assert_eq!(
        call(&mut core, json!({"op":"discovery_home","section":"1"})).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(call(
        &mut core,
        json!({"op":"hub_items","key":"//other.invalid/hubs/sections/1","start":0})
    )
    .is_err());
}

#[test]
fn automatic_refresh_is_nonblocking_obeys_network_gate_and_replaces_membership() {
    let changed = Arc::new(AtomicBool::new(false));
    let flag = changed.clone();
    let server = Server::new(move |r| {
        if r.starts_with("GET /playlists/5/items") {
            if flag.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(200));
            }
            return json!({"MediaContainer":{"size":1,"Metadata":[track(if flag.load(Ordering::SeqCst){"2"}else{"1"})]}});
        }
        if r.starts_with("GET /playlists/5") {
            return json!({"MediaContainer":{"Metadata":[{"type":"playlist","ratingKey":"5","title":"Fixture"}]}});
        }
        empty()
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    call(
        &mut core,
        json!({"op":"download_plan","kind":"playlist","key":"5","minutes":30}),
    )
    .unwrap();
    changed.store(true, Ordering::SeqCst);
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
    // Still paused: maintenance must not fetch/commit the changed plan.
    call(&mut core, json!({"op":"refresh_downloads"})).unwrap();
    thread::sleep(Duration::from_millis(250));
    let saved: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("session.json")).unwrap()).unwrap();
    assert_eq!(saved["downloadGroups"]["playlist:5"], json!(["1"]));
    call(
        &mut core,
        json!({"op":"download_policy","wifi_only":true,"paused":false}),
    )
    .unwrap();
    let started = std::time::Instant::now();
    call(&mut core, json!({"op":"refresh_downloads"})).unwrap();
    assert!(started.elapsed() < Duration::from_millis(100));
    let until = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        call(&mut core, json!({"op":"refresh_downloads"})).unwrap();
        let saved: Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("session.json")).unwrap())
                .unwrap();
        if saved["downloadGroups"]["playlist:5"] == json!(["2"]) {
            break;
        }
        assert!(std::time::Instant::now() < until);
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn listening_insights_count_qualified_occurrences_not_seeks_or_checkpoints() {
    let server = Server::new(|_| empty());
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    let play = call(
        &mut core,
        json!({"op":"play","items":[track("1")],"index":0}),
    )
    .unwrap();
    let occurrence = play["playbackOccurrence"].clone();
    call(&mut core,json!({"op":"playback_event","key":"1","occurrence":occurrence,"listened":10000,"duration":180000})).unwrap();
    assert_eq!(
        call(&mut core, json!({"op":"listening_insights","days":30})).unwrap()["insights"]["plays"],
        0
    );
    for listened in [90000, 120000, 120000] {
        call(&mut core,json!({"op":"playback_event","key":"1","occurrence":occurrence,"listened":listened,"duration":180000})).unwrap();
    }
    let insights = call(&mut core, json!({"op":"listening_insights","days":30})).unwrap();
    assert_eq!(insights["insights"]["plays"], 1);
    assert_eq!(insights["insights"]["listenedMs"], 120000);
    assert_eq!(
        insights["insights"]["artists"][0]["title"],
        "Offline artist"
    );
    drop(core);
    let mut core = Core::new(dir.path().into()).unwrap();
    assert_eq!(
        call(&mut core, json!({"op":"listening_insights","days":30})).unwrap()["insights"]["plays"],
        1
    );
}

#[test]
fn normalized_offline_index_invalidates_after_external_snapshot_replacement() {
    use plexfreq_core::{
        cache::namespace,
        model::{Container, Item},
        offline::Library,
    };
    let server = Server::new(|_| empty());
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    let library = Library::new(dir.path().join("library-cache"), true).unwrap();
    let ns = namespace(&server.url, "fixture");
    let save = |title: &str| {
        library.save(
            &ns,
            "/library/metadata/10",
            &[],
            &Container {
                items: vec![Item {
                    rating_key: "10".into(),
                    kind: "artist".into(),
                    title: title.into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
    };
    save("Björk");
    call(&mut core, json!({"op":"offline_mode","enabled":true})).unwrap();
    assert_eq!(
        call(
            &mut core,
            json!({"op":"offline_search","query":"BJÖRK","kind":"artist","start":0})
        )
        .unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    save("Updated artist");
    assert!(call(
        &mut core,
        json!({"op":"offline_search","query":"BJÖRK","kind":"artist","start":0})
    )
    .unwrap()["items"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        call(
            &mut core,
            json!({"op":"offline_search","query":"updated artist","kind":"artist","start":0})
        )
        .unwrap()["items"][0]["ratingKey"],
        "10"
    );
}

#[test]
fn smart_rules_roundtrip_and_unsupported_boolean_rules_are_not_silently_flattened() {
    let advanced = Arc::new(AtomicBool::new(false));
    let flag = advanced.clone();
    let server = Server::new(move |r| {
        if r.starts_with("GET /playlists/5") {
            let content = if flag.load(Ordering::SeqCst) {
                "server://fixture-server/com.plexapp.plugins.library/library/sections/1/all?type=10&push=1&track.genre=7&or=1&track.genre=8&pop=1"
            } else {
                "server://fixture-server/com.plexapp.plugins.library/library/sections/1/all?type=10&track.genre=7&track.year%3E%3D=2000&sort=titleSort%3Aasc&limit=100"
            };
            return json!({"MediaContainer":{"Metadata":[{"type":"playlist","ratingKey":"5","smart":true,"content":content}]}});
        }
        empty()
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    let rules = call(&mut core, json!({"op":"smart_rules","key":"5"})).unwrap();
    assert_eq!(rules["smartEditable"], true);
    assert_eq!(rules["smartFilters"]["genre"], "7");
    assert_eq!(rules["smartFilters"]["yearFrom"], 2000);
    advanced.store(true, Ordering::SeqCst);
    assert_eq!(
        call(&mut core, json!({"op":"smart_rules","key":"5"})).unwrap()["smartEditable"],
        false
    );
    assert!(call(&mut core,json!({"op":"filtered_browse","section":"1","kind":"track","sort":"title","filters":{"yearFrom":2010,"yearTo":2000},"start":0})).is_err());
    assert!(call(&mut core,json!({"op":"filtered_browse","section":"1","kind":"track","sort":"title","filters":{"arbitrary":"bad"},"start":0})).is_err());
}

#[test]
fn multi_waypoint_sonic_journey_joins_paths_without_duplicate_junctions() {
    let server = Server::new(|r| {
        if r.starts_with("GET /library/sections/1/computePath?") {
            let ids = if r.contains("startID=1") {
                ["1", "2", "3"]
            } else {
                ["3", "4", "5"]
            };
            return json!({"MediaContainer":{"size":3,"Metadata":ids.map(track)}});
        }
        empty()
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    let journey = call(
        &mut core,
        json!({"op":"sonic_journey","section":"1","keys":["1","3","5"]}),
    )
    .unwrap();
    let ids: Vec<_> = journey["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["ratingKey"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["1", "2", "3", "4", "5"]);
    assert!(call(
        &mut core,
        json!({"op":"sonic_journey","section":"1","keys":["1"]})
    )
    .is_err());
}

#[test]
fn progressive_playback_probe_restarts_at_time_offset_without_recording_a_listen() {
    let server = Server::bytes(|r| {
        if r.starts_with("GET /library/metadata/1?") {
            let mut item = track("1");
            item["duration"] = json!(10000);
            return (
                "application/json",
                json!({"MediaContainer":{"Metadata":[item]}})
                    .to_string()
                    .into_bytes(),
            );
        }
        if r.starts_with("GET /music/:/transcode/universal/start.m3u8?") {
            let path = r.split_whitespace().nth(1).unwrap();
            let uri = url::Url::parse(&format!("http://fixture{path}")).unwrap();
            let offset = uri
                .query_pairs()
                .find(|(key, _)| key == "offset")
                .unwrap()
                .1
                .parse::<f64>()
                .unwrap();
            let frames = ((10. - offset) * 48000.) as u32;
            let mut wave = Vec::new();
            wave.extend(b"RIFF");
            wave.extend((36 + frames * 2).to_le_bytes());
            wave.extend(b"WAVEfmt ");
            wave.extend(16u32.to_le_bytes());
            wave.extend(1u16.to_le_bytes());
            wave.extend(1u16.to_le_bytes());
            wave.extend(48000u32.to_le_bytes());
            wave.extend(96000u32.to_le_bytes());
            wave.extend(2u16.to_le_bytes());
            wave.extend(16u16.to_le_bytes());
            wave.extend(b"data");
            wave.extend((frames * 2).to_le_bytes());
            wave.resize(44 + frames as usize * 2, 0);
            return ("audio/wav", wave);
        }
        ("application/json", empty().to_string().into_bytes())
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    let result = call(
        &mut core,
        json!({"op":"probe_playback","key":"1","kbps":160}),
    )
    .unwrap();
    assert_eq!(result["playbackProbe"]["seeked"], true);
    assert_eq!(
        call(&mut core, json!({"op":"listening_insights","days":0})).unwrap()["insights"]["plays"],
        0
    );
}

#[test]
fn filter_choices_use_advertised_same_section_endpoints_and_canonical_scopes() {
    let server = Server::new(|r| {
        if r.starts_with("GET /library/sections/1/genre?") {
            return json!({"MediaContainer":{"Directory":[{"key":"7","title":"Fixture genre"}]}});
        }
        if r.starts_with("GET /library/sections/1/all?") {
            if r.contains("includeMeta=1") {
                return json!({"MediaContainer":{"Meta":{"Type":[{"type":"track","Field":[{"key":"album.genre"}],"Filter":[{"filter":"genre","key":"/library/sections/1/genre?type=10"}]}]}}});
            }
            assert!(r.contains("album.genre=7"));
            return json!({"MediaContainer":{"size":1,"Metadata":[track("1")]}});
        }
        empty()
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    let choices = call(
        &mut core,
        json!({"op":"filter_options","section":"1","kind":"track","field":"genre"}),
    )
    .unwrap();
    assert_eq!(choices["filterChoices"][0]["key"], "7");
    assert_eq!(call(&mut core,json!({"op":"filtered_browse","section":"1","kind":"track","sort":"title","filters":{"genre":"7"},"start":0})).unwrap()["items"][0]["ratingKey"],"1");
}
