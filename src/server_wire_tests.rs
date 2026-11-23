//! Golden tests of the Plex wire format. Every expected literal below was
//! written by hand from the call sites of the code that existed before the
//! `MediaServer` boundary (paths, ordered query pairs, methods, headers), not
//! produced by the functions under test.
//!
//! Two kinds of rows are not pre-boundary request literals:
//! - The two `url_string_*` tests pin the URL strings handed to the audio
//!   engine. Before the boundary those strings carried `X-Plex-Token` as a
//!   query parameter that the decoder stripped and sent as a header; they are
//!   now credential-free. The request on the wire is unchanged, which
//!   `a_direct_stream_reaches_the_server_*` and `a_transcoded_stream_*` below
//!   pin end to end.
//! - `Query::Browse` and `PlaylistContent::Smart` rows take a semantic
//!   [`BrowseSpec`]; their expected pairs are the pre-boundary ones of the
//!   call site named in each row's comment.
//!
//! A request with no parameters still ends in a bare `?` (e.g. `GET /identity?`):
//! `Url::query_pairs_mut().extend_pairs([])` in `Plex::container`/`mutate`
//! leaves it, and that is what the pre-boundary code sent too.
use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
};

const TOKEN: &str = "fixture-secret";
const SESSION: &str = "123e4567-e89b-12d3-a456-426614174000";
const MACHINE_URI: &str = "server%3A%2F%2Fmach-1%2Fcom.plexapp.plugins.library";

