//! Core driven through the in-memory backend (no HTTP). Needs `--features mock-backend`.
#![cfg(feature = "mock-backend")]
use plexfreq_core::{
    model::Container,
    server::{Action, Backend, MockError, MockServer, Page, Query, Recorded},
    Command, Core,
};

fn connected(mock: &MockServer) -> (Core, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::with_backend(dir.path().into(), Backend::Mock(mock.clone())).unwrap();
    core.execute(Command::Connect {
        url: "http://127.0.0.1:32400/".into(),
        token: "synthetic".into(),
    })
    .unwrap();
    (core, dir)
}

#[test]
fn favorite_is_recorded_as_a_rate_action() {
    let mock = MockServer::default();
    mock.script(Query::Sections, None, Ok(Container::default()));
    let (mut core, _dir) = connected(&mock);
    core.execute(Command::Favorite {
        key: "5".into(),
        enabled: true,
    })
    .unwrap();
    assert_eq!(
        mock.actions(),
        [Action::Rate {
            id: "5".into(),
            rating: 10
        }]
    );
    assert!(matches!(
        mock.log()[0],
        Recorded::Fetch(Query::Sections, None)
    ));
}

#[test]
fn a_network_error_falls_back_to_the_saved_library_but_unauthorized_does_not() {
    let mock = MockServer::default();
    // Connect consumes the first reply and saves it; the last one repeats.
    mock.script(Query::Sections, None, Ok(Container::default()));
    mock.script(Query::Sections, None, Err(MockError::Network));
    let (mut core, _dir) = connected(&mock);
    let libraries = core.execute(Command::Libraries).unwrap();
    assert_eq!(libraries["offline"], true, "{libraries}");

    let mock = MockServer::default();
    mock.script(Query::Sections, None, Ok(Container::default()));
    mock.script(Query::Sections, None, Err(MockError::Unauthorized));
    let (mut core, _dir) = connected(&mock);
    let error = core.execute(Command::Libraries).unwrap_err();
    assert!(
        matches!(error, plexfreq_core::Error::Unauthorized),
        "{error}"
    );
}

// ---------------------------------------------------------------------------
// Backend-neutral boundary: GUID identifiers, base-path servers, capabilities
// ---------------------------------------------------------------------------

use plexfreq_core::{
    model::{Item, ItemKind},
    server::{BrowseSpec, Caps, MockIds, Music, Sort},
};
use serde_json::{json, Value};

const ALBUM: &str = "11111111111111111111111111111111";
const TRACK_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TRACK_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const BASE: &str = "http://127.0.0.1:8096/jellyfin/";

fn track(id: &str) -> Item {
    serde_json::from_value(json!({
        "ratingKey": id, "key": format!("/Items/{id}"), "type": "track",
        "title": format!("Song {id}"), "duration": 240000,
        "parentRatingKey": ALBUM, "parentTitle": "Album",
        "Media": [{"audioCodec": "mp3", "container": "mp3",
                   "Part": [{"key": format!("/Audio/{id}/stream")}]}]
    }))
    .unwrap()
}

fn album(id: &str) -> Item {
    serde_json::from_value(json!({"ratingKey": id, "key": format!("/Items/{id}"),
        "type": "album", "title": "Album"}))
    .unwrap()
}

fn guid_core(mock: &MockServer) -> (Core, tempfile::TempDir) {
    mock.script_ids(MockIds::Guid);
    mock.script(Query::Sections, None, Ok(Container::default()));
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::with_backend(dir.path().into(), Backend::Mock(mock.clone())).unwrap();
    core.execute(Command::Connect {
        url: BASE.into(),
        token: "synthetic".into(),
    })
    .unwrap();
    (core, dir)
}

fn tracks_container(ids: &[&str]) -> Container {
    Container {
        size: ids.len(),
        items: ids.iter().map(|id| track(id)).collect(),
        ..Default::default()
    }
}

