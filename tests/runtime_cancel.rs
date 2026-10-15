//! Navigation aborts a view-bound Plex request that is still in flight.
//! Private dbus-daemon, fake audio sink and a local slow HTTP server; the
//! session fixture lives in a temporary directory with a dummy token.
use plexfreq_core::{runtime::Runtime, store};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

struct Bus(std::process::Child);
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn request_line(stream: &mut TcpStream) -> Option<String> {
    let mut request = Vec::new();
    let mut byte = [0];
    while !request.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).ok()? == 0 {
            return None;
        }
        request.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&request).into_owned();
    text.lines().next().map(str::to_owned)
}

#[derive(Debug)]
enum Seen {
    /// The slow request arrived.
    Stalled,
    /// The client closed the stalled connection, after this long.
    Aborted(Duration),
}

#[test]
fn navigation_cancels_in_flight_view_request_promptly() {
    let mut child = std::process::Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("dbus-daemon fixture is required");
    let mut address = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    let _bus = Bus(child);
    std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", address.trim());
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", address.trim());
    std::env::set_var("PLEXFREQ_AUDIO_SINK", "fake");

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let (seen, observed) = mpsc::channel();
    let stalled = Arc::new(AtomicBool::new(false));
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let seen = seen.clone();
            let stalled = stalled.clone();
            thread::spawn(move || {
                while let Some(line) = request_line(&mut stream) {
                    let playlists = line.starts_with("GET /playlists?");
                    if playlists && !stalled.swap(true, Ordering::SeqCst) {
                        // Hold the reply far beyond the test's deadlines and
                        // report when the client gives up on the connection.
                        let _ = seen.send(Seen::Stalled);
                        let started = Instant::now();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(20)))
                            .unwrap();
                        let mut rest = [0; 64];
                        while matches!(stream.read(&mut rest), Ok(n) if n > 0) {}
                        let _ = seen.send(Seen::Aborted(started.elapsed()));
                        return;
                    }
                    let body = if playlists {
                        json!({"MediaContainer":{"size":1,"totalSize":1,"Metadata":[
                            {"ratingKey":"5","key":"/playlists/5/items","type":"playlist","title":"Fresh"}]}})
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
            });
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
    runtime.submit(json!({"op":"playlists","start":0})).unwrap();
    // Wait for the slow request to be in flight on the server.
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        match observed.recv_timeout(Duration::from_millis(100)) {
            Ok(Seen::Stalled) => break,
            Ok(other) => panic!("unexpected {other:?}"),
            Err(_) => assert!(Instant::now() < end, "slow request never arrived"),
        }
    }
    thread::sleep(Duration::from_millis(200));
    let navigated = Instant::now();
    runtime.submit(json!({"op":"playlists","start":0})).unwrap();
    let mut replies: Vec<(Duration, Value)> = Vec::new();
    while replies.len() < 2 && navigated.elapsed() < Duration::from_secs(10) {
        if let Some(events) = runtime.poll()["events"].as_array() {
            for event in events {
                if event["request"]["op"] == "playlists" {
                    replies.push((navigated.elapsed(), event["response"].clone()));
                }
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(replies.len(), 2, "replies: {replies:?}");
    // The stale request ends as a dropped reply, promptly (the Plex request
    // deadline is 15 s), and its connection is closed.
    let (stale_after, stale) = &replies[0];
    assert_eq!(stale["data"]["_discarded"], true, "{stale}");
    assert!(
        stale["error"].is_null(),
        "cancellation is not an error: {stale}"
    );
    assert!(
        *stale_after < Duration::from_secs(2),
        "stale request took {stale_after:?}"
    );
    match observed.recv_timeout(Duration::from_secs(3)) {
        Ok(Seen::Aborted(after)) => assert!(after < Duration::from_secs(3), "{after:?}"),
        other => panic!("in-flight connection was not aborted: {other:?}"),
    }
    // The current view's reply is delivered normally.
    let (_, fresh) = &replies[1];
    assert_eq!(fresh["ok"], true, "{fresh}");
    assert_eq!(fresh["data"]["items"][0]["title"], "Fresh", "{fresh}");
    drop(runtime);
}
