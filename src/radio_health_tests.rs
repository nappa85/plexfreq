//! Station radio over items marked gone (stale-queue stage 1b): a station
//! that can no longer supply tracks continues like offline, cycling the fully
//! cached radio items; a healthy one keeps playing its successors.
use crate::{
    model::{Item, ItemKind, Media, Part},
    plex::Probe,
    radio::{Radio, RadioKind, Source},
    Command, Core, Error,
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    thread,
};

pub(crate) fn item(key: usize) -> Item {
    Item {
        rating_key: key.to_string(),
        kind: ItemKind::Track,
        title: format!("Track {key}"),
        duration: 1000,
        play_queue_item_id: Some(100 + key as u64),
        media: vec![Media {
            parts: vec![Part {
                key: format!("/library/parts/{key}/file.flac"),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    }
}

pub(crate) fn station_json(items: &[usize]) -> String {
    let metadata: Vec<_> = items
        .iter()
        .map(|key| {
            serde_json::json!({"ratingKey":key.to_string(),"type":"track","title":"Fixture",
                "duration":1000,"playQueueItemID":100 + key,
                "Media":[{"Part":[{"key":format!("/library/parts/{key}/file.flac")}]}]})
        })
        .collect();
    serde_json::json!({"MediaContainer":{"playQueueID":50,"Metadata":metadata}}).to_string()
}

/// A server answering by path; every request line is logged.
pub(crate) struct Server {
    pub(crate) origin: String,
    log: Arc<Mutex<Vec<String>>>,
}
impl Server {
    pub(crate) fn requests(&self, path: &str) -> Vec<String> {
        crate::mutex_lock(&self.log)
            .iter()
            .filter(|line| line.starts_with(path))
            .cloned()
            .collect()
    }
}

pub(crate) fn server(route: impl Fn(&str) -> (&'static str, String) + Send + 'static) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    thread::spawn(move || {
        for socket in listener.incoming() {
            let Ok(mut socket) = socket else { return };
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                match socket.read(&mut buffer) {
                    Ok(n) if n > 0 => bytes.extend_from_slice(&buffer[..n]),
                    _ => break,
                }
            }
            let request = String::from_utf8_lossy(&bytes).to_string();
            let target = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("")
                .to_owned();
            crate::mutex_lock(&seen).push(target.clone());
            let (status, body) = route(&target);
            let _ = write!(
                socket,
                "HTTP/1.1 {status}\r\nX-Plex-Protocol: 1.0\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    Server { origin, log }
}

/// A station Core over `count` items, current at `start`, with `cached`
/// items fully downloaded.
pub(crate) fn station(
    dir: &std::path::Path,
    server: &Server,
    count: usize,
    start: usize,
    cached: &[usize],
) -> Core {
    let items: Vec<_> = (1..=count).map(item).collect();
    let namespace = crate::cache::namespace(&server.origin, "fixture-token");
    for key in cached {
        crate::cache::seed_completed(
            &dir.join("audio-cache"),
            &namespace,
            &items[key - 1],
            "flac",
            b"complete",
        );
    }
    let mut settings = crate::store::Settings {
        client_identifier: "fixture-client".into(),
        server_url: server.origin.clone(),
        server_token: "fixture-token".into(),
        autoplay: false,
        cache: crate::cache::Config {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };
    settings.playback.queue.replace(items, start).unwrap();
    settings.playback.radio = Some(Radio::new(
        RadioKind::Artist,
        "Fixture".into(),
        Source::Station { queue_id: 50 },
    ));
    crate::store::save(dir, &settings).unwrap();
    Core::new(dir.into()).unwrap()
}

fn empty_station() -> Server {
    server(|target| {
        if target.starts_with("/:/timeline") {
            ("404 Not Found", "<html></html>".into())
        } else {
            ("200 OK", station_json(&[]))
        }
    })
}

fn current(core: &Core) -> Option<String> {
    core.current_key().map(str::to_owned)
}

#[test]
fn a_deleted_current_track_on_a_healthy_station_moves_to_the_next_one() {
    let server = server(|_| ("200 OK", station_json(&[4])));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[]);
    core.record_probe("1", Probe::Gone);
    let plan = core.execute(Command::Next { automatic: false }).unwrap();
    assert_eq!(plan["track"]["ratingKey"], "2", "{plan}");
    // The gone current item is no center: PMS answers nothing after an item
    // it no longer holds. The window's last held item is used instead.
    let refills = server.requests("/playQueues/50");
    assert_eq!(refills.len(), 1, "{refills:?}");
    assert!(refills[0].contains("center=103"), "{refills:?}");
    assert_eq!(plan["queue"]["items"].as_array().unwrap().len(), 4);
}

#[test]
fn a_healthy_current_track_stays_the_refill_center() {
    let server = server(|_| ("200 OK", station_json(&[4])));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[]);
    core.execute(Command::Next { automatic: false }).unwrap();
    assert!(server.requests("/playQueues/50")[0].contains("center=101"));
}

#[test]
fn an_empty_station_mid_window_still_tries_the_next_item() {
    let server = empty_station();
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[3]);
    core.record_probe("1", Probe::Gone);
    let plan = core.execute(Command::Next { automatic: false }).unwrap();
    // Item 2 may well be alive: no jump to the cached item 3.
    assert_eq!(plan["track"]["ratingKey"], "2", "{plan}");
}

#[test]
fn an_empty_station_at_its_end_cycles_cached_items_like_offline() {
    let server = empty_station();
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 2, &[1]);
    // An automatic end: the completion report gets a 404 and is tolerated.
    let plan = core.execute(Command::Next { automatic: true }).unwrap();
    assert_eq!(plan["track"]["ratingKey"], "1", "{plan}");
    assert_eq!(plan["playbackSource"], "cache", "{plan}");
    assert_eq!(server.requests("/:/timeline").len(), 1);
    assert_eq!(plan["queue"]["items"].as_array().unwrap().len(), 3);
    assert!(!core.take_gone_notice());
}

#[test]
fn an_empty_station_with_nothing_cached_ends_with_the_gone_notice_or_its_error() {
    let server = empty_station();
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 2, &[]);
    // A healthy current track: today's error stays.
    assert_eq!(
        core.execute(Command::Next { automatic: false })
            .unwrap_err()
            .to_string(),
        "Radio returned no playable tracks"
    );
    assert_eq!(current(&core).as_deref(), Some("3"));
    // The current track is gone: the gone end, persisted.
    core.record_probe("3", Probe::Gone);
    let plan = core.execute(Command::Next { automatic: false }).unwrap();
    assert_eq!(plan["stream"], "", "{plan}");
    assert!(core.take_gone_notice());
    assert_eq!(current(&core), None);
    let reloaded = Core::inspect(dir.path().into())
        .unwrap()
        .execute(Command::Status)
        .unwrap();
    assert!(reloaded["track"].is_null(), "{reloaded}");
}

#[test]
fn an_ended_station_resumes_when_it_refills_again() {
    let refilled = Arc::new(Mutex::new(false));
    let flag = refilled.clone();
    let server = server(move |_| {
        if *crate::mutex_lock(&flag) {
            ("200 OK", station_json(&[4]))
        } else {
            ("200 OK", station_json(&[]))
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 2, &[]);
    core.record_probe("3", Probe::Gone);
    core.execute(Command::Next { automatic: false }).unwrap();
    assert!(core.take_gone_notice());
    assert_eq!(current(&core), None);
    // Next from the ended station starts from its last audible item.
    *crate::mutex_lock(&refilled) = true;
    let plan = core.execute(Command::Next { automatic: false }).unwrap();
    assert_eq!(plan["track"]["ratingKey"], "4", "{plan}");
    assert!(server
        .requests("/playQueues/50")
        .last()
        .unwrap()
        .contains("center=102"));
}

#[test]
fn a_gone_tail_cycles_cached_history_and_keeps_the_window() {
    let server = empty_station();
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 4, 1, &[1]);
    core.record_probe("3", Probe::Gone);
    core.record_probe("4", Probe::Gone);
    // Speculation picks the same cached item and reports nothing.
    let prepared = core.prepare_next().unwrap().expect("a cached successor");
    assert_eq!(prepared.key(), Some("1"));
    assert!(!core.take_gone_notice());
    assert_eq!(current(&core).as_deref(), Some("2"));
    let plan = core.commit_prepared(prepared).unwrap();
    assert_eq!(plan["track"]["ratingKey"], "1", "{plan}");
    // The window is kept whole (no history trim), also after a reload.
    assert_eq!(plan["queue"]["items"].as_array().unwrap().len(), 4);
    drop(core);
    let mut core = Core::new(dir.path().into()).unwrap();
    // From the cached item every other one is gone: the sole cached item
    // repeats, as offline.
    for key in ["2", "3", "4"] {
        core.record_probe(key, Probe::Gone);
    }
    for expected in ["1", "1"] {
        let plan = core.execute(Command::Next { automatic: false }).unwrap();
        assert_eq!(plan["track"]["ratingKey"], expected, "{plan}");
        assert_eq!(plan["queue"]["items"].as_array().unwrap().len(), 4);
    }
}

#[test]
fn a_rejected_session_stays_visible() {
    let server = server(|_| ("401 Unauthorized", String::new()));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 2, &[1]);
    core.record_probe("3", Probe::Gone);
    // The refill is rejected at the end: no cycling, no gone end.
    let error = core
        .execute(Command::Next { automatic: false })
        .unwrap_err();
    assert!(matches!(error, Error::Unauthorized), "{error}");
    assert_eq!(current(&core).as_deref(), Some("3"));
    // Mid-window a rejected refill is not reported: the successor plays.
    drop(core);
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[]);
    let plan = core.execute(Command::Next { automatic: false }).unwrap();
    assert_eq!(plan["track"]["ratingKey"], "2", "{plan}");
}

#[test]
fn only_a_rejected_session_stops_at_the_completion_report() {
    // The station itself still answers: only the report decides.
    let report = Arc::new(Mutex::new("401 Unauthorized"));
    let status = report.clone();
    let server = server(move |target| {
        if target.starts_with("/:/timeline") {
            (*crate::mutex_lock(&status), String::new())
        } else {
            ("200 OK", station_json(&[4]))
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[]);
    let error = core.execute(Command::Next { automatic: true }).unwrap_err();
    assert!(matches!(error, Error::Unauthorized), "{error}");
    assert_eq!(current(&core).as_deref(), Some("1"));
    assert!(server.requests("/playQueues/50").is_empty());
    for failure in [
        "500 Internal Server Error",
        "404 Not Found",
        "400 Bad Request",
    ] {
        *crate::mutex_lock(&report) = failure;
        let before = current(&core);
        let plan = core.execute(Command::Next { automatic: true }).unwrap();
        assert_ne!(
            plan["track"]["ratingKey"].as_str(),
            before.as_deref(),
            "{failure}"
        );
    }
}

#[test]
fn the_refill_center_falls_back_to_held_items() {
    let server = server(|_| ("200 OK", station_json(&[])));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[]);
    // Current and tail gone: the last item that is not gone.
    core.record_probe("1", Probe::Gone);
    core.record_probe("3", Probe::Gone);
    core.execute(Command::Next { automatic: false }).unwrap();
    assert!(server.requests("/playQueues/50")[0].contains("center=102"));
    // Everything gone: the last held item, once.
    drop(core);
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[]);
    for key in ["1", "2", "3"] {
        core.record_probe(key, Probe::Gone);
    }
    let plan = core.execute(Command::Next { automatic: false }).unwrap();
    let refills = server.requests("/playQueues/50");
    assert_eq!(refills.len(), 2, "{refills:?}");
    assert!(refills[1].contains("center=103"), "{refills:?}");
    // Nothing cached and nothing ahead: the gone end.
    assert_eq!(plan["stream"], "", "{plan}");
    assert!(core.take_gone_notice());
}

#[test]
fn a_network_failure_keeps_the_offline_cache_first_rule() {
    // Unchanged behaviour: a 5xx refill cycles cached items even with
    // successors ahead; any other failure keeps the successor.
    let failure = Arc::new(Mutex::new("503 Service Unavailable"));
    let status = failure.clone();
    let server = server(move |_| (*crate::mutex_lock(&status), String::new()));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[3]);
    let plan = core.execute(Command::Next { automatic: false }).unwrap();
    assert_eq!(plan["track"]["ratingKey"], "3", "{plan}");
    drop(core);
    *crate::mutex_lock(&failure) = "404 Not Found";
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[3]);
    let plan = core.execute(Command::Next { automatic: false }).unwrap();
    assert_eq!(plan["track"]["ratingKey"], "2", "{plan}");
}

