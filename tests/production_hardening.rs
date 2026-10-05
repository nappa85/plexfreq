//! Production hardening: logic bugs that pass current CI but are wrong in
//! production. Each test fails until the underlying fix lands.

use plexfreq_core::{
    model::{Envelope, Item},
    queue::{Queue, Repeat},
    Command, Core,
};
use serde_json::{json, Value};
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

fn server(responses: Vec<(&'static str, Value)>) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let task = thread::spawn(move || {
        for (expected, body) in responses {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 4096];
            loop {
                let n = socket.read(&mut buf).unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buf[..n]);
                if request.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.contains(expected), "expected {expected}: {request}");
            let body = body.to_string();
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        }
    });
    (origin, task)
}

// Security: internal worker ops must never be reachable via public submit().
// QML/FFI callers control the JSON op; accepting "_shutdown" lets any view
// kill the Rust worker, and "_audio_checkpoint"/"_update" let it forge
// playback state.
#[test]
fn runtime_rejects_internal_worker_ops() {
    use plexfreq_core::runtime::Runtime;
    let dir = tempfile::tempdir().unwrap();
    let runtime = Runtime::new(dir.path().into()).unwrap();
    for op in ["_shutdown", "_audio_checkpoint", "_update"] {
        let result = runtime.submit(json!({"op": op}));
        assert!(
            result.is_err(),
            "internal op {op} must be rejected, got {result:?}"
        );
    }
}

// Queue semantics: enabling repeat-all after natural end must allow restart.
// advance() early-returns on current==None, so a queue that ended with
// Repeat::Off stays stuck even after switching to All.
#[test]
fn queue_advance_repeat_all_after_natural_end_restarts() {
    let mut q = Queue::default();
    q.replace(vec![track("0"), track("1")], 0).unwrap();
    assert!(q.advance(false).is_some());
    assert!(q.advance(false).is_none());
    assert!(q.track().is_none());
    q.repeat = Repeat::All;
    let restarted = q.advance(false);
    assert!(
        restarted.is_some(),
        "repeat-all after natural end must restart, got None"
    );
    assert_eq!(restarted.unwrap().rating_key, "0");
}

// Robustness: Plex numeric IDs are small. numeric() accepts arbitrary length
// ("9"*100) which later becomes a huge URL path. Overlong IDs must be
// rejected as Input before any network work. Detail checks numeric() first,
// so a fresh Core (no server) must fail with the numeric message, not with
// "Connect first" or a network error.
#[test]
fn numeric_rejects_unbounded_ids_before_network() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    let long = "9".repeat(100);
    let err = core
        .execute(Command::Detail {
            key: long,
            start: 0,
        })
        .unwrap_err()
        .to_string();
    assert!(
        err.to_lowercase().contains("numeric"),
        "100-digit ID must be rejected as numeric input, got: {err}"
    );
}

// Data robustness: lenient_i32 uses `as i32` casts which wrap out-of-range
// Plex numbers (99999999999 -> 1215752191, u64::MAX -> -1) into nonsense
// years. Out-of-range values must default instead of wrapping.
#[test]
fn model_lenient_i32_does_not_wrap_large_numbers() {
    let payload = json!({"MediaContainer":{"size":1,"Metadata":[{"ratingKey":"1","key":"/a","title":"T","type":"track","year":99999999999u64}]}});
    let env: Envelope = serde_json::from_value(payload).unwrap();
    let year = env.container.items[0].year;
    assert!(
        (0..=2100).contains(&year),
        "year 99999999999 wrapped to {year}, must default/clamp"
    );
}

// Pagination: page_sized() computes hasMore as `c.size == 100`, ignoring its
// `size` argument. JumpArtist requests start+100 (e.g. 150) from zero; a full
// 150-item window with no totalSize is wrongly reported as last page.
#[test]
fn jump_artist_has_more_respects_requested_window() {
    let artists: Vec<Value> = (0..150)
        .map(|i| json!({"ratingKey": i.to_string(), "key": format!("/library/metadata/{i}"), "type": "artist", "title": format!("Artist {i:03}")}))
        .collect();
    let (url, task) = server(vec![
        (
            "GET /library/sections",
            json!({"MediaContainer":{"Directory":[]}}),
        ),
        (
            "GET /library/sections/1/firstCharacter",
            json!({"MediaContainer":{"Directory":[{"key":"A","title":"A","size":50},{"key":"B","title":"B","size":80}]}}),
        ),
        (
            "X-Plex-Container-Size=150",
            json!({"MediaContainer":{"size":150,"Metadata": artists}}),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    core.execute(Command::Connect {
        url,
        token: "fixture".into(),
    })
    .unwrap();
    let jump = core
        .execute(Command::JumpArtist {
            section: "1".into(),
            letter: "B".into(),
        })
        .unwrap();
    task.join().unwrap();
    assert_eq!(jump["selectedIndex"], 50);
    assert_eq!(
        jump["hasMore"], true,
        "full 150-item window with no totalSize must report hasMore=true, got {}",
        jump["hasMore"]
    );
}

// Presentation safety: presentation() maps current via cursor, not via the
// position of current in order. For corrupt order [1,0] with current 0 and
// cursor 0 it returns Some(0) pointing at the wrong track (B) instead of the
// actual current (A at index 1).
#[test]
fn queue_presentation_corrupt_cursor_mismatch_does_not_point_to_wrong_track() {
    let mut q = Queue::default();
    q.replace(vec![track("0"), track("1")], 0).unwrap();
    let mut value = serde_json::to_value(&q).unwrap();
    value["order"] = json!([1, 0]);
    value["current"] = json!(0);
    value["cursor"] = json!(0);
    let corrupt: Queue = serde_json::from_value(value).unwrap();
    assert!(
        corrupt.validate().is_err(),
        "test setup must be corrupt (cursor 0 != current 0 position)"
    );
    let presented = corrupt.presentation();
    // q.items from order [1,0] is ["1","0"]; current 0 lives at index 1.
    assert_eq!(
        presented.current,
        Some(1),
        "corrupt cursor must not invent selection {:?} pointing at wrong track",
        presented.current
    );
    assert_eq!(presented.items[presented.current.unwrap()].rating_key, "0");
}
