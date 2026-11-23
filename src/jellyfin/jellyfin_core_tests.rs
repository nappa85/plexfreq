//! Core against a Jellyfin fixture server: sign-in, session files, playback
//! reports, history, playlists, radio and the capability refusals, end to end
//! through typed commands.
use super::jellyfin_fixture_tests::*;
use crate::{
    model::Item,
    radio::{Radio, RadioKind, Source},
    server::{Capability, MediaServer},
    store, Command, Core, Error,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

fn cmd(value: Value) -> Command {
    serde_json::from_value(value).unwrap()
}

struct Session {
    core: Core,
    dir: tempfile::TempDir,
    server: Server,
    script: Arc<Mutex<Script>>,
}

fn login_command(server: &Server) -> Command {
    cmd(json!({"op":"jellyfin_login","url":server.url,"username":"tester","password":PASSWORD}))
}

fn signed_in() -> Session {
    signed_in_at("")
}

fn signed_in_at(prefix: &str) -> Session {
    let script = Arc::new(Mutex::new(Script::default()));
    let server = Server::start(prefix, standard(script.clone()));
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    core.execute(login_command(&server)).unwrap();
    core.execute(cmd(
        json!({"op":"cache_config","enabled":false,"limit_mb":512,"ahead":5}),
    ))
    .unwrap();
    server.take();
    Session {
        core,
        dir,
        server,
        script,
    }
}

impl Session {
    fn run(&mut self, value: Value) -> Value {
        self.core.execute(cmd(value)).unwrap()
    }
    fn fail(&mut self, value: Value) -> Error {
        self.core.execute(cmd(value)).err().unwrap()
    }
    /// The album's three tracks as the UI would hold them.
    fn tracks(&mut self) -> Vec<Item> {
        let page = self.run(json!({"op":"detail","key":album(),"kind":"album","start":0}));
        serde_json::from_value(page["items"].clone()).unwrap()
    }
    fn play(&mut self, items: &[Item], index: usize) -> Value {
        self.run(json!({"op":"play","items":items,"index":index}))
    }
    fn lines(&self) -> Vec<String> {
        self.server.lines()
    }
    fn saved(&self) -> store::Settings {
        store::load(self.dir.path()).unwrap()
    }
}

fn timeline(state: &str) -> Value {
    json!({"op":"timeline","state":state,"position":5000,"duration":240000,"continuing":false})
}

#[test]
fn a_password_sign_in_connects_and_saves_one_session_without_the_password() {
    let script = Arc::new(Mutex::new(Script::default()));
    let server = Server::start("", standard(script));
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    let reply = core.execute(login_command(&server)).unwrap();
    assert_eq!(reply["serverKind"], "jellyfin");
    assert_eq!(reply["libraries"][0]["key"], library());
    assert_eq!(reply["libraries"].as_array().unwrap().len(), 1);
    let saved = store::load(dir.path()).unwrap();
    assert_eq!(saved.server_kind, store::ServerKind::Jellyfin);
    assert_eq!(saved.server_url, format!("{}/", server.url));
    assert_eq!(saved.server_token, TOKEN);
    assert_eq!(
        (saved.server_user_id.as_str(), saved.server_id.as_str()),
        (USER, SERVER)
    );
    // Neither the password nor the sign-in request is stored anywhere.
    for entry in std::fs::read_dir(dir.path()).unwrap().flatten() {
        if entry.path().is_file() {
            let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
            assert!(!text.contains(PASSWORD), "{:?}", entry.path());
        }
    }
    let status = core.execute(cmd(json!({"op":"status"}))).unwrap();
    assert_eq!(status["signedIn"], true);
    assert_eq!(status["serverKind"], "jellyfin");
    assert_eq!(status["quickConnect"], Value::Null);
    assert_eq!(status["capabilities"]["sonic"], false);
    assert_eq!(status["capabilities"]["serverHubs"], true);
    assert_eq!(status["capabilities"]["smartPlaylists"], false);
    // The session survives a restart as a Jellyfin one.
    drop(core);
    let mut core = Core::new(dir.path().into()).unwrap();
    server.take();
    core.execute(cmd(json!({"op":"libraries"}))).unwrap();
    assert_eq!(server.lines(), [format!("GET /UserViews?userId={USER}")]);
    assert!(server.take()[0]
        .header("authorization")
        .unwrap()
        .contains(TOKEN));
}

#[test]
fn a_rejected_sign_in_changes_nothing() {
    let script = Arc::new(Mutex::new(Script::default()));
    let server = Server::start("", standard(script));
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    let wrong =
        cmd(json!({"op":"jellyfin_login","url":server.url,"username":"tester","password":"wrong"}));
    assert!(matches!(core.execute(wrong), Err(Error::Unauthorized)));
    for bad in [
        json!({"op":"jellyfin_login","url":server.url,"username":"  ","password":PASSWORD}),
        json!({"op":"jellyfin_login","url":"ftp://x","username":"u","password":"p"}),
    ] {
        assert!(core.execute(cmd(bad)).is_err());
    }
    let saved = store::load(dir.path()).unwrap();
    assert!(saved.server_url.is_empty() && saved.server_token.is_empty());
    assert_eq!(saved.server_kind, store::ServerKind::Plex);
    let status = core.execute(cmd(json!({"op":"status"}))).unwrap();
    assert_eq!(status["signedIn"], false);
}

#[test]
fn quick_connect_shows_a_code_polls_and_signs_in_once_approved() {
    let script = Arc::new(Mutex::new(Script::default()));
    let server = Server::start("", standard(script.clone()));
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    let started = core
        .execute(cmd(json!({"op":"jellyfin_quick_connect","url":server.url})))
        .unwrap();
    assert_eq!(started["quickConnect"]["code"], "123456");
    let status = core.execute(cmd(json!({"op":"status"}))).unwrap();
    assert_eq!(status["quickConnect"]["code"], "123456");
    assert_eq!(status["signedIn"], false);
    // The secret is never presented.
    assert!(!status.to_string().contains("fixture-secret"));
    // Pending: nothing changes and the code stays.
    let pending = core
        .execute(cmd(json!({"op":"jellyfin_quick_connect_poll"})))
        .unwrap();
    assert!(pending.get("signedIn").is_none());
    assert_eq!(pending["quickConnect"]["code"], "123456");
    assert!(pending.get("sessionReset").is_none());
    assert!(store::load(dir.path()).unwrap().server_url.is_empty());
    script.lock().unwrap().approved = true;
    let done = core
        .execute(cmd(json!({"op":"jellyfin_quick_connect_poll"})))
        .unwrap();
    assert_eq!(done["signedIn"], true);
    assert_eq!(done["sessionReset"], true);
    assert_eq!(done["serverKind"], "jellyfin");
    assert_eq!(done["capabilities"]["sonic"], false);
    assert_eq!(done["quickConnect"], Value::Null);
    assert_eq!(done["libraries"][0]["key"], library());
    let saved = store::load(dir.path()).unwrap();
    assert_eq!(saved.server_kind, store::ServerKind::Jellyfin);
    assert_eq!(
        (
            saved.server_token.as_str(),
            saved.server_user_id.as_str(),
            saved.server_id.as_str()
        ),
        (TOKEN, USER, SERVER)
    );
    let status = core.execute(cmd(json!({"op":"status"}))).unwrap();
    assert_eq!(status["quickConnect"], Value::Null);
    assert_eq!(status["signedIn"], true);
    // The request is spent.
    assert_eq!(
        core.execute(cmd(json!({"op":"jellyfin_quick_connect_poll"})))
            .unwrap()["quickConnect"],
        Value::Null
    );
}

#[test]
fn quick_connect_ends_on_expiry_cancellation_and_a_server_without_it() {
    let script = Arc::new(Mutex::new(Script::default()));
    let server = Server::start("", standard(script.clone()));
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    let poll = || cmd(json!({"op":"jellyfin_quick_connect_poll"}));
    let start = || cmd(json!({"op":"jellyfin_quick_connect","url":server.url}));
    // Polling with nothing pending (before starting, or after a cancel the
    // poll crossed) ends the login without an error.
    assert_eq!(core.execute(poll()).unwrap()["quickConnect"], Value::Null);
    // Expired on the server.
    core.execute(start()).unwrap();
    script.lock().unwrap().expired = true;
    let expired = core.execute(poll()).unwrap();
    assert_eq!(expired["quickConnect"], Value::Null);
    assert_eq!(expired["notice"], "Login expired; start again");
    assert!(expired.get("signedIn").is_none());
    assert_eq!(
        core.execute(cmd(json!({"op":"status"}))).unwrap()["quickConnect"],
        Value::Null
    );
    // A transient failure keeps the request pending for the next poll.
    script.lock().unwrap().expired = false;
    core.execute(start()).unwrap();
    let live = core.quick_connect.as_ref().unwrap().base.clone();
    core.quick_connect.as_mut().unwrap().base = "http://127.0.0.1:1/".parse().unwrap();
    assert!(core.execute(poll()).is_err());
    assert_eq!(
        core.execute(cmd(json!({"op":"status"}))).unwrap()["quickConnect"]["code"],
        "123456"
    );
    core.quick_connect.as_mut().unwrap().base = live;
    assert_eq!(
        core.execute(poll()).unwrap()["quickConnect"]["code"],
        "123456"
    );
    // Cancelled by the user.
    let cancelled = core
        .execute(cmd(json!({"op":"jellyfin_quick_connect_cancel"})))
        .unwrap();
    assert_eq!(cancelled["quickConnect"], Value::Null);
    assert_eq!(
        core.execute(cmd(json!({"op":"status"}))).unwrap()["quickConnect"],
        Value::Null
    );
    assert_eq!(core.execute(poll()).unwrap()["quickConnect"], Value::Null);
    // The feature is off on this server.
    script.lock().unwrap().quick_connect_enabled = false;
    assert!(core.execute(start()).is_err());
    assert_eq!(
        core.execute(cmd(json!({"op":"status"}))).unwrap()["quickConnect"],
        Value::Null
    );
    assert!(store::load(dir.path()).unwrap().server_url.is_empty());
    // A pending request that outlived its lifetime is dropped without asking.
    script.lock().unwrap().quick_connect_enabled = true;
    core.execute(start()).unwrap();
    let lifetime = crate::jellyfin::QUICK_CONNECT_LIFETIME + std::time::Duration::from_secs(1);
    let pending = core.quick_connect.as_mut().unwrap();
    pending.started = std::time::Instant::now()
        .checked_sub(lifetime)
        .expect("the machine has been up for ten minutes");
    let before = server.count();
    let expired = core.execute(poll()).unwrap();
    assert_eq!(expired["quickConnect"], Value::Null);
    assert_eq!(expired["notice"], "Login expired; start again");
    assert_eq!(server.count(), before);
}

#[test]
fn only_the_sign_in_with_a_password_resets_the_session_up_front() {
    use crate::command::Class;
    let login = cmd(json!({"op":"jellyfin_login","url":"http://x","username":"u","password":"p"}));
    let class: Class = login.class();
    assert!(class.resets_session && class.transport);
    for op in [
        json!({"op":"jellyfin_quick_connect","url":"http://x"}),
        json!({"op":"jellyfin_quick_connect_poll"}),
        json!({"op":"jellyfin_quick_connect_cancel"}),
    ] {
        let class = cmd(op).class();
        assert!(!class.resets_session && !class.transport);
    }
    // The password is a redacted field in diagnostics.
    let line =
        crate::diagnostics::redact(r#"{"op":"jellyfin_login","password":"hunter2-fixture"}"#);
    assert!(!line.contains("hunter2-fixture"), "{line}");
    let header = crate::diagnostics::redact(
        r#"Authorization: MediaBrowser Client="PlexFreq", Device="h", DeviceId="d", Version="1", Token="tok-fixture-secret""#,
    );
    assert!(!header.contains("tok-fixture-secret"), "{header}");
    let url = crate::diagnostics::redact("GET http://127.0.0.1/Items?api_key=key-fixture-secret");
    assert!(!url.contains("key-fixture-secret"), "{url}");
}

#[test]
fn listing_commands_reach_jellyfin_with_its_vocabulary() {
    let mut session = signed_in();
    let page =
        session.run(json!({"op":"browse","section":library(),"kind":"album","query":"","start":0}));
    assert_eq!(page["items"][0]["ratingKey"], album());
    assert!(session.lines()[0].starts_with(&format!(
        "GET /Items?ParentId={}&IncludeItemTypes=MusicAlbum",
        library()
    )));
    // Detail of an album: its metadata, then its tracks in disc/track order.
    session.server.take();
    let detail = session.run(json!({"op":"detail","key":album(),"kind":"album","start":0}));
    assert_eq!(detail["items"].as_array().unwrap().len(), 3);
    assert_eq!(detail["items"][0]["ratingKey"], track_id(1));
    assert_eq!(detail["detail"]["ratingKey"], album());
    // A thumbnail, an artwork id the provider can resolve and no token in it.
    assert!(
        detail["items"][0]["artwork"]
            .as_str()
            .unwrap()
            .starts_with("image://")
            || !detail["items"][0]["artwork"].as_str().unwrap().is_empty()
    );
    let favourites =
        session.run(json!({"op":"collection","section":library(),"view":"favorites","start":0}));
    assert_eq!(favourites["items"].as_array().unwrap().len(), 3);
    let last = session.lines().pop().unwrap();
    assert!(
        last.contains("Filters=IsFavorite") && last.contains("IncludeItemTypes=Audio"),
        "{last}"
    );
    let filtered = session.run(json!({
        "op":"filtered_browse","section":library(),"kind":"track","sort":"popular",
        "filters":{"genre":id('7'),"yearFrom":1990,"yearTo":1992,"unplayed":true},"start":0
    }));
    assert!(filtered["items"].is_array());
    let last = session.lines().pop().unwrap();
    assert!(
        last.contains("SortBy=PlayCount") && last.contains("Years=1990%2C1991%2C1992"),
        "{last}"
    );
    assert!(
        last.contains("Filters=IsUnplayed") && last.contains(&format!("GenreIds={}", id('7'))),
        "{last}"
    );
    // Genre choices come from the library.
    let options = session
        .run(json!({"op":"filter_options","section":library(),"kind":"track","field":"genre"}));
    assert_eq!(options["filterChoices"][0]["key"], id('7'));
    assert_eq!(options["filterAvailable"], true);
    // Mood and style have no Jellyfin equivalent.
    session.server.take();
    assert!(session
        .core
        .execute(cmd(
            json!({"op":"filter_options","section":library(),"kind":"track","field":"mood"})
        ))
        .is_err());
    assert!(session
        .core
        .execute(cmd(
            json!({"op":"filtered_browse","section":library(),"kind":"track","sort":"title",
                            "filters":{"mood":id('7')},"start":0})
        ))
        .is_err());
    assert_eq!(session.server.count(), 0);
    let search = session.run(json!({"op":"search","query":"art"}));
    assert!(search["items"].is_array());
    let similar = session.run(json!({"op":"similar_artists","key":artist()}));
    assert_eq!(similar["similarArtists"].as_array().unwrap().len(), 1);
    let playlists = session.run(json!({"op":"playlists","start":0}));
    assert_eq!(playlists["items"].as_array().unwrap().len(), 1);
    let entries = session.run(json!({"op":"playlist_items","key":playlist(),"start":0}));
    assert_eq!(entries["items"].as_array().unwrap().len(), 3);
    assert_eq!(entries["items"][0]["playlistItemId"], id('1'));
}

#[test]
fn commands_jellyfin_cannot_serve_are_refused_before_any_request() {
    let mut session = signed_in();
    for op in [
        json!({"op":"sonic_neighbors","key":track_id(1),"kind":"track"}),
        json!({"op":"sonic_adventure","section":library(),"start_key":track_id(1),"end_key":track_id(2)}),
        json!({"op":"sonic_journey","section":library(),"keys":[track_id(1),track_id(2)]}),
        json!({"op":"station","key":"/library/metadata/1/station/x","title":"t"}),
        json!({"op":"download_station","key":"/library/metadata/1/station/x","title":"t","minutes":60}),
        json!({"op":"smart_rules","key":playlist()}),
        json!({"op":"smart_playlist","action":"create","section":library(),"title":"t","sort":"title","limit":10,"filters":{}}),
        json!({"op":"alphabet","section":library()}),
        json!({"op":"jump_artist","section":library(),"letter":"A"}),
        json!({"op":"filtered_browse","section":library(),"kind":"track","sort":"title","filters":{"minRating":8},"start":0}),
    ] {
        let error = session.fail(op.clone());
        assert_eq!(
            error.to_string(),
            "This server does not support this feature",
            "{op}"
        );
    }
    assert_eq!(session.server.count(), 0);
    assert!(!session
        .core
        .backend
        .capabilities()
        .allows(Capability::Sonic));
}

#[test]
fn hubs_are_listed_and_their_keys_open_the_full_listing() {
    let mut session = signed_in();
    let home = session.run(json!({"op":"discovery_home","section":library()}));
    let items = home["items"].as_array().unwrap();
    assert!(!items.is_empty());
    let first = &items[0];
    assert_eq!(first["discoveryGroup"], "Recently added");
    assert_eq!(
        first["hubIdentifier"],
        format!("music.recent.added.{}", library())
    );
    assert_eq!(first["discoveryGrid"], true);
    assert_eq!(first["hubMore"], true);
    session.server.take();
    let key = items
        .iter()
        .find(|i| i["discoveryGroup"] == "Most played")
        .unwrap()["hubKey"]
        .clone();
    let page = session.run(json!({"op":"hub_items","key":key,"start":0}));
    assert_eq!(page["items"].as_array().unwrap().len(), 3);
    let line = session.lines().pop().unwrap();
    assert!(
        line.contains("SortBy=PlayCount") && line.contains("StartIndex=0"),
        "{line}"
    );
    // A key that names no hub of ours is refused.
    session.server.take();
    assert!(session
        .core
        .execute(cmd(
            json!({"op":"hub_items","key":"/hubs/jellyfin/zz/recent-added","start":0})
        ))
        .is_err());
    assert!(session
        .core
        .execute(cmd(
            json!({"op":"hub_items","key":"/hubs/jellyfin/../x","start":0})
        ))
        .is_err());
    assert_eq!(session.server.count(), 0);
}

#[test]
fn playback_streams_the_original_or_a_transcode_without_a_token_in_the_url() {
    let mut session = signed_in();
    let tracks = session.tracks();
    let direct = session.play(&tracks, 0);
    let stream = direct["stream"].as_str().unwrap();
    assert_eq!(direct["playbackSource"], "stream");
    assert!(
        stream.ends_with(&format!("/Audio/{}/stream?static=true", track_id(1))),
        "{stream}"
    );
    assert!(!stream.contains(TOKEN));
    session.run(json!({"op":"quality_config","config":{
        "wifiKbps":128,"mobileKbps":128,"downloadKbps":0,"codecFallback":true}}));
    let transcoded = session.play(&tracks, 1);
    assert_eq!(transcoded["playbackSource"], "transcode");
    assert_eq!(transcoded["streamKbps"], 128);
    let url = url::Url::parse(transcoded["stream"].as_str().unwrap()).unwrap();
    assert_eq!(url.path(), format!("/Audio/{}/stream.mp3", track_id(2)));
    let get = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };
    assert_eq!(get("audioBitRate").as_deref(), Some("128000"));
    assert_eq!(get("deviceId").unwrap(), session.saved().client_identifier);
    assert!(uuid::Uuid::parse_str(&get("playSessionId").unwrap()).is_ok());
    assert!(!url.as_str().contains(TOKEN));
    // The gain Jellyfin computed reaches the engine's track description.
    assert_eq!(
        transcoded["track"]["Media"][0]["Part"][0]["Stream"][0]["gain"],
        -3.5
    );
}

#[test]
fn the_start_of_a_play_is_reported_once_per_occurrence_across_a_restart() {
    let mut session = signed_in();
    let tracks = session.tracks();
    let plan = session.play(&tracks, 0);
    let occurrence = plan["playbackOccurrence"].as_str().unwrap().to_owned();
    session.server.take();
    session.run(timeline("playing"));
    session.run(timeline("playing"));
    let seen = session.server.take();
    assert_eq!(seen[0].path, "/Sessions/Playing");
    assert_eq!(seen[1].path, "/Sessions/Playing/Progress");
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].json()["PlaySessionId"], occurrence);
    assert_eq!(seen[0].json()["PositionTicks"], 50_000_000u64);
    assert_eq!(seen[0].json()["PlayMethod"], "DirectPlay");
    assert_eq!(session.saved().playback.start_reported, occurrence);
    // Restored after a restart: the same occurrence, not told again.
    let Session {
        core,
        dir,
        server,
        script,
    } = session;
    drop(core);
    let mut core = Core::new(dir.path().into()).unwrap();
    server.take();
    core.execute(cmd(timeline("playing"))).unwrap();
    assert_eq!(
        server
            .take()
            .iter()
            .map(|r| r.path.clone())
            .collect::<Vec<_>>(),
        ["/Sessions/Playing/Progress"]
    );
    // A new play is a new occurrence and is told.
    let plan = core
        .execute(cmd(json!({"op":"play","items":tracks,"index":1})))
        .unwrap();
    assert_ne!(plan["playbackOccurrence"].as_str().unwrap(), occurrence);
    server.take();
    core.execute(cmd(timeline("playing"))).unwrap();
    assert_eq!(server.take()[0].path, "/Sessions/Playing");
    // Stopping is its own report, with the transcode flag of the source.
    core.execute(cmd(timeline("stopped"))).unwrap();
    assert_eq!(server.take()[0].path, "/Sessions/Playing/Stopped");
    drop(script);
}

