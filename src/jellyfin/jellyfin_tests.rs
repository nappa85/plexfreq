//! Jellyfin backend tests: pure helpers, then the backend against a local
//! fixture server (exact request lines, headers, bodies and conversions).
use super::jellyfin_fixture_tests::*;
use super::*;
use crate::{
    model::EntryId,
    server::{FilterField, Reporting, Sort},
};
use std::sync::{Arc, Mutex};

const FIELDS_ENCODED: &str =
    "MediaSources%2CGenres%2CDateCreated%2CChildCount%2CPrimaryImageAspectRatio";

fn jellyfin() -> Jellyfin {
    Jellyfin::new("fixture-client-id", SERVER)
}

fn conn(server: &Server) -> Conn {
    Conn {
        base: server_url_with_base_path(&server.url).unwrap(),
        token: TOKEN.into(),
        user_id: Some(USER.into()),
    }
}

fn script() -> Arc<Mutex<Script>> {
    Arc::new(Mutex::new(Script::default()))
}

fn start() -> (Server, Conn, Arc<Mutex<Script>>) {
    let script = script();
    let server = Server::start("", standard(script.clone()));
    let conn = conn(&server);
    (server, conn, script)
}

fn spec(kind: ItemKind, sort: Sort) -> BrowseSpec {
    BrowseSpec::new(kind, sort)
}