fn plex() -> Plex {
    Plex::new("fixture-client", Url::parse("https://plex.tv/").unwrap()).unwrap()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn page(start: usize, size: usize) -> Option<Page> {
    Some(Page { start, size })
}

// ---------------------------------------------------------------------------
// 1. Query -> (path, ordered params)
// ---------------------------------------------------------------------------

type Case = (
    Query,
    Option<Page>,
    &'static str,
    Vec<(&'static str, &'static str)>,
);

fn browse(section: &str, spec: BrowseSpec) -> Query {
    Query::browse(section, spec)
}

fn spec(kind: ItemKind, sort: Sort) -> BrowseSpec {
    BrowseSpec::new(kind, sort)
}

/// Every music filter field set (the title is trimmed on the wire).
fn every_filter() -> Music {
    Music {
        genre: "12".into(),
        mood: "13".into(),
        style: "14".into(),
        title: "  Blue & Green ".into(),
        year_from: Some(1990),
        year_to: Some(1999),
        min_rating: Some(8),
        unplayed: true,
    }
}

fn query_cases() -> Vec<Case> {
    let start = |s: &'static str| ("X-Plex-Container-Start", s);
    let size = |s: &'static str| ("X-Plex-Container-Size", s);
    vec![
        // Core::container_cached("/library/sections", &[]).
        (Query::Sections, None, "/library/sections", vec![]),
        // Core::item / Command::Item: "/library/metadata/{id}", includeStations=1.
        (
            Query::Item {
                id: "7".into(),
                stations: true,
            },
            None,
            "/library/metadata/7",
            vec![("includeStations", "1")],
        ),
        // Item without stations: "/library/metadata/{key}" with no params.
        (
            Query::Item {
                id: "7".into(),
                stations: false,
            },
            None,
            "/library/metadata/7",
            vec![],
        ),
        // Children unpaged, and the 100-row page cache lookup of Core::children.
        (
            Query::Children {
                id: "7".into(),
                kind: ItemKind::Album,
            },
            None,
            "/library/metadata/7/children",
            vec![],
        ),
        (
            Query::Children {
                id: "7".into(),
                kind: ItemKind::Album,
            },
            page(200, 100),
            "/library/metadata/7/children",
            vec![start("200"), size("100")],
        ),
        // Mix seed pages: allLeaves Start/Size 100 (start 0 and 100).
        (
            Query::AllLeaves { id: "7".into() },
            page(0, 100),
            "/library/metadata/7/allLeaves",
            vec![start("0"), size("100")],
        ),
        (
            Query::AllLeaves { id: "7".into() },
            page(100, 100),
            "/library/metadata/7/allLeaves",
            vec![start("100"), size("100")],
        ),
        (
            Query::AllLeaves { id: "7".into() },
            None,
            "/library/metadata/7/allLeaves",
            vec![],
        ),
        // Similar: count=12.
        (
            Query::Similar {
                id: "7".into(),
                count: 12,
            },
            None,
            "/library/metadata/7/similar",
            vec![("count", "12")],
        ),
        // Sonic nearest: limit=50 (radio refill and sonic radio).
        (
            Query::Nearest {
                id: "7".into(),
                limit: 50,
            },
            None,
            "/library/metadata/7/nearest",
            vec![("limit", "50")],
        ),
        // library_browse: type, sort then Start/Size 100.
        (
            browse("3", spec(ItemKind::Album, Sort::Title)),
            page(100, 100),
            "/library/sections/3/all",
            vec![
                ("type", "9"),
                ("sort", "titleSort:asc"),
                start("100"),
                size("100"),
            ],
        ),
        // library_browse sorts (filters.rs SORTS), unpaged.
        (
            browse("3", spec(ItemKind::Track, Sort::Added)),
            None,
            "/library/sections/3/all",
            vec![("type", "10"), ("sort", "addedAt:desc")],
        ),
        (
            browse("3", spec(ItemKind::Album, Sort::Year)),
            None,
            "/library/sections/3/all",
            vec![("type", "9"), ("sort", "year:desc")],
        ),
        (
            browse("3", spec(ItemKind::Track, Sort::LastPlayed)),
            None,
            "/library/sections/3/all",
            vec![("type", "10"), ("sort", "lastViewedAt:desc")],
        ),
        (
            browse("3", spec(ItemKind::Artist, Sort::PlayCount)),
            None,
            "/library/sections/3/all",
            vec![("type", "8"), ("sort", "viewCount:desc")],
        ),
        (
            browse("3", spec(ItemKind::Track, Sort::Random)),
            None,
            "/library/sections/3/all",
            vec![("type", "10"), ("sort", "random")],
        ),
        // Command::Browse with a query: type, sort=titleSort:asc, title.
        (
            browse(
                "3",
                BrowseSpec {
                    search: Some("a b".into()),
                    ..spec(ItemKind::Album, Sort::Title)
                },
            ),
            None,
            "/library/sections/3/all",
            vec![("type", "9"), ("sort", "titleSort:asc"), ("title", "a b")],
        ),
        // Command::Collection favorites: type=10, userRating=10, sort.
        (
            browse(
                "3",
                BrowseSpec {
                    favourite: true,
                    ..spec(ItemKind::Track, Sort::Title)
                },
            ),
            page(0, 100),
            "/library/sections/3/all",
            vec![
                ("type", "10"),
                ("userRating", "10"),
                ("sort", "titleSort:asc"),
                start("0"),
                size("100"),
            ],
        ),
        // Command::Collection played: type=10, sort, viewCount>>=0.
        (
            browse(
                "3",
                BrowseSpec {
                    played: true,
                    ..spec(ItemKind::Track, Sort::LastPlayed)
                },
            ),
            None,
            "/library/sections/3/all",
            vec![
                ("type", "10"),
                ("sort", "lastViewedAt:desc"),
                ("viewCount>>", "0"),
            ],
        ),
        // filtered_browse without schema fields: `{kind}.*` filter pairs in
        // Music::params order, then type and sort.
        (
            browse(
                "3",
                BrowseSpec {
                    filter: every_filter(),
                    ..spec(ItemKind::Album, Sort::Year)
                },
            ),
            None,
            "/library/sections/3/all",
            vec![
                ("album.genre", "12"),
                ("album.mood", "13"),
                ("album.style", "14"),
                ("title", "Blue & Green"),
                ("album.year>=", "1990"),
                ("album.year<=", "1999"),
                ("album.userRating>=", "8"),
                ("album.viewCount", "0"),
                ("type", "9"),
                ("sort", "year:desc"),
            ],
        ),
        // filter_params: canonical scopes from the schema replace the keys.
        (
            browse(
                "3",
                BrowseSpec {
                    filter: every_filter(),
                    fields: vec![
                        (FilterField::Genre, "album.genre".into()),
                        (FilterField::Style, "album.style".into()),
                        (FilterField::Year, "album.year".into()),
                    ],
                    ..spec(ItemKind::Track, Sort::Added)
                },
            ),
            page(200, 100),
            "/library/sections/3/all",
            vec![
                ("album.genre", "12"),
                ("track.mood", "13"),
                ("album.style", "14"),
                ("title", "Blue & Green"),
                ("album.year>=", "1990"),
                ("album.year<=", "1999"),
                ("track.userRating>=", "8"),
                ("track.viewCount", "0"),
                ("type", "10"),
                ("sort", "addedAt:desc"),
                start("200"),
                size("100"),
            ],
        ),
        // JumpArtist / Command::Browse artists: type=8 sort=titleSort:asc.
        (
            browse("3", spec(ItemKind::Artist, Sort::Title)),
            page(0, 100),
            "/library/sections/3/all",
            vec![
                ("type", "8"),
                ("sort", "titleSort:asc"),
                start("0"),
                size("100"),
            ],
        ),
        // Filter schema: type, includeMeta, includeAdvanced, Start 0, Size 0.
        (
            Query::FilterSchema {
                section: "3".into(),
                kind: ItemKind::Track,
            },
            page(0, 0),
            "/library/sections/3/all",
            vec![
                ("type", "10"),
                ("includeMeta", "1"),
                ("includeAdvanced", "1"),
                start("0"),
                size("0"),
            ],
        ),
        (
            Query::FilterSchema {
                section: "3".into(),
                kind: ItemKind::Track,
            },
            None,
            "/library/sections/3/all",
            vec![
                ("type", "10"),
                ("includeMeta", "1"),
                ("includeAdvanced", "1"),
            ],
        ),
        // firstCharacter: type=8 sort=titleSort:asc.
        (
            Query::FirstCharacter {
                section: "3".into(),
            },
            None,
            "/library/sections/3/firstCharacter",
            vec![("type", "8"), ("sort", "titleSort:asc")],
        ),
        // computePath: startID, endID, Start 0, Size 100.
        (
            Query::ComputePath {
                section: "3".into(),
                start: "11".into(),
                end: "22".into(),
            },
            page(0, 100),
            "/library/sections/3/computePath",
            vec![("startID", "11"), ("endID", "22"), start("0"), size("100")],
        ),
        (
            Query::ComputePath {
                section: "3".into(),
                start: "11".into(),
                end: "22".into(),
            },
            None,
            "/library/sections/3/computePath",
            vec![("startID", "11"), ("endID", "22")],
        ),
        // discovery_home: count 12, includeStations 1, includeMyMixes 1.
        (
            Query::Hubs {
                section: "3".into(),
            },
            None,
            "/hubs/sections/3",
            vec![
                ("count", "12"),
                ("includeStations", "1"),
                ("includeMyMixes", "1"),
            ],
        ),
        // Search: query then limit=100.
        (
            Query::Search {
                query: "a b&c".into(),
                limit: 100,
            },
            None,
            "/hubs/search",
            vec![("query", "a b&c"), ("limit", "100")],
        ),
        // Playlists: playlistType=audio then Start/Size.
        (
            Query::Playlists,
            None,
            "/playlists",
            vec![("playlistType", "audio")],
        ),
        (
            Query::Playlists,
            page(300, 100),
            "/playlists",
            vec![("playlistType", "audio"), start("300"), size("100")],
        ),
        (
            Query::Playlist { id: "4".into() },
            None,
            "/playlists/4",
            vec![],
        ),
        (
            Query::PlaylistItems { id: "4".into() },
            None,
            "/playlists/4/items",
            vec![],
        ),
        (
            Query::PlaylistItems { id: "4".into() },
            page(100, 100),
            "/playlists/4/items",
            vec![start("100"), size("100")],
        ),
        // Play queue window: center, window=50, includeBefore=0, includeAfter=1.
        (
            Query::PlayQueueWindow { id: 9, center: 30 },
            None,
            "/playQueues/9",
            vec![
                ("center", "30"),
                ("window", "50"),
                ("includeBefore", "0"),
                ("includeAfter", "1"),
            ],
        ),
        // Server-advertised keys pass through untouched (path + query pairs).
        (
            Query::advertised(
                "/hubs/sections/3/manage",
                pairs(&[("a", "1"), ("b", "x y")]),
            ),
            None,
            "/hubs/sections/3/manage",
            vec![("a", "1"), ("b", "x y")],
        ),
        (
            Query::advertised("/library/sections/3/genre", vec![]),
            page(0, 100),
            "/library/sections/3/genre",
            vec![start("0"), size("100")],
        ),
    ]
}

#[test]
fn every_query_maps_to_the_original_plex_path_and_ordered_params() {
    let server = plex();
    for (query, paging, path, params) in query_cases() {
        let (got_path, got_params) = server.offline_key(&query, paging).unwrap();
        assert_eq!(got_path, path, "{query:?} {paging:?}");
        assert_eq!(got_params, pairs(&params), "{query:?} {paging:?}");
    }
}

#[test]
fn the_case_table_names_every_query_variant() {
    // A new `Query` variant must get a golden row: the variants are
    // enumerated from the type itself.
    let covered: Vec<QueryDiscriminants> = query_cases()
        .iter()
        .map(|(query, ..)| query.into())
        .collect();
    for variant in <QueryDiscriminants as strum::IntoEnumIterator>::iter() {
        assert!(covered.contains(&variant), "{variant:?} has no golden row");
    }
}

// ---------------------------------------------------------------------------
// Local fixture HTTP server capturing raw requests
// ---------------------------------------------------------------------------

struct Reply {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    extra: Option<(&'static str, &'static str)>,
}

fn reply(content_type: &'static str, body: impl Into<Vec<u8>>) -> Reply {
    Reply {
        status: 200,
        content_type,
        body: body.into(),
        extra: None,
    }
}

fn route(path: &str) -> Reply {
    match path {
        "/identity" => reply(
            "application/json",
            r#"{"MediaContainer":{"machineIdentifier":"mach-1","size":0}}"#,
        ),
        "/music/:/transcode/universal/start.m3u8" => reply("audio/mpeg", vec![0x55; 2000]),
        "/library/streams/99" => reply("text/plain", "[00:01.00]fixture lyric"),
        "/library/metadata/404" => Reply {
            status: 404,
            content_type: "text/html",
            body: b"<html></html>".to_vec(),
            extra: Some(("X-Plex-Protocol", "1.0")),
        },
        "/api/v2/pins" | "/api/v2/pins/5" => reply(
            "application/json",
            r#"{"id":5,"code":"ABCD","expiresIn":900}"#,
        ),
        "/api/v2/resources" => reply("application/json", "[]"),
        _ => reply("application/json", r#"{"MediaContainer":{"size":0}}"#),
    }
}

struct Captured {
    method: String,
    target: String,
    version: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Captured {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
    fn line(&self) -> String {
        format!("{} {}", self.method, self.target)
    }
}

struct Fixture {
    base: Url,
    seen: Arc<Mutex<Vec<Captured>>>,
}

impl Fixture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                let mut raw = Vec::new();
                let mut byte = [0];
                while !raw.ends_with(b"\r\n\r\n") {
                    if !matches!(stream.read(&mut byte), Ok(1)) {
                        break;
                    }
                    raw.push(byte[0]);
                }
                let text = String::from_utf8_lossy(&raw).into_owned();
                let mut lines = text.split("\r\n");
                let mut first = lines.next().unwrap_or_default().split(' ');
                let method = first.next().unwrap_or_default().to_owned();
                let target = first.next().unwrap_or_default().to_owned();
                let version = first.next().unwrap_or_default().to_owned();
                let headers: Vec<(String, String)> = lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
                let length: usize = headers
                    .iter()
                    .find(|(k, _)| k == "content-length")
                    .and_then(|(_, v)| v.parse().ok())
                    .unwrap_or(0);
                let mut body = vec![0; length];
                let _ = stream.read_exact(&mut body);
                let path = target.split('?').next().unwrap_or_default().to_owned();
                let answer = route(&path);
                log.lock().unwrap().push(Captured {
                    method,
                    target,
                    version,
                    headers,
                    body: String::from_utf8_lossy(&body).into_owned(),
                });
                let extra = answer
                    .extra
                    .map(|(k, v)| format!("{k}: {v}\r\n"))
                    .unwrap_or_default();
                let _ = write!(
                    stream,
                    "HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
                    answer.status,
                    answer.content_type,
                    answer.body.len()
                );
                let _ = stream.write_all(&answer.body);
            }
        });
        Self { base, seen }
    }
    fn conn(&self) -> Conn {
        Conn {
            base: self.base.clone(),
            token: TOKEN.into(),
            user_id: None,
        }
    }
    fn lines(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(Captured::line)
            .collect()
    }
    /// The credential is a header on every request and never in a URL.
    fn assert_token_only_in_header(&self) {
        let seen = self.seen.lock().unwrap();
        assert!(!seen.is_empty());
        for request in seen.iter() {
            assert_eq!(request.version, "HTTP/1.1");
            assert_eq!(
                request.header("x-plex-token"),
                Some(TOKEN),
                "{}",
                request.line()
            );
            assert!(!request.target.contains(TOKEN), "{}", request.target);
            assert!(!request.body.contains(TOKEN));
        }
    }
}