#[test]
fn a_cache_served_gone_radio_item_still_plays() {
    let server = server(|_| ("200 OK", station_json(&[4])));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[2]);
    core.record_probe("2", Probe::Gone);
    let plan = core.execute(Command::Next { automatic: false }).unwrap();
    assert_eq!(plan["track"]["ratingKey"], "2", "{plan}");
    assert_eq!(plan["playbackSource"], "cache", "{plan}");
}

#[test]
fn previous_on_radio_steps_back_over_gone_history() {
    let server = server(|_| ("200 OK", station_json(&[])));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 2, &[]);
    core.record_probe("2", Probe::Gone);
    let plan = core.execute(Command::Previous).unwrap();
    assert_eq!(plan["track"]["ratingKey"], "1", "{plan}");
}

#[test]
fn sonic_radio_whose_seed_is_gone_cycles_its_cached_items() {
    let server = server(|_| ("404 Not Found", "<html></html>".into()));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 1, 0, &[1]);
    let mut settings = crate::store::load(dir.path()).unwrap();
    settings.playback.radio = Some(Radio::new(
        RadioKind::Track,
        "Fixture".into(),
        Source::Sonic {
            seed: Box::new(item(1)),
            recent: vec!["1".into()],
        },
    ));
    drop(core);
    crate::store::save(dir.path(), &settings).unwrap();
    core = Core::new(dir.path().into()).unwrap();
    let plan = core.execute(Command::Next { automatic: true }).unwrap();
    assert_eq!(plan["track"]["ratingKey"], "1", "{plan}");
    assert_eq!(plan["playbackSource"], "cache", "{plan}");
    assert!(!server.requests("/library/metadata/1/nearest").is_empty());
}

#[test]
fn cycling_a_dead_station_keeps_every_cached_item_of_a_long_window() {
    let server = empty_station();
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 20, 18, &[1, 10]);
    for key in (2..=20).filter(|key| *key != 10) {
        core.record_probe(&key.to_string(), Probe::Gone);
    }
    // Wrap past the gone tail to 1, then forward over gone items to 10
    // (a plain forward move, not the exhausted tail), and around again.
    for expected in ["1", "10", "1", "10"] {
        let plan = core.execute(Command::Next { automatic: false }).unwrap();
        assert_eq!(plan["track"]["ratingKey"], expected, "{plan}");
        assert_eq!(plan["queue"]["items"].as_array().unwrap().len(), 20);
    }
}