fn params(request: &Req) -> Vec<(String, String)> {
    request
        .target
        .split_once('?')
        .map(|(_, q)| {
            url::form_urlencoded::parse(q.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default()
}

fn value<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

// ---- pure helpers ----------------------------------------------------------

#[test]
fn the_authorization_header_is_ascii_percent_encoded_and_carries_the_token_only_after_sign_in() {
    let value = authorization("Wohnzimmer-ü \u{1}", "id+1", Some("tok+en,\"x%")).unwrap();
    assert_eq!(
        value.to_str().unwrap(),
        format!(
            "MediaBrowser Client=\"PlexFreq\", Device=\"Wohnzimmer-_ _\", DeviceId=\"id%2B1\", Version=\"{}\", Token=\"tok%2Ben%2C%22x%25\"",
            env!("CARGO_PKG_VERSION")
        )
    );
    assert!(value.is_sensitive());
    let anonymous = authorization("host", "id", None).unwrap();
    assert_eq!(
        anonymous.to_str().unwrap(),
        format!(
            "MediaBrowser Client=\"PlexFreq\", Device=\"host\", DeviceId=\"id\", Version=\"{}\"",
            env!("CARGO_PKG_VERSION")
        )
    );
    assert!(!anonymous.is_sensitive());
    assert!(device_name().is_ascii());
}

#[test]
fn ids_accept_dashed_guids_and_normalise_to_lowercase_without_dashes() {
    let plain = "0123456789abcdef0123456789abcdef";
    assert_eq!(normalize_id(plain).as_deref(), Some(plain));
    assert_eq!(
        normalize_id("01234567-89AB-cdef-0123-456789abcdef").as_deref(),
        Some(plain)
    );
    assert_eq!(
        normalize_id("0123456789ABCDEF0123456789ABCDEF").as_deref(),
        Some(plain)
    );
    for bad in [
        "",
        "12",
        "0123456789abcdef0123456789abcdeg",
        "0123456789abcdef0123456789abcdef0",
        "0123456-789ab-cdef-0123-456789abcdef0",
        "../0123456789abcdef0123456789abcdef",
        "012345678-9abc-def0-1234-56789abcdef",
    ] {
        assert!(normalize_id(bad).is_none(), "{bad}");
    }
    let server = jellyfin();
    assert!(server.is_id(plain));
    assert!(server.is_id("01234567-89ab-cdef-0123-456789abcdef"));
    assert!(!server.is_id("123"));
    assert!(server.is_library_key(&format!("/Items/{plain}")));
    assert!(!server.is_library_key("/library/metadata/1"));
    assert!(!server.is_library_key("/Items/xyz"));
}

#[test]
fn timestamps_and_year_ranges_are_expanded_exactly() {
    assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
    assert_eq!(iso8601(1_700_000_000), "2023-11-14T22:13:20Z");
    assert_eq!(iso8601(951_782_400), "2000-02-29T00:00:00Z");
    assert_eq!(years(None, None).unwrap(), None);
    assert_eq!(
        years(Some(1990), Some(1993)).unwrap().as_deref(),
        Some("1990,1991,1992,1993")
    );
    assert_eq!(
        years(Some(2001), Some(2001)).unwrap().as_deref(),
        Some("2001")
    );
    // An open start is bounded to 200 years, an open end reaches next year.
    let open_start = years(None, Some(2000)).unwrap().unwrap();
    assert!(open_start.starts_with("1801,") && open_start.ends_with(",2000"));
    assert_eq!(open_start.split(',').count(), 200);
    assert!(years(Some(2020), None)
        .unwrap()
        .unwrap()
        .starts_with("2020,2021"));
    assert!(years(Some(1000), Some(1200)).is_err());
    assert!(years(Some(1000), Some(1199)).is_ok());
}

#[test]
fn only_the_known_credential_parameters_are_refused_in_keys() {
    let server = jellyfin();
    for key in [
        "/Items/x?api_key=1",
        "/Items/x?ApiKey=1",
        "/Items/x?X-Emby-Token=1",
        "//evil.example/x",
        "/Items/../x",
        "http://evil.example/x",
    ] {
        assert!(server.advertised_key(key).is_err(), "{key}");
    }
    let (path, pairs) = server
        .advertised_key("/Items/x?SortBy=Name&Limit=5")
        .unwrap();
    assert_eq!(path, "/Items/x");
    assert_eq!(pairs.len(), 2);
}

#[test]
fn the_capabilities_are_jellyfins() {
    let caps = jellyfin().capabilities();
    assert_eq!(caps, CAPS);
    assert!(caps.server_hubs && !caps.sonic && !caps.sonic_adventure && !caps.smart_playlists);
    assert!(!caps.server_stations && !caps.first_character && !caps.star_rating);
    assert!(!caps.mood_filter && !caps.style_filter && !caps.rating_filter);
}

#[test]
fn the_namespace_is_the_server_and_the_user_not_the_address_or_the_token() {
    let base = |url: &str| Url::parse(url).unwrap();
    let conn = |url: &str, token: &str, user: Option<&str>| Conn {
        base: base(url),
        token: token.into(),
        user_id: user.map(Into::into),
    };
    let server = jellyfin();
    let first = server.namespace(&conn("http://192.0.2.1:8096/", "token-1", Some(USER)));
    assert_eq!(
        first,
        crate::sha256_hex(&format!("jellyfin\0{SERVER}\0{USER}"))
    );
    // Re-login (new token) and another address keep it.
    assert_eq!(
        first,
        server.namespace(&conn(
            "https://music.example/jellyfin/",
            "token-2",
            Some(USER)
        ))
    );
    // Another user or another server does not.
    assert_ne!(
        first,
        server.namespace(&conn("http://192.0.2.1:8096/", "token-1", Some("c")))
    );
    assert_ne!(
        first,
        Jellyfin::new("fixture-client-id", "ffffffffffffffffffffffffffffffff").namespace(&conn(
            "http://192.0.2.1:8096/",
            "token-1",
            Some(USER)
        ))
    );
}

#[test]
fn children_keys_become_kind_aware_queries() {
    let server = jellyfin();
    let key = format!("/Items/{}", album());
    assert_eq!(
        server.children_query(&key, Some(&ItemKind::Album)).unwrap(),
        Query::Children {
            id: album(),
            kind: ItemKind::Album
        }
    );
    assert_eq!(
        server
            .children_query(&format!("/Items/{}", playlist()), Some(&ItemKind::Playlist))
            .unwrap(),
        Query::PlaylistItems { id: playlist() }
    );
    // A dashed id in the key reaches the query normalised.
    let dashed = "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee";
    assert_eq!(
        server.children_query(dashed, None).unwrap(),
        Query::Children {
            id: album(),
            kind: ItemKind::default()
        }
    );
    assert!(server.children_query("/Items/nope", None).is_err());
    assert!(server.children_need_kind());
    assert!(server.filter_schema("x", &ItemKind::Track).is_none());
}

#[test]
fn the_instant_mix_read_uses_the_generic_item_route() {
    // Every seed kind uses `/Items/{id}/InstantMix`, the route mix plugins
    // (AudioMuse-AI) replace; the server tells the kind from the item.
    let server = jellyfin();
    for kind in [RadioKind::Artist, RadioKind::Album, RadioKind::Track] {
        assert_eq!(
            server.mix_query(&artist(), kind, 50),
            Some(Query::advertised(
                &format!("/Items/{}/InstantMix", artist()),
                vec![("limit".into(), "50".into())]
            ))
        );
    }
    assert!(server.mix_query("nope", RadioKind::Track, 50).is_none());
    // Only the generic route is an instant mix.
    for family in ["Artists", "Albums", "Songs"] {
        let query = Query::advertised(
            &format!("/{family}/{}/InstantMix", artist()),
            vec![("limit".into(), "50".into())],
        );
        assert!(request(&query, None).is_err());
    }
    // The limit is clamped; every field and image type is its own parameter
    // (the AudioMuse-AI action does not split comma lists) and images and
    // user data are asked for (it leaves them out otherwise).
    let query = Query::advertised(
        &format!("/Items/{}/InstantMix", artist()),
        vec![("limit".into(), "500".into())],
    );
    let mix = request(&query, None).unwrap();
    assert_eq!(mix.path, format!("/Items/{}/InstantMix", artist()));
    let mut expected: Vec<(String, String)> = vec![("Limit".into(), "200".into())];
    expected.extend(
        FIELDS
            .split(',')
            .map(|field| ("Fields".to_owned(), field.to_owned())),
    );
    expected.extend(
        [
            ("EnableImages", "true"),
            ("EnableUserData", "true"),
            ("EnableImageTypes", "Primary"),
            ("EnableImageTypes", "Backdrop"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned())),
    );
    assert_eq!(mix.params, expected);
    // The repeated keys reach the wire.
    let (server, conn, _) = start();
    jellyfin().fetch(&conn, &query, None).unwrap();
    let seen = server.take();
    let sent = seen
        .iter()
        .find(|r| r.path.ends_with("/InstantMix"))
        .unwrap();
    let query_text = sent.target.split_once('?').unwrap().1;
    let count = |name: &str| {
        url::form_urlencoded::parse(query_text.as_bytes())
            .filter(|(k, _)| k == name)
            .count()
    };
    assert_eq!(count("Fields"), FIELDS.split(',').count());
    assert_eq!(count("EnableImageTypes"), 2);
    assert!(request(&Query::advertised("/Genres/x/InstantMix", vec![]), None).is_err());
}

#[test]
fn the_sort_table_maps_every_ui_sort() {
    for (sort, by, order) in [
        (Sort::Title, "SortName", "Ascending"),
        (Sort::Added, "DateCreated", "Descending"),
        (
            Sort::Year,
            "ProductionYear,SortName",
            "Descending,Ascending",
        ),
        (Sort::LastPlayed, "DatePlayed", "Descending"),
        (Sort::PlayCount, "PlayCount", "Descending"),
        (Sort::Random, "Random", "Ascending"),
    ] {
        let request = browse_request(&library(), &spec(ItemKind::Album, sort), false).unwrap();
        let pairs = &request.params;
        assert_eq!(value(pairs, "SortBy"), Some(by));
        assert_eq!(value(pairs, "SortOrder"), Some(order));
    }
    for sort in Sort::ALL {
        assert!(browse_request(&library(), &spec(ItemKind::Track, sort), false).is_ok());
    }
}

#[test]
fn a_listing_with_two_title_conditions_or_an_unlistable_kind_is_refused() {
    let mut both = spec(ItemKind::Track, Sort::Title);
    both.search = Some("a".into());
    both.filter.title = "b".into();
    assert!(browse_request(&library(), &both, false).is_err());
    assert!(browse_request(&library(), &spec(ItemKind::Playlist, Sort::Title), false).is_err());
    assert!(browse_request(
        &library(),
        &spec(ItemKind::Other("x".into()), Sort::Title),
        false
    )
    .is_err());
}

// ---- reads over HTTP -------------------------------------------------------

#[test]
fn sections_are_the_music_views() {
    let (server, conn, _) = start();
    let c = jellyfin().fetch(&conn, &Query::Sections, None).unwrap();
    assert_eq!(c.sections.len(), 1);
    assert_eq!(c.sections[0].key, library());
    assert_eq!(c.sections[0].title, "Music");
    assert_eq!(c.sections[0].kind, ItemKind::Artist);
    assert_eq!(server.lines(), [format!("GET /UserViews?userId={USER}")]);
}

#[test]
fn browse_requests_carry_paging_sorting_and_filters() {
    let (server, conn, _) = start();
    let backend = jellyfin();
    let page = Some(Page {
        start: 100,
        size: 100,
    });
    backend
        .fetch(
            &conn,
            &Query::browse(&library(), spec(ItemKind::Album, Sort::Title)),
            page,
        )
        .unwrap();
    assert_eq!(
        server.lines(),
        [format!(
            "GET /Items?ParentId={}&IncludeItemTypes=MusicAlbum&Recursive=true&SortBy=SortName&SortOrder=Ascending&Fields={FIELDS_ENCODED}&StartIndex=100&Limit=100&userId={USER}",
            library()
        )]
    );
    server.take();
    // Artists come from the album-artist endpoint.
    backend
        .fetch(
            &conn,
            &Query::browse(&library(), spec(ItemKind::Artist, Sort::Added)),
            Some(Page {
                start: 0,
                size: 100,
            }),
        )
        .unwrap();
    assert_eq!(
        server.lines(),
        [format!(
            "GET /Artists/AlbumArtists?ParentId={}&SortBy=DateCreated&SortOrder=Descending&Fields={FIELDS_ENCODED}&StartIndex=0&Limit=100&userId={USER}",
            library()
        )]
    );
    server.take();
    // Every filter at once.
    let mut filtered = spec(ItemKind::Track, Sort::PlayCount);
    filtered.favourite = true;
    filtered.played = true;
    filtered.filter.unplayed = true;
    filtered.filter.genre = id('7');
    filtered.filter.year_from = Some(1990);
    filtered.filter.year_to = Some(1991);
    filtered.filter.title = " rock ".into();
    backend
        .fetch(&conn, &Query::browse(&library(), filtered), page)
        .unwrap();
    let seen = server.take();
    let pairs = params(&seen[0]);
    assert_eq!(value(&pairs, "IncludeItemTypes"), Some("Audio"));
    assert_eq!(
        value(&pairs, "Filters"),
        Some("IsFavorite,IsPlayed,IsUnplayed")
    );
    assert_eq!(value(&pairs, "GenreIds"), Some(id('7').as_str()));
    assert_eq!(value(&pairs, "Years"), Some("1990,1991"));
    assert_eq!(value(&pairs, "SearchTerm"), Some("rock"));
    assert_eq!(value(&pairs, "SortBy"), Some("PlayCount"));
    // The browse search box is the same condition.
    let mut searched = spec(ItemKind::Album, Sort::Title);
    searched.search = Some("zeppelin".into());
    searched.limit = Some(30);
    backend
        .fetch(&conn, &Query::browse(&library(), searched), page)
        .unwrap();
    let pairs = params(&server.take()[0]);
    assert_eq!(value(&pairs, "SearchTerm"), Some("zeppelin"));
    // The smaller of the page size and the spec's limit.
    assert_eq!(value(&pairs, "Limit"), Some("30"));
    assert_eq!(value(&pairs, "StartIndex"), Some("100"));
}

#[test]
fn artist_listing_falls_back_to_items_when_the_endpoint_is_missing() {
    let script = script();
    let inner = standard(script);
    let server = Server::start("", move |req: &Req| {
        if req.path == "/Artists/AlbumArtists" {
            Reply::status(404)
        } else if req.path == "/Items"
            && req.param("IncludeItemTypes").as_deref() == Some("MusicArtist")
        {
            Reply::json(items(vec![
                json!({"Id": artist(), "Name": "Artist", "Type": "MusicArtist"}),
            ]))
        } else {
            inner(req)
        }
    });
    let conn = conn(&server);
    let c = jellyfin()
        .fetch(
            &conn,
            &Query::browse(&library(), spec(ItemKind::Artist, Sort::Title)),
            Some(Page {
                start: 0,
                size: 100,
            }),
        )
        .unwrap();
    assert_eq!(c.items.len(), 1);
    let lines = server.lines();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("GET /Artists/AlbumArtists?"));
    assert!(lines[1].starts_with("GET /Items?ParentId="));
    assert!(lines[1].contains("IncludeItemTypes=MusicArtist"));
    // Other failures are not retried.
    let failing = Server::start("", |_| Reply::status(500));
    let conn = Conn {
        base: server_url_with_base_path(&failing.url).unwrap(),
        ..conn
    };
    assert!(matches!(
        jellyfin().fetch(
            &conn,
            &Query::browse(&library(), spec(ItemKind::Artist, Sort::Title)),
            None
        ),
        Err(Error::Http(500))
    ));
    assert_eq!(failing.count(), 1);
}

#[test]
fn children_of_artists_albums_and_playlists_are_requested_by_kind() {
    let (server, conn, _) = start();
    let backend = jellyfin();
    let page = Some(Page {
        start: 0,
        size: 100,
    });
    for (query, expected) in [
        (
            Query::Children { id: album(), kind: ItemKind::Album },
            format!(
                "GET /Items?ParentId={}&IncludeItemTypes=Audio&Recursive=true&SortBy=ParentIndexNumber%2CIndexNumber%2CSortName&Fields={FIELDS_ENCODED}&StartIndex=0&Limit=100&userId={USER}",
                album()
            ),
        ),
        (
            Query::Children { id: artist(), kind: ItemKind::Artist },
            format!(
                "GET /Items?AlbumArtistIds={}&IncludeItemTypes=MusicAlbum&Recursive=true&SortBy=ProductionYear%2CSortName&SortOrder=Descending%2CAscending&Fields={FIELDS_ENCODED}&StartIndex=0&Limit=100&userId={USER}",
                artist()
            ),
        ),
        (
            Query::Children { id: playlist(), kind: ItemKind::Playlist },
            format!(
                "GET /Playlists/{}/Items?Fields={FIELDS_ENCODED}&StartIndex=0&Limit=100&userId={USER}",
                playlist()
            ),
        ),
        (
            Query::PlaylistItems { id: playlist() },
            format!(
                "GET /Playlists/{}/Items?Fields={FIELDS_ENCODED}&StartIndex=0&Limit=100&userId={USER}",
                playlist()
            ),
        ),
    ] {
        backend.fetch(&conn, &query, page).unwrap();
        assert_eq!(server.take().iter().map(Req::line).collect::<Vec<_>>(), [expected]);
    }
    // Without a kind the item is read first.
    let c = backend
        .fetch(
            &conn,
            &Query::Children {
                id: album(),
                kind: ItemKind::default(),
            },
            page,
        )
        .unwrap();
    assert_eq!(c.items.len(), 3);
    let lines = server.lines();
    assert!(lines[0].starts_with(&format!("GET /Items?Ids={}", album())));
    assert!(lines[1].starts_with(&format!("GET /Items?ParentId={}", album())));
}

#[test]
fn artist_leaves_similar_search_playlists_and_items() {
    let (server, conn, _) = start();
    let backend = jellyfin();
    for (query, page, expected) in [
        (
            Query::AllLeaves { id: artist() },
            Some(Page { start: 100, size: 100 }),
            format!(
                "GET /Items?ArtistIds={}&IncludeItemTypes=Audio&Recursive=true&SortBy=Album%2CParentIndexNumber%2CIndexNumber%2CSortName&Fields={FIELDS_ENCODED}&StartIndex=100&Limit=100&userId={USER}",
                artist()
            ),
        ),
        (
            Query::Similar { id: artist(), count: 12 },
            None,
            format!(
                "GET /Items/{}/Similar?Limit=12&Fields={FIELDS_ENCODED}&userId={USER}",
                artist()
            ),
        ),
        (
            Query::Playlists,
            Some(Page { start: 0, size: 100 }),
            format!(
                "GET /Items?IncludeItemTypes=Playlist&Recursive=true&SortBy=SortName&Fields={FIELDS_ENCODED}&StartIndex=0&Limit=100&userId={USER}"
            ),
        ),
        (
            Query::Item { id: track_id(1), stations: true },
            None,
            format!(
                "GET /Items?Ids={}&Fields={FIELDS_ENCODED}%2COverview&userId={USER}",
                track_id(1)
            ),
        ),
        (
            Query::Playlist { id: playlist() },
            None,
            format!(
                "GET /Items?Ids={}&Fields={FIELDS_ENCODED}%2COverview&userId={USER}",
                playlist()
            ),
        ),
    ] {
        backend.fetch(&conn, &query, page).unwrap();
        assert_eq!(server.take().iter().map(Req::line).collect::<Vec<_>>(), [expected], "{query:?}");
    }
    // Only audio playlists are listed.
    let playlists = backend.fetch(&conn, &Query::Playlists, None).unwrap();
    assert_eq!(playlists.items.len(), 1);
    assert_eq!(playlists.items[0].kind, ItemKind::Playlist);
    assert_eq!(playlists.items[0].playlist_type, "audio");
}

#[test]
fn queries_without_a_jellyfin_equivalent_are_refused_before_any_request() {
    let (server, conn, _) = start();
    let backend = jellyfin();
    for query in [
        Query::Nearest {
            id: artist(),
            limit: 50,
        },
        Query::FilterSchema {
            section: library(),
            kind: ItemKind::Track,
        },
        Query::FirstCharacter { section: library() },
        Query::ComputePath {
            section: library(),
            start: id('1'),
            end: id('2'),
        },
        Query::PlayQueueWindow { id: 1, center: 2 },
        Query::advertised("/System/Info", vec![]),
        Query::advertised("/hubs/jellyfin/nope/recent-added", vec![]),
        Query::Children {
            id: album(),
            kind: ItemKind::Track,
        },
        Query::Item {
            id: "7".into(),
            stations: false,
        },
    ] {
        assert!(backend.fetch(&conn, &query, None).is_err(), "{query:?}");
    }
    assert_eq!(server.count(), 0);
}

#[test]
fn hubs_are_synthesised_and_their_keys_page_through_advertised_reads() {
    let (server, conn, _) = start();
    let backend = jellyfin();
    let c = backend
        .fetch(&conn, &Query::Hubs { section: library() }, None)
        .unwrap();
    let identifiers: Vec<_> = c.hubs.iter().map(|h| h.hub_identifier.clone()).collect();
    assert_eq!(identifiers[0], format!("music.recent.added.{}", library()));
    assert_eq!(identifiers[1], format!("music.recent.played.{}", library()));
    assert!(c.hubs.iter().all(|h| h.more && !h.items.is_empty()));
    assert_eq!(c.hubs[0].items[0].kind, ItemKind::Album);
    // `/Items/Latest` answers a bare array.
    let lines = server.lines();
    assert!(
        lines[0].starts_with("GET /Items/Latest?ParentId="),
        "{lines:?}"
    );
    // Each hub's key is an advertised read of the full listing.
    let key = &c.hubs[1].key;
    assert_eq!(key, &format!("/hubs/jellyfin/{}/recent-played", library()));
    let (path, pairs) = backend.advertised_key(key).unwrap();
    assert!(path.starts_with("/hubs/") && pairs.is_empty());
    server.take();
    backend
        .fetch(
            &conn,
            &Query::advertised(&path, pairs),
            Some(Page {
                start: 100,
                size: 100,
            }),
        )
        .unwrap();
    let request = &server.take()[0];
    let pairs = params(request);
    assert_eq!(value(&pairs, "SortBy"), Some("DatePlayed"));
    assert_eq!(value(&pairs, "Filters"), Some("IsPlayed"));
    assert_eq!(value(&pairs, "StartIndex"), Some("100"));
    assert_eq!(value(&pairs, "Limit"), Some("100"));
}

#[test]
fn genre_choices_come_from_the_library_and_only_genre_has_any() {
    let (server, conn, _) = start();
    let backend = jellyfin();
    let query = backend
        .filter_choices(&library(), &ItemKind::Track, FilterField::Genre, None)
        .unwrap()
        .unwrap();
    for field in [FilterField::Mood, FilterField::Style, FilterField::Year] {
        assert!(backend
            .filter_choices(&library(), &ItemKind::Track, field, None)
            .unwrap()
            .is_none());
    }
    let c = backend.fetch(&conn, &query, None).unwrap();
    // The malformed id is skipped.
    assert_eq!(c.sections.len(), 1);
    assert_eq!(c.sections[0].key, id('7'));
    assert_eq!(c.sections[0].title, "Rock");
    assert_eq!(
        server.lines(),
        [format!(
            "GET /Genres?ParentId={}&SortBy=SortName&userId={USER}",
            library()
        )]
    );
}

#[test]
fn every_request_carries_the_authorization_header_and_never_the_token_in_a_url() {
    let (server, conn, _) = start();
    let backend = jellyfin();
    backend.fetch(&conn, &Query::Sections, None).unwrap();
    backend
        .fetch(
            &conn,
            &Query::Item {
                id: track_id(1),
                stations: false,
            },
            None,
        )
        .unwrap();
    backend
        .act(
            &conn,
            &Action::Rate {
                id: track_id(1),
                rating: 10,
            },
        )
        .unwrap();
    backend
        .lyrics(&conn, &format!("/Audio/{}/Lyrics", track_id(1)))
        .unwrap();
    let seen = server.take();
    assert_eq!(seen.len(), 4);
    for request in seen {
        let header = request.header("authorization").unwrap();
        assert!(
            header.starts_with("MediaBrowser Client=\"PlexFreq\", Device=\""),
            "{header}"
        );
        assert!(header.contains("DeviceId=\"fixture-client-id\""));
        assert!(header.ends_with(&format!("Token=\"{TOKEN}\"")), "{header}");
        assert_eq!(request.header("accept"), Some("application/json"));
        assert!(!request.target.contains(TOKEN), "{}", request.target);
        assert!(!request.target.to_ascii_lowercase().contains("api_key"));
    }
}

#[test]
fn offline_keys_are_stable_and_need_no_request() {
    let (server, _conn, _) = start();
    let backend = jellyfin();
    let query = Query::browse(&library(), spec(ItemKind::Album, Sort::Title));
    let page = Some(Page {
        start: 0,
        size: 100,
    });
    let (path, params) = backend.offline_key(&query, page).unwrap();
    assert_eq!(path, "/Items");
    assert_eq!(value(&params, "StartIndex"), Some("0"));
    assert_eq!(value(&params, "Limit"), Some("100"));
    assert_eq!(backend.offline_key(&query, page).unwrap(), (path, params));
    let (artists, _) = backend
        .offline_key(
            &Query::browse(&library(), spec(ItemKind::Artist, Sort::Title)),
            None,
        )
        .unwrap();
    assert_eq!(artists, "/Artists/AlbumArtists");
    assert!(backend
        .offline_key(
            &Query::Nearest {
                id: artist(),
                limit: 5
            },
            None
        )
        .is_err());
    assert_eq!(server.count(), 0);
}

// ---- conversions -----------------------------------------------------------

#[test]
fn a_track_converts_ticks_images_gains_favourite_lyrics_and_media() {
    let (_server, conn, _) = start();
    let c = jellyfin()
        .fetch(
            &conn,
            &Query::Children {
                id: album(),
                kind: ItemKind::Album,
            },
            Some(Page {
                start: 0,
                size: 100,
            }),
        )
        .unwrap();
    assert_eq!(c.size, 3);
    assert_eq!(c.total_size, Some(3));
    let one = &c.items[0];
    assert_eq!(one.rating_key, track_id(1));
    assert_eq!(one.key, format!("/Items/{}", track_id(1)));
    assert_eq!(one.kind, ItemKind::Track);
    assert_eq!(one.title, "Track 1");
    // 2.4e9 ticks of 100 ns: 240 s.
    assert_eq!(one.duration, 240_000);
    assert_eq!((one.index, one.parent_index, one.year), (1, 1, 1999));
    assert_eq!(one.parent_title, "Album");
    assert_eq!(one.parent_rating_key, album());
    assert_eq!(one.grandparent_title, "Artist");
    assert_eq!(one.grandparent_rating_key, artist());
    // No art of its own: the album's, at the fixed size and quality.
    assert_eq!(one.thumb, "");
    assert_eq!(
        one.parent_thumb,
        format!(
            "/Items/{}/Images/Primary?tag=albumtag&maxWidth=1000&quality=90",
            album()
        )
    );
    assert_eq!(one.user_rating, 0.0);
    assert_eq!(c.items[1].user_rating, 10.0);
    let media = &one.media[0];
    assert_eq!(
        (media.container.as_str(), media.audio_codec.as_str()),
        ("flac", "flac")
    );
    assert_eq!(media.bitrate, 900);
    let part = &media.parts[0];
    assert_eq!(
        part.key,
        format!("/Audio/{}/stream?static=true", track_id(1))
    );
    assert_eq!(part.size, Some(27_000_000));
    assert_eq!(part.streams[0].stream_type, 2);
    assert_eq!(part.streams[0].gain, Some(-3.5));
    assert_eq!(part.streams[0].album_gain, None);
    // Lyrics only where the server says so.
    assert_eq!(part.streams.len(), 2);
    assert_eq!(part.streams[1].stream_type, 4);
    assert_eq!(
        part.streams[1].key,
        format!("/Audio/{}/Lyrics", track_id(1))
    );
    assert_eq!(c.items[1].media[0].parts[0].streams.len(), 1);
}

#[test]
fn album_gain_images_and_artists_convert() {
    let body = json!({"Items": [
        {"Id": track_id(1), "Name": "T", "Type": "Audio", "NormalizationGain": -1.0,
         "AlbumNormalizationGain": -2.0, "ImageTags": {"Primary": "own tag"},
         "AlbumId": "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee", "AlbumPrimaryImageTag": "al",
         "Artists": ["Feat"], "AlbumArtist": "Main",
         "MediaSources": [{"Container": "mov,mp4,m4a,3gp,3g2,mj2", "Bitrate": null,
           "MediaStreams": [{"Type": "Video", "Codec": "mjpeg"}, {"Type": "Audio", "Codec": "AAC"}]}]},
        {"Id": album(), "Name": "A", "Type": "MusicAlbum", "ChildCount": 12,
         "AlbumArtist": "Main", "AlbumArtists": [{"Id": artist(), "Name": "Main"}],
         "BackdropImageTags": ["bd"]},
        {"Id": artist(), "Name": "Main", "Type": "MusicArtist"},
        {"Id": "broken"},
        {"Id": id('5'), "Name": null, "Type": "Photo", "RunTimeTicks": null}
    ], "TotalRecordCount": 9, "StartIndex": 5});
    let c = dto::container(body.to_string().as_bytes()).unwrap();
    // A malformed id is skipped but still counts in the page size.
    assert_eq!((c.size, c.total_size, c.offset), (5, Some(9), 5));
    assert_eq!(c.items.len(), 4);
    let track = &c.items[0];
    assert_eq!(
        track.thumb,
        format!(
            "/Items/{}/Images/Primary?tag=own+tag&maxWidth=1000&quality=90",
            track_id(1)
        )
    );
    assert_eq!(track.parent_rating_key, album());
    assert_eq!(track.original_title, "Feat");
    assert_eq!(track.grandparent_title, "Main");
    let media = &track.media[0];
    assert_eq!(media.container, "mp4");
    assert_eq!(media.audio_codec, "aac");
    assert_eq!(media.bitrate, 0);
    let gains = &media.parts[0].streams[0];
    assert_eq!((gains.gain, gains.album_gain), (Some(-1.0), Some(-2.0)));
    let album_item = &c.items[1];
    assert_eq!(album_item.leaf_count, Some(12));
    assert_eq!(album_item.parent_title, "Main");
    assert_eq!(album_item.parent_rating_key, artist());
    assert!(album_item.art.contains("/Images/Backdrop/0?tag=bd"));
    assert_eq!(c.items[3].kind, ItemKind::Other("Photo".into()));
    assert_eq!(c.items[3].title, "");
}

#[test]
fn playlist_entries_keep_their_server_entry_id() {
    let (_server, conn, _) = start();
    let c = jellyfin()
        .fetch(
            &conn,
            &Query::PlaylistItems { id: playlist() },
            Some(Page {
                start: 1,
                size: 100,
            }),
        )
        .unwrap();
    assert_eq!(c.items.len(), 2);
    assert_eq!(c.items[0].rating_key, track_id(2));
    assert_eq!(
        c.items[0].playlist_item_id,
        Some(EntryId::from(id('2').as_str()))
    );
    assert_eq!(c.total_size, Some(3));
    assert_eq!(c.offset, 1);
}

#[test]
fn views_keep_only_music_libraries_and_lyrics_convert_ticks_to_milliseconds() {
    let views = dto::views(
        json!({"Items": [
            {"Id": library(), "Name": "Music", "CollectionType": "music"},
            {"Id": id('9'), "Name": "Films", "CollectionType": "movies"},
            {"Id": id('8'), "Name": "Playlists", "CollectionType": "playlists"},
            {"Id": id('7'), "Name": "Mixed"}
        ]})
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    assert_eq!(views.sections.len(), 1);
    let lines = dto::lyrics(
        json!({"Metadata": {"Offset": 5_000_000}, "Lyrics": [
            {"Text": "one", "Start": 12_500_000u64},
            {"Text": "two", "Start": 30_000_000u64},
            {"Text": "unsynced"},
            {"Text": null, "Start": null}
        ]})
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    assert_eq!(
        lines
            .iter()
            .map(|l| (l.time, l.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (Some(1250), "one"),
            (Some(3000), "two"),
            (None, "unsynced"),
            (None, "")
        ]
    );
    assert!(dto::lyrics(b"{}").is_err());
    assert!(dto::lyrics(b"nope").is_err());
}

#[test]
fn lyrics_are_read_by_key_and_a_missing_document_is_none() {
    let (server, conn, script) = start();
    let backend = jellyfin();
    let key = format!("/Audio/{}/Lyrics", track_id(1));
    // 404: the item has none.
    assert!(backend.lyrics(&conn, &key).unwrap().is_empty());
    script.lock().unwrap().lyrics = Some(json!({"Lyrics": [{"Text": "la", "Start": 10_000_000}]}));
    let lines = backend.lyrics(&conn, &key).unwrap();
    assert_eq!((lines[0].time, lines[0].text.as_str()), (Some(1000), "la"));
    assert_eq!(
        server.lines()[0],
        format!("GET /Audio/{}/Lyrics?userId={USER}", track_id(1))
    );
    for bad in ["/Audio/zz/Lyrics", "/library/streams/1", "/Audio/../Lyrics"] {
        assert!(backend.lyrics(&conn, bad).is_err(), "{bad}");
    }
}

// ---- actions ---------------------------------------------------------------

fn only(server: &Server) -> Req {
    let mut seen = server.take();
    assert_eq!(
        seen.len(),
        1,
        "{:?}",
        seen.iter().map(Req::line).collect::<Vec<_>>()
    );
    seen.remove(0)
}

#[test]
fn favourites_are_the_flag_and_scrobbles_carry_the_play_date() {
    let (server, conn, _) = start();
    let backend = jellyfin();
    backend
        .act(
            &conn,
            &Action::Rate {
                id: track_id(1),
                rating: 10,
            },
        )
        .unwrap();
    assert_eq!(
        only(&server).line(),
        format!("POST /UserFavoriteItems/{}?userId={USER}", track_id(1))
    );
    backend
        .act(
            &conn,
            &Action::Rate {
                id: track_id(1),
                rating: 0,
            },
        )
        .unwrap();
    assert_eq!(
        only(&server).line(),
        format!("DELETE /UserFavoriteItems/{}?userId={USER}", track_id(1))
    );
    backend
        .act(
            &conn,
            &Action::Scrobble {
                id: track_id(2),
                played_at: 1_700_000_000,
            },
        )
        .unwrap();
    assert_eq!(
        only(&server).line(),
        format!(
            "POST /UserPlayedItems/{}?datePlayed=2023-11-14T22%3A13%3A20Z&userId={USER}",
            track_id(2)
        )
    );
    assert!(backend
        .act(
            &conn,
            &Action::Rate {
                id: "7".into(),
                rating: 10
            }
        )
        .is_err());
    assert_eq!(server.count(), 0);
}

fn timeline(state: &str, start: bool, transcode: bool) -> Action {
    Action::Timeline {
        id: track_id(1),
        state: state.into(),
        position: 12_345,
        duration: 240_000,
        continuing: false,
        queue: Some((9, 9)),
        reporting: Reporting {
            occurrence: "occurrence-1".into(),
            play_session: "session-1".into(),
            start,
            transcode,
        },
    }
}

#[test]
fn the_timeline_maps_to_start_progress_and_stopped_reports() {
    let (server, conn, _) = start();
    let backend = jellyfin();
    backend
        .act(&conn, &timeline("playing", true, false))
        .unwrap();
    let start = only(&server);
    assert_eq!(
        start.line(),
        format!("POST /Sessions/Playing?userId={USER}")
    );
    let body = start.json();
    assert_eq!(body["ItemId"], track_id(1));
    assert_eq!(body["MediaSourceId"], track_id(1));
    assert_eq!(body["PlaySessionId"], "session-1");
    assert_eq!(body["PositionTicks"], 123_450_000u64);
    assert_eq!(body["PlayMethod"], "DirectPlay");
    assert_eq!(body["CanSeek"], true);
    assert_eq!(body["IsPaused"], false);
    assert!(start
        .header("content-type")
        .unwrap()
        .starts_with("application/json"));
    // Not a start: progress; paused is reported as such; a transcode says so.
    backend
        .act(&conn, &timeline("playing", false, true))
        .unwrap();
    let progress = only(&server);
    assert!(progress.path == "/Sessions/Playing/Progress");
    assert_eq!(progress.json()["PlayMethod"], "Transcode");
    assert_eq!(progress.json()["IsPaused"], false);
    backend
        .act(&conn, &timeline("paused", false, false))
        .unwrap();
    assert_eq!(only(&server).json()["IsPaused"], true);
    // A start flag on a paused report is only progress.
    backend
        .act(&conn, &timeline("paused", true, false))
        .unwrap();
    assert_eq!(only(&server).path, "/Sessions/Playing/Progress");
    backend
        .act(&conn, &timeline("stopped", false, false))
        .unwrap();
    let stopped = only(&server);
    assert_eq!(stopped.path, "/Sessions/Playing/Stopped");
    assert_eq!(stopped.json()["PositionTicks"], 123_450_000u64);
    // The Plex play-queue pair is not sent anywhere.
    assert!(!String::from_utf8_lossy(&stopped.body).contains("playQueue"));
}

#[test]
fn playlists_are_created_extended_trimmed_renamed_and_deleted() {
    let (server, conn, _) = start();
    let backend = jellyfin();
    let created = backend
        .create_playlist(
            &conn,
            "Mine",
            &PlaylistContent::Tracks(vec![track_id(1), track_id(2)]),
        )
        .unwrap();
    let request = only(&server);
    assert_eq!(request.line(), format!("POST /Playlists?userId={USER}"));
    assert_eq!(
        request.json(),
        json!({"Name": "Mine", "Ids": [track_id(1), track_id(2)], "UserId": USER, "MediaType": "Audio"})
    );
    assert_eq!(created.items[0].rating_key, playlist());
    assert_eq!(created.items[0].kind, ItemKind::Playlist);
    assert_eq!(created.items[0].title, "Mine");
    // 250 tracks are added in requests of 100.
    let ids: Vec<String> = (0..250).map(|n| format!("{n:032x}")).collect();
    backend
        .act(
            &conn,
            &Action::PlaylistAdd {
                id: playlist(),
                content: PlaylistContent::Tracks(ids.clone()),
            },
        )
        .unwrap();
    let seen = server.take();
    assert_eq!(seen.len(), 3);
    let sizes: Vec<_> = seen
        .iter()
        .map(|r| r.param("ids").unwrap().split(',').count())
        .collect();
    assert_eq!(sizes, [100, 100, 50]);
    assert!(seen[0]
        .line()
        .starts_with(&format!("POST /Playlists/{}/Items?ids=", playlist())));
    backend
        .act(
            &conn,
            &Action::PlaylistRemove {
                id: playlist(),
                item: id('2'),
            },
        )
        .unwrap();
    // The entry is read first (a shared id would be ambiguous), then removed.
    let seen = server.take();
    assert_eq!(
        seen.len(),
        2,
        "{:?}",
        seen.iter().map(Req::line).collect::<Vec<_>>()
    );
    assert_eq!(seen[0].method, "GET");
    assert_eq!(
        seen[1].line(),
        format!(
            "DELETE /Playlists/{}/Items?entryIds={}&userId={USER}",
            playlist(),
            id('2')
        )
    );
    backend
        .act(
            &conn,
            &Action::PlaylistRename {
                id: playlist(),
                title: "New".into(),
            },
        )
        .unwrap();
    let rename = only(&server);
    assert_eq!(
        rename.line(),
        format!("POST /Playlists/{}?userId={USER}", playlist())
    );
    assert_eq!(rename.json(), json!({"Name": "New"}));
    backend
        .act(&conn, &Action::PlaylistDelete { id: playlist() })
        .unwrap();
    assert_eq!(
        only(&server).line(),
        format!("DELETE /Items/{}?userId={USER}", playlist())
    );
    // Smart playlists and stations do not exist; entry ids are checked.
    assert!(backend
        .create_playlist(
            &conn,
            "S",
            &PlaylistContent::Smart {
                section: library(),
                spec: spec(ItemKind::Track, Sort::Title)
            }
        )
        .is_err());
    assert!(backend.create_radio(&conn, "/x").is_err());
    assert!(backend
        .act(
            &conn,
            &Action::PlaylistRemove {
                id: playlist(),
                item: "a&b".into()
            }
        )
        .is_err());
    assert_eq!(server.count(), 0);
}

#[test]
fn duplicated_or_missing_entries_are_refused_before_any_write() {
    let (server, conn, script) = start();
    let backend = jellyfin();
    script.lock().unwrap().playlist = vec![(id('1'), 1), (id('2'), 2), (id('1'), 1), (id('3'), 3)];
    let move_of = |item: String, after: Option<String>| Action::PlaylistMove {
        id: playlist(),
        item,
        after,
        index: None,
    };
    let remove = Action::PlaylistRemove {
        id: playlist(),
        item: id('1'),
    };
    let indexed = |item: String, index: usize| Action::PlaylistMove {
        id: playlist(),
        item,
        after: None,
        index: Some(index),
    };
    const DUPLICATE: &str =
        "This track is in the playlist more than once and the server cannot tell the copies apart";
    const MISSING: &str = "The playlist entry was not found";
    // Entry duplicated (by `after` and by `index`), `after` duplicated, entry
    // or `after` missing, remove duplicated or missing.
    let cases = [
        (move_of(id('1'), Some(id('3'))), DUPLICATE),
        (move_of(id('3'), Some(id('1'))), DUPLICATE),
        (indexed(id('1'), 3), DUPLICATE),
        (move_of(id('7'), None), MISSING),
        (indexed(id('7'), 0), MISSING),
        (move_of(id('3'), Some(id('7'))), MISSING),
        (remove, DUPLICATE),
        (
            Action::PlaylistRemove {
                id: playlist(),
                item: id('7'),
            },
            MISSING,
        ),
    ];
    for (action, message) in cases {
        let error = backend.act(&conn, &action).unwrap_err();
        assert!(
            matches!(&error, Error::Input(m) if *m == message),
            "{error}"
        );
    }
    let requests = server.take();
    assert!(!requests.is_empty());
    assert!(requests.iter().all(|r| r.method == "GET"));
    assert_eq!(entries(&script), [id('1'), id('2'), id('1'), id('3')]);
}

#[test]
fn a_malformed_entry_on_a_page_keeps_its_place_in_the_read() {
    let server = Server::start("", |req| {
        let start: usize = req
            .param("StartIndex")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let all: Vec<Value> = (0..=ENTRY_PAGE)
            .map(|n| {
                if n == 0 {
                    json!({"Id": 5, "PlaylistItemId": format!("{:032x}", n)})
                } else {
                    json!({"Id": track_id(1), "Name": "t", "Type": "Audio",
                           "PlaylistItemId": format!("{n:032x}")})
                }
            })
            .collect();
        let limit: usize = req
            .param("Limit")
            .and_then(|s| s.parse().ok())
            .unwrap_or(usize::MAX);
        let rows: Vec<Value> = all.iter().skip(start).take(limit).cloned().collect();
        Reply::json(json!({"Items": rows, "TotalRecordCount": all.len(), "StartIndex": start}))
    });
    let conn = conn(&server);
    let entries = jellyfin().playlist_entries(&conn, &playlist()).unwrap();
    assert_eq!(entries.len(), ENTRY_PAGE + 1);
    assert_eq!(entries[0], format!("{:032x}", 0));
    assert_eq!(entries[ENTRY_PAGE], format!("{ENTRY_PAGE:032x}"));
}

fn entries(script: &Arc<Mutex<Script>>) -> Vec<String> {
    script
        .lock()
        .unwrap()
        .playlist
        .iter()
        .map(|(e, _)| e.clone())
        .collect()
}

#[test]
fn a_move_after_an_entry_derives_the_index_and_is_confirmed_by_order() {
    let (server, conn, script) = start();
    let backend = jellyfin();
    let moving = |item: String, after: Option<String>| Action::PlaylistMove {
        id: playlist(),
        item,
        after,
        index: None,
    };
    // [1,2,3]: entry 1 after entry 3 goes to index 2 (its own slot counted out).
    backend.act(&conn, &moving(id('1'), Some(id('3')))).unwrap();
    assert_eq!(entries(&script), [id('2'), id('3'), id('1')]);
    let seen = server.take();
    let post = seen.iter().find(|r| r.method == "POST").unwrap();
    assert_eq!(
        post.line(),
        format!(
            "POST /Playlists/{}/Items/{}/Move/2?userId={USER}",
            playlist(),
            id('1')
        )
    );
    // Read before and after the write, every page whole.
    assert_eq!(seen.iter().filter(|r| r.method == "GET").count(), 2);
    assert!(
        seen[0].param("Limit").is_some() && seen[0].param("StartIndex").as_deref() == Some("0")
    );
    // No `after`: first place.
    backend.act(&conn, &moving(id('1'), None)).unwrap();
    assert_eq!(entries(&script), [id('1'), id('2'), id('3')]);
    // Up by one: after the entry before the previous one.
    backend.act(&conn, &moving(id('3'), Some(id('1')))).unwrap();
    assert_eq!(entries(&script), [id('1'), id('3'), id('2')]);
    // Already there is still confirmed.
    backend.act(&conn, &moving(id('3'), Some(id('1')))).unwrap();
    // An unknown entry, or an unknown `after`, fails without moving.
    server.take();
    let before = entries(&script);
    for (item, after) in [
        (id('7'), None),
        (id('1'), Some(id('7'))),
        (id('1'), Some(id('1'))),
    ] {
        let error = backend.act(&conn, &moving(item, after)).unwrap_err();
        assert!(matches!(error, Error::Input(_)), "{error}");
    }
    assert!(server.take().iter().all(|r| r.method == "GET"));
    assert_eq!(entries(&script), before);
}

#[test]
fn a_move_is_confirmed_on_the_relative_order_not_an_absolute_index() {
    let (server, live, script) = start();
    let backend = jellyfin();
    // The server reports a list in which the entry is not after `after`
    // (a move it silently ignored): an error, never a success.
    let silent = Server::start("", {
        let script = script.clone();
        let standard = standard(script);
        move |req| {
            if req.method == "POST" {
                Reply::empty()
            } else {
                standard(req)
            }
        }
    });
    let conn_silent = conn(&silent);
    let action = Action::PlaylistMove {
        id: playlist(),
        item: id('1'),
        after: Some(id('3')),
        index: None,
    };
    let error = backend.act(&conn_silent, &action).unwrap_err();
    assert!(matches!(error, Error::Input(_)), "{error}");
    // An explicit index with no `after` is confirmed at that position.
    backend
        .act(
            &live,
            &Action::PlaylistMove {
                id: playlist(),
                item: id('3'),
                after: None,
                index: Some(0),
            },
        )
        .unwrap();
    assert_eq!(entries(&script)[0], id('3'));
    assert!(server.count() > 0);
}

#[test]
fn refused_writes_report_403_as_http_and_401_as_unauthorized() {
    for (code, unauthorized) in [(401, true), (403, false)] {
        let server = Server::start("", move |_| Reply::status(code));
        let conn = conn(&server);
        for action in [
            Action::Rate {
                id: track_id(1),
                rating: 10,
            },
            Action::Scrobble {
                id: track_id(1),
                played_at: 1_700_000_000,
            },
            Action::PlaylistDelete { id: playlist() },
        ] {
            let error = jellyfin().act(&conn, &action).unwrap_err();
            match (unauthorized, &error) {
                (true, Error::Unauthorized) => {}
                (false, Error::Http(403)) => {}
                _ => panic!("{code}: {error}"),
            }
        }
        // Reads keep their mapping.
        assert!(matches!(
            jellyfin().fetch(&conn, &Query::Sections, None),
            Err(Error::Unauthorized)
        ));
    }
}

#[test]
fn a_search_reads_artists_albums_and_tracks_with_a_limit_each() {
    let (server, conn, _) = start();
    let found = jellyfin()
        .fetch(
            &conn,
            &Query::Search {
                query: "a b&c".into(),
                limit: 25,
            },
            None,
        )
        .unwrap();
    let lines: Vec<_> = server.take().iter().map(Req::line).collect();
    assert_eq!(
        lines,
        [
            format!("GET /Artists?SearchTerm=a+b%26c&Limit=25&Fields={FIELDS_ENCODED}&userId={USER}"),
            format!("GET /Items?searchTerm=a+b%26c&Recursive=true&IncludeItemTypes=MusicAlbum&Limit=25&Fields={FIELDS_ENCODED}&userId={USER}"),
            format!("GET /Items?searchTerm=a+b%26c&Recursive=true&IncludeItemTypes=Audio&Limit=25&Fields={FIELDS_ENCODED}&userId={USER}"),
        ]
    );
    let kinds: Vec<_> = found.items.iter().map(|i| i.kind.clone()).collect();
    assert_eq!(
        kinds,
        [
            ItemKind::Artist,
            ItemKind::Album,
            ItemKind::Track,
            ItemKind::Track,
            ItemKind::Track
        ]
    );
    assert_eq!(found.size, 5);
}

// ---- stream rules ----------------------------------------------------------

#[test]
fn streams_are_header_authenticated_urls_without_a_token() {
    let (_server, conn, _) = start();
    let backend = jellyfin();
    let part = format!("/Audio/{}/stream?static=true", track_id(1));
    let direct = backend.stream_url(&conn, &part).unwrap();
    assert!(direct.ends_with(&part), "{direct}");
    let session = uuid::Uuid::new_v4().to_string();
    let transcode = backend
        .transcode_stream(&conn, &track_id(1), 128, &session)
        .unwrap();
    let url = Url::parse(&transcode).unwrap();
    assert_eq!(url.path(), format!("/Audio/{}/stream.mp3", track_id(1)));
    let pairs: Vec<_> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("audioCodec", "mp3"),
            ("audioBitRate", "128000"),
            ("startTimeTicks", "0"),
            ("deviceId", "fixture-client-id"),
            ("playSessionId", session.as_str()),
            ("static", "false"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
    );
    for text in [&direct, &transcode] {
        assert!(!text.contains(TOKEN));
        assert!(!text.to_ascii_lowercase().contains("api_key"));
    }
    assert!(backend
        .transcode_stream(&conn, &track_id(1), 100, &session)
        .is_err());
    assert!(backend
        .transcode_stream(&conn, &track_id(1), 128, "not-a-uuid")
        .is_err());
    assert!(backend.transcode_stream(&conn, "7", 128, &session).is_err());
    // The policy sends the header to this server's URLs and to nothing else.
    let policy = backend.stream_policy(&conn);
    let header = policy.headers_for(&url);
    let value = header.get("authorization").unwrap().to_str().unwrap();
    assert!(value.contains(&format!("Token=\"{TOKEN}\"")));
    assert!(policy
        .headers_for(&Url::parse("http://evil.example/Audio/x/stream.mp3").unwrap())
        .is_empty());
    assert!(policy.valid_id(&track_id(1)) && !policy.valid_id("7"));
    assert!(policy.is_transcode(&url));
    assert!(!policy.is_transcode(&Url::parse(&direct).unwrap()));
}

#[test]
fn a_seek_restarts_the_transcode_under_a_new_session_and_stop_names_it() {
    let (_server, conn, _) = start();
    let policy = jellyfin().stream_policy(&conn);
    let session = uuid::Uuid::new_v4().to_string();
    let first = policy.transcode_url(&track_id(1), 192, &session).unwrap();
    let (seeked, restarted) = policy.seek(first.as_str(), 12_345).unwrap();
    assert!(restarted);
    let url = Url::parse(&seeked).unwrap();
    let get = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };
    assert_eq!(get("startTimeTicks").as_deref(), Some("123450000"));
    let new_session = get("playSessionId").unwrap();
    assert_ne!(new_session, session);
    assert!(uuid::Uuid::parse_str(&new_session).is_ok());
    assert_eq!(get("audioBitRate").as_deref(), Some("192000"));
    // A second seek is another new session.
    let (again, _) = policy.seek(&seeked, 1).unwrap();
    assert_ne!(Url::parse(&again).unwrap().query(), url.query());
    // Stop names the device and the session of that URL.
    let (method, stop) = policy.stop(&url).unwrap();
    assert_eq!(method, Method::DELETE);
    assert_eq!(stop.path(), "/Videos/ActiveEncodings");
    assert_eq!(
        stop.query(),
        Some(format!("deviceId=fixture-client-id&playSessionId={new_session}").as_str())
    );
    // A direct URL neither restarts nor stops.
    let direct = policy
        .download_url(&format!("/Audio/{}/stream?static=true", track_id(1)))
        .unwrap();
    assert_eq!(
        policy.seek(direct.as_str(), 5).unwrap(),
        (direct.to_string(), false)
    );
    assert!(policy.stop(&direct).is_none());
}

#[test]
fn raw_requests_stay_below_the_base_path_and_never_take_a_credential_parameter() {
    let base = server_url_with_base_path("https://media.example/jellyfin").unwrap();
    let conn = Conn {
        base: base.clone(),
        token: TOKEN.into(),
        user_id: Some(USER.into()),
    };
    let backend = jellyfin();
    let policy = backend.stream_policy(&conn);
    let session = uuid::Uuid::new_v4().to_string();
    let transcode = policy.transcode_url(&track_id(1), 128, &session).unwrap();
    assert_eq!(
        transcode.path(),
        format!("/jellyfin/Audio/{}/stream.mp3", track_id(1))
    );
    let (_, stop) = policy.stop(&transcode).unwrap();
    assert_eq!(stop.path(), "/jellyfin/Videos/ActiveEncodings");
    assert_eq!(
        policy
            .artwork_url("/Items/x/Images/Primary?tag=t&maxWidth=1000")
            .unwrap()
            .path(),
        "/jellyfin/Items/x/Images/Primary"
    );
    assert!(policy.covers(&transcode));
    assert!(!policy.covers(&Url::parse("https://media.example/other/x").unwrap()));
    for path in [
        "/../x",
        "//evil.example/x",
        "/Items/%2e%2e/x",
        "/Items/x?api_key=1",
        "/Items/x?ApiKey=1",
        "http://evil.example/x",
    ] {
        assert!(policy.artwork_url(path).is_err(), "{path}");
        assert!(policy.download_url(path).is_err(), "{path}");
    }
    let metadata = policy.metadata_url(&track_id(1)).unwrap();
    assert_eq!(metadata.path(), "/jellyfin/Items");
    assert!(metadata
        .query()
        .unwrap()
        .contains(&format!("Ids={}", track_id(1))));
    assert!(policy.metadata_url("7").is_err());
    assert!(backend
        .parse_url("https://media.example/jellyfin/a%2fb")
        .is_err());
    assert!(backend
        .parse_url("https://media.example/jellyfin//x")
        .is_err());
}

#[test]
fn a_server_below_a_path_prefix_receives_every_request_under_it() {
    let script = script();
    let server = Server::start("/jellyfin", standard(script));
    let conn = conn(&server);
    let backend = jellyfin();
    backend.fetch(&conn, &Query::Sections, None).unwrap();
    backend
        .fetch(
            &conn,
            &Query::Children {
                id: album(),
                kind: ItemKind::Album,
            },
            None,
        )
        .unwrap();
    backend
        .act(
            &conn,
            &Action::Rate {
                id: track_id(1),
                rating: 10,
            },
        )
        .unwrap();
    backend
        .fetch(
            &conn,
            &Query::advertised("/Genres", vec![("parentId".into(), library())]),
            None,
        )
        .unwrap();
    for line in server.lines() {
        assert!(
            line.split(' ').nth(1).unwrap().starts_with("/jellyfin/"),
            "{line}"
        );
    }
    // The server answers nothing outside its prefix: a request there is 404.
    let outside = Conn {
        base: server_url_with_base_path(&server.url.replace("/jellyfin", "")).unwrap(),
        ..conn
    };
    assert!(matches!(
        backend.fetch(&outside, &Query::Sections, None),
        Err(Error::Http(404))
    ));
}

#[test]
fn a_redirect_is_refused_not_followed() {
    let target = Server::start("", |_| Reply::json(items(vec![])));
    let other = target.url.clone();
    let server = Server::start("", move |_| {
        Reply::status(302).header("Location", &format!("{other}/UserViews"))
    });
    let conn = conn(&server);
    assert!(matches!(
        jellyfin().fetch(&conn, &Query::Sections, None),
        Err(Error::Http(302))
    ));
    assert_eq!(target.count(), 0);
}

#[test]
fn status_codes_map_to_the_errors_core_reacts_to() {
    for (code, expected) in [
        (401, "unauthorized"),
        (403, "unauthorized"),
        (404, "http"),
        (500, "http"),
    ] {
        let server = Server::start("", move |_| Reply::status(code));
        let error = jellyfin()
            .fetch(&conn(&server), &Query::Sections, None)
            .err()
            .unwrap();
        match (expected, &error) {
            ("unauthorized", Error::Unauthorized) => {}
            ("http", Error::Http(got)) if *got == code => {}
            _ => panic!("{code}: {error}"),
        }
    }
    let down = Conn {
        base: Url::parse("http://127.0.0.1:1/").unwrap(),
        token: TOKEN.into(),
        user_id: None,
    };
    assert!(matches!(
        jellyfin().fetch(&down, &Query::Sections, None),
        Err(Error::Network)
    ));
}

#[test]
fn an_oversized_reply_is_refused_instead_of_read() {
    let server = Server::start("", |_| {
        let mut reply = Reply::json(json!({"Items": []}));
        reply.body = vec![b' '; crate::plex::JSON_LIMIT + 1];
        reply
    });
    assert!(jellyfin()
        .fetch(&conn(&server), &Query::Sections, None)
        .is_err());
}

#[test]
fn a_transcode_probe_reads_audio_and_stops_its_session() {
    let (server, conn, _) = start();
    let session = uuid::Uuid::new_v4().to_string();
    let (bytes, mime, stopped) = jellyfin()
        .transcode_probe(&conn, &track_id(1), 128, &session)
        .unwrap();
    assert!(bytes > 0);
    assert_eq!(mime, "audio/mpeg");
    assert!(stopped);
    let seen = server.take();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].path, format!("/Audio/{}/stream.mp3", track_id(1)));
    assert_eq!(
        seen[0].param("playSessionId").as_deref(),
        Some(session.as_str())
    );
    assert_eq!(seen[0].header("accept"), Some("*/*"));
    assert!(seen[0].header("authorization").unwrap().contains(TOKEN));
    assert_eq!(seen[1].method, "DELETE");
    assert_eq!(seen[1].path, "/Videos/ActiveEncodings");
    assert_eq!(
        seen[1].param("playSessionId").as_deref(),
        Some(session.as_str())
    );
    assert!(seen[1].header("authorization").unwrap().contains(TOKEN));
}

// ---- gone probe ------------------------------------------------------------

#[test]
fn only_the_servers_own_404_is_gone() {
    let (server, conn, _) = start();
    let backend = jellyfin();
    let probe = |id: String| crate::rt::block_on(backend.probe(&conn, &id).unwrap());
    assert_eq!(probe(track_id(1)), Probe::Unknown);
    // The fixture's 404 for an unknown item carries Jellyfin's header.
    assert_eq!(probe(track_id(5)), Probe::Gone);
    let seen = server.take();
    assert_eq!(
        seen[1].line(),
        format!("GET /Items/{}?userId={USER}", track_id(5))
    );
    assert!(seen[1].header("authorization").is_some());
    // A 404 without it (a proxy), 401/403 and other statuses conclude otherwise.
    for (code, header, expected) in [
        (404, false, Probe::Unknown),
        (401, false, Probe::Auth),
        (403, false, Probe::Auth),
        (500, true, Probe::Unknown),
    ] {
        let server = Server::start("", move |_| {
            let reply = Reply::status(code);
            if header {
                reply.header("X-Response-Time-ms", "1")
            } else {
                reply
            }
        });
        let conn = Conn {
            base: server_url_with_base_path(&server.url).unwrap(),
            ..conn.clone()
        };
        assert_eq!(
            crate::rt::block_on(backend.probe(&conn, &track_id(1)).unwrap()),
            expected,
            "{code}"
        );
    }
    assert!(backend.probe(&conn, "7").is_err());
}

// ---- sign-in ---------------------------------------------------------------

fn base_of(server: &Server) -> Url {
    server_url_with_base_path(&server.url).unwrap()
}

#[test]
fn password_sign_in_returns_the_session_and_sends_the_credentials_once() {
    let (server, _conn, _) = start();
    let login = jellyfin()
        .login(&base_of(&server), "tester", PASSWORD)
        .unwrap();
    assert_eq!(
        (
            login.token.as_str(),
            login.user_id.as_str(),
            login.server_id.as_str()
        ),
        (TOKEN, USER, SERVER)
    );
    let seen = server.take();
    assert_eq!(seen[0].line(), "GET /System/Info/Public");
    assert_eq!(seen[1].line(), "POST /Users/AuthenticateByName");
    assert_eq!(
        seen[1].json(),
        json!({"Username": "tester", "Pw": PASSWORD})
    );
    // Before sign-in the header has the identity but no token.
    for request in &seen {
        let header = request.header("authorization").unwrap();
        assert!(
            header.contains("DeviceId=\"fixture-client-id\"") && !header.contains("Token="),
            "{header}"
        );
        assert!(!request.target.contains(PASSWORD));
    }
    // A wrong password is 401.
    assert!(matches!(
        jellyfin().login(&base_of(&server), "tester", "wrong"),
        Err(Error::Unauthorized)
    ));
}

#[test]
fn the_server_check_refuses_other_products_and_old_versions() {
    for (product, version, ok) in [
        ("Jellyfin Server", "10.9.0", true),
        ("Jellyfin Server", "10.11.4", true),
        ("Jellyfin Server", "12.0.0", true),
        ("Jellyfin Server", "10.8.13", false),
        ("Jellyfin Server", "9.99.0", false),
        ("Jellyfin Server", "garbage", false),
        ("Emby Server", "10.11.0", false),
    ] {
        let script = script();
        {
            let mut script = script.lock().unwrap();
            script.product = product;
            script.version = version;
        }
        let server = Server::start("", standard(script));
        let result = jellyfin().check_server(&base_of(&server));
        assert_eq!(result.is_ok(), ok, "{product} {version}");
        if !ok {
            assert!(matches!(result, Err(Error::Input(_))));
            // Nothing is sent to a server that is not a supported Jellyfin.
            assert!(jellyfin().login(&base_of(&server), "u", PASSWORD).is_err());
            assert_eq!(
                server
                    .lines()
                    .iter()
                    .filter(|l| l.starts_with("POST"))
                    .count(),
                0
            );
        }
    }
}

#[test]
fn quick_connect_asks_polls_and_exchanges_the_secret() {
    let (server, _conn, script) = start();
    let backend = jellyfin();
    let base = base_of(&server);
    script.lock().unwrap().quick_connect_enabled = false;
    assert!(matches!(
        backend.quick_connect_start(&base),
        Err(Error::Input(_))
    ));
    script.lock().unwrap().quick_connect_enabled = true;
    server.take();
    let request = backend.quick_connect_start(&base).unwrap();
    assert_eq!(
        (request.secret.as_str(), request.code.as_str()),
        ("fixture-secret", "123456")
    );
    assert!(server
        .lines()
        .contains(&"POST /QuickConnect/Initiate".to_owned()));
    server.take();
    assert!(!backend
        .quick_connect_approved(&base, &request.secret)
        .unwrap());
    // The secret travels in the query of the poll only.
    assert_eq!(
        server.lines(),
        ["GET /QuickConnect/Connect?secret=fixture-secret"]
    );
    script.lock().unwrap().approved = true;
    assert!(backend
        .quick_connect_approved(&base, &request.secret)
        .unwrap());
    server.take();
    let login = backend.quick_connect_login(&base, &request.secret).unwrap();
    assert_eq!(
        (login.token.as_str(), login.user_id.as_str()),
        (TOKEN, USER)
    );
    let seen = server.take();
    assert_eq!(
        seen.last().unwrap().json(),
        json!({"Secret": "fixture-secret"})
    );
    // An expired or unknown secret is a 404 on the poll.
    script.lock().unwrap().expired = true;
    assert!(matches!(
        backend.quick_connect_approved(&base, &request.secret),
        Err(Error::Http(404))
    ));
}

#[test]
fn logout_is_best_effort_and_bounded() {
    let (server, conn, _) = start();
    jellyfin().logout(&conn);
    assert_eq!(
        server.lines(),
        [format!("POST /Sessions/Logout?userId={USER}")]
    );
    // An unreachable server does not make it fail.
    let down = Conn {
        base: Url::parse("http://127.0.0.1:1/").unwrap(),
        ..conn
    };
    jellyfin().logout(&down);
}
