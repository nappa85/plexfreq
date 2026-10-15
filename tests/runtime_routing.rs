//! Typed admission and worker post-processing of the runtime's JSON API.
//! Fake audio sink and a local HTTP fixture; the session lives in a temporary
//! directory with a dummy token.
use plexfreq_core::{runtime::Runtime, store};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
};

fn serve(mut stream: TcpStream) {
    loop {
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            if !matches!(stream.read(&mut byte), Ok(1)) {
                return;
            }
            request.push(byte[0]);
        }
        let line = String::from_utf8_lossy(&request)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        let body = if line.starts_with("GET /playlists?") {
            json!({"MediaContainer":{"size":1,"totalSize":300,"offset":0,"Metadata":[
                {"ratingKey":"5","key":"/playlists/5/items","type":"playlist","title":"Fixture"}]}})
        } else {
            json!({"MediaContainer":{"size":0}})
        }
        .to_string();
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
    }
}

fn reply(runtime: &Runtime, op: &str) -> Value {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if let Some(events) = runtime.poll()["events"].as_array() {
            if let Some(event) = events.iter().find(|e| e["request"]["op"] == op) {
                return event.clone();
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("no reply for {op}");
}

#[test]
fn typed_requests_keep_the_json_contract() {
    std::env::set_var("PLEXFREQ_AUDIO_SINK", "fake");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            thread::spawn(move || serve(stream));
        }
    });
    let dir = tempfile::tempdir().unwrap();
    store::save(
        dir.path(),
        &store::Settings {
            client_identifier: "fixture-client".into(),
            server_url: origin,
            server_token: "fixture-token".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let runtime = Runtime::new(dir.path().into()).unwrap();

    // Malformed and internal requests fail at admission with the same
    // user-facing messages instead of reaching the worker.
    for (request, message) in [
        (json!({"op":"_shutdown"}), "Invalid request data"),
        (json!({"op":"browse"}), "Invalid request data"),
        (json!({"op":"repeat","mode":"twice"}), "Unknown repeat mode"),
        (
            json!({"op":"audio_seek","position":"soon"}),
            "Invalid seek position",
        ),
    ] {
        let error = runtime.submit(request.clone()).unwrap_err().to_string();
        assert_eq!(error, message, "{request}");
    }

    // A first page replaces the list and raises busy; the echo is the
    // request as submitted.
    let accepted = runtime.submit(json!({"op":"playlists","start":0})).unwrap();
    assert_eq!(accepted["resetItems"], true, "{accepted}");
    assert_eq!(accepted["clearError"], true, "{accepted}");
    let first = reply(&runtime, "playlists");
    assert_eq!(first["request"], json!({"op":"playlists","start":0}));
    assert_eq!(
        first["response"]["data"]["_listAction"], "replace",
        "{first}"
    );
    assert!(first["response"]["data"].get("_pageStart").is_none());

    // A later page is passive, counted as loading more, and appends.
    let accepted = runtime
        .submit(json!({"op":"playlists","start":100}))
        .unwrap();
    assert_eq!(accepted["resetItems"], false, "{accepted}");
    assert_eq!(accepted["clearError"], false, "{accepted}");
    let page = reply(&runtime, "playlists");
    assert_eq!(page["response"]["data"]["_listAction"], "append", "{page}");
    assert_eq!(page["response"]["data"]["_pageStart"], 100, "{page}");
    // `_replace` turns an offset request back into a replacing listing.
    runtime
        .submit(json!({"op":"playlists","start":100,"_replace":true}))
        .unwrap();
    let replaced = reply(&runtime, "playlists");
    assert_eq!(replaced["response"]["data"]["_listAction"], "replace");
    // `_refresh` reloads in place: no item reset at admission.
    let accepted = runtime
        .submit(json!({"op":"playlists","start":0,"_refresh":true,"_background":true}))
        .unwrap();
    assert_eq!(accepted["resetItems"], false, "{accepted}");
    assert_eq!(accepted["clearError"], false, "{accepted}");
    reply(&runtime, "playlists");

    // Request fields correlated into replies, on success or failure.
    runtime.submit(json!({"op":"lyrics","key":"7"})).unwrap();
    let lyrics = reply(&runtime, "lyrics");
    assert_eq!(lyrics["response"]["data"]["lyricsKey"], "7", "{lyrics}");
    assert_eq!(
        lyrics["response"]["data"]["_listAction"], "none",
        "{lyrics}"
    );
    runtime
        .submit(json!({"op":"filter_options","section":"1","kind":"album","field":"genre"}))
        .unwrap();
    let options = reply(&runtime, "filter_options");
    let data = &options["response"]["data"];
    assert_eq!(data["filterSection"], "1", "{options}");
    assert_eq!(data["filterKind"], "album", "{options}");
    assert_eq!(data["filterField"], "genre", "{options}");
    runtime
        .submit(json!({"op":"smart_rules","key":"9"}))
        .unwrap();
    let rules = reply(&runtime, "smart_rules");
    assert_eq!(rules["response"]["data"]["smartKey"], "9", "{rules}");
}