#[test]
fn a_base_path_server_and_guid_ids_play_and_record_a_listen() {
    let mock = MockServer::default();
    let (mut core, _dir) = guid_core(&mock);
    let plan = core
        .execute(Command::Play {
            items: vec![track(TRACK_A), track(TRACK_B)],
            index: 0,
        })
        .unwrap();
    // The stream URL stays under the base path and carries no credential.
    assert_eq!(
        plan["stream"],
        format!("{BASE}Audio/{TRACK_A}/stream"),
        "{plan}"
    );
    assert_eq!(plan["queue"]["items"][1]["ratingKey"], TRACK_B);
    let listened = core
        .execute(Command::PlaybackEvent {
            key: TRACK_A.into(),
            occurrence: plan["playbackOccurrence"].as_str().unwrap().into(),
            listened: 200_000,
            duration: 240_000,
        })
        .unwrap();
    assert_eq!(listened["history"]["pending"], 1, "{listened}");
    // A key the backend does not accept never becomes history.
    let numeric = core.execute(Command::PlaybackEvent {
        key: "5".into(),
        occurrence: plan["playbackOccurrence"].as_str().unwrap().into(),
        listened: 200_000,
        duration: 240_000,
    });
    assert!(numeric.is_ok(), "an unknown current track is ignored");
    let status = core.execute(Command::Status).unwrap();
    assert_eq!(status["history"]["pending"], 1);
}

#[test]
fn guid_ids_pass_download_admission_and_numeric_ids_do_not() {
    let mock = MockServer::default();
    let (mut core, _dir) = guid_core(&mock);
    core.execute(Command::CacheTracks {
        items: vec![track(TRACK_A)],
    })
    .unwrap();
    let refused = core
        .execute(Command::CacheTracks {
            items: vec![track("123")],
        })
        .unwrap_err();
    assert!(refused.to_string().contains("valid tracks"), "{refused}");

    // A download plan reads the album's tracks through (kind, id).
    mock.script(
        Query::Item {
            id: ALBUM.into(),
            stations: true,
        },
        None,
        Ok(Container {
            items: vec![album(ALBUM)],
            ..Default::default()
        }),
    );
    mock.script(
        Query::Children {
            id: ALBUM.into(),
            kind: ItemKind::Album,
        },
        Some(Page {
            start: 0,
            size: 100,
        }),
        Ok(tracks_container(&[TRACK_A, TRACK_B])),
    );
    let planned = core
        .execute(Command::DownloadPlan {
            kind: plexfreq_core::DownloadKind::Album,
            key: ALBUM.into(),
            minutes: 0,
        })
        .unwrap();
    assert!(
        planned["downloads"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["group"] == format!("album:{ALBUM}") && row["total"] == 2),
        "{planned}"
    );
    assert!(mock.log().iter().any(|r| matches!(
        r,
        Recorded::Fetch(
            Query::Children {
                kind: ItemKind::Album,
                ..
            },
            _
        )
    )));
}

