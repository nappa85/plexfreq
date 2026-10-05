use plexfreq_core::{
    model::{Item, Media, Part},
    Command, Core,
};
use serde_json::json;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

#[test]
fn failed_track_resolution_and_reconnect_preserve_current_session() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let task = thread::spawn(move || {
        for (status, body) in [
            (
                "200 OK",
                json!({"MediaContainer":{"Directory":[]}}).to_string(),
            ),
            (
                "200 OK",
                json!({"MediaContainer":{"Metadata":[]}}).to_string(),
            ),
            (
                "401 Unauthorized",
                "Do not show server body or secret-token".into(),
            ),
        ] {
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
            write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
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
        url: origin.clone(),
        token: "secret-token".into(),
    })
    .unwrap();
    let playable = Item {
        rating_key: "1".into(),
        kind: "track".into(),
        media: vec![Media {
            parts: vec![Part {
                key: "/library/parts/1/file.flac".into(),
                size: None,
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let missing = Item {
        rating_key: "2".into(),
        kind: "track".into(),
        ..Default::default()
    };
    core.execute(Command::Play {
        items: vec![playable, missing],
        index: 0,
    })
    .unwrap();
    assert!(core.execute(Command::Next { automatic: false }).is_err());
    assert_eq!(
        core.execute(Command::Status).unwrap()["queue"]["current"],
        0
    );
    let error = core
        .execute(Command::Connect {
            url: origin.clone(),
            token: "wrong-token".into(),
        })
        .unwrap_err();
    assert!(error.to_string().contains("authorization"));
    assert!(!error.to_string().contains("secret-token"));
    assert_eq!(core.execute(Command::Status).unwrap()["serverUrl"], origin);
    let saved = std::fs::read_to_string(dir.path().join("session.json")).unwrap();
    assert!(saved.contains("secret-token"));
    assert!(!saved.contains("wrong-token"));
    task.join().unwrap();
}