fn run_action(action: &Action) -> Vec<String> {
    let fixture = Fixture::start();
    plex().act(&fixture.conn(), action).unwrap();
    fixture.assert_token_only_in_header();
    fixture.lines()
}

// ---------------------------------------------------------------------------
// Reads over HTTP: exact encoded request lines and the default headers
// ---------------------------------------------------------------------------

#[test]
fn reads_send_the_original_encoded_request_lines() {
    let cases: Vec<(Query, Option<Page>, &str)> = vec![
        (Query::Sections, None, "GET /library/sections?"),
        (
            Query::Item { id: "7".into(), stations: true },
            None,
            "GET /library/metadata/7?includeStations=1",
        ),
        (
            Query::Search { query: "a b&c".into(), limit: 100 },
            None,
            "GET /hubs/search?query=a+b%26c&limit=100",
        ),
        (
            browse("3", spec(ItemKind::Album, Sort::Title)),
            page(100, 100),
            "GET /library/sections/3/all?type=9&sort=titleSort%3Aasc&X-Plex-Container-Start=100&X-Plex-Container-Size=100",
        ),
        (
            Query::FilterSchema { section: "3".into(), kind: ItemKind::Track },
            page(0, 0),
            "GET /library/sections/3/all?type=10&includeMeta=1&includeAdvanced=1&X-Plex-Container-Start=0&X-Plex-Container-Size=0",
        ),
        (
            Query::FirstCharacter { section: "3".into() },
            None,
            "GET /library/sections/3/firstCharacter?type=8&sort=titleSort%3Aasc",
        ),
        (
            Query::Hubs { section: "3".into() },
            None,
            "GET /hubs/sections/3?count=12&includeStations=1&includeMyMixes=1",
        ),
        (
            Query::PlayQueueWindow { id: 9, center: 30 },
            None,
            "GET /playQueues/9?center=30&window=50&includeBefore=0&includeAfter=1",
        ),
        (
            Query::Playlists,
            page(0, 100),
            "GET /playlists?playlistType=audio&X-Plex-Container-Start=0&X-Plex-Container-Size=100",
        ),
        (
            Query::advertised("/hubs/sections/3/manage", pairs(&[("a", "1"), ("b", "x y")])),
            None,
            "GET /hubs/sections/3/manage?a=1&b=x+y",
        ),
        (
            Query::Similar { id: "7".into(), count: 12 },
            None,
            "GET /library/metadata/7/similar?count=12",
        ),
        (
            Query::ComputePath { section: "3".into(), start: "11".into(), end: "22".into() },
            page(0, 100),
            "GET /library/sections/3/computePath?startID=11&endID=22&X-Plex-Container-Start=0&X-Plex-Container-Size=100",
        ),
    ];
    for (query, paging, expected) in cases {
        let fixture = Fixture::start();
        plex().fetch(&fixture.conn(), &query, paging).unwrap();
        assert_eq!(fixture.lines(), [expected], "{query:?}");
        fixture.assert_token_only_in_header();
    }
}