#[test]
fn a_mix_of_guid_albums_queues_their_tracks() {
    let mock = MockServer::default();
    let (mut core, _dir) = guid_core(&mock);
    mock.script(
        Query::Children {
            id: ALBUM.into(),
            kind: ItemKind::Album,
        },
        Some(Page {
            start: 0,
            size: 100,
        }),
        Ok(tracks_container(&[TRACK_A, TRACK_B])),
    );
    let mix = core
        .execute(Command::Mix {
            seeds: vec![album(ALBUM)],
        })
        .unwrap();
    assert_eq!(mix["mix"], true);
    let keys: Vec<_> = mix["queue"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["ratingKey"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(keys.len(), 2, "{mix}");
    assert!(keys.contains(&TRACK_A.to_owned()) && keys.contains(&TRACK_B.to_owned()));
}

#[test]
fn children_are_requested_by_kind_and_id_for_a_guid_backend() {
    let mock = MockServer::default();
    let (mut core, _dir) = guid_core(&mock);
    mock.script(
        Query::Children {
            id: ALBUM.into(),
            kind: ItemKind::Album,
        },
        Some(Page {
            start: 0,
            size: 100,
        }),
        Ok(tracks_container(&[TRACK_A])),
    );
    let page = core
        .execute(Command::Children {
            key: format!("/Items/{ALBUM}"),
            kind: Some(ItemKind::Album),
            start: 0,
        })
        .unwrap();
    assert_eq!(page["items"][0]["ratingKey"], TRACK_A);
    // An id this backend does not accept is refused before any request.
    let before = mock.log().len();
    assert!(core
        .execute(Command::Children {
            key: "/library/metadata/7/children".into(),
            kind: None,
            start: 0,
        })
        .is_err());
    assert_eq!(mock.log().len(), before);
}

#[test]
fn commands_needing_a_missing_capability_are_refused_before_any_request() {
    let mock = MockServer::default();
    mock.script_caps(Caps {
        sonic: false,
        sonic_adventure: false,
        server_stations: false,
        smart_playlists: false,
        server_hubs: false,
        first_character: false,
        star_rating: false,
        mood_filter: false,
        style_filter: false,
        rating_filter: false,
    });
    mock.script(Query::Sections, None, Ok(Container::default()));
    let (mut core, _dir) = connected(&mock);
    let before = mock.log().len();
    let refused = [
        Command::SonicNeighbors {
            key: "1".into(),
            kind: ItemKind::Track,
        },
        Command::SonicAdventure {
            section: "1".into(),
            start_key: "2".into(),
            end_key: "3".into(),
        },
        Command::SonicJourney {
            section: "1".into(),
            keys: vec!["2".into(), "3".into()],
        },
        Command::Station {
            key: "/library/metadata/1/station/x".into(),
            title: "Radio".into(),
        },
        Command::SmartRules { key: "5".into() },
        Command::HubItems {
            key: "/hubs/sections/1".into(),
            start: 0,
        },
        Command::Alphabet {
            section: "1".into(),
        },
        Command::JumpArtist {
            section: "1".into(),
            letter: "A".into(),
        },
    ];
    for command in refused {
        let op = format!("{:?}", std::mem::discriminant(&command));
        let error = core.execute(command).unwrap_err();
        assert_eq!(
            error.to_string(),
            "This server does not support this feature",
            "{op}"
        );
    }
    assert_eq!(mock.log().len(), before, "no request was made");

    // The status the UI hides actions by.
    let status = core.execute(Command::Status).unwrap();
    for field in [
        "sonic",
        "sonicAdventure",
        "serverStations",
        "smartPlaylists",
        "serverHubs",
        "firstCharacter",
        "starRating",
        "moodFilter",
        "styleFilter",
        "ratingFilter",
    ] {
        assert_eq!(status["capabilities"][field], false, "{field}");
    }
}

#[test]
fn a_plex_like_server_publishes_every_capability() {
    let mock = MockServer::default();
    mock.script(Query::Sections, None, Ok(Container::default()));
    let (mut core, _dir) = connected(&mock);
    let status: Value = core.execute(Command::Status).unwrap();
    assert_eq!(
        status["capabilities"],
        json!({"sonic":true,"sonicAdventure":true,"serverStations":true,
               "smartPlaylists":true,"serverHubs":true,"firstCharacter":true,
               "starRating":true,"moodFilter":true,"styleFilter":true,
               "ratingFilter":true})
    );
}

#[test]
fn a_playlist_move_carries_string_ids_and_the_target_index() {
    let mock = MockServer::default();
    let (mut core, _dir) = guid_core(&mock);
    let playlist = "22222222222222222222222222222222";
    mock.script(
        Query::Playlist {
            id: playlist.into(),
        },
        None,
        Ok(Container {
            items: vec![serde_json::from_value(
                json!({"ratingKey": playlist, "type": "playlist", "title": "P"}),
            )
            .unwrap()],
            ..Default::default()
        }),
    );
    core.execute(
        serde_json::from_value::<Command>(json!({
            "op": "playlist_edit", "action": "move", "key": playlist,
            "item_id": "entry-guid-1", "after": "entry-guid-0", "index": 3
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        mock.actions(),
        [Action::PlaylistMove {
            id: playlist.into(),
            item: "entry-guid-1".into(),
            after: Some("entry-guid-0".into()),
            index: Some(3),
        }]
    );
    // The old numeric JSON still deserializes (Plex clients send numbers).
    let command: Command = serde_json::from_value(json!({
        "op": "playlist_edit", "action": "remove", "key": "9", "item_id": 101
    }))
    .unwrap();
    assert!(
        matches!(command, Command::PlaylistEdit { item_id: Some(id), .. } if id.as_str() == "101")
    );
}

#[test]
fn the_mock_namespace_follows_the_user_not_the_url_or_token() {
    use plexfreq_core::server::{Conn, MediaServer};
    let mock = MockServer::default();
    mock.script_ids(MockIds::Guid);
    let conn = |url: &str, token: &str, user: Option<&str>| Conn {
        base: url::Url::parse(url).unwrap(),
        token: token.into(),
        user_id: user.map(str::to_owned),
    };
    let lan = mock.namespace(&conn("http://192.168.1.2:8096/", "t1", Some("u1")));
    let remote = mock.namespace(&conn("https://music.example/jf/", "t2", Some("u1")));
    let other = mock.namespace(&conn("http://192.168.1.2:8096/", "t1", Some("u2")));
    assert_eq!(
        lan, remote,
        "a LAN/remote switch or re-login keeps the scope"
    );
    assert_ne!(lan, other);
}

const ARTIST: &str = "22222222222222222222222222222222";

fn artist_container() -> Container {
    Container {
        size: 1,
        items: vec![serde_json::from_value(json!({"ratingKey": ARTIST,
            "key": format!("/Items/{ARTIST}"), "type": "artist", "title": "Artist"}))
        .unwrap()],
        ..Default::default()
    }
}

fn page(start: usize) -> Option<Page> {
    Some(Page { start, size: 100 })
}

fn fetches(mock: &MockServer) -> Vec<Query> {
    mock.log()
        .into_iter()
        .filter_map(|r| match r {
            Recorded::Fetch(query, _) => Some(query),
            _ => None,
        })
        .collect()
}

#[test]
fn later_detail_pages_list_children_of_the_real_kind() {
    let item = Query::Item {
        id: ARTIST.into(),
        stations: true,
    };
    let children = Query::Children {
        id: ARTIST.into(),
        kind: ItemKind::Artist,
    };
    let mock = MockServer::default();
    let (mut core, _dir) = guid_core(&mock);
    mock.script(item.clone(), None, Ok(artist_container()));
    for start in [0, 100] {
        mock.script(children.clone(), page(start), Ok(Container::default()));
    }
    // The current UI sends no kind: page 0 learns it from the item...
    let first = core
        .execute(Command::Detail {
            key: ARTIST.into(),
            kind: None,
            start: 0,
        })
        .unwrap();
    assert_eq!(first["detail"]["ratingKey"], ARTIST, "{first}");
    // ...and the next page reuses it without fetching the item again.
    let before = fetches(&mock).len();
    core.execute(Command::Detail {
        key: ARTIST.into(),
        kind: None,
        start: 100,
    })
    .unwrap();
    assert_eq!(&fetches(&mock)[before..], std::slice::from_ref(&children));

    // A fresh Core given the kind needs no item fetch for a later page.
    let mock = MockServer::default();
    let (mut core, _dir) = guid_core(&mock);
    mock.script(children.clone(), page(100), Ok(Container::default()));
    let command: Command =
        serde_json::from_value(json!({"op":"detail","key":ARTIST,"kind":"artist","start":100}))
            .unwrap();
    core.execute(command).unwrap();
    assert_eq!(&fetches(&mock)[1..], std::slice::from_ref(&children));

    // Nothing known: the (cached) item supplies the kind first.
    let mock = MockServer::default();
    let (mut core, _dir) = guid_core(&mock);
    mock.script(item.clone(), None, Ok(artist_container()));
    mock.script(children.clone(), page(100), Ok(Container::default()));
    let page_100 = core
        .execute(Command::Detail {
            key: ARTIST.into(),
            kind: None,
            start: 100,
        })
        .unwrap();
    assert!(page_100.get("detail").is_none(), "{page_100}");
    assert_eq!(fetches(&mock)[1..], [item, children]);
}

#[test]
fn a_backend_listing_children_without_kind_skips_the_item_read() {
    // Plex-style ids: children do not depend on the kind, so a later page
    // of an unknown item lists them directly.
    let mock = MockServer::default();
    mock.script(Query::Sections, None, Ok(Container::default()));
    let (mut core, _dir) = connected(&mock);
    let children = Query::Children {
        id: "8".into(),
        kind: ItemKind::default(),
    };
    mock.script(children.clone(), page(100), Ok(Container::default()));
    let before = fetches(&mock).len();
    let value = core
        .execute(Command::Detail {
            key: "8".into(),
            kind: None,
            start: 100,
        })
        .unwrap();
    assert!(value.get("detail").is_none(), "{value}");
    assert_eq!(&fetches(&mock)[before..], [children]);
}

#[test]
fn logout_forgets_the_remembered_detail_kinds() {
    let item = Query::Item {
        id: ARTIST.into(),
        stations: true,
    };
    let children = Query::Children {
        id: ARTIST.into(),
        kind: ItemKind::Artist,
    };
    let mock = MockServer::default();
    let (mut core, _dir) = guid_core(&mock);
    mock.script(item.clone(), None, Ok(artist_container()));
    for start in [0, 100] {
        mock.script(children.clone(), page(start), Ok(Container::default()));
    }
    core.execute(Command::Detail {
        key: ARTIST.into(),
        kind: None,
        start: 0,
    })
    .unwrap();
    assert_eq!(core.remembered_detail_kinds(), 1);
    core.execute(Command::Logout).unwrap();
    // Forgotten by the logout itself, not by a later connection.
    assert_eq!(core.remembered_detail_kinds(), 0);
    core.execute(Command::Connect {
        url: BASE.into(),
        token: "synthetic".into(),
    })
    .unwrap();
    // The kind learnt before the logout is gone: the item is read again.
    let before = fetches(&mock).len();
    core.execute(Command::Detail {
        key: ARTIST.into(),
        kind: None,
        start: 100,
    })
    .unwrap();
    assert_eq!(&fetches(&mock)[before..], [item, children]);
}

#[test]
fn filters_the_server_cannot_apply_are_refused_before_any_request() {
    let mock = MockServer::default();
    mock.script_caps(Caps {
        mood_filter: false,
        style_filter: false,
        rating_filter: false,
        ..Caps::ALL
    });
    mock.script(Query::Sections, None, Ok(Container::default()));
    let (mut core, _dir) = connected(&mock);
    let command = |value: Value| serde_json::from_value::<Command>(value).unwrap();
    let browse = |filters: Value| {
        command(
            json!({"op":"filtered_browse","section":"1","kind":"track","sort":"title","filters":filters}),
        )
    };
    let before = mock.log().len();
    for refused in [
        browse(json!({"mood":"13"})),
        browse(json!({"style":"14"})),
        browse(json!({"genre":"12","minRating":6})),
        command(json!({"op":"filter_options","section":"1","kind":"track","field":"mood"})),
        command(json!({"op":"filter_options","section":"1","kind":"track","field":"style"})),
        command(
            json!({"op":"smart_playlist","action":"create","section":"1","title":"S",
                       "sort":"title","limit":50,"filters":{"mood":"13"}}),
        ),
    ] {
        let error = core.execute(refused).unwrap_err();
        assert_eq!(
            error.to_string(),
            "This server does not support this feature"
        );
    }
    assert_eq!(mock.log().len(), before, "no request was made");
    // The filters it supports still apply: a genre listing reads the schema,
    // then the listing.
    let schema = Query::FilterSchema {
        section: "1".into(),
        kind: ItemKind::Track,
    };
    let listing = Query::browse(
        "1",
        BrowseSpec {
            filter: Music {
                genre: "12".into(),
                ..Music::default()
            },
            ..BrowseSpec::new(ItemKind::Track, Sort::Title)
        },
    );
    mock.script(
        schema.clone(),
        Some(Page { start: 0, size: 0 }),
        Ok(Container::default()),
    );
    mock.script(listing.clone(), page(0), Ok(tracks_container(&["5"])));
    let value = core.execute(browse(json!({"genre":"12"}))).unwrap();
    assert_eq!(value["items"][0]["ratingKey"], "5", "{value}");
    assert_eq!(&fetches(&mock)[before..], [schema, listing]);
}

#[test]
fn advertised_keys_are_server_relative_on_a_base_path_server() {
    let mock = MockServer::default();
    let (mut core, _dir) = guid_core(&mock);
    let advertised = Query::Advertised {
        path: format!("/Items/{ALBUM}"),
        params: vec![("SortBy".into(), "DateCreated".into())],
    };
    mock.script(
        advertised.clone(),
        page(0),
        Ok(tracks_container(&[TRACK_A])),
    );
    let hub = core
        .execute(Command::HubItems {
            key: format!("/Items/{ALBUM}?SortBy=DateCreated"),
            start: 0,
        })
        .unwrap();
    assert_eq!(hub["items"][0]["ratingKey"], TRACK_A, "{hub}");
    assert_eq!(fetches(&mock)[1..], [advertised]);
    // This backend's credential parameter, keys outside the library and
    // absolute URLs are refused before any request.
    let before = mock.log().len();
    for key in [
        format!("/Items/{ALBUM}?api_key=other"),
        format!("/Items/{ALBUM}?ApiKey=other"),
        "/System/Info".into(),
        format!("http://127.0.0.1:8096/jellyfin/Items/{ALBUM}"),
        format!("/jellyfin/Items/{ALBUM}"),
        format!("/Items/../Items/{ALBUM}"),
    ] {
        assert!(
            core.execute(Command::HubItems {
                key: key.clone(),
                start: 0
            })
            .is_err(),
            "{key}"
        );
    }
    assert_eq!(mock.log().len(), before);
}

#[test]
fn connecting_forgets_the_previous_sign_ins_user() {
    let mock = MockServer::default();
    mock.script_ids(MockIds::Guid);
    mock.script(Query::Sections, None, Ok(Container::default()));
    let dir = tempfile::tempdir().unwrap();
    plexfreq_core::store::save(
        dir.path(),
        &plexfreq_core::store::Settings {
            server_url: BASE.into(),
            server_token: "old-synthetic".into(),
            server_user_id: "old-user".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let mut core = Core::with_backend(dir.path().into(), Backend::Mock(mock.clone())).unwrap();
    core.execute(Command::Connect {
        url: BASE.into(),
        token: "synthetic".into(),
    })
    .unwrap();
    let saved = plexfreq_core::store::load(dir.path()).unwrap();
    assert_eq!(saved.server_token, "synthetic");
    assert!(saved.server_user_id.is_empty());
}

#[test]
fn smart_rules_of_a_source_the_server_cannot_filter_are_not_editable() {
    // Smart playlists exist, but this server has no mood filter: the editor
    // could not show (and would drop) the playlist's mood rule.
    let mock = MockServer::default();
    mock.script_caps(Caps {
        mood_filter: false,
        ..Caps::ALL
    });
    mock.script(Query::Sections, None, Ok(Container::default()));
    let playlist = |content: &str| {
        let mut container = Container::default();
        container.items.push(plexfreq_core::model::Item {
            rating_key: "9".into(),
            kind: ItemKind::Playlist,
            smart: true,
            content: content.into(),
            ..Default::default()
        });
        container
    };
    let (mut core, _dir) = connected(&mock);
    let rules = |core: &mut Core| {
        core.execute(
            serde_json::from_value::<Command>(json!({"op":"smart_rules","key":"9"})).unwrap(),
        )
        .unwrap()
    };
    let read = Query::Playlist { id: "9".into() };
    mock.script(
        read.clone(),
        None,
        Ok(playlist(
            "server://m/com.plexapp.plugins.library/library/sections/3/all?type=10&mood=5&sort=titleSort%3Aasc&limit=50",
        )),
    );
    let value = rules(&mut core);
    assert_eq!(value["smartEditable"], false, "{value}");
    assert_eq!(value["smartFilters"]["mood"], "5");
    // The same server with a source it can filter is editable.
    let mock = MockServer::default();
    mock.script_caps(Caps {
        mood_filter: false,
        ..Caps::ALL
    });
    mock.script(Query::Sections, None, Ok(Container::default()));
    mock.script(
        read,
        None,
        Ok(playlist(
            "server://m/com.plexapp.plugins.library/library/sections/3/all?type=10&sort=titleSort%3Aasc&limit=50",
        )),
    );
    let (mut core, _dir) = connected(&mock);
    assert_eq!(rules(&mut core)["smartEditable"], true);
}

#[test]
fn a_smart_update_the_server_cannot_apply_is_refused_before_the_playlist_is_read() {
    let mock = MockServer::default();
    mock.script_caps(Caps {
        mood_filter: false,
        ..Caps::ALL
    });
    mock.script(Query::Sections, None, Ok(Container::default()));
    let (mut core, _dir) = connected(&mock);
    let before = mock.log().len();
    let error = core
        .execute(
            serde_json::from_value::<Command>(
                json!({"op":"smart_playlist","action":"update","key":"9","section":"1",
                       "sort":"title","limit":50,"filters":{"mood":"13"}}),
            )
            .unwrap(),
        )
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "This server does not support this feature"
    );
    assert_eq!(mock.log().len(), before, "the playlist was not read");
}

#[test]
fn filter_options_checks_the_listing_kind_before_anything_else() {
    let mock = MockServer::default();
    mock.script(Query::Sections, None, Ok(Container::default()));
    let (mut core, _dir) = connected(&mock);
    let before = mock.log().len();
    for kind in ["playlist", "photo", ""] {
        let error = core
            .execute(
                serde_json::from_value::<Command>(
                    json!({"op":"filter_options","section":"1","kind":kind,"field":"genre"}),
                )
                .unwrap(),
            )
            .unwrap_err();
        assert_eq!(error.to_string(), "Unknown music type", "{kind}");
    }
    assert_eq!(mock.log().len(), before, "no request was made");
}