#[test]
fn a_failed_start_is_retried_and_does_not_count_as_told() {
    let script = Arc::new(Mutex::new(Script::default()));
    let down = Arc::new(AtomicBool::new(true));
    let inner = standard(script);
    let flag = down.clone();
    let server = Server::start("", move |req: &Req| {
        if req.path == "/Sessions/Playing" && flag.load(Ordering::SeqCst) {
            Reply::status(500)
        } else {
            inner(req)
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    core.execute(login_command(&server)).unwrap();
    core.execute(cmd(
        json!({"op":"cache_config","enabled":false,"limit_mb":512,"ahead":5}),
    ))
    .unwrap();
    let page = core
        .execute(cmd(
            json!({"op":"detail","key":album(),"kind":"album","start":0}),
        ))
        .unwrap();
    let plan = core
        .execute(cmd(json!({"op":"play","items":page["items"],"index":0})))
        .unwrap();
    let occurrence = plan["playbackOccurrence"].as_str().unwrap().to_owned();
    assert!(core.execute(cmd(timeline("playing"))).is_err());
    assert!(store::load(dir.path())
        .unwrap()
        .playback
        .start_reported
        .is_empty());
    // The play qualifies while the start is still untold: it stays to be
    // replayed with its date.
    core.execute(cmd(
        json!({"op":"playback_event","key":track_id(1),"occurrence":occurrence,
                            "listened":200000,"duration":240000}),
    ))
    .unwrap();
    let pending = core.execute(cmd(json!({"op":"status"}))).unwrap()["history"]["pending"].clone();
    assert_eq!(pending, 1);
    down.store(false, Ordering::SeqCst);
    server.take();
    core.execute(cmd(timeline("playing"))).unwrap();
    assert_eq!(server.take()[0].path, "/Sessions/Playing");
    // Told after all: the pending play was counted by that start.
    let pending = core.execute(cmd(json!({"op":"status"}))).unwrap()["history"]["pending"].clone();
    assert_eq!(pending, 0);
    core.execute(cmd(json!({"op":"sync_history"}))).unwrap();
    assert!(server
        .take()
        .iter()
        .all(|r| !r.path.starts_with("/UserPlayedItems")));
}

#[test]
fn offline_plays_replay_with_their_date_and_reported_ones_do_not() {
    let mut session = signed_in();
    let tracks = session.tracks();
    // Track 1: online, start reported, then qualifies: nothing to deliver.
    let plan = session.play(&tracks, 0);
    session.run(timeline("playing"));
    session.run(json!({"op":"playback_event","key":track_id(1),
        "occurrence":plan["playbackOccurrence"],"listened":200000,"duration":240000}));
    // Tracks 2 and 3 were heard without the server hearing the start.
    for n in [1usize, 2] {
        let plan = session.play(&tracks, n);
        session.run(json!({"op":"playback_event","key":track_id(n as u32 + 1),
            "occurrence":plan["playbackOccurrence"],"listened":200000,"duration":240000}));
    }
    assert_eq!(session.run(json!({"op":"status"}))["history"]["pending"], 2);
    session.server.take();
    session.run(json!({"op":"sync_history"}));
    let seen = session.server.take();
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen.iter().map(|r| r.path.clone()).collect::<Vec<_>>(),
        [
            format!("/UserPlayedItems/{}", track_id(2)),
            format!("/UserPlayedItems/{}", track_id(3))
        ]
    );
    let dates: Vec<_> = seen
        .iter()
        .map(|r| r.param("datePlayed").unwrap())
        .collect();
    assert!(
        dates
            .iter()
            .all(|d| d.len() == 20 && d.ends_with('Z') && d.contains('T')),
        "{dates:?}"
    );
    assert!(dates[0] <= dates[1]);
    assert_eq!(session.run(json!({"op":"status"}))["history"]["pending"], 0);
    // Nothing is sent twice.
    session.server.take();
    session.run(json!({"op":"sync_history"}));
    assert_eq!(session.server.count(), 0);
}

#[test]
fn playlist_edits_use_entry_ids_and_confirm_moves() {
    let mut session = signed_in();
    let tracks = session.tracks();
    let created =
        session.run(json!({"op":"playlist_edit","action":"create","title":"Mine","items":tracks}));
    assert_eq!(created["createdPlaylist"]["ratingKey"], playlist());
    let request = session
        .server
        .take()
        .into_iter()
        .find(|r| r.path == "/Playlists")
        .unwrap();
    assert_eq!(request.json()["Ids"].as_array().unwrap().len(), 3);
    assert_eq!(request.json()["MediaType"], "Audio");
    session.run(json!({"op":"playlist_edit","action":"add","key":playlist(),"items":[tracks[0]]}));
    assert!(session
        .server
        .take()
        .iter()
        .any(|r| r.method == "POST" && r.path == format!("/Playlists/{}/Items", playlist())));
    session.run(json!({"op":"playlist_edit","action":"remove","key":playlist(),"item_id":id('2')}));
    assert!(session
        .server
        .take()
        .iter()
        .any(|r| r.method == "DELETE" && r.param("entryIds").as_deref() == Some(id('2').as_str())));
    // The JSON the QML sends: the entry and the one it goes after, no index.
    // Playlist [1,2,3]: entry 3 moves up after entry 1 (index 1 on the wire).
    session.server.take();
    session.run(json!({"op":"playlist_edit","action":"move","key":playlist(),"item_id":id('3'),"after":id('1')}));
    let seen = session.server.take();
    assert!(seen.iter().any(|r| r.method == "POST"
        && r.path == format!("/Playlists/{}/Items/{}/Move/1", playlist(), id('3'))));
    let order = |s: &Session| -> Vec<String> {
        s.script
            .lock()
            .unwrap()
            .playlist
            .iter()
            .map(|(e, _)| e.clone())
            .collect()
    };
    assert_eq!(order(&session), [id('1'), id('3'), id('2')]);
    // Down: entry 1 after entry 3; then to the top (`after` null).
    session.run(json!({"op":"playlist_edit","action":"move","key":playlist(),"item_id":id('1'),"after":id('3')}));
    assert_eq!(order(&session), [id('3'), id('1'), id('2')]);
    session.run(json!({"op":"playlist_edit","action":"move","key":playlist(),"item_id":id('2'),"after":null}));
    assert_eq!(order(&session), [id('2'), id('3'), id('1')]);
    // An entry the playlist does not have is an error, not a success.
    assert!(session
        .core
        .execute(cmd(
            json!({"op":"playlist_edit","action":"move","key":playlist(),
        "item_id":id('7'),"after":null})
        ))
        .is_err());
    session.run(json!({"op":"playlist_edit","action":"rename","key":playlist(),"title":"New"}));
    assert!(session
        .server
        .take()
        .iter()
        .any(|r| r.method == "POST" && r.json() == json!({"Name":"New"})));
    session.run(json!({"op":"playlist_edit","action":"delete","key":playlist()}));
    assert!(session
        .server
        .take()
        .iter()
        .any(|r| r.method == "DELETE" && r.path == format!("/Items/{}", playlist())));
}

#[test]
fn favourites_toggle_the_flag_and_lyrics_come_from_the_audio_endpoint() {
    let mut session = signed_in();
    let rated = session.run(json!({"op":"favorite","key":track_id(1),"enabled":true}));
    assert_eq!(rated["userRating"], 10);
    session.run(json!({"op":"favorite","key":track_id(1),"enabled":false}));
    let seen = session.server.take();
    assert_eq!(
        seen[0].line(),
        format!("POST /UserFavoriteItems/{}?userId={USER}", track_id(1))
    );
    assert_eq!(seen[1].method, "DELETE");
    // No lyrics on the server: an empty list, not an error.
    let none = session.run(json!({"op":"lyrics","key":track_id(1)}));
    assert_eq!(none["lyrics"], json!([]));
    session.script.lock().unwrap().lyrics = Some(json!({"Lyrics":[
        {"Text":"first","Start":15_000_000u64},{"Text":"second"}]}));
    let lyrics = session.run(json!({"op":"lyrics","key":track_id(1)}));
    assert_eq!(lyrics["lyrics"][0]["time"], 1500);
    assert_eq!(lyrics["lyrics"][0]["text"], "first");
    assert_eq!(lyrics["lyrics"][1]["time"], Value::Null);
    // A track the server says has no lyrics is not asked for them.
    session.server.take();
    let two = session.run(json!({"op":"lyrics","key":track_id(2)}));
    assert_eq!(two["lyrics"], json!([]));
    assert!(session
        .server
        .take()
        .iter()
        .all(|r| !r.path.ends_with("/Lyrics")));
}

#[test]
fn a_mix_radio_asks_the_instant_mix_again_and_appends_only_unseen_tracks() {
    let mut session = signed_in();
    let started = session.run(json!({"op":"radio","key":artist(),"kind":"artist"}));
    assert_eq!(started["radio"]["source"], "mix");
    assert!(started["stream"].as_str().unwrap().contains("/Audio/"));
    let mix = session
        .server
        .take()
        .into_iter()
        .find(|r| r.path.ends_with("/InstantMix"))
        .unwrap();
    assert_eq!(mix.path, format!("/Items/{}/InstantMix", artist()));
    assert_eq!(mix.param("Limit").as_deref(), Some("50"));
    let keys = |core: &Core| -> Vec<String> {
        core.queue
            .items()
            .iter()
            .map(|i| i.rating_key.clone())
            .collect()
    };
    assert_eq!(
        keys(&session.core),
        (1..=5).map(track_id).collect::<Vec<_>>()
    );
    // The next batch overlaps the queue: only 6 and 7 are new.
    session.script.lock().unwrap().mix = vec![4, 5, 6, 7];
    session.run(json!({"op":"next","automatic":false}));
    let mixes = session
        .server
        .take()
        .into_iter()
        .filter(|r| r.path.ends_with("/InstantMix"))
        .count();
    assert_eq!(mixes, 1);
    assert_eq!(
        keys(&session.core),
        (1..=7).map(track_id).collect::<Vec<_>>()
    );
    // The radio survives a restart as a mix radio.
    let saved = session.saved();
    let radio = saved.playback.radio.as_ref().unwrap();
    assert!(
        matches!(&radio.source, Source::Mix { seed, kind, .. } if *seed == artist() && *kind == RadioKind::Artist)
    );
    // A track radio starts with its seed.
    session.script.lock().unwrap().mix = vec![3, 4, 5];
    let started = session.run(json!({"op":"radio","key":track_id(2),"kind":"track"}));
    assert_eq!(started["track"]["ratingKey"], track_id(2));
    let mix = session
        .server
        .take()
        .into_iter()
        .find(|r| r.path.ends_with("/InstantMix"))
        .unwrap();
    assert_eq!(mix.path, format!("/Items/{}/InstantMix", track_id(2)));
    assert_eq!(keys(&session.core)[..2], [track_id(2), track_id(3)]);
    // Nothing playable in the mix: the radio does not start.
    session.script.lock().unwrap().mix = vec![];
    let error = session.fail(json!({"op":"radio","key":album(),"kind":"album"}));
    assert_eq!(error.to_string(), "Radio returned no playable tracks");
}

fn mix_seeds(session: &Session) -> Vec<String> {
    session
        .server
        .take()
        .into_iter()
        .filter(|r| r.path.ends_with("/InstantMix"))
        .map(|r| r.path.split('/').nth(2).unwrap().to_owned())
        .collect()
}

fn mix_used(session: &Session) -> Vec<String> {
    match session.saved().playback.radio.unwrap().source {
        Source::Mix { used, .. } => used,
        _ => panic!("not a mix radio"),
    }
}

#[test]
fn a_mix_stable_per_seed_continues_from_a_queued_track() {
    let mut session = signed_in();
    // The server answers the same batch for the radio's seed, and other
    // batches for the tracks it queued.
    session
        .script
        .lock()
        .unwrap()
        .mix_by_seed
        .insert(track_id(5), vec![5, 6, 7]);
    session.run(json!({"op":"radio","key":artist(),"kind":"artist"}));
    assert_eq!(mix_seeds(&session), [artist()]);
    session.run(json!({"op":"next","automatic":false}));
    // The seed's mix held nothing new: the newest queued track seeds again.
    assert_eq!(mix_seeds(&session), [artist(), track_id(5)]);
    let keys: Vec<String> = session
        .core
        .queue
        .items()
        .iter()
        .map(|i| i.rating_key.clone())
        .collect();
    assert_eq!(keys, (1..=7).map(track_id).collect::<Vec<_>>());
    assert_eq!(mix_used(&session), [track_id(5)]);
    // Stock behaviour is untouched: a mix with new tracks asks only the seed.
    session.script.lock().unwrap().mix = vec![7, 8];
    session.run(json!({"op":"next","automatic":false}));
    assert_eq!(mix_seeds(&session), [artist()]);
    assert_eq!(mix_used(&session), [track_id(5)]);
}

#[test]
fn a_mix_radio_fails_as_before_once_every_candidate_seed_is_exhausted() {
    let mut session = signed_in();
    session.run(json!({"op":"radio","key":artist(),"kind":"artist"}));
    session.server.take();
    // Every mix returns the queued tracks: three requests per refill, newest
    // seeds first, none asked twice.
    for expected in [
        vec![artist(), track_id(5), track_id(4)],
        vec![artist(), track_id(3), track_id(2)],
        vec![artist(), track_id(1)],
        vec![artist()],
    ] {
        let error = session.core.extend_radio().unwrap_err();
        assert_eq!(error.to_string(), "Radio returned no playable tracks");
        assert_eq!(mix_seeds(&session), expected);
    }
    match &session.core.radio.as_ref().unwrap().source {
        Source::Mix { used, .. } => assert_eq!(used.len(), 5),
        _ => panic!("not a mix radio"),
    }
}

fn mix_used_now(session: &Session) -> Vec<String> {
    match &session.core.radio.as_ref().unwrap().source {
        Source::Mix { used, .. } => used.clone(),
        _ => panic!("not a mix radio"),
    }
}

#[test]
fn a_failing_fallback_seed_is_skipped_and_remembered() {
    let mut session = signed_in();
    session.run(json!({"op":"radio","key":artist(),"kind":"artist"}));
    session.server.take();
    {
        let mut script = session.script.lock().unwrap();
        script.mix_status.insert(track_id(5), 404);
        script.mix_status.insert(track_id(4), 500);
    }
    let error = session.core.extend_radio().unwrap_err();
    // The radio's own reply stands (all seen), so the old error surfaces
    // rather than the fallbacks' failures.
    assert_eq!(error.to_string(), "Radio returned no playable tracks");
    assert_eq!(mix_seeds(&session), [artist(), track_id(5), track_id(4)]);
    // The gone seed (404) is not asked again; the server failure (500) stops
    // the rotation without using its seed up.
    assert_eq!(mix_used_now(&session), [track_id(5)]);
}

#[test]
fn a_failing_fallback_moves_on_to_the_next_seed() {
    let mut session = signed_in();
    session.run(json!({"op":"radio","key":artist(),"kind":"artist"}));
    session.server.take();
    session
        .script
        .lock()
        .unwrap()
        .mix_status
        .insert(track_id(5), 404);
    session
        .script
        .lock()
        .unwrap()
        .mix_by_seed
        .insert(track_id(4), vec![4, 6, 7]);
    session.core.extend_radio().unwrap();
    assert_eq!(mix_seeds(&session), [artist(), track_id(5), track_id(4)]);
    assert_eq!(session.core.queue.items().len(), 7);
    assert_eq!(mix_used_now(&session), [track_id(5), track_id(4)]);
}

#[test]
fn the_background_refill_rotates_seeds_and_keeps_them() {
    let mut session = signed_in();
    session
        .script
        .lock()
        .unwrap()
        .mix_by_seed
        .insert(track_id(5), vec![5, 6, 7]);
    session.run(json!({"op":"radio","key":artist(),"kind":"artist"}));
    session.server.take();
    session.core.radio.as_mut().unwrap().refill.owe();
    let job = session.core.refill_job().expect("an owed refill");
    let update = session.core.refill_landed(job.fetch(), None).unwrap();
    assert_eq!(update["queue"]["items"].as_array().unwrap().len(), 7);
    assert_eq!(mix_seeds(&session), [artist(), track_id(5)]);
    assert_eq!(mix_used_now(&session), [track_id(5)]);
}

#[test]
fn a_failed_move_keeps_the_seeds_its_refill_asked() {
    let mut session = signed_in();
    session.run(json!({"op":"radio","key":artist(),"kind":"artist"}));
    session.server.take();
    let last = session.core.queue.items().len() - 1;
    session.core.queue.select(last).unwrap();
    session.fail(json!({"op":"next","automatic":false}));
    assert_eq!(mix_seeds(&session), [artist(), track_id(5), track_id(4)]);
    assert_eq!(mix_used_now(&session), [track_id(5), track_id(4)]);
}

#[test]
fn queues_and_radios_saved_by_plex_load_unchanged() {
    for text in [
        r#"{"Station":{"queue_id":7}}"#,
        r#"{"Sonic":{"seed":{"ratingKey":"5","type":"track"},"recent":["5"]}}"#,
    ] {
        let source: Source = serde_json::from_str(text).unwrap();
        // The variants keep their external tag and fields.
        let again = serde_json::to_value(&source).unwrap();
        assert_eq!(again.as_object().unwrap().keys().count(), 1);
        assert_eq!(
            again.as_object().unwrap().keys().next().map(String::as_str),
            Some(if text.contains("Station") {
                "Station"
            } else {
                "Sonic"
            })
        );
        assert!(serde_json::from_value::<Source>(again).is_ok());
    }
    assert_eq!(
        serde_json::to_string(&Source::Station { queue_id: 7 }).unwrap(),
        r#"{"Station":{"queue_id":7}}"#
    );
    let mix: Source = serde_json::from_str(r#"{"Mix":{"seed":"abc","kind":"track"}}"#).unwrap();
    // A mix radio saved before the rotation seeds existed loads without them.
    assert!(matches!(
        &mix,
        Source::Mix {
            kind: RadioKind::Track,
            used,
            ..
        } if used.is_empty()
    ));
    let rotated: Source = serde_json::from_str(&serde_json::to_string(&mix).unwrap()).unwrap();
    assert!(matches!(rotated, Source::Mix { seed, .. } if seed == "abc"));
    // A queue file written by the Plex-only code (Station radio) still loads.
    let dir = tempfile::tempdir().unwrap();
    let mut settings = store::Settings::default();
    settings.playback.radio = Some(Radio::new(
        RadioKind::Artist,
        "R".into(),
        Source::Station { queue_id: 7 },
    ));
    store::save(dir.path(), &settings).unwrap();
    let text = std::fs::read_to_string(dir.path().join("queue.json")).unwrap();
    assert!(
        text.contains(r#""source":{"Station":{"queue_id":7}}"#),
        "{text}"
    );
    let loaded = store::load(dir.path()).unwrap();
    assert!(matches!(
        loaded.playback.radio.unwrap().source,
        Source::Station { queue_id: 7 }
    ));
}

#[test]
fn a_session_saved_by_plex_loads_as_plex_and_is_saved_without_the_new_fields() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = r#"{"clientIdentifier":"fixture-client","accountToken":"fixture-account",
        "serverUrl":"http://127.0.0.1:32400/","serverToken":"fixture-token",
        "playback":{"position":0,"generation":3,"occurrence":"o","listened":0}}"#;
    std::fs::write(dir.path().join("session.json"), legacy).unwrap();
    let loaded = store::load(dir.path()).unwrap();
    assert_eq!(loaded.server_kind, store::ServerKind::Plex);
    assert!(loaded.server_id.is_empty() && loaded.playback.start_reported.is_empty());
    let mut core = Core::new(dir.path().into()).unwrap();
    let status = core.execute(cmd(json!({"op":"status"}))).unwrap();
    assert_eq!(status["serverKind"], "plex");
    assert_eq!(status["capabilities"]["sonic"], true);
    assert_eq!(status["signedIn"], true);
    // Saved again, a Plex session has no trace of the Jellyfin fields.
    core.execute(cmd(
        json!({"op":"audio_config","config":crate::audio::dsp::Config::default()}),
    ))
    .ok();
    store::save(dir.path(), &loaded).unwrap();
    let text = std::fs::read_to_string(dir.path().join("session.json")).unwrap();
    for field in ["serverKind", "serverId", "startReported", "serverUserId"] {
        assert!(!text.contains(field), "{field} in {text}");
    }
    // A Jellyfin session spells them.
    let jellyfin = store::Settings {
        server_kind: store::ServerKind::Jellyfin,
        server_id: "s".into(),
        ..Default::default()
    };
    store::save(dir.path(), &jellyfin).unwrap();
    let text = std::fs::read_to_string(dir.path().join("session.json")).unwrap();
    assert!(
        text.contains(r#""serverKind":"jellyfin""#) && text.contains(r#""serverId":"s""#),
        "{text}"
    );
}

#[test]
fn signing_in_again_keeps_the_downloads_scope_and_another_server_drops_it() {
    let mut session = signed_in();
    let tracks = session.tracks();
    session.play(&tracks, 0);
    assert_eq!(session.saved().playback.queue.items().len(), 3);
    // Same server and user, a new token: the queue and downloads stay.
    session.script.lock().unwrap().token = "fixture-second-token";
    let url = session.server.url.clone();
    session
        .core
        .execute(cmd(
            json!({"op":"jellyfin_login","url":url,"username":"tester","password":PASSWORD}),
        ))
        .unwrap();
    let saved = session.saved();
    assert_eq!(saved.server_token, "fixture-second-token");
    assert_eq!(saved.playback.queue.items().len(), 3);
    // The same server by another address keeps them too.
    let other = Server::start("", standard(session.script.clone()));
    session
        .core
        .execute(cmd(
            json!({"op":"jellyfin_login","url":other.url,"username":"tester","password":PASSWORD}),
        ))
        .unwrap();
    assert_eq!(session.saved().playback.queue.items().len(), 3);
    // Another server is another scope.
    session.script.lock().unwrap().server = "99999999999999999999999999999999";
    let url = other.url.clone();
    session
        .core
        .execute(cmd(
            json!({"op":"jellyfin_login","url":url,"username":"tester","password":PASSWORD}),
        ))
        .unwrap();
    assert!(session.saved().playback.queue.items().is_empty());
}

#[test]
fn a_server_behind_a_path_prefix_gets_every_request_below_it() {
    let mut session = signed_in_at("/jellyfin");
    assert!(session.saved().server_url.ends_with("/jellyfin/"));
    let tracks = session.tracks();
    let plan = session.play(&tracks, 0);
    assert!(
        plan["stream"]
            .as_str()
            .unwrap()
            .contains("/jellyfin/Audio/"),
        "{plan}"
    );
    session.run(timeline("playing"));
    for line in session.lines() {
        assert!(
            line.split(' ').nth(1).unwrap().starts_with("/jellyfin/"),
            "{line}"
        );
    }
}

#[test]
fn connecting_to_plex_replaces_jellyfin_and_signing_out_returns_to_plex() {
    let mut session = signed_in();
    let plex = Server::start("", |req: &Req| {
        if req.path == "/library/sections" {
            Reply::json(json!({"MediaContainer":{"size":1,"Directory":[
                {"key":"3","title":"Music","type":"artist"}]}}))
        } else {
            Reply::status(404)
        }
    });
    let reply =
        session.run(json!({"op":"connect","url":plex.url.clone(),"token":"fixture-plex-token"}));
    assert_eq!(reply["serverKind"], "plex");
    assert_eq!(reply["capabilities"]["sonic"], true);
    let saved = session.saved();
    assert_eq!(saved.server_kind, store::ServerKind::Plex);
    assert!(saved.server_id.is_empty() && saved.server_user_id.is_empty());
    assert_eq!(saved.server_token, "fixture-plex-token");
    let status = session.run(json!({"op":"status"}));
    assert_eq!(status["capabilities"]["sonic"], true);
    assert!(plex.take()[0].header("x-plex-token").is_some());
    // And back: sign in to Jellyfin, then sign out.
    let url = session.server.url.clone();
    session
        .core
        .execute(cmd(
            json!({"op":"jellyfin_login","url":url,"username":"tester","password":PASSWORD}),
        ))
        .unwrap();
    session.server.take();
    let out = session.run(json!({"op":"logout"}));
    assert_eq!(out["signedIn"], false);
    assert_eq!(out["serverKind"], "plex");
    assert_eq!(out["capabilities"]["sonic"], true);
    assert_eq!(out["quickConnect"], Value::Null);
    assert!(session
        .lines()
        .iter()
        .any(|l| l.starts_with("POST /Sessions/Logout")));
    let saved = session.saved();
    assert!(saved.server_token.is_empty() && saved.server_id.is_empty());
    assert_eq!(saved.server_kind, store::ServerKind::Plex);
    let status = session.run(json!({"op":"status"}));
    assert_eq!(status["serverKind"], "plex");
    assert_eq!(status["capabilities"]["sonic"], true);
    assert_eq!(status["signedIn"], false);
}

#[test]
fn plex_account_sign_in_works_while_a_jellyfin_session_is_active() {
    let account = Server::start("", |req: &Req| {
        match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/api/v2/pins") => {
                Reply::json(json!({"id":7,"code":"abcd","expiresIn":600,"authToken":null}))
            }
            ("GET", "/api/v2/pins/7") => Reply::json(
                json!({"id":7,"code":"abcd","expiresIn":600,"authToken":"fixture-account-token"}),
            ),
            ("GET", "/api/v2/resources") => Reply::json(json!([{
                "name":"Plex box","clientIdentifier":"plex-1","provides":"server",
                "accessToken":"fixture-server-token","connections":[]
            }])),
            _ => Reply::status(404),
        }
    });
    let script = Arc::new(Mutex::new(Script::default()));
    let server = Server::start("", standard(script));
    let dir = tempfile::tempdir().unwrap();
    let mut core =
        Core::with_account_url(dir.path().into(), url::Url::parse(&account.url).unwrap()).unwrap();
    core.execute(login_command(&server)).unwrap();
    server.take();
    let login = core.execute(cmd(json!({"op":"login"}))).unwrap();
    assert!(login["url"].as_str().unwrap().contains("abcd"));
    let polled = core.execute(cmd(json!({"op":"poll_login"}))).unwrap();
    assert_eq!(polled["signedIn"], true);
    let servers = core.execute(cmd(json!({"op":"servers"}))).unwrap();
    assert_eq!(servers["servers"][0]["id"], "plex-1");
    // The Jellyfin session is untouched: still its kind, no request to it.
    assert_eq!(server.count(), 0);
    let status = core.execute(cmd(json!({"op":"status"}))).unwrap();
    assert_eq!(status["serverKind"], "jellyfin");
    let saved = store::load(dir.path()).unwrap();
    assert_eq!(saved.account_token, "fixture-account-token");
    assert_eq!(saved.server_kind, store::ServerKind::Jellyfin);
}
