use plexfreq_core::{
    model::Item,
    plex::{authenticated_url, server_path, server_url},
    queue::{Queue, Repeat},
    Command, Core,
};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    thread,
};

fn tracks() -> Vec<Item> {
    (0..4)
        .map(|i| Item {
            rating_key: i.to_string(),
            kind: "track".into(),
            title: format!("Track {i}"),
            ..Default::default()
        })
        .collect()
}

// A real local HTTP socket: verifies request paths, headers, encoding and response parsing.
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
            assert!(request.to_lowercase().contains("x-plex-client-identifier:"));
            assert!(request.to_lowercase().contains("accept: application/json"));
            let body = body.to_string();
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        }
    });
    (origin, task)
}

#[test]
fn queue_repeat_manual_skip_and_end() {
    let mut q = Queue::default();
    assert!(q.advance(true).is_none());
    assert!(q.replace(tracks(), 8).is_err());
    q.replace(tracks(), 2).unwrap();
    q.repeat = Repeat::One;
    assert_eq!(q.advance(true).unwrap().rating_key, "2");
    assert_eq!(q.advance(false).unwrap().rating_key, "3");
    q.repeat = Repeat::Off;
    assert!(q.advance(true).is_none());
    assert_eq!(q.previous().unwrap().rating_key, "2");
    q.select(3).unwrap();
    q.repeat = Repeat::All;
    assert_eq!(q.advance(true).unwrap().rating_key, "0");
}

#[test]
fn shuffle_is_permutation_preserving_current_and_history() {
    let mut q = Queue::default();
    q.replace(tracks(), 2).unwrap();
    q.shuffle(true);
    assert_eq!(q.current, Some(2));
    let mut seen = vec![q.track().unwrap().rating_key.clone()];
    for _ in 0..3 {
        seen.push(q.advance(false).unwrap().rating_key.clone());
    }
    seen.sort();
    assert_eq!(seen, vec!["0", "1", "2", "3"]);
    q.previous();
    let current = q.current;
    q.shuffle(false);
    assert_eq!(q.current, current);
    assert!(q.select(9).is_err());
}

#[test]
fn tokens_encoded_and_paths_origin_confined() {
    let base = server_url("https://music.example:32400").unwrap();
    let stream =
        authenticated_url(&base, "/library/parts/42/file.flac?download=0", "a&b+/").unwrap();
    let parsed = url::Url::parse(&stream).unwrap();
    assert!(parsed
        .query_pairs()
        .any(|(k, v)| k == "X-Plex-Token" && v == "a&b+/"));
    for path in [
        "https://evil.test/",
        "//evil.test/a",
        "/\\evil.test/a",
        "/a#fragment",
    ] {
        assert!(server_path(&base, path).is_err(), "{path}");
    }
    for url in [
        "file:///tmp/a",
        "https://user:pass@music.example/",
        "https://music.example/path",
        "https://music.example/?token=x",
    ] {
        assert!(server_url(url).is_err(), "{url}");
    }
}

