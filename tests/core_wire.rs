//! Core-level golden tests of the Plex requests user commands produce, end to
//! end: a real `Core` (real Plex backend) against a local fixture HTTP server.
//!
//! Every expected request line is a hand-written literal derived from the
//! query-building code at git HEAD before the `MediaServer` boundary
//! (`Core::page`/`page_sized`, `Command::Browse`/`Collection`/`JumpArtist`/
//! `Children` in lib.rs, `filters.rs` `Music::params`/`filter_params`/`SORTS`/
//! `smart_playlist`, `features.rs` playlist edits, `daily.rs`
//! `discovery_home`/`sonic_adventure`/`sonic_neighbors`/`start_station`/
//! `download_plan`/`paged_tracks`/`radio_download_tracks`, lib.rs
//! `start_radio`/`nearest`/`Command::Lyrics`, `discovery.rs` `hub_items`),
//! never computed by the code
//! under test. Query values are `application/x-www-form-urlencoded` as
//! `Url::query_pairs_mut` writes them (space `+`, `:` `%3A`, `>` `%3E`, ...);
//! a request without parameters still ends in a bare `?`.
//!
//! The fixture serves only synthetic data; the token is a dummy.
use plexfreq_core::{Command, Core, Error};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    time::Duration,
};

const TOKEN: &str = "fixture-server-token";
const ACCOUNT_TOKEN: &str = "fixture-account-token";
const START0: &str = "X-Plex-Container-Start=0&X-Plex-Container-Size=100";
const MACHINE: &str = "server%3A%2F%2Fmach-1%2Fcom.plexapp.plugins.library";

// ---------------------------------------------------------------------------
// Fixture server
// ---------------------------------------------------------------------------

