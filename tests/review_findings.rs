//! Review findings: failing regressions for production issues.
//! Each test FAILS on the current implementation and must keep failing
//! until the corresponding issue is fixed.

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
    json!({"ratingKey":id,"key":format!("/library/metadata/{id}"),"type":"track","title":format!("Track {id}"),"duration":180000,"parentRatingKey":"20","parentTitle":"Offline album","grandparentRatingKey":"10","grandparentTitle":"Offline artist","Media":[{"audioCodec":"flac","container":"flac","Part":[{"key":format!("/library/parts/{id}/audio.flac")}]}]})
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

#[test]
fn lyrics_timed_text_has_no_leading_space() {
    let lines = plexfreq_core::lyrics::parse("[00:01.00] Hello");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].text, "Hello");
    assert_eq!(lines[0].time, Some(1000));
}

#[test]
fn quality_validation_and_transcode_url_agree_on_bitrates() {
    let base = url::Url::parse("http://fixture.invalid/").unwrap();
    let session = uuid::Uuid::new_v4().to_string();
    let bad = plexfreq_core::quality::Config {
        wifi_kbps: 100,
        mobile_kbps: 0,
        download_kbps: 0,
        codec_fallback: true,
    };
    assert_eq!(
        bad.validate().is_ok(),
        plexfreq_core::quality::transcode_url(&base, "1", 100, &session).is_ok(),
        "100 kbps must be accepted or rejected consistently"
    );
}

#[test]
fn server_path_rejects_query_and_encoded_traversal() {
    let base = url::Url::parse("http://fixture.invalid/").unwrap();
    assert!(
        plexfreq_core::plex::server_path(&base, "/library/metadata/1?X-Plex-Token=evil").is_err(),
        "query string in server-relative path must be rejected"
    );
    assert!(
        plexfreq_core::plex::server_path(&base, "/library/%2e%2e/secret").is_err(),
        "encoded dot segments must be rejected"
    );
    assert!(
        plexfreq_core::plex::server_path(&base, "/library/%2fetc%2fpasswd").is_err(),
        "encoded slashes must be rejected"
    );
}

#[test]
fn offline_empty_query_matches_online_empty_result() {
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

    let catalogue = call(
        &mut core,
        json!({"op":"offline_search","query":"Track","kind":"","start":0}),
    )
    .unwrap();
    assert!(
        !catalogue["items"].as_array().unwrap().is_empty(),
        "fixture must populate offline catalogue"
    );
    let offline_empty = call(
        &mut core,
        json!({"op":"offline_search","query":"","kind":"","start":0}),
    )
    .unwrap();
    assert_eq!(
        offline_empty["items"],
        json!([]),
        "empty offline query must return [] like online search, got {} items",
        offline_empty["items"].as_array().unwrap().len()
    );
}

#[test]
fn sonic_journey_has_no_global_duplicates() {
    let server = Server::new(|r| {
        if r.starts_with("GET /library/sections/1/computePath?") {
            if r.contains("startID=1") {
                return json!({"MediaContainer":{"size":3,"Metadata":[track("1"),track("2"),track("3")]}});
            } else {
                return json!({"MediaContainer":{"size":3,"Metadata":[track("2"),track("4"),track("5")]}});
            }
        }
        empty()
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    let journey = call(
        &mut core,
        json!({"op":"sonic_journey","section":"1","keys":["1","2","3"]}),
    )
    .unwrap();
    let ids: Vec<String> = journey["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["ratingKey"].as_str().unwrap().to_owned())
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    for id in &ids {
        assert!(
            seen.insert(id.clone()),
            "sonic journey contains global duplicate ratingKey {id} in {ids:?}"
        );
    }
}