#[test]
fn a_read_carries_the_client_headers_and_the_token_header() {
    let fixture = Fixture::start();
    plex()
        .fetch(&fixture.conn(), &Query::Sections, None)
        .unwrap();
    let seen = fixture.seen.lock().unwrap();
    let request = &seen[0];
    assert_eq!(request.header("accept"), Some("application/json"));
    assert_eq!(request.header("x-plex-product"), Some("PlexFreq"));
    assert_eq!(
        request.header("x-plex-version"),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(request.header("x-plex-platform"), Some("Linux"));
    assert_eq!(
        request.header("x-plex-client-identifier"),
        Some("fixture-client")
    );
    assert_eq!(request.header("x-plex-token"), Some(TOKEN));
}

// ---------------------------------------------------------------------------
// 2. Actions, playlist contents, radio
// ---------------------------------------------------------------------------

fn action_cases() -> Vec<(Action, Vec<String>)> {
    let uri_tracks = format!("{MACHINE_URI}%2Flibrary%2Fmetadata%2F1%2C2");
    // smart_playlist: filter pairs, type=10, sort, limit (a `&` in a value
    // is encoded in the content URI, then again as the `uri` value).
    let uri_smart = format!(
        "{MACHINE_URI}%2Flibrary%2Fsections%2F3%2Fall%3Ftitle%3DR%2526B%26type%3D10%26sort%3DtitleSort%253Aasc%26limit%3D50"
    );
    let smart = PlaylistContent::Smart {
        section: "3".into(),
        spec: BrowseSpec {
            filter: Music {
                title: "R&B".into(),
                ..Music::default()
            },
            limit: Some(50),
            ..spec(ItemKind::Track, Sort::Title)
        },
    };
    let timeline = |state: &str, continuing: bool, queue: Option<(u64, u64)>| Action::Timeline {
        id: "5".into(),
        state: state.into(),
        position: 12345,
        duration: 234000,
        continuing,
        queue,
        reporting: Reporting::default(),
    };
    vec![
        (
            Action::Rate { id: "5".into(), rating: 10 },
            vec!["PUT /:/rate?key=5&identifier=com.plexapp.plugins.library&rating=10".into()],
        ),
        (
            Action::Rate { id: "5".into(), rating: 0 },
            vec!["PUT /:/rate?key=5&identifier=com.plexapp.plugins.library&rating=0".into()],
        ),
        (
            Action::Scrobble { id: "5".into(), played_at: 1_700_000_000 },
            vec!["GET /:/scrobble?key=5&identifier=com.plexapp.plugins.library".into()],
        ),
        (
            timeline("playing", true, None),
            vec!["POST /:/timeline?key=%2Flibrary%2Fmetadata%2F5&ratingKey=5&state=playing&time=12345&duration=234000&continuing=1".into()],
        ),
        (
            timeline("stopped", false, Some((11, 12))),
            vec!["POST /:/timeline?key=%2Flibrary%2Fmetadata%2F5&ratingKey=5&playQueueID=11&playQueueItemID=12&state=stopped&time=12345&duration=234000&continuing=0".into()],
        ),
        (
            Action::PlaylistRename { id: "3".into(), title: "A & B".into() },
            vec!["PUT /playlists/3?title=A+%26+B".into()],
        ),
        (
            Action::PlaylistDelete { id: "3".into() },
            vec!["DELETE /playlists/3?".into()],
        ),
        (
            Action::PlaylistAdd {
                id: "3".into(),
                content: PlaylistContent::Tracks(vec!["1".into(), "2".into()]),
            },
            vec![
                "GET /identity?".into(),
                format!("PUT /playlists/3/items?uri={uri_tracks}"),
            ],
        ),
        (
            Action::PlaylistAdd { id: "3".into(), content: smart },
            vec![
                "GET /identity?".into(),
                format!("PUT /playlists/3/items?uri={uri_smart}"),
            ],
        ),
        (
            Action::PlaylistRemove { id: "3".into(), item: "44".into() },
            vec!["DELETE /playlists/3/items/44?".into()],
        ),
        (
            Action::PlaylistMove { id: "3".into(), item: "44".into(), after: Some("55".into()), index: None },
            vec!["PUT /playlists/3/items/44/move?after=55".into()],
        ),
        (
            Action::PlaylistMove { id: "3".into(), item: "44".into(), after: None, index: None },
            vec!["PUT /playlists/3/items/44/move?".into()],
        ),
    ]
}

#[test]
fn every_action_sends_the_original_method_path_and_ordered_params() {
    for (action, expected) in action_cases() {
        assert_eq!(run_action(&action), expected, "{action:?}");
    }
}

#[test]
fn the_action_table_names_every_variant() {
    // A new `Action` variant must get a golden row: the variants are
    // enumerated from the type itself.
    let covered: Vec<ActionDiscriminants> = action_cases()
        .iter()
        .map(|(action, _)| action.into())
        .collect();
    for variant in <ActionDiscriminants as strum::IntoEnumIterator>::iter() {
        assert!(covered.contains(&variant), "{variant:?} has no golden row");
    }
}

#[test]
fn the_playlist_content_tables_name_every_variant() {
    // Each `PlaylistContent` variant needs a row both as playlist content
    // added to a playlist and as the content of a created playlist.
    let added: Vec<PlaylistContentDiscriminants> = action_cases()
        .iter()
        .filter_map(|(action, _)| match action {
            Action::PlaylistAdd { content, .. } => Some(content.into()),
            _ => None,
        })
        .collect();
    let created: Vec<PlaylistContentDiscriminants> = create_playlist_cases()
        .iter()
        .map(|(_, content, _)| content.into())
        .collect();
    for variant in <PlaylistContentDiscriminants as strum::IntoEnumIterator>::iter() {
        assert!(
            added.contains(&variant),
            "{variant:?} has no PlaylistAdd row"
        );
        assert!(created.contains(&variant), "{variant:?} has no create row");
    }
}

fn create_playlist_cases() -> Vec<(&'static str, PlaylistContent, String)> {
    vec![
        (
            "Mix & Match",
            PlaylistContent::Tracks(vec!["1".into(), "2".into(), "3".into()]),
            format!(
                "POST /playlists?title=Mix+%26+Match&type=audio&smart=0&uri={MACHINE_URI}%2Flibrary%2Fmetadata%2F1%2C2%2C3"
            ),
        ),
        (
            "Smart",
            PlaylistContent::Smart {
                section: "3".into(),
                spec: BrowseSpec {
                    limit: Some(50),
                    ..spec(ItemKind::Track, Sort::Title)
                },
            },
            format!(
                "POST /playlists?title=Smart&type=audio&smart=1&uri={MACHINE_URI}%2Flibrary%2Fsections%2F3%2Fall%3Ftype%3D10%26sort%3DtitleSort%253Aasc%26limit%3D50"
            ),
        ),
    ]
}

#[test]
fn create_playlist_prefetches_identity_then_posts_title_type_smart_uri() {
    for (title, content, post) in create_playlist_cases() {
        let fixture = Fixture::start();
        MediaServer::create_playlist(&plex(), &fixture.conn(), title, &content).unwrap();
        assert_eq!(fixture.lines(), ["GET /identity?".to_owned(), post]);
        fixture.assert_token_only_in_header();
    }
}

#[test]
fn create_radio_prefetches_identity_then_posts_the_play_queue() {
    let fixture = Fixture::start();
    plex()
        .create_radio(&fixture.conn(), "/library/metadata/9/station/abc")
        .unwrap();
    assert_eq!(
        fixture.lines(),
        [
            "GET /identity?".to_owned(),
            format!(
                "POST /playQueues?type=audio&uri={MACHINE_URI}%2Flibrary%2Fmetadata%2F9%2Fstation%2Fabc&continuous=1&includeRelated=1"
            ),
        ]
    );
    fixture.assert_token_only_in_header();
}

// ---------------------------------------------------------------------------
// 3. URLs, lyrics, probes, account, login
// ---------------------------------------------------------------------------

/// Query up to and including the client profile, in the original order.
fn transcode_prefix() -> String {
    transcode_start("")
        .split("&session=")
        .next()
        .unwrap()
        .to_owned()
}

fn transcode_start(origin: &str) -> String {
    format!(
        "{origin}/music/:/transcode/universal/start.m3u8?path=%2Flibrary%2Fmetadata%2F7&protocol=http&mediaIndex=0&partIndex=0&directPlay=0&directStream=0&audioCodec=mp3&musicBitrate=128&maxAudioBitrate=128&minAudioBitrate=128&directStreamAudio=0&hasMDE=1&X-Plex-Client-Profile-Name=Chrome&X-Plex-Client-Profile-Extra=add-transcode-target%28type%3DmusicProfile%26context%3Dstreaming%26protocol%3Dhttp%26container%3Dmp3%26audioCodec%3Dmp3%29%2Badd-limitation%28scope%3DmusicCodec%26scopeName%3Dmp3%26type%3DupperBound%26name%3Daudio.bitrate%26value%3D128%26replace%3Dtrue%29&session={SESSION}&offset=0&X-Plex-Platform=Linux&X-Plex-Product=PlexFreq"
    )
}

fn offline_conn() -> Conn {
    Conn {
        base: Url::parse("http://192.0.2.1:32400/").unwrap(),
        token: TOKEN.into(),
        user_id: None,
    }
}

#[test]
fn url_string_stream_url_is_the_part_key_without_any_credential() {
    let conn = offline_conn();
    assert_eq!(
        plex()
            .stream_url(&conn, "/library/parts/5/1/file.mp3")
            .unwrap(),
        "http://192.0.2.1:32400/library/parts/5/1/file.mp3"
    );
    assert_eq!(
        plex()
            .stream_url(&conn, "/library/parts/5/1/file.mp3?x=1")
            .unwrap(),
        "http://192.0.2.1:32400/library/parts/5/1/file.mp3?x=1"
    );
    // Credential parameters in an advertised key are refused, not forwarded.
    assert!(plex().stream_url(&conn, "/p?X-Plex-Token=other").is_err());
}

#[test]
fn url_string_transcode_stream_is_the_original_universal_start_url_without_token() {
    let got = plex()
        .transcode_stream(&offline_conn(), "7", 128, SESSION)
        .unwrap();
    assert_eq!(got, transcode_start("http://192.0.2.1:32400"));
    // Invalid inputs are rejected before any URL exists.
    assert!(plex()
        .transcode_stream(&offline_conn(), "7", 100, SESSION)
        .is_err());
    assert!(plex()
        .transcode_stream(&offline_conn(), "7", 128, "nope")
        .is_err());
}

#[test]
fn transcode_probe_gets_the_start_url_then_stops_the_session_with_header_token() {
    let fixture = Fixture::start();
    let (bytes, mime, stopped) = plex()
        .transcode_probe(&fixture.conn(), "7", 128, SESSION)
        .unwrap();
    assert_eq!((bytes, mime.as_str(), stopped), (2000, "audio/mpeg", true));
    let expected_probe = transcode_start("");
    assert_eq!(
        fixture.lines(),
        [
            format!("GET {expected_probe}"),
            format!("GET /video/:/transcode/universal/stop?session={SESSION}"),
        ]
    );
    fixture.assert_token_only_in_header();
    let seen = fixture.seen.lock().unwrap();
    assert_eq!(seen[0].header("accept"), Some("*/*"));
    assert_eq!(seen[0].header("accept-encoding"), Some("identity"));
}

#[test]
fn lyrics_fetch_is_a_plain_get_of_the_stream_key_with_header_token() {
    let fixture = Fixture::start();
    let lines = plex()
        .lyrics(&fixture.conn(), "/library/streams/99")
        .unwrap();
    assert_eq!(
        lines,
        [crate::lyrics::Line {
            time: Some(1000),
            text: "fixture lyric".into()
        }]
    );
    assert_eq!(fixture.lines(), ["GET /library/streams/99"]);
    fixture.assert_token_only_in_header();
    assert_eq!(
        fixture.seen.lock().unwrap()[0].header("accept"),
        Some("application/json")
    );
}

#[test]
fn probe_is_a_status_only_metadata_get_with_header_token() {
    let fixture = Fixture::start();
    let future = plex().probe(&fixture.conn(), "404").unwrap();
    assert_eq!(crate::rt::block_on(future), Probe::Gone);
    assert_eq!(fixture.lines(), ["GET /library/metadata/404"]);
    fixture.assert_token_only_in_header();
    // A non-numeric key never reaches the network.
    assert!(plex().probe(&fixture.conn(), "../x").is_err());
    assert_eq!(fixture.lines().len(), 1);
}

#[test]
fn login_url_is_the_original_app_plex_tv_auth_fragment() {
    let pin = Pin {
        id: 5,
        code: "ABCD".into(),
        expires_in: 900,
        auth_token: None,
    };
    assert_eq!(
        plex().login_url("cid-1", &pin),
        "https://app.plex.tv/auth#?clientID=cid-1&code=ABCD&context%5Bdevice%5D%5Bproduct%5D=PlexFreq"
    );
    assert_eq!(
        plex().login_url("id with space&", &pin),
        "https://app.plex.tv/auth#?clientID=id+with+space%26&code=ABCD&context%5Bdevice%5D%5Bproduct%5D=PlexFreq"
    );
}

#[test]
fn account_requests_use_the_original_endpoints_and_token_placement() {
    let fixture = Fixture::start();
    let account = Plex::new("fixture-client", fixture.base.clone()).unwrap();
    let pin = account.pin().unwrap();
    assert_eq!(pin.code, "ABCD");
    account.poll(5, "AB CD").unwrap();
    account.resources(TOKEN).unwrap();
    assert_eq!(
        fixture.lines(),
        [
            "POST /api/v2/pins",
            "GET /api/v2/pins/5?code=AB+CD",
            "GET /api/v2/resources?includeHttps=1&includeRelay=1",
        ]
    );
    let seen = fixture.seen.lock().unwrap();
    assert_eq!(seen[0].body, "strong=true");
    assert_eq!(
        seen[0].header("content-type"),
        Some("application/x-www-form-urlencoded")
    );
    // Sign-in has no credential yet; resource discovery sends the account
    // token as a header, never in the URL.
    assert_eq!(seen[0].header("x-plex-token"), None);
    assert_eq!(seen[1].header("x-plex-token"), None);
    assert_eq!(seen[2].header("x-plex-token"), Some(TOKEN));
    assert!(seen.iter().all(|r| !r.target.contains(TOKEN)));
}

// ---------------------------------------------------------------------------
// 4. Offline snapshot keys
// ---------------------------------------------------------------------------

fn marked(size: usize) -> Container {
    Container {
        size,
        ..Container::default()
    }
}

#[test]
fn offline_keys_address_the_snapshots_saved_under_the_original_keys() {
    let dir = tempfile::tempdir().unwrap();
    let library = crate::offline::Library::new(dir.path().into(), true).unwrap();
    let server = plex();
    // (query, page, original path, original params) saved by the code before
    // the boundary existed; the new key must hit the very same file.
    for (n, (query, paging, path, params)) in query_cases().into_iter().enumerate() {
        let original: Vec<(&str, String)> =
            params.iter().map(|(k, v)| (*k, (*v).to_owned())).collect();
        library.save("ns", path, &original, &marked(n + 1));
        let (key_path, key_params) = server.offline_key(&query, paging).unwrap();
        let refs: Vec<(&str, String)> = key_params
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        let loaded = library.load("ns", &key_path, &refs);
        assert_eq!(loaded.map(|c| c.size), Some(n + 1), "{query:?} {paging:?}");
    }
}

#[test]
fn invalidate_playlist_removes_exactly_the_original_snapshot_keys() {
    let dir = tempfile::tempdir().unwrap();
    let library = crate::offline::Library::new(dir.path().into(), true).unwrap();
    let audio = [("playlistType", "audio".to_owned())];
    let window = |start: usize, size: usize| {
        [
            ("X-Plex-Container-Start", start.to_string()),
            ("X-Plex-Container-Size", size.to_string()),
        ]
    };
    let list = |start: usize, size: usize| {
        let mut p = audio.to_vec();
        p.extend(window(start, size));
        p
    };
    let mut removed: Vec<(String, Vec<(&str, String)>)> = vec![("/playlists/4".into(), vec![])];
    for start in (0..10000).step_by(100) {
        removed.push(("/playlists/4/items".into(), window(start, 100).to_vec()));
        removed.push(("/playlists".into(), list(start, 100)));
    }
    // Neighbours that must survive: another playlist, another page size, the
    // unpaged list, and a page beyond the 10000-row sweep.
    let kept: Vec<(String, Vec<(&str, String)>)> = vec![
        ("/playlists/5".into(), vec![]),
        ("/playlists/5/items".into(), window(0, 100).to_vec()),
        ("/playlists/4/items".into(), window(0, 50).to_vec()),
        ("/playlists/4/items".into(), vec![]),
        ("/playlists".into(), audio.to_vec()),
        ("/playlists".into(), list(10000, 100)),
        ("/playlists/4/items".into(), window(10000, 100).to_vec()),
    ];
    for (path, params) in removed.iter().chain(&kept) {
        library.save("ns", path, params, &marked(1));
    }
    for (path, params) in removed.iter().chain(&kept) {
        assert!(library.load("ns", path, params).is_some(), "{path}");
    }
    library.invalidate_playlist(&plex(), "ns", "4");
    for (path, params) in &removed {
        assert!(
            library.load("ns", path, params).is_none(),
            "{path} {params:?}"
        );
    }
    for (path, params) in &kept {
        assert!(
            library.load("ns", path, params).is_some(),
            "{path} {params:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Playback wire: what the audio engine really sends for these URLs
// ---------------------------------------------------------------------------

/// Load `url` into a real engine (capture sink) and wait until the fixture has
/// seen a request satisfying `done`.
fn play_and_wait(fixture: &Fixture, url: String, done: impl Fn(&[Captured]) -> bool) {
    // The URL is credential-free; the credential travels in the policy's
    // headers, which is what Core hands the engine with a plan.
    let policy = plex().stream_policy(&fixture.conn());
    let engine = crate::audio::Engine::new(crate::audio::Sink::Capture).unwrap();
    engine
        .handle
        .load(
            crate::audio::Source {
                id: 1,
                url,
                duration: 10000,
                policy,
                ..Default::default()
            },
            false,
        )
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !done(&fixture.seen.lock().unwrap()) {
        assert!(
            std::time::Instant::now() < deadline,
            "no request reached the fixture"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let _ = engine.handle.stop();
}

#[test]
fn a_direct_stream_reaches_the_server_with_the_token_only_in_a_header() {
    let fixture = Fixture::start();
    let url = plex()
        .stream_url(&fixture.conn(), "/library/parts/5/1/file.mp3?x=1")
        .unwrap();
    play_and_wait(&fixture, url, |seen| !seen.is_empty());
    let seen = fixture.seen.lock().unwrap();
    for request in seen.iter() {
        assert_eq!(request.method, "GET");
        assert_eq!(request.target, "/library/parts/5/1/file.mp3?x=1");
        assert_eq!(request.header("x-plex-token"), Some(TOKEN));
        assert_eq!(request.header("accept-encoding"), Some("identity"));
        assert!(!request.target.contains("Token"));
    }
}

#[test]
fn a_transcoded_stream_and_its_stop_reach_the_server_with_header_tokens() {
    let fixture = Fixture::start();
    let url = plex()
        .transcode_stream(&fixture.conn(), "7", 128, SESSION)
        .unwrap();
    let stop_seen = |seen: &[Captured]| {
        seen.iter()
            .any(|r| r.target.starts_with("/video/:/transcode/universal/stop"))
    };
    play_and_wait(&fixture, url, stop_seen);
    let seen = fixture.seen.lock().unwrap();
    let start = &seen[0];
    assert_eq!(start.method, "GET");
    // The playback reader re-issues the session (fresh uuid) and offset, in
    // that order, after the original pairs; the token is never in the query.
    let (head, tail) = start.target.split_once("&session=").unwrap();
    let (session, offset) = tail.split_once("&offset=").unwrap();
    assert!(uuid::Uuid::parse_str(session).is_ok());
    assert_ne!(session, SESSION);
    assert_eq!(offset, "0.000");
    assert_eq!(
        head,
        format!(
            "{}&X-Plex-Platform=Linux&X-Plex-Product=PlexFreq",
            transcode_prefix()
        )
    );
    assert_eq!(start.header("x-plex-token"), Some(TOKEN));
    assert_eq!(start.header("accept-encoding"), Some("identity"));
    let stop = seen
        .iter()
        .find(|r| r.target.starts_with("/video/:/transcode/universal/stop"))
        .unwrap();
    assert_eq!(stop.method, "GET");
    assert_eq!(
        stop.target,
        format!("/video/:/transcode/universal/stop?session={session}")
    );
    assert_eq!(stop.header("x-plex-token"), Some(TOKEN));
    assert!(seen.iter().all(|r| !r.target.contains("Token")));
}