struct Captured {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Captured {
    fn line(&self) -> String {
        format!("{} {}", self.method, self.target)
    }
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

struct Wire {
    origin: String,
    seen: Arc<Mutex<Vec<Captured>>>,
}

fn metadata(items: Value) -> String {
    let size = items.as_array().map_or(0, Vec::len);
    json!({"MediaContainer":{"size":size,"totalSize":size,"Metadata":items}}).to_string()
}

fn directory(entries: Value) -> String {
    json!({"MediaContainer":{"size":entries.as_array().map_or(0, Vec::len),"Directory":entries}})
        .to_string()
}

/// The filter schema of track listings: canonical field scopes (tracks filter
/// on `album.genre`), an album scope listed first that must sort after the
/// track scope, and two advertised choice endpoints.
fn track_schema() -> String {
    json!({"MediaContainer":{"size":0,"Meta":{"Type":[
        {"type":"album","Field":[{"key":"artist.style"},{"key":"album.userRating"}]},
        {"type":"track",
         "Field":[{"key":"album.genre"},{"key":"track.mood"},{"key":"album.style"},
                  {"key":"album.year"},{"key":"track.userRating"},{"key":"track.viewCount"}],
         "Filter":[{"filter":"genre","key":"/library/sections/3/genre?type=10"},
                   {"filter":"mood","key":"/library/sections/3/mood?type=10"}]}
    ]}}})
    .to_string()
}

/// A playable track with a one-minute duration.
fn track(key: &str) -> Value {
    json!({"ratingKey":key,"key":format!("/library/metadata/{key}"),"type":"track",
           "title":format!("T{key}"),"duration":60000,
           "Media":[{"container":"flac","audioCodec":"flac",
                     "Part":[{"key":format!("/library/parts/{key}/file.flac")}]}]})
}

/// `count` tracks numbered from `first`.
fn tracks(first: usize, count: usize) -> Vec<Value> {
    (first..first + count)
        .map(|n| track(&n.to_string()))
        .collect()
}

/// A page of items with an explicit (or absent) `totalSize`.
fn page(items: Vec<Value>, total: Option<usize>) -> String {
    let mut container = json!({"size":items.len(),"Metadata":items});
    if let Some(total) = total {
        container["totalSize"] = json!(total);
    }
    json!({ "MediaContainer": container }).to_string()
}

/// The value of query parameter `name` in a request target.
fn param(target: &str, name: &str) -> Option<String> {
    let query = target.split_once('?')?.1;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

/// Routes of the pure reads behind sonic, lyrics, radio, station and
/// download commands (ids 30-99, apart from the ones above).
fn route_reads(method: &str, path: &str, target: &str) -> Option<(u16, String)> {
    let start: usize = param(target, "X-Plex-Container-Start")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = match (method, path) {
        // Sonic Adventure: two tracks from start to end.
        ("GET", "/library/sections/3/computePath") => metadata(json!([track("40"), track("41")])),
        ("GET", "/library/sections/5/computePath") => {
            metadata(json!([track("40"), {"ratingKey":"7","type":"album","title":"A"}]))
        }
        // A track with a lyric stream and one without.
        ("GET", "/library/metadata/40") => metadata(json!([{
            "ratingKey":"40","key":"/library/metadata/40","type":"track","title":"T40",
            "duration":60000,
            "Media":[{"Part":[{"key":"/library/parts/40/file.flac",
                "Stream":[{"streamType":2,"key":"/library/streams/89"},
                          {"streamType":4,"key":"/library/streams/90","format":"lrc"}]}]}]
        }])),
        ("GET", "/library/streams/90") => "[00:01.00]Hello\n[00:02.50]World".into(),
        ("GET", "/library/metadata/41") => metadata(json!([track("41")])),
        // Sonic neighbours of track 40: itself and an album are filtered out.
        ("GET", "/library/metadata/40/nearest") => metadata(json!([
            track("40"),
            track("42"),
            {"ratingKey":"7","type":"album","title":"A"},
            track("43")
        ])),
        ("GET", "/library/metadata/42/nearest") => metadata(json!([track("40"), track("44")])),
        // Album 50 (no station): two tracks; its neighbour album 53.
        ("GET", "/library/metadata/50") => {
            metadata(json!([{"ratingKey":"50","type":"album","title":"Album 50"}]))
        }
        ("GET", "/library/metadata/50/children") => metadata(json!(tracks(51, 2))),
        ("GET", "/library/metadata/50/nearest") => metadata(json!([
            {"ratingKey":"50","type":"album","title":"Album 50"},
            {"ratingKey":"53","type":"album","title":"Album 53"}
        ])),
        ("GET", "/library/metadata/53/children") => metadata(json!(tracks(54, 1))),
        ("GET", "/library/metadata/53/nearest") => metadata(json!([])),
        // Artist 60 advertises a station.
        ("GET", "/library/metadata/60") => metadata(json!([{
            "ratingKey":"60","type":"artist","title":"Artist 60",
            "Stations":[{"key":"/library/metadata/60/station/abc","title":"Radio"}]
        }])),
        ("POST", "/playQueues") => {
            let items: Vec<Value> = (1..=14)
                .map(|n| {
                    let mut item = track(&(70 + n).to_string());
                    item["playQueueItemID"] = json!(n);
                    item
                })
                .collect();
            json!({"MediaContainer":{"size":items.len(),"playQueueID":77,
                "playQueueSelectedItemID":2,"Metadata":items}})
            .to_string()
        }
        ("GET", "/playQueues/77") => {
            let center: u64 = param(target, "center")
                .and_then(|c| c.parse().ok())
                .unwrap_or(0);
            let mut item = track(&(70 + center).to_string());
            item["playQueueItemID"] = json!(center);
            let mut next = track(&(71 + center).to_string());
            next["playQueueItemID"] = json!(center + 1);
            json!({"MediaContainer":{"size":2,"playQueueID":77,"Metadata":[item,next]}}).to_string()
        }
        // Playlist 30: 150 one-minute tracks in pages of 100 with a total.
        ("GET", "/playlists/30") => {
            metadata(json!([{"ratingKey":"30","type":"playlist","title":"Long","smart":0}]))
        }
        ("GET", "/playlists/30/items") => page(
            tracks(1000 + start, 100.min(150 - start.min(150))),
            Some(150),
        ),
        // Playlist 32: full pages forever without a total (bounded at 1000).
        ("GET", "/playlists/32") => {
            metadata(json!([{"ratingKey":"32","type":"playlist","title":"Endless","smart":0}]))
        }
        ("GET", "/playlists/32/items") => page(tracks(2000 + start, 100), None),
        // Album 31: a full page then a short page, without totals.
        ("GET", "/library/metadata/31") => {
            metadata(json!([{"ratingKey":"31","type":"album","title":"Album 31"}]))
        }
        ("GET", "/library/metadata/31/children") => page(
            tracks(3000 + start, if start == 0 { 100 } else { 20 }),
            None,
        ),
        _ => return None,
    };
    Some((200, body))
}

/// (status, JSON body) for a request.
fn route(method: &str, path: &str, target: &str) -> (u16, String) {
    let ok = |body: String| (200, body);
    if let Some(answer) = route_reads(method, path, target) {
        return answer;
    }
    if target.contains("includeMeta=1") {
        return ok(if target.contains("type=10&") {
            track_schema()
        } else {
            json!({"MediaContainer":{"size":0,"Meta":{}}}).to_string()
        });
    }
    match (method, path) {
        ("GET", "/library/sections") => ok(directory(
            json!([{"key":"3","title":"Music","type":"artist"}]),
        )),
        ("GET", "/library/sections/3/firstCharacter") => ok(directory(json!([
            {"key":"A","title":"A","size":150},
            {"key":"B","title":"B","size":30}
        ]))),
        ("GET", "/library/sections/3/genre" | "/library/sections/3/mood") => ok(directory(
            json!([{"key":"12","title":"Rock"},{"key":"bad","title":"Ignored"}]),
        )),
        ("GET", "/library/sections/401/all") => (401, String::new()),
        ("GET", "/library/sections/403/all") => (403, String::new()),
        ("GET", "/library/metadata/7") => ok(metadata(
            json!([{"ratingKey":"7","type":"album","title":"Album"}]),
        )),
        ("GET", "/library/metadata/8") => ok(metadata(
            json!([{"ratingKey":"8","type":"artist","title":"Artist"}]),
        )),
        // One album per page of a two-album artist: ArtistAlbums pages twice.
        ("GET", "/library/metadata/8/children") => ok(json!({"MediaContainer":{
            "size":1,"totalSize":2,
            "Metadata":[{"ratingKey":"7","type":"album","title":"Album"}]}})
        .to_string()),
        ("GET", "/library/metadata/9/children") => ok(metadata(json!([
            {"ratingKey":"1","type":"track","title":"One"},
            {"ratingKey":"2","type":"track","title":"Two"}
        ]))),
        ("GET", p) if p.starts_with("/library/metadata/400") => (400, String::new()),
        ("GET", p) if p.starts_with("/library/metadata/401") => (401, String::new()),
        ("GET", p) if p.starts_with("/library/metadata/403") => (403, String::new()),
        ("GET", p) if p.starts_with("/library/metadata/404") => (404, String::new()),
        ("GET", "/playlists/4") => ok(metadata(
            json!([{"ratingKey":"4","type":"playlist","title":"Mine","smart":0}]),
        )),
        ("GET", "/playlists/6") => ok(metadata(json!([{
            "ratingKey":"6","type":"playlist","title":"Smart","smart":1,
            "content":"server://mach-1/com.plexapp.plugins.library/library/sections/3/all?type=10&sort=titleSort%3Aasc&limit=50"
        }]))),
        ("GET", "/identity") => {
            ok(json!({"MediaContainer":{"size":0,"machineIdentifier":"mach-1"}}).to_string())
        }
        ("POST", "/playlists") => ok(if target.contains("smart=1") {
            metadata(json!([{"ratingKey":"20","type":"playlist","title":"S","smart":1}]))
        } else {
            metadata(json!([{"ratingKey":"21","type":"playlist","title":"R","smart":0}]))
        }),
        ("POST", "/api/v2/pins") => {
            ok(json!({"id":5,"code":"ABCD","expiresIn":900,"authToken":null}).to_string())
        }
        ("GET", "/api/v2/pins/5") => ok(json!(
            {"id":5,"code":"ABCD","expiresIn":900,"authToken":ACCOUNT_TOKEN}
        )
        .to_string()),
        ("GET", "/api/v2/resources") => ok("[]".into()),
        _ => ok(json!({"MediaContainer":{"size":0}}).to_string()),
    }
}

fn serve(mut stream: TcpStream, seen: &Mutex<Vec<Captured>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut raw = Vec::new();
    let mut byte = [0];
    while !raw.ends_with(b"\r\n\r\n") {
        if !matches!(stream.read(&mut byte), Ok(1)) {
            return;
        }
        raw.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let method = first.next().unwrap_or_default().to_owned();
    let target = first.next().unwrap_or_default().to_owned();
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
    let (status, answer) = route(&method, &path, &target);
    seen.lock().unwrap().push(Captured {
        method,
        target,
        headers,
        body,
    });
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
        answer.len()
    );
}

impl Wire {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        // Core requests are sequential, so connections are served in order.
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                serve(stream, &log);
            }
        });
        Self { origin, seen }
    }
    /// Request lines seen since the previous call.
    fn take(&self) -> Vec<String> {
        let mut seen = self.seen.lock().unwrap();
        let lines = seen.iter().map(Captured::line).collect();
        seen.clear();
        lines
    }
    /// Every server request carries the credential as a header only.
    fn assert_token_only_in_header(&self) {
        for request in self.seen.lock().unwrap().iter() {
            assert_eq!(
                request.header("x-plex-token"),
                Some(TOKEN),
                "{}",
                request.line()
            );
            assert!(!request.target.contains(TOKEN), "{}", request.target);
        }
    }
}

fn command(value: Value) -> Command {
    serde_json::from_value(value).unwrap()
}

