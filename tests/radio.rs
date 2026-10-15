use plexfreq_core::{model::Item, radio::RadioKind, Command, Core};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

fn track(key: &str, queue_id: Option<u64>) -> Value {
    json!({"ratingKey":key,"key":format!("/library/metadata/{key}"),"type":"track","title":"Fixture track","duration":10000,"musicAnalysisVersion":1,"playQueueItemID":queue_id,"Media":[{"Part":[{"key":format!("/library/parts/{key}/audio.flac")}]}]})
}

type Response = (
    &'static str,
    &'static str,
    Vec<(&'static str, &'static str)>,
    Value,
);

fn fixture(responses: Vec<Response>) -> (Core, tempfile::TempDir, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let task = thread::spawn(move || {
        for (method, path, params, body) in responses {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = socket.read(&mut buffer).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
            }
            let request = String::from_utf8(bytes).unwrap();
            let mut parts = request.lines().next().unwrap().split_whitespace();
            assert_eq!(parts.next().unwrap(), method);
            let url = url::Url::parse(&format!("http://fixture{}", parts.next().unwrap())).unwrap();
            assert_eq!(url.path(), path);
            for (k, v) in params {
                assert!(
                    url.query_pairs().any(|(key, value)| key == k && value == v),
                    "missing query parameter {k}"
                );
            }
            assert!(request
                .to_lowercase()
                .contains("x-plex-token: fixture-token"));
            let body = body.to_string();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    core.execute(Command::CacheConfig {
        enabled: false,
        limit_mb: 512,
        ahead: 5,
    })
    .unwrap();
    core.execute(Command::Connect {
        url: origin,
        token: "fixture-token".into(),
    })
    .unwrap();
    (core, dir, task)
}

fn connect() -> Response {
    (
        "GET",
        "/library/sections",
        vec![],
        json!({"MediaContainer":{"Directory":[]}}),
    )
}

#[test]
fn artist_station_uses_advertised_key_continues_by_queue_item_id_and_reports_timeline() {
    let (mut core,_dir,task) = fixture(vec![connect(),
        ("GET","/library/metadata/42",vec![("includeStations","1")],json!({"MediaContainer":{"Metadata":[{"ratingKey":"42","type":"artist","title":"Fixture artist","Stations":{"size":1,"Metadata":[{"key":"/library/metadata/42/station/example?type=10","radio":true,"playlistType":"audio"}]}}]}})),
        ("GET","/identity",vec![],json!({"MediaContainer":{"machineIdentifier":"test-server"}})),
        ("POST","/playQueues",vec![("type","audio"),("continuous","1"),("uri","server://test-server/com.plexapp.plugins.library/library/metadata/42/station/example?type=10")],json!({"MediaContainer":{"playQueueID":50,"playQueueSelectedItemID":101,"Metadata":[track("1",Some(101)),track("2",Some(102))]}})),
        ("POST","/:/timeline",vec![("playQueueID","50"),("playQueueItemID","101"),("state","stopped"),("time","10000"),("duration","10000"),("continuing","1")],json!({"MediaContainer":{"size":0}})),
        ("GET","/playQueues/50",vec![("center","101"),("includeBefore","0"),("includeAfter","1")],json!({"MediaContainer":{"playQueueID":50,"Metadata":[track("1",Some(101)),track("2",Some(102)),track("1",Some(103)),track("4",Some(104))]}})),
        ("POST","/:/timeline",vec![("playQueueID","50"),("playQueueItemID","102"),("state","stopped"),("time","10000"),("duration","10000"),("continuing","1")],json!({"MediaContainer":{"size":0}})),
        ("GET","/playQueues/50",vec![("center","102"),("includeBefore","0"),("includeAfter","1")],json!({"MediaContainer":{"playQueueID":50,"Metadata":[track("2",Some(102)),track("1",Some(103)),track("4",Some(104))]}})),
        ("POST","/:/timeline",vec![("playQueueID","50"),("playQueueItemID","103"),("state","playing"),("time","2500")],json!({"MediaContainer":{"size":0}})),
    ]);
    let started = core
        .execute(Command::Radio {
            key: "42".into(),
            kind: RadioKind::Artist,
        })
        .unwrap();
    assert_eq!(started["radio"]["source"], "station");
    assert_eq!(started["queue"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        core.execute(Command::Next { automatic: true }).unwrap()["track"]["ratingKey"],
        "2"
    );
    let continued = core.execute(Command::Next { automatic: true }).unwrap();
    assert_eq!(continued["queue"]["items"].as_array().unwrap().len(), 4);
    assert_eq!(continued["track"]["playQueueItemID"], 103);
    core.execute(Command::Timeline {
        state: "playing".into(),
        position: 2500,
        duration: 10000,
        continuing: false,
    })
    .unwrap();
    assert!(core.execute(Command::Shuffle { enabled: true }).is_err());
    assert!(core
        .execute(Command::Repeat {
            mode: plexfreq_core::queue::Repeat::All,
        })
        .is_err());
    let stopped = core.execute(Command::StopRadio).unwrap();
    assert_eq!(stopped["radio"], Value::Null);
    assert_eq!(stopped["stream"], "");
    task.join().unwrap();
}

#[test]
fn album_radio_plays_whole_albums_in_order_and_pages_tracks() {
    let (mut core, _dir, task) = fixture(vec![
        connect(),
        (
            "GET",
            "/library/metadata/10",
            vec![],
            json!({"MediaContainer":{"Metadata":[{"ratingKey":"10","type":"album","title":"Seed album","musicAnalysisVersion":1}]}}),
        ),
        (
            "GET",
            "/library/metadata/10/nearest",
            vec![("limit", "50")],
            json!({"MediaContainer":{"Metadata":[{"ratingKey":"20","type":"album","musicAnalysisVersion":1}]}}),
        ),
        (
            "GET",
            "/library/metadata/10/children",
            vec![("X-Plex-Container-Start", "0")],
            json!({"MediaContainer":{"totalSize":2,"Metadata":[track("1",None)]}}),
        ),
        (
            "GET",
            "/library/metadata/10/children",
            vec![("X-Plex-Container-Start", "1")],
            json!({"MediaContainer":{"totalSize":2,"Metadata":[track("2",None)]}}),
        ),
        (
            "GET",
            "/library/metadata/10/nearest",
            vec![],
            json!({"MediaContainer":{"Metadata":[{"ratingKey":"20","type":"album","musicAnalysisVersion":1}]}}),
        ),
        (
            "GET",
            "/library/metadata/20/children",
            vec![],
            json!({"MediaContainer":{"totalSize":2,"Metadata":[track("3",None),track("4",None)]}}),
        ),
    ]);
    core.execute(Command::Radio {
        key: "10".into(),
        kind: RadioKind::Album,
    })
    .unwrap();
    assert_eq!(
        core.execute(Command::Next { automatic: true }).unwrap()["track"]["ratingKey"],
        "2"
    );
    assert_eq!(
        core.execute(Command::Next { automatic: true }).unwrap()["track"]["ratingKey"],
        "3"
    );
    assert_eq!(
        core.execute(Command::Next { automatic: true }).unwrap()["track"]["ratingKey"],
        "4"
    );
    task.join().unwrap();
}

#[test]
fn track_radio_uses_sonic_neighbors_and_normal_play_exits_radio() {
    let (mut core, _dir, task) = fixture(vec![
        connect(),
        (
            "GET",
            "/library/metadata/1",
            vec![],
            json!({"MediaContainer":{"Metadata":[track("1",None)]}}),
        ),
        (
            "GET",
            "/library/metadata/1/nearest",
            vec![],
            json!({"MediaContainer":{"Metadata":[track("2",None)]}}),
        ),
        (
            "GET",
            "/library/metadata/1/nearest",
            vec![],
            json!({"MediaContainer":{"Metadata":[track("1",None),track("2",None)]}}),
        ),
    ]);
    core.execute(Command::Radio {
        key: "1".into(),
        kind: RadioKind::Track,
    })
    .unwrap();
    let next = core.execute(Command::Next { automatic: false }).unwrap();
    assert_eq!(next["track"]["ratingKey"], "2");
    assert_eq!(next["radio"]["kind"], "track");
    let item: Item = serde_json::from_value(track("8", None)).unwrap();
    assert_eq!(
        core.execute(Command::Play {
            items: vec![item],
            index: 0
        })
        .unwrap()["radio"],
        Value::Null
    );
    task.join().unwrap();
}

#[test]
fn unavailable_or_empty_radio_keeps_existing_playback_and_can_retry_continuation() {
    let (mut core, _dir, task) = fixture(vec![
        connect(),
        (
            "GET",
            "/library/metadata/10",
            vec![],
            json!({"MediaContainer":{"Metadata":[{"ratingKey":"10","type":"album"}]}}),
        ),
        (
            "GET",
            "/library/metadata/10/nearest",
            vec![],
            json!({"MediaContainer":{"Metadata":[]}}),
        ),
        (
            "GET",
            "/library/metadata/1",
            vec![],
            json!({"MediaContainer":{"Metadata":[track("1",None)]}}),
        ),
        (
            "GET",
            "/library/metadata/1/nearest",
            vec![],
            json!({"MediaContainer":{"Metadata":[track("2",None)]}}),
        ),
        (
            "GET",
            "/library/metadata/1/nearest",
            vec![],
            json!({"MediaContainer":{"Metadata":[]}}),
        ),
        (
            "GET",
            "/library/metadata/1/nearest",
            vec![],
            json!({"MediaContainer":{"Metadata":[track("2",None)]}}),
        ),
    ]);
    let item: Item = serde_json::from_value(track("8", None)).unwrap();
    core.execute(Command::Play {
        items: vec![item],
        index: 0,
    })
    .unwrap();
    let error = core
        .execute(Command::Radio {
            key: "10".into(),
            kind: RadioKind::Album,
        })
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("no sonic recommendations for this album"));
    assert_eq!(
        core.execute(Command::Status).unwrap()["queue"]["items"][0]["ratingKey"],
        "8"
    );
    core.execute(Command::Radio {
        key: "1".into(),
        kind: RadioKind::Track,
    })
    .unwrap();
    assert!(core.execute(Command::Next { automatic: true }).is_err());
    assert_eq!(
        core.execute(Command::Status).unwrap()["queue"]["current"],
        0
    );
    assert_eq!(
        core.execute(Command::Next { automatic: true }).unwrap()["track"]["ratingKey"],
        "2"
    );
    task.join().unwrap();
}

#[test]
fn radio_history_is_bounded_without_losing_current_track_or_order() {
    let mut queue = plexfreq_core::queue::Queue::default();
    let items = (0..200)
        .map(|i| serde_json::from_value(track(&i.to_string(), None)).unwrap())
        .collect();
    queue.replace(items, 199).unwrap();
    queue.trim_history(100);
    assert_eq!(queue.items().len(), 101);
    assert_eq!(queue.track().unwrap().rating_key, "199");
    queue
        .append(vec![serde_json::from_value(track("200", None)).unwrap()])
        .unwrap();
    assert_eq!(queue.advance(false).unwrap().rating_key, "200");
    assert_eq!(queue.previous().unwrap().rating_key, "199");
}

#[test]
fn station_replenishes_one_track_per_skip_and_keeps_a_bounded_window() {
    let mut responses = vec![
        connect(),
        (
            "GET",
            "/library/metadata/42",
            vec![],
            json!({"MediaContainer":{"Metadata":[{"ratingKey":"42","type":"artist","Stations":{"Metadata":[{"key":"/library/metadata/42/station/test","radio":true}]}}]}}),
        ),
        (
            "GET",
            "/identity",
            vec![],
            json!({"MediaContainer":{"machineIdentifier":"fixture"}}),
        ),
        (
            "POST",
            "/playQueues",
            vec![],
            json!({"MediaContainer":{"playQueueID":50,"Metadata":[track("1",Some(1)),track("2",Some(2)),track("3",Some(3))]}}),
        ),
    ];
    for id in 4..34 {
        responses.push(("GET", "/playQueues/50", vec![], json!({"MediaContainer":{"playQueueID":50,"Metadata":[track(&id.to_string(),Some(id)),track("999",Some(999))]}})));
    }
    let (mut core, _dir, task) = fixture(responses);
    core.execute(Command::Radio {
        key: "42".into(),
        kind: RadioKind::Artist,
    })
    .unwrap();
    for id in 2..32 {
        let result = core.execute(Command::Next { automatic: false }).unwrap();
        let items = result["queue"]["items"].as_array().unwrap();
        assert_eq!(result["track"]["ratingKey"], id.to_string());
        assert_eq!(items.len(), (id as usize + 2).min(11));
        assert_eq!(items.last().unwrap()["ratingKey"], (id + 2).to_string());
        assert!(!items.iter().any(|item| item["ratingKey"] == "999"));
    }
    task.join().unwrap();
}

#[test]
fn disconnected_radio_skips_missing_audio_and_cycles_completed_cache_after_restart() {
    use plexfreq_core::{
        cache::{namespace, Cache, Config},
        queue::Queue,
        radio::{Radio, Source},
        store,
    };
    use std::time::{Duration, Instant};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                let mut buffer = [0; 4096];
                let n = socket.read(&mut buffer).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/flac\r\nContent-Length: 10\r\nConnection: close\r\n\r\nabcdefghij").unwrap();
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let items: Vec<Item> = (1..=3)
        .map(|id| serde_json::from_value(track(&id.to_string(), Some(id))).unwrap())
        .collect();
    let cache = Cache::new(dir.path().join("audio-cache"), Config::default(), "fixture").unwrap();
    cache
        .schedule(
            &origin,
            "fixture-token",
            vec![items[0].clone(), items[2].clone()],
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while cache.items(&namespace(&origin, "fixture-token")).len() != 2 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    }
    drop(cache);
    server.join().unwrap(); // The endpoint is now unreachable, including without network hints.
    let mut queue = Queue::default();
    queue.replace(items, 0).unwrap();
    let mut settings = store::Settings {
        server_url: origin,
        server_token: "fixture-token".into(),
        client_identifier: "fixture".into(),
        ..Default::default()
    };
    settings.cache.enabled = false;
    settings.playback.queue = queue;
    settings.playback.radio = Some(Radio {
        kind: RadioKind::Artist,
        title: "Fixture".into(),
        source: Source::Station { queue_id: 50 },
    });
    store::save(dir.path(), &settings).unwrap();

    for mode in 0..3 {
        let mut core = Core::new(dir.path().into()).unwrap();
        if mode == 1 {
            core.execute(Command::OfflineMode { enabled: true })
                .unwrap();
        } else if mode == 2 {
            core.execute(Command::NetworkState {
                wifi: false,
                online: false,
                live_hint: false,
            })
            .unwrap();
        }
        for id in ["3", "1", "3", "1"] {
            let result = core.execute(Command::Next { automatic: true }).unwrap();
            assert_eq!(result["track"]["ratingKey"], id);
            assert_eq!(result["playbackSource"], "cache");
            assert_eq!(result["radio"]["source"], "station");
            assert_eq!(result["queue"]["items"].as_array().unwrap().len(), 3);
        }
    }
}

#[test]
fn sonic_neighbors_are_authoritative_when_analysis_version_is_not_returned() {
    let mut seed = track("1", None);
    seed.as_object_mut().unwrap().remove("musicAnalysisVersion");
    let (mut core, _dir, task) = fixture(vec![
        connect(),
        (
            "GET",
            "/library/metadata/1",
            vec![],
            json!({"MediaContainer":{"Metadata":[seed]}}),
        ),
        (
            "GET",
            "/library/metadata/1/nearest",
            vec![],
            json!({"MediaContainer":{"Metadata":[track("2",None)]}}),
        ),
    ]);
    let result = core
        .execute(Command::Radio {
            key: "1".into(),
            kind: RadioKind::Track,
        })
        .unwrap();
    assert_eq!(result["radio"]["kind"], "track");
    task.join().unwrap();
}