#[test]
fn music_browse_play_and_persistent_session() {
    let (url, task) = server(vec![
        (
            "GET /library/sections",
            json!({"MediaContainer":{"Directory":[{"key":"1","title":"Music","type":"artist"},{"key":"2","title":"Films","type":"movie"}]}}),
        ),
        (
            "title=Bj%C3%B6rk+%26+friends",
            json!({"MediaContainer":{"size":1,"totalSize":101,"Metadata":[{"ratingKey":"42","key":"/library/metadata/42","type":"track","title":"Song","parentTitle":"Album","parentThumb":"/library/metadata/9/thumb/1","Media":[{"Part":[{"key":"/library/parts/42/file.flac"}]}]}]}}),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    core.execute(Command::CacheConfig {
        enabled: false,
        limit_mb: 512,
        ahead: 5,
    })
    .unwrap();
    let connected = core
        .execute(Command::Connect {
            url: url.clone(),
            token: "secret".into(),
        })
        .unwrap();
    assert_eq!(connected["libraries"].as_array().unwrap().len(), 1);
    let page = core
        .execute(Command::Browse {
            section: "1".into(),
            kind: "track".into(),
            query: "Björk & friends".into(),
            start: 100,
        })
        .unwrap();
    assert_eq!(page["hasMore"], false);
    assert_eq!(page["next"], 101);
    let items = serde_json::from_value(page["items"].clone()).unwrap();
    let play = core.execute(Command::Play { items, index: 0 }).unwrap();
    assert!(play["stream"]
        .as_str()
        .unwrap()
        .contains("/library/parts/42/file.flac?X-Plex-Token=secret"));
    assert!(play["track"]["artwork"]
        .as_str()
        .unwrap()
        .contains("/library/metadata/9/thumb/1"));
    task.join().unwrap();
    let state = std::fs::metadata(dir.path().join("session.json")).unwrap();
    assert_eq!(state.permissions().mode() & 0o777, 0o600);
    drop(core);
    let mut restored = Core::new(dir.path().into()).unwrap();
    assert_eq!(restored.execute(Command::Status).unwrap()["serverUrl"], url);
    assert!(restored
        .execute(Command::Browse {
            section: "../1".into(),
            kind: "track".into(),
            query: String::new(),
            start: 0
        })
        .is_err());
    restored.execute(Command::Logout).unwrap();
    assert!(!std::fs::read_to_string(dir.path().join("session.json"))
        .unwrap()
        .contains("secret"));
}

#[test]
fn pin_login_pending_authorized_discovery_and_redaction() {
    let (url, task) = server(vec![
        (
            "POST /api/v2/pins",
            json!({"id":7,"code":"strong-code","expiresIn":300,"authToken":null}),
        ),
        (
            "GET /api/v2/pins/7?code=strong-code",
            json!({"id":7,"code":"strong-code","expiresIn":300,"authToken":null}),
        ),
        (
            "GET /api/v2/pins/7?code=strong-code",
            json!({"id":7,"code":"strong-code","expiresIn":300,"authToken":"account-secret"}),
        ),
        (
            "x-plex-token: account-secret",
            json!([
                {"name":"Music server","clientIdentifier":"server-1","provides":"server","accessToken":"server-secret","connections":[{"uri":"http://127.0.0.1:32400","local":true,"relay":false}]},
                {"name":"Desktop player","clientIdentifier":"player-1","provides":"client,player,pubsub-player","connections":[]},
                {"name":"Phone player","clientIdentifier":"player-2","provides":"player","accessToken":null,"connections":[]}
            ]),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let mut core =
        Core::with_account_url(dir.path().into(), url::Url::parse(&url).unwrap()).unwrap();
    let login = core.execute(Command::Login).unwrap();
    assert!(login["url"]
        .as_str()
        .unwrap()
        .starts_with("https://app.plex.tv/auth#?clientID="));
    assert_eq!(core.execute(Command::PollLogin).unwrap()["signedIn"], false);
    assert_eq!(core.execute(Command::PollLogin).unwrap()["signedIn"], true);
    let servers = core.execute(Command::Servers).unwrap();
    assert_eq!(servers["servers"].as_array().unwrap().len(), 1);
    assert!(!servers.to_string().contains("secret"));
    assert!(!core
        .execute(Command::Status)
        .unwrap()
        .to_string()
        .contains("secret"));
    task.join().unwrap();
}

#[test]
fn expired_pin_and_corrupt_state_have_actionable_errors() {
    let (url, task) = server(vec![(
        "POST /api/v2/pins",
        json!({"id":1,"code":"x","expiresIn":0,"authToken":null}),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let mut core =
        Core::with_account_url(dir.path().into(), url::Url::parse(&url).unwrap()).unwrap();
    core.execute(Command::Login).unwrap();
    assert!(core
        .execute(Command::PollLogin)
        .unwrap_err()
        .to_string()
        .contains("expired"));
    task.join().unwrap();
    std::fs::write(dir.path().join("session.json"), "broken").unwrap();
    assert!(Core::new(dir.path().into()).is_err());
}

#[test]
fn discovery_validates_only_servers_and_reports_safe_context_for_invalid_servers() {
    let (url, task) = server(vec![
        (
            "GET /api/v2/resources?",
            json!([
                {"provides":"client,player","accessToken":null},
                {"name":"Server","clientIdentifier":"one","provides":"server,other","accessToken":"server-secret","connections":[]}
            ]),
        ),
        (
            "GET /api/v2/resources?",
            json!([
                {"name":"Broken server","clientIdentifier":"two","provides":"server","accessToken":null,"connections":[]}
            ]),
        ),
    ]);
    let plex =
        plexfreq_core::plex::Plex::new("test-client", url::Url::parse(&url).unwrap()).unwrap();
    let resources = plex.resources("account-secret").unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].client_identifier, "one");
    let error = plex.resources("account-secret").err().unwrap().to_string();
    assert_eq!(error, "Unexpected Plex response during server discovery");
    assert!(!error.contains("secret"));
    task.join().unwrap();
}

#[test]
fn artist_album_details_include_descriptions_gallery_and_paged_children() {
    let (url, task) = server(vec![
        (
            "GET /library/sections",
            json!({"MediaContainer":{"Directory":[]}}),
        ),
        (
            "GET /library/metadata/10?includeStations=1",
            json!({"MediaContainer":{"Metadata":[{"ratingKey":"10","type":"artist","title":"Artist","summary":"Biography","thumb":"/library/metadata/10/thumb/1","art":"/library/metadata/10/art/1","Image":[{"url":"/library/metadata/10/thumb/1"},{"url":"/library/metadata/10/thumb/2"},{"url":"//untrusted.example/photo"}]}]}}),
        ),
        (
            "GET /library/metadata/10/children?",
            json!({"MediaContainer":{"size":1,"totalSize":2,"Metadata":[{"ratingKey":"20","type":"album","title":"Album","year":2020}]}}),
        ),
        (
            "X-Plex-Container-Start=1",
            json!({"MediaContainer":{"size":1,"totalSize":2,"Metadata":[{"ratingKey":"21","type":"album","title":"Second album"}]}}),
        ),
        (
            "GET /library/metadata/20?includeStations=1",
            json!({"MediaContainer":{"Metadata":[{"ratingKey":"20","type":"album","title":"Album","summary":"Release notes","thumb":"/library/metadata/20/thumb/1","year":2020}]}}),
        ),
        (
            "GET /library/metadata/20/children?",
            json!({"MediaContainer":{"size":1,"Metadata":[{"ratingKey":"30","type":"track","title":"Song","index":1,"parentIndex":2}]}}),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    core.execute(Command::Connect {
        url,
        token: "secret".into(),
    })
    .unwrap();
    let artist = core
        .execute(Command::Detail {
            key: "10".into(),
            start: 0,
        })
        .unwrap();
    assert_eq!(artist["detail"]["summary"], "Biography");
    assert_eq!(artist["detail"]["photos"].as_array().unwrap().len(), 3);
    assert!(!artist["detail"]["photos"]
        .to_string()
        .contains("untrusted.example"));
    assert_eq!(artist["items"][0]["type"], "album");
    assert_eq!(artist["hasMore"], true);
    let next = core
        .execute(Command::Detail {
            key: "10".into(),
            start: 1,
        })
        .unwrap();
    assert!(next.get("detail").is_none());
    assert_eq!(next["hasMore"], false);
    let album = core
        .execute(Command::Detail {
            key: "20".into(),
            start: 0,
        })
        .unwrap();
    assert_eq!(album["detail"]["summary"], "Release notes");
    assert_eq!(album["detail"]["year"], 2020);
    assert_eq!(album["items"][0]["index"], 1);
    assert_eq!(album["items"][0]["parentIndex"], 2);
    assert!(core
        .execute(Command::Detail {
            key: "../20".into(),
            start: 0
        })
        .is_err());
    task.join().unwrap();
}

#[test]
fn optional_detail_fields_accept_null_without_breaking_music_lists() {
    let item: Item = serde_json::from_value(json!({"ratingKey":"30","type":"track","title":"Song",
        "summary":null,"art":null,"year":null,"index":null,"parentIndex":null,"Image":null}))
    .unwrap();
    assert!(item.summary.is_empty());
    assert!(item.images.is_empty());
    assert_eq!(item.year, 0);
    let unknown: Item = serde_json::from_value(json!({"ratingKey":"31","type":"track","title":"Unknown tags","year":-1,"index":-1,"parentIndex":-1})).unwrap();
    assert_eq!(unknown.index, -1);
    assert_eq!(unknown.parent_index, -1);
}

#[test]
fn artist_alphabet_offsets_jump_to_server_sorted_page_and_search_is_encoded() {
    let (url, task) = server(vec![
        (
            "GET /library/sections",
            json!({"MediaContainer":{"Directory":[]}}),
        ),
        (
            "GET /library/sections/1/firstCharacter?type=8&sort=titleSort%3Aasc",
            json!({"MediaContainer":{"Directory":[{"key":"#","title":"#","size":2},{"key":"A","title":"A","size":150},{"key":"B","title":"B","size":80}]}}),
        ),
        (
            "X-Plex-Container-Start=152",
            json!({"MediaContainer":{"size":1,"totalSize":232,"Metadata":[{"ratingKey":"10","type":"artist","title":"B fixture"}]}}),
        ),
        (
            "title=Bj%C3%B6rk+%26+friends",
            json!({"MediaContainer":{"size":0,"Metadata":[]}}),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    core.execute(Command::Connect {
        url,
        token: "fixture".into(),
    })
    .unwrap();
    let groups = core
        .execute(Command::Alphabet {
            section: "1".into(),
        })
        .unwrap();
    assert_eq!(groups["alphabet"][2]["offset"], 152);
    let jump = core
        .execute(Command::JumpArtist {
            section: "1".into(),
            letter: "B".into(),
        })
        .unwrap();
    assert_eq!(jump["start"], 152);
    assert_eq!(jump["next"], 153);
    assert_eq!(jump["replaceItems"], true);
    assert!(core
        .execute(Command::JumpArtist {
            section: "1".into(),
            letter: "Z".into()
        })
        .is_err());
    core.execute(Command::Browse {
        section: "1".into(),
        kind: "artist".into(),
        query: "Björk & friends".into(),
        start: 0,
    })
    .unwrap();
    task.join().unwrap();
}

#[test]
fn similar_artists_are_filtered_deduplicated_and_keep_library_list_separate() {
    let (url, task) = server(vec![
        (
            "GET /library/sections",
            json!({"MediaContainer":{"Directory":[]}}),
        ),
        (
            "GET /library/metadata/10/similar?count=12",
            json!({"MediaContainer":{"Metadata":[
                {"ratingKey":"10","type":"artist","title":"Self"},
                {"ratingKey":"20","type":"artist","title":"Related","thumb":"/library/metadata/20/thumb/1"},
                {"ratingKey":"20","type":"artist","title":"Duplicate"},
                {"ratingKey":"30","type":"album","title":"Wrong type"},
                {"ratingKey":"not-local","type":"artist","title":"External tag"}
            ]}}),
        ),
        (
            "GET /library/metadata/11/similar?count=12",
            json!({"MediaContainer":{"Metadata":[]}}),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    core.execute(Command::Connect {
        url,
        token: "fixture".into(),
    })
    .unwrap();
    let related = core
        .execute(Command::SimilarArtists { key: "10".into() })
        .unwrap();
    assert_eq!(related["similarArtists"].as_array().unwrap().len(), 1);
    assert!(related.get("items").is_none());
    assert_eq!(related["similarKey"], "10");
    assert_eq!(related["similarArtists"][0]["ratingKey"], "20");
    let empty = core
        .execute(Command::SimilarArtists { key: "11".into() })
        .unwrap();
    assert_eq!(empty["similarArtists"], json!([]));
    assert!(core
        .execute(Command::SimilarArtists {
            key: "../10".into()
        })
        .is_err());
    task.join().unwrap();
}