/// A Core with the download cache off, connected to the fixture.
fn connected(wire: &Wire) -> (Core, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    core.execute(command(
        json!({"op":"cache_config","enabled":false,"limit_mb":512,"ahead":5}),
    ))
    .unwrap();
    core.execute(command(
        json!({"op":"connect","url":wire.origin,"token":TOKEN}),
    ))
    .unwrap();
    // Core::connect: container(base, token, "/library/sections", &[]).
    wire.assert_token_only_in_header();
    assert_eq!(wire.take(), ["GET /library/sections?"]);
    (core, dir)
}

/// A Core with the download cache on but downloads paused (no background
/// audio requests), connected to the fixture.
fn connected_caching(wire: &Wire) -> (Core, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    core.execute(command(
        json!({"op":"download_policy","wifi_only":false,"paused":true}),
    ))
    .unwrap();
    core.execute(command(
        json!({"op":"cache_config","enabled":true,"limit_mb":512,"ahead":0}),
    ))
    .unwrap();
    core.execute(command(
        json!({"op":"connect","url":wire.origin,"token":TOKEN}),
    ))
    .unwrap();
    wire.assert_token_only_in_header();
    assert_eq!(wire.take(), ["GET /library/sections?"]);
    (core, dir)
}

/// The rating keys of a JSON item list.
fn keys(items: &Value) -> Vec<String> {
    items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["ratingKey"].as_str().unwrap().to_owned())
        .collect()
}

/// Run one command and return its result with the request lines it sent.
fn run(core: &mut Core, wire: &Wire, value: Value) -> (plexfreq_core::Result<Value>, Vec<String>) {
    let result = core.execute(command(value));
    wire.assert_token_only_in_header();
    (result, wire.take())
}

/// Run commands that must succeed, asserting the exact lines of each.
fn expect(core: &mut Core, wire: &Wire, cases: Vec<(Value, Vec<String>)>) {
    for (value, lines) in cases {
        let (result, got) = run(core, wire, value.clone());
        assert!(result.is_ok(), "{value}: {:?}", result.err());
        assert_eq!(got, lines, "{value}");
    }
}

fn lines(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

fn all(query: &str) -> Vec<String> {
    vec![format!("GET /library/sections/3/all?{query}")]
}

// ---------------------------------------------------------------------------
// Browsing and listings
// ---------------------------------------------------------------------------

#[test]
fn browse_listings_collections_and_alphabet_send_the_head_requests() {
    let wire = Wire::start();
    let (mut core, _dir) = connected(&wire);
    expect(
        &mut core,
        &wire,
        vec![
            // Command::Browse: type, sort=titleSort:asc, [title], Start, Size.
            (
                json!({"op":"browse","section":"3","kind":"artist"}),
                all(&format!("type=8&sort=titleSort%3Aasc&{START0}")),
            ),
            (
                json!({"op":"browse","section":"3","kind":"album","query":"Björk & friends","start":100}),
                all("type=9&sort=titleSort%3Aasc&title=Bj%C3%B6rk+%26+friends&X-Plex-Container-Start=100&X-Plex-Container-Size=100"),
            ),
            (
                json!({"op":"browse","section":"3","kind":"track","query":"a+b"}),
                all(&format!("type=10&sort=titleSort%3Aasc&title=a%2Bb&{START0}")),
            ),
            // Command::Collection views.
            (
                json!({"op":"collection","section":"3","view":"favorites"}),
                all(&format!("type=10&userRating=10&sort=titleSort%3Aasc&{START0}")),
            ),
            (
                json!({"op":"collection","section":"3","view":"added","start":200}),
                all("type=9&sort=addedAt%3Adesc&X-Plex-Container-Start=200&X-Plex-Container-Size=100"),
            ),
            (
                json!({"op":"collection","section":"3","view":"played"}),
                all(&format!("type=10&sort=lastViewedAt%3Adesc&viewCount%3E%3E=0&{START0}")),
            ),
            // Command::Playlists: playlistType=audio, Start, Size.
            (
                json!({"op":"playlists"}),
                lines(&[&format!("GET /playlists?playlistType=audio&{START0}")]),
            ),
            (
                json!({"op":"playlists","start":100}),
                lines(&["GET /playlists?playlistType=audio&X-Plex-Container-Start=100&X-Plex-Container-Size=100"]),
            ),
            // Command::Libraries: the sections list, no parameters.
            (json!({"op":"libraries"}), lines(&["GET /library/sections?"])),
            // JumpArtist: firstCharacter (type=8, sort), then one page from 0
            // sized offset+100 (B starts after A's 150 artists).
            (
                json!({"op":"jump_artist","section":"3","letter":"B"}),
                lines(&[
                    "GET /library/sections/3/firstCharacter?type=8&sort=titleSort%3Aasc",
                    "GET /library/sections/3/all?type=8&sort=titleSort%3Aasc&X-Plex-Container-Start=0&X-Plex-Container-Size=250",
                ]),
            ),
            // The index is memoized per section until the next connect.
            (json!({"op":"alphabet","section":"3"}), vec![]),
            (
                json!({"op":"jump_artist","section":"3","letter":"A"}),
                all(&format!("type=8&sort=titleSort%3Aasc&{START0}")),
            ),
        ],
    );
    let (result, got) = run(
        &mut core,
        &wire,
        json!({"op":"jump_artist","section":"3","letter":"Z"}),
    );
    assert!(result.is_err());
    assert!(got.is_empty(), "{got:?}");
}

#[test]
fn library_browse_sends_every_sort_of_the_head_sort_table() {
    let wire = Wire::start();
    let (mut core, _dir) = connected(&wire);
    // filters.rs SORTS: title, newest, year, played, popular, random.
    let sorts = [
        ("title", "titleSort%3Aasc"),
        ("newest", "addedAt%3Adesc"),
        ("year", "year%3Adesc"),
        ("played", "lastViewedAt%3Adesc"),
        ("popular", "viewCount%3Adesc"),
        ("random", "random"),
    ];
    let mut cases = Vec::new();
    for (kind, code) in [("artist", "8"), ("album", "9"), ("track", "10")] {
        for (sort, value) in sorts {
            if kind == "artist" && sort == "year" {
                continue;
            }
            cases.push((
                json!({"op":"library_browse","section":"3","kind":kind,"sort":sort,"start":300}),
                all(&format!(
                    "type={code}&sort={value}&X-Plex-Container-Start=300&X-Plex-Container-Size=100"
                )),
            ));
        }
    }
    expect(&mut core, &wire, cases);
    // Rejected before any request: artist year sort, unknown sort, bad kind.
    for value in [
        json!({"op":"library_browse","section":"3","kind":"artist","sort":"year"}),
        json!({"op":"library_browse","section":"3","kind":"album","sort":"rating"}),
        json!({"op":"library_browse","section":"3","kind":"playlist","sort":"title"}),
        json!({"op":"browse","section":"3","kind":"playlist"}),
    ] {
        let (result, got) = run(&mut core, &wire, value.clone());
        assert!(result.is_err(), "{value}");
        assert!(got.is_empty(), "{value}: {got:?}");
    }
}

#[test]
fn children_details_hubs_search_and_similar_send_the_head_requests() {
    let wire = Wire::start();
    let (mut core, _dir) = connected(&wire);
    expect(
        &mut core,
        &wire,
        vec![
            // Command::Children key forms.
            (
                json!({"op":"children","key":"7"}),
                lines(&[&format!("GET /library/metadata/7/children?{START0}")]),
            ),
            (
                json!({"op":"children","key":"/library/metadata/7/children","start":100}),
                lines(&["GET /library/metadata/7/children?X-Plex-Container-Start=100&X-Plex-Container-Size=100"]),
            ),
            (
                json!({"op":"children","key":"/library/metadata/8"}),
                lines(&[&format!("GET /library/metadata/8/children?{START0}")]),
            ),
            (
                json!({"op":"children","key":"/playlists/4/items","start":200}),
                lines(&["GET /playlists/4/items?X-Plex-Container-Start=200&X-Plex-Container-Size=100"]),
            ),
            // Detail: metadata with stations on the first page only.
            (
                json!({"op":"detail","key":"7"}),
                lines(&[
                    "GET /library/metadata/7?includeStations=1",
                    &format!("GET /library/metadata/7/children?{START0}"),
                ]),
            ),
            (
                json!({"op":"detail","key":"7","start":100}),
                lines(&["GET /library/metadata/7/children?X-Plex-Container-Start=100&X-Plex-Container-Size=100"]),
            ),
            // ArtistAlbums: detail, then children pages until the total.
            (
                json!({"op":"artist_albums","key":"8"}),
                lines(&[
                    "GET /library/metadata/8?includeStations=1",
                    &format!("GET /library/metadata/8/children?{START0}"),
                    "GET /library/metadata/8/children?X-Plex-Container-Start=1&X-Plex-Container-Size=100",
                ]),
            ),
            // PlaylistItems: playlist metadata, then the items page.
            (
                json!({"op":"playlist_items","key":"4","start":100}),
                lines(&[
                    "GET /playlists/4?",
                    "GET /playlists/4/items?X-Plex-Container-Start=100&X-Plex-Container-Size=100",
                ]),
            ),
            (
                json!({"op":"playlist_choices"}),
                lines(&[&format!("GET /playlists?playlistType=audio&{START0}")]),
            ),
            (
                json!({"op":"similar_artists","key":"8"}),
                lines(&["GET /library/metadata/8/similar?count=12"]),
            ),
            (
                json!({"op":"discovery_home","section":"3"}),
                lines(&["GET /hubs/sections/3?count=12&includeStations=1&includeMyMixes=1"]),
            ),
            // HubItems: the advertised key's decoded pairs, then Start/Size.
            (
                json!({"op":"hub_items","key":"/library/sections/3/all?type=9&sort=addedAt:desc"}),
                all(&format!("type=9&sort=addedAt%3Adesc&{START0}")),
            ),
            (
                json!({"op":"hub_items","key":"/hubs/sections/3/manage?x=a%20b&y=1","start":100}),
                lines(&["GET /hubs/sections/3/manage?x=a+b&y=1&X-Plex-Container-Start=100&X-Plex-Container-Size=100"]),
            ),
            (
                json!({"op":"hub_items","key":"/playlists/4/items"}),
                lines(&[&format!("GET /playlists/4/items?{START0}")]),
            ),
            (
                json!({"op":"search","query":"a b&c"}),
                lines(&["GET /hubs/search?query=a+b%26c&limit=100"]),
            ),
            // A blank search never reaches the server.
            (json!({"op":"search","query":"  "}), vec![]),
        ],
    );
}

// ---------------------------------------------------------------------------
// Filters (filters.rs Music::params / filter_params / SORTS)
// ---------------------------------------------------------------------------

#[test]
fn filtered_browse_sends_head_filter_pairs_before_type_sort_and_page() {
    let wire = Wire::start();
    let (mut core, _dir) = connected(&wire);
    let schema = |code: &str| {
        format!(
            "GET /library/sections/3/all?type={code}&includeMeta=1&includeAdvanced=1&X-Plex-Container-Start=0&X-Plex-Container-Size=0"
        )
    };
    let browse = |kind: &str, sort: &str, filters: Value, start: usize| json!({"op":"filtered_browse","section":"3","kind":kind,"sort":sort,"filters":filters,"start":start});
    expect(
        &mut core,
        &wire,
        vec![
            // No filter pairs: no schema request; type, sort, Start, Size.
            (
                browse("album", "newest", json!({}), 0),
                all(&format!("type=9&sort=addedAt%3Adesc&{START0}")),
            ),
            // Every field (album schema has no Type: keys stay `album.*`);
            // title is trimmed. Schema is fetched once.
            (
                browse(
                    "album",
                    "year",
                    json!({"genre":"12","mood":"13","style":"14","title":"  Blue & Green ",
                           "yearFrom":1990,"yearTo":1999,"minRating":8,"unplayed":true}),
                    100,
                ),
                vec![
                    schema("9"),
                    "GET /library/sections/3/all?album.genre=12&album.mood=13&album.style=14&title=Blue+%26+Green&album.year%3E%3D=1990&album.year%3C%3D=1999&album.userRating%3E%3D=8&album.viewCount=0&type=9&sort=year%3Adesc&X-Plex-Container-Start=100&X-Plex-Container-Size=100".into(),
                ],
            ),
            // Memoized schema; a whitespace title adds no pair.
            (
                browse("album", "random", json!({"genre":"12","title":"   "}), 0),
                all(&format!("album.genre=12&type=9&sort=random&{START0}")),
            ),
            // Artist kind: own schema, `artist.*` keys.
            (
                browse("artist", "popular", json!({"genre":"12","minRating":0}), 0),
                vec![
                    schema("8"),
                    format!("GET /library/sections/3/all?artist.genre=12&artist.userRating%3E%3D=0&type=8&sort=viewCount%3Adesc&{START0}"),
                ],
            ),
            // Track kind: canonical scopes from the schema Meta.
            (
                browse("track", "title", json!({"genre":"12"}), 0),
                vec![
                    schema("10"),
                    format!("GET /library/sections/3/all?album.genre=12&type=10&sort=titleSort%3Aasc&{START0}"),
                ],
            ),
            (
                browse("track", "newest", json!({"mood":"13"}), 0),
                all(&format!("track.mood=13&type=10&sort=addedAt%3Adesc&{START0}")),
            ),
            // Suffix match prefers the track scope over the album scope listed first.
            (
                browse("track", "year", json!({"style":"14"}), 0),
                all(&format!("album.style=14&type=10&sort=year%3Adesc&{START0}")),
            ),
            (
                browse("track", "played", json!({"title":"x y"}), 0),
                all(&format!("title=x+y&type=10&sort=lastViewedAt%3Adesc&{START0}")),
            ),
            (
                browse("track", "popular", json!({"yearFrom":2000}), 0),
                all(&format!("album.year%3E%3D=2000&type=10&sort=viewCount%3Adesc&{START0}")),
            ),
            (
                browse("track", "random", json!({"yearTo":2010}), 0),
                all(&format!("album.year%3C%3D=2010&type=10&sort=random&{START0}")),
            ),
            (
                browse("track", "title", json!({"minRating":6}), 0),
                all(&format!("track.userRating%3E%3D=6&type=10&sort=titleSort%3Aasc&{START0}")),
            ),
            (
                browse("track", "title", json!({"unplayed":true}), 0),
                all(&format!("track.viewCount=0&type=10&sort=titleSort%3Aasc&{START0}")),
            ),
            (
                browse("track", "newest", json!({"genre":"12","mood":"13","style":"14","title":"t",
                    "yearFrom":2000,"yearTo":2010,"minRating":6,"unplayed":true}), 200),
                all("album.genre=12&track.mood=13&album.style=14&title=t&album.year%3E%3D=2000&album.year%3C%3D=2010&track.userRating%3E%3D=6&track.viewCount=0&type=10&sort=addedAt%3Adesc&X-Plex-Container-Start=200&X-Plex-Container-Size=100"),
            ),
        ],
    );
    // Invalid filters are rejected before any request.
    for (kind, filters) in [
        ("track", json!({"genre":"rock"})),
        ("track", json!({"yearFrom":2001,"yearTo":2000})),
        ("track", json!({"yearFrom":0})),
        ("track", json!({"minRating":11})),
        ("artist", json!({"yearFrom":2000})),
    ] {
        let value = browse(kind, "title", filters, 0);
        let (result, got) = run(&mut core, &wire, value.clone());
        assert!(result.is_err(), "{value}");
        assert!(got.is_empty(), "{value}: {got:?}");
    }
}

#[test]
fn filter_choices_fetch_the_schema_then_the_advertised_choice_key() {
    let wire = Wire::start();
    let (mut core, _dir) = connected(&wire);
    let (result, got) = run(
        &mut core,
        &wire,
        json!({"op":"filter_options","section":"3","kind":"track","field":"genre"}),
    );
    let value = result.unwrap();
    assert_eq!(
        got,
        [
            "GET /library/sections/3/all?type=10&includeMeta=1&includeAdvanced=1&X-Plex-Container-Start=0&X-Plex-Container-Size=0",
            "GET /library/sections/3/genre?type=10",
        ]
    );
    assert_eq!(value["filterChoices"], json!([{"key":"12","title":"Rock"}]));
    expect(
        &mut core,
        &wire,
        vec![
            (
                json!({"op":"filter_options","section":"3","kind":"track","field":"mood"}),
                lines(&["GET /library/sections/3/mood?type=10"]),
            ),
            // No advertised filter: unavailable, no request.
            (
                json!({"op":"filter_options","section":"3","kind":"track","field":"style"}),
                vec![],
            ),
            (
                json!({"op":"filter_options","section":"3","kind":"album","field":"genre"}),
                lines(&["GET /library/sections/3/all?type=9&includeMeta=1&includeAdvanced=1&X-Plex-Container-Start=0&X-Plex-Container-Size=0"]),
            ),
        ],
    );
    let (result, got) = run(
        &mut core,
        &wire,
        json!({"op":"filter_options","section":"3","kind":"track","field":"year"}),
    );
    assert!(result.is_err());
    assert!(got.is_empty());
}

// ---------------------------------------------------------------------------
// Smart playlists and playlist edits
// ---------------------------------------------------------------------------

#[test]
fn smart_playlists_post_and_put_the_head_content_uri() {
    let wire = Wire::start();
    let (mut core, _dir) = connected(&wire);
    // Content: filter pairs, type=10, sort, limit, form-encoded into the
    // `server://` URI, which is form-encoded again as the `uri` value.
    let filtered = format!(
        "{MACHINE}%2Flibrary%2Fsections%2F3%2Fall%3Falbum.genre%3D12%26track.viewCount%3D0%26type%3D10%26sort%3DlastViewedAt%253Adesc%26limit%3D50"
    );
    let plain = format!(
        "{MACHINE}%2Flibrary%2Fsections%2F3%2Fall%3Ftype%3D10%26sort%3Drandom%26limit%3D1000"
    );
    expect(
        &mut core,
        &wire,
        vec![
            (
                json!({"op":"smart_playlist","action":"create","section":"3","title":" Smart & Co ",
                       "sort":"played","limit":50,"filters":{"genre":"12","unplayed":true}}),
                vec![
                    "GET /library/sections/3/all?type=10&includeMeta=1&includeAdvanced=1&X-Plex-Container-Start=0&X-Plex-Container-Size=0".into(),
                    "GET /identity?".into(),
                    format!("POST /playlists?title=Smart+%26+Co&type=audio&smart=1&uri={filtered}"),
                ],
            ),
            (
                json!({"op":"smart_playlist","action":"create","section":"3","title":"All",
                       "sort":"random","limit":1000,"filters":{}}),
                vec![
                    "GET /identity?".into(),
                    format!("POST /playlists?title=All&type=audio&smart=1&uri={plain}"),
                ],
            ),
            // Update: smart check, (memoized schema), identity, PUT items.
            (
                json!({"op":"smart_playlist","action":"update","key":"6","section":"3",
                       "sort":"played","limit":50,"filters":{"genre":"12","unplayed":true}}),
                vec![
                    "GET /playlists/6?".into(),
                    "GET /identity?".into(),
                    format!("PUT /playlists/6/items?uri={filtered}"),
                ],
            ),
            (json!({"op":"smart_rules","key":"6"}), lines(&["GET /playlists/6?"])),
        ],
    );
    // A regular playlist is never rewritten as smart.
    let (result, got) = run(
        &mut core,
        &wire,
        json!({"op":"smart_playlist","action":"update","key":"4","section":"3",
               "sort":"title","limit":50,"filters":{}}),
    );
    assert!(result.is_err());
    assert_eq!(got, ["GET /playlists/4?"]);
    // Invalid limit or empty title: no request.
    for value in [
        json!({"op":"smart_playlist","action":"create","section":"3","title":"x","sort":"title","limit":0,"filters":{}}),
        json!({"op":"smart_playlist","action":"create","section":"3","title":" ","sort":"title","limit":5,"filters":{}}),
    ] {
        let (result, got) = run(&mut core, &wire, value.clone());
        assert!(result.is_err(), "{value}");
        assert!(got.is_empty(), "{value}: {got:?}");
    }
}

#[test]
fn playlist_edits_and_favorites_send_the_head_writes() {
    let wire = Wire::start();
    let (mut core, _dir) = connected(&wire);
    let track = |key: &str| json!({"ratingKey":key,"type":"track","title":"T"});
    let edit = |action: &str, extra: Value| {
        let mut value = json!({"op":"playlist_edit","action":action,"key":"4"});
        for (k, v) in extra.as_object().unwrap() {
            value[k] = v.clone();
        }
        value
    };
    expect(
        &mut core,
        &wire,
        vec![
            (
                edit("create", json!({"key":"","title":" Mix & Match ","items":[track("1"),track("2")]})),
                vec![
                    "GET /identity?".into(),
                    format!("POST /playlists?title=Mix+%26+Match&type=audio&smart=0&uri={MACHINE}%2Flibrary%2Fmetadata%2F1%2C2"),
                ],
            ),
            // An album expands to its tracks (children page) first.
            (
                edit("create", json!({"key":"","title":"Album","items":[
                    {"ratingKey":"9","type":"album","title":"A"}, track("3")]})),
                vec![
                    format!("GET /library/metadata/9/children?{START0}"),
                    "GET /identity?".into(),
                    format!("POST /playlists?title=Album&type=audio&smart=0&uri={MACHINE}%2Flibrary%2Fmetadata%2F1%2C2%2C3"),
                ],
            ),
            (
                edit("rename", json!({"title":"A & B"})),
                lines(&["GET /playlists/4?", "PUT /playlists/4?title=A+%26+B"]),
            ),
            (
                edit("add", json!({"items":[track("3")]})),
                vec![
                    "GET /playlists/4?".into(),
                    "GET /identity?".into(),
                    format!("PUT /playlists/4/items?uri={MACHINE}%2Flibrary%2Fmetadata%2F3"),
                ],
            ),
            (
                edit("remove", json!({"item_id":44})),
                lines(&["GET /playlists/4?", "DELETE /playlists/4/items/44?"]),
            ),
            (
                edit("move", json!({"item_id":44,"after":55})),
                lines(&["GET /playlists/4?", "PUT /playlists/4/items/44/move?after=55"]),
            ),
            (
                edit("move", json!({"item_id":44})),
                lines(&["GET /playlists/4?", "PUT /playlists/4/items/44/move?"]),
            ),
            (
                edit("delete", json!({})),
                lines(&["GET /playlists/4?", "DELETE /playlists/4?"]),
            ),
            (
                json!({"op":"favorite","key":"5","enabled":true}),
                lines(&["PUT /:/rate?key=5&identifier=com.plexapp.plugins.library&rating=10"]),
            ),
            (
                json!({"op":"favorite","key":"5","enabled":false}),
                lines(&["PUT /:/rate?key=5&identifier=com.plexapp.plugins.library&rating=0"]),
            ),
        ],
    );
    // Smart playlist contents and a missing occurrence: metadata only / nothing.
    for (value, expected) in [
        (
            json!({"op":"playlist_edit","action":"add","key":"6","items":[track("3")]}),
            lines(&["GET /playlists/6?"]),
        ),
        (edit("remove", json!({})), lines(&["GET /playlists/4?"])),
        (edit("rename", json!({"title":" "})), vec![]),
    ] {
        let (result, got) = run(&mut core, &wire, value.clone());
        assert!(result.is_err(), "{value}");
        assert_eq!(got, expected, "{value}");
    }
}

#[test]
fn server_writes_carry_no_body_or_content_type_and_the_client_headers() {
    let wire = Wire::start();
    let (mut core, _dir) = connected(&wire);
    let favourite = json!({"op":"favorite","key":"5","enabled":true});
    core.execute(command(favourite)).unwrap();
    core.execute(command(
        json!({"op":"playlist_edit","action":"delete","key":"4"}),
    ))
    .unwrap();
    core.execute(command(
        json!({"op":"playlist_edit","action":"create","title":"T",
        "items":[{"ratingKey":"1","type":"track","title":"T"}]}),
    ))
    .unwrap();
    let seen = wire.seen.lock().unwrap();
    let writes: Vec<_> = seen.iter().filter(|r| r.method != "GET").collect();
    assert_eq!(
        writes.iter().map(|r| r.line()).collect::<Vec<_>>(),
        [
            "PUT /:/rate?key=5&identifier=com.plexapp.plugins.library&rating=10".to_owned(),
            "DELETE /playlists/4?".to_owned(),
            format!(
                "POST /playlists?title=T&type=audio&smart=0&uri={MACHINE}%2Flibrary%2Fmetadata%2F1"
            ),
        ]
    );
    for write in &writes {
        assert!(write.body.is_empty(), "{}", write.line());
        assert_eq!(write.header("content-type"), None, "{}", write.line());
    }
    // The header set of one write (the rating PUT).
    let rate = writes[0];
    let mut names: Vec<_> = rate.headers.iter().map(|(k, _)| k.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "accept",
            "host",
            "x-plex-client-identifier",
            "x-plex-platform",
            "x-plex-product",
            "x-plex-token",
            "x-plex-version",
        ],
        "{:?}",
        rate.headers
    );
    assert_eq!(rate.header("accept"), Some("application/json"));
    assert_eq!(rate.header("x-plex-product"), Some("PlexFreq"));
    assert_eq!(rate.header("x-plex-platform"), Some("Linux"));
    assert_eq!(
        rate.header("x-plex-version"),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(rate.header("x-plex-token"), Some(TOKEN));
    assert!(!rate.header("x-plex-client-identifier").unwrap().is_empty());
}

#[test]
fn account_sign_in_and_discovery_use_the_head_endpoints() {
    let wire = Wire::start();
    let dir = tempfile::tempdir().unwrap();
    let mut core =
        Core::with_account_url(dir.path().into(), url::Url::parse(&wire.origin).unwrap()).unwrap();
    core.execute(command(json!({"op":"login"}))).unwrap();
    assert_eq!(
        core.execute(command(json!({"op":"poll_login"}))).unwrap()["signedIn"],
        true
    );
    core.execute(command(json!({"op":"servers"}))).unwrap();
    let seen = wire.seen.lock().unwrap();
    assert_eq!(
        seen.iter().map(Captured::line).collect::<Vec<_>>(),
        [
            "POST /api/v2/pins",
            "GET /api/v2/pins/5?code=ABCD",
            "GET /api/v2/resources?includeHttps=1&includeRelay=1",
        ]
    );
    // The pin is the one write with a body: a form, without a credential.
    assert_eq!(seen[0].body, b"strong=true");
    assert_eq!(
        seen[0].header("content-type"),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(seen[1].body, b"");
    assert_eq!(seen[1].header("content-type"), None);
    assert_eq!(seen[2].header("content-type"), None);
    assert_eq!(seen[0].header("x-plex-token"), None);
    assert_eq!(seen[1].header("x-plex-token"), None);
    assert_eq!(seen[2].header("x-plex-token"), Some(ACCOUNT_TOKEN));
    assert!(seen.iter().all(|r| !r.target.contains(ACCOUNT_TOKEN)));
}

// ---------------------------------------------------------------------------
// Error mapping through Core
// ---------------------------------------------------------------------------

#[test]
fn http_errors_map_through_core_as_at_head() {
    let wire = Wire::start();
    let (mut core, _dir) = connected(&wire);
    // Similar artists: 400/404 mean "not available", not a failure.
    for code in ["400", "404"] {
        let (result, got) = run(&mut core, &wire, json!({"op":"similar_artists","key":code}));
        assert_eq!(
            result.unwrap(),
            json!({"similarKey":code,"similarArtists":[],"similarAvailable":false})
        );
        assert_eq!(
            got,
            [format!("GET /library/metadata/{code}/similar?count=12")]
        );
    }
    // 401/403 are Unauthorized wherever they occur.
    for (value, line) in [
        (
            json!({"op":"similar_artists","key":"401"}),
            "GET /library/metadata/401/similar?count=12".to_owned(),
        ),
        (
            json!({"op":"similar_artists","key":"403"}),
            "GET /library/metadata/403/similar?count=12".to_owned(),
        ),
        (
            json!({"op":"browse","section":"401","kind":"album"}),
            format!("GET /library/sections/401/all?type=9&sort=titleSort%3Aasc&{START0}"),
        ),
        (
            json!({"op":"library_browse","section":"403","kind":"track","sort":"title"}),
            format!("GET /library/sections/403/all?type=10&sort=titleSort%3Aasc&{START0}"),
        ),
        (
            json!({"op":"children","key":"401"}),
            format!("GET /library/metadata/401/children?{START0}"),
        ),
    ] {
        let (result, got) = run(&mut core, &wire, value.clone());
        assert!(
            matches!(result, Err(Error::Unauthorized)),
            "{value}: {result:?}"
        );
        assert_eq!(got, [line], "{value}");
    }
    // 400/404 on children and metadata fetches surface as Http(code): no
    // saved-copy fallback (that is only for network and 5xx failures).
    for code in [400u16, 404] {
        let (result, got) = run(
            &mut core,
            &wire,
            json!({"op":"children","key":code.to_string()}),
        );
        assert!(
            matches!(result, Err(Error::Http(c)) if c == code),
            "{result:?}"
        );
        assert_eq!(
            got,
            [format!("GET /library/metadata/{code}/children?{START0}")]
        );
        let (result, got) = run(
            &mut core,
            &wire,
            json!({"op":"detail","key":code.to_string()}),
        );
        assert!(
            matches!(result, Err(Error::Http(c)) if c == code),
            "{result:?}"
        );
        assert_eq!(
            got,
            [format!("GET /library/metadata/{code}?includeStations=1")]
        );
    }
}

// ---------------------------------------------------------------------------
// Pure reads: sonic, lyrics, radio, stations and download plans
// ---------------------------------------------------------------------------

#[test]
fn sonic_adventure_and_neighbors_send_the_head_reads() {
    let wire = Wire::start();
    let (mut core, _dir) = connected(&wire);
    // daily.rs sonic_adventure: computePath with startID, endID and one
    // 100-row page; every returned item must be a track.
    let (result, got) = run(
        &mut core,
        &wire,
        json!({"op":"sonic_adventure","section":"3","start_key":"40","end_key":"41"}),
    );
    let value = result.unwrap();
    assert_eq!(
        got,
        [format!(
            "GET /library/sections/3/computePath?startID=40&endID=41&{START0}"
        )]
    );
    assert_eq!(keys(&value["items"]), ["40", "41"]);
    assert_eq!(value["offline"], false);
    let (result, got) = run(
        &mut core,
        &wire,
        json!({"op":"sonic_adventure","section":"5","start_key":"40","end_key":"7"}),
    );
    assert!(matches!(result, Err(Error::ProtocolAt(_))), "{result:?}");
    assert_eq!(
        got,
        [format!(
            "GET /library/sections/5/computePath?startID=40&endID=7&{START0}"
        )]
    );
    // daily.rs sonic_neighbors: the seed's metadata (with stations), then
    // nearest?limit=50; the seed itself and other kinds are dropped.
    let (result, got) = run(
        &mut core,
        &wire,
        json!({"op":"sonic_neighbors","key":"40","kind":"track"}),
    );
    let value = result.unwrap();
    assert_eq!(
        got,
        [
            "GET /library/metadata/40?includeStations=1",
            "GET /library/metadata/40/nearest?limit=50",
        ]
    );
    assert_eq!(keys(&value["items"]), ["42", "43"]);
    // Rejected before any request: bad ids, a non-music kind.
    for value in [
        json!({"op":"sonic_adventure","section":"x","start_key":"40","end_key":"41"}),
        json!({"op":"sonic_adventure","section":"3","start_key":"40","end_key":"4 1"}),
        json!({"op":"sonic_neighbors","key":"40","kind":"playlist"}),
    ] {
        let (result, got) = run(&mut core, &wire, value.clone());
        assert!(result.is_err(), "{value}");
        assert!(got.is_empty(), "{value}: {got:?}");
    }
}

#[test]
fn lyrics_read_the_track_metadata_then_its_lyric_stream() {
    let wire = Wire::start();
    let (mut core, _dir) = connected(&wire);
    // lib.rs Command::Lyrics: metadata without parameters, then the advertised
    // stream key of the streamType 4 stream, fetched as text.
    let (result, got) = run(&mut core, &wire, json!({"op":"lyrics","key":"40"}));
    let value = result.unwrap();
    assert_eq!(
        got,
        ["GET /library/metadata/40?", "GET /library/streams/90"]
    );
    assert_eq!(value["lyricsKey"], "40");
    assert_eq!(
        value["lyrics"],
        json!([{"time":1000,"text":"Hello"},{"time":2500,"text":"World"}])
    );
    // No lyric stream: metadata only, no lines.
    let (result, got) = run(&mut core, &wire, json!({"op":"lyrics","key":"41"}));
    assert_eq!(result.unwrap()["lyrics"], json!([]));
    assert_eq!(got, ["GET /library/metadata/41?"]);
    let (result, got) = run(&mut core, &wire, json!({"op":"lyrics","key":"/x"}));
    assert!(result.is_err());
    assert!(got.is_empty());
}

#[test]
fn radio_and_stations_send_the_head_reads_and_start_window() {
    let wire = Wire::start();
    let (mut core, _dir) = connected(&wire);
    // lib.rs start_radio, track without a station: metadata with stations,
    // then nearest?limit=50 (non-empty, so the radio starts with the seed).
    let (result, got) = run(
        &mut core,
        &wire,
        json!({"op":"radio","key":"40","kind":"track"}),
    );
    let value = result.unwrap();
    assert_eq!(
        got,
        [
            "GET /library/metadata/40?includeStations=1",
            "GET /library/metadata/40/nearest?limit=50",
        ]
    );
    assert_eq!(keys(&value["queue"]["items"]), ["40"]);
    // Album without a station: nearest, then the album's tracks.
    let (result, got) = run(
        &mut core,
        &wire,
        json!({"op":"radio","key":"50","kind":"album"}),
    );
    let value = result.unwrap();
    assert_eq!(
        got,
        [
            "GET /library/metadata/50?includeStations=1".to_owned(),
            "GET /library/metadata/50/nearest?limit=50".to_owned(),
            format!("GET /library/metadata/50/children?{START0}"),
        ]
    );
    assert_eq!(keys(&value["queue"]["items"]), ["51", "52"]);
    // Artist with a station: identity, then a continuous play queue for the
    // station URI; the window starts at the selected item and holds 11.
    let station = format!("{MACHINE}%2Flibrary%2Fmetadata%2F60%2Fstation%2Fabc");
    let queue_post =
        format!("POST /playQueues?type=audio&uri={station}&continuous=1&includeRelated=1");
    let window: Vec<String> = (72..=82).map(|n| n.to_string()).collect();
    let (result, got) = run(
        &mut core,
        &wire,
        json!({"op":"radio","key":"60","kind":"artist"}),
    );
    let value = result.unwrap();
    assert_eq!(
        got,
        [
            "GET /library/metadata/60?includeStations=1".to_owned(),
            "GET /identity?".to_owned(),
            queue_post.clone(),
        ]
    );
    assert_eq!(keys(&value["queue"]["items"]), window);
    // daily.rs start_station: an advertised station key, same queue.
    let (result, got) = run(
        &mut core,
        &wire,
        json!({"op":"station","key":"/library/metadata/60/station/abc","title":"R"}),
    );
    let value = result.unwrap();
    assert_eq!(got, ["GET /identity?".to_owned(), queue_post]);
    assert_eq!(keys(&value["queue"]["items"]), window);
    // Rejected before any request: a station key outside the library, a
    // non-numeric seed.
    for value in [
        json!({"op":"station","key":"/hubs/x","title":"R"}),
        json!({"op":"radio","key":"x","kind":"track"}),
    ] {
        let (result, got) = run(&mut core, &wire, value.clone());
        assert!(result.is_err(), "{value}");
        assert!(got.is_empty(), "{value}: {got:?}");
    }
}

#[test]
fn download_plans_page_bounded_tracks_with_the_head_reads() {
    let wire = Wire::start();
    let (mut core, _dir) = connected_caching(&wire);
    let pages = |path: &str, starts: &[usize]| -> Vec<String> {
        starts
            .iter()
            .map(|s| format!("GET {path}?X-Plex-Container-Start={s}&X-Plex-Container-Size=100"))
            .collect()
    };
    let plan = |kind: &str, key: &str, minutes: u32| json!({"op":"download_plan","kind":kind,"key":key,"minutes":minutes});
    let total = |value: &Value, key: &str| {
        value["downloads"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["group"].as_str().unwrap().ends_with(&format!(":{key}")))
            .map(|row| row["total"].clone())
            .unwrap()
    };
    // Playlist: metadata, then pages until the advertised total (150).
    let (result, got) = run(&mut core, &wire, plan("playlist", "30", 0));
    let value = result.unwrap();
    let mut expected = vec!["GET /playlists/30?".to_owned()];
    expected.extend(pages("/playlists/30/items", &[0, 100]));
    assert_eq!(got, expected);
    assert_eq!(total(&value, "30"), 150);
    // A timed plan stops once the minutes are collected (30 one-minute tracks).
    let (result, got) = run(&mut core, &wire, plan("playlist", "30", 30));
    let value = result.unwrap();
    let mut expected = vec!["GET /playlists/30?".to_owned()];
    expected.extend(pages("/playlists/30/items", &[0]));
    assert_eq!(got, expected);
    assert_eq!(total(&value, "30"), 30);
    // Full pages without a total: 11 pages, then the 1000-track bound.
    let (result, got) = run(&mut core, &wire, plan("playlist", "32", 0));
    assert!(result.is_err());
    let mut expected = vec!["GET /playlists/32?".to_owned()];
    expected.extend(pages(
        "/playlists/32/items",
        &(0..11).map(|n| n * 100).collect::<Vec<_>>(),
    ));
    assert_eq!(got, expected);
    // Album: fresh metadata with stations, then children until a short page.
    let (result, got) = run(&mut core, &wire, plan("album", "31", 0));
    let value = result.unwrap();
    let mut expected = vec!["GET /library/metadata/31?includeStations=1".to_owned()];
    expected.extend(pages("/library/metadata/31/children", &[0, 100]));
    assert_eq!(got, expected);
    assert_eq!(total(&value, "31"), 120);
    // Track radio without a station: walk nearest neighbours, one track per
    // seed, until a seed has no unseen neighbour.
    let (result, got) = run(&mut core, &wire, plan("track_radio", "40", 30));
    let value = result.unwrap();
    assert_eq!(
        got,
        [
            "GET /library/metadata/40?includeStations=1",
            "GET /library/metadata/40/nearest?limit=50",
            "GET /library/metadata/42/nearest?limit=50",
            "GET /library/metadata/44/nearest?limit=50",
        ]
    );
    assert_eq!(total(&value, "40"), 3);
    // Album radio: each seed album's tracks, then its nearest album.
    let (result, got) = run(&mut core, &wire, plan("album_radio", "50", 30));
    let value = result.unwrap();
    assert_eq!(
        got,
        [
            "GET /library/metadata/50?includeStations=1".to_owned(),
            format!("GET /library/metadata/50/children?{START0}"),
            "GET /library/metadata/50/nearest?limit=50".to_owned(),
            format!("GET /library/metadata/53/children?{START0}"),
            "GET /library/metadata/53/nearest?limit=50".to_owned(),
        ]
    );
    assert_eq!(total(&value, "50"), 3);
    // Artist radio with a station: the queue, then windows centred on the
    // last item until 30 minutes (14 queued tracks plus one new per window).
    let (result, got) = run(&mut core, &wire, plan("artist_radio", "60", 30));
    let value = result.unwrap();
    let mut expected = vec![
        "GET /library/metadata/60?includeStations=1".to_owned(),
        "GET /identity?".to_owned(),
        format!(
            "POST /playQueues?type=audio&uri={MACHINE}%2Flibrary%2Fmetadata%2F60%2Fstation%2Fabc&continuous=1&includeRelated=1"
        ),
    ];
    expected.extend((14..30).map(|center| {
        format!("GET /playQueues/77?center={center}&window=50&includeBefore=0&includeAfter=1")
    }));
    assert_eq!(got, expected);
    assert_eq!(total(&value, "60"), 30);
    // Rejected before any request.
    for value in [
        plan("track", "40", 0),
        plan("playlist", "x", 0),
        plan("track_radio", "40", 10),
        plan("album", "31", 481),
    ] {
        let (result, got) = run(&mut core, &wire, value.clone());
        assert!(result.is_err(), "{value}");
        assert!(got.is_empty(), "{value}: {got:?}");
    }
}
