//! Jellyfin backend: the `impl MediaServer for Jellyfin` in the vocabulary of
//! the Jellyfin REST API (jellyfin/jellyfin `release-10.11.z` controllers and
//! the api.jellyfin.org stable spec; see docs/architecture.md for the
//! endpoints), its sign-in flows (password and Quick Connect) and the stream
//! rules the audio engine and downloaders follow.
//!
//! The credential is the `Authorization: MediaBrowser ...` header, never a URL
//! parameter: media, transcode, artwork and metadata requests all carry it
//! through [`StreamPolicy`](crate::server::StreamPolicy).
mod dto;

use crate::{
    lyrics::Line,
    model::{Container, Item, ItemKind},
    plex::{
        advertised_parts, advertised_url, bounded_body, classify_probe, client, server_path,
        server_url_with_base_path, status, Probe, ViewScope, JSON_LIMIT, PROBE_TIMEOUT,
    },
    server::{
        unsupported, Action, BrowseSpec, Caps, Conn, MediaServer, OfflineKey, Page,
        PlaylistContent, Query, Sort, StreamPolicy, StreamRules,
    },
    Error, RadioKind, Result,
};
use reqwest::{
    header::{HeaderMap, HeaderValue},
    Client, Method, RequestBuilder,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{future::Future, sync::Arc, time::Duration};
use url::Url;

/// What Jellyfin offers: no sonic analysis, stations, smart playlists,
/// alphabet index, mood/style/rating filters or star ratings; hubs are
/// synthesised from ordinary queries.
pub const CAPS: Caps = Caps {
    sonic: false,
    sonic_adventure: false,
    server_stations: false,
    smart_playlists: false,
    server_hubs: true,
    first_character: false,
    star_rating: false,
    mood_filter: false,
    style_filter: false,
    rating_filter: false,
};

/// The oldest server this client supports (Quick Connect POST,
/// `/UserPlayedItems`, `/UserFavoriteItems`, lyrics, loudness normalisation).
const MIN_VERSION: (u32, u32) = (10, 9);
const PRODUCT: &str = "Jellyfin Server";
/// Fields every item listing asks for (media sources carry the codec, size
/// and bitrate Core plans playback with).
const FIELDS: &str = "MediaSources,Genres,DateCreated,ChildCount,PrimaryImageAspectRatio";
/// Playlist occurrences read per request, and the most one playlist may have.
const ENTRY_PAGE: usize = 500;
const MAX_ENTRIES: usize = 100_000;
const LYRICS_LIMIT: usize = 512 * 1024;
/// Ticks (100 ns) per millisecond.
const TICKS_PER_MS: u64 = 10_000;
/// Playlist additions are sent in requests of at most this many ids, so the
/// query string stays far below a server's request-line limit.
const ADD_CHUNK: usize = 100;
/// A Quick Connect request lives this long on the server; Core gives up on
/// it a little earlier.
pub const QUICK_CONNECT_LIFETIME: Duration = Duration::from_secs(9 * 60);

/// A Jellyfin identifier: 32 hex digits, lowercase and without dashes. Dashed
/// GUIDs (and uppercase hex) are normalised; anything else is not an id.
pub fn normalize_id(value: &str) -> Option<String> {
    let dashed = value.len() == 36
        && value
            .bytes()
            .enumerate()
            .all(|(i, b)| (b == b'-') == matches!(i, 8 | 13 | 18 | 23));
    let hex: String = if dashed {
        value.chars().filter(|c| *c != '-').collect()
    } else {
        value.to_owned()
    };
    (hex.len() == 32 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| hex.to_ascii_lowercase())
}

/// A credential-carrying query parameter a server-handed key must not smuggle
/// into a request.
fn is_credential(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "api_key" | "apikey" | "token" | "x-emby-token" | "x-mediabrowser-token" | "authorization"
    )
}

/// The entry occurs exactly once; a missing or duplicated id is refused
/// before any write. Jellyfin does not document `PlaylistItemId` as unique
/// per occurrence, so an id that may name several entries is never sent:
/// the server could act on any or all of them.
fn unique(entries: &[String], entry: &str) -> Result<()> {
    match entries.iter().filter(|e| *e == entry).count() {
        1 => Ok(()),
        0 => Err(Error::Input("The playlist entry was not found")),
        _ => Err(Error::Input(
            "This track is in the playlist more than once and the server cannot tell the copies apart",
        )),
    }
}

/// A header parameter value: ASCII only (anything else becomes `_`) with the
/// characters the header syntax and the server's URL-decoding of it use
/// percent-encoded.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '%' | '+' | '"' | ',' => out.push_str(&format!("%{:02X}", c as u32)),
            c if c.is_ascii() && !c.is_ascii_control() => out.push(c),
            _ => out.push('_'),
        }
    }
    out
}

/// This machine's name for the `Device` header parameter.
fn device_name() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: the buffer is valid for its whole length and gethostname
    // writes at most that many bytes.
    let result = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len() - 1) };
    let name = if result == 0 {
        let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
        String::from_utf8_lossy(&buffer[..end]).into_owned()
    } else {
        String::new()
    };
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.trim().is_empty() {
        "PlexFreq".into()
    } else {
        name
    }
}

/// The `Authorization` header value; `token` is absent before sign-in.
fn authorization(device: &str, device_id: &str, token: Option<&str>) -> Option<HeaderValue> {
    let mut text = format!(
        "MediaBrowser Client=\"PlexFreq\", Device=\"{}\", DeviceId=\"{}\", Version=\"{}\"",
        escape(device),
        escape(device_id),
        env!("CARGO_PKG_VERSION"),
    );
    if let Some(token) = token.filter(|t| !t.is_empty()) {
        text.push_str(&format!(", Token=\"{}\"", escape(token)));
    }
    let mut value = HeaderValue::from_str(&text).ok()?;
    value.set_sensitive(token.is_some_and(|t| !t.is_empty()));
    Some(value)
}

fn credential_headers(device: &str, device_id: &str, token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(value) = authorization(device, device_id, Some(token)) {
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    headers
}

/// A read: server-relative path plus ordered query pairs.
struct Request {
    path: String,
    params: Vec<(String, String)>,
}
impl Request {
    fn new(path: impl Into<String>, params: &[(&str, &str)]) -> Self {
        Self {
            path: path.into(),
            params: params
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }
    fn page(mut self, page: Option<Page>, limit: Option<usize>) -> Self {
        let size = match (page.map(|p| p.size), limit) {
            (Some(size), Some(limit)) => Some(size.min(limit)),
            (size, limit) => size.or(limit),
        };
        if let Some(Page { start, .. }) = page {
            self.params.push(("StartIndex".into(), start.to_string()));
        }
        if let Some(size) = size {
            self.params.push(("Limit".into(), size.to_string()));
        }
        self
    }
}

/// The synthesised home-screen hubs: (name in the key, identifier prefix,
/// title).
const HUBS: [(&str, &str, &str); 4] = [
    ("recent-added", "music.recent.added", "Recently added"),
    ("recent-played", "music.recent.played", "Recently played"),
    ("favorites", "music.favorites", "Favorites"),
    ("most-played", "music.most.played", "Most played"),
];

/// The items of hub `name` of `section`; `preview` is the short home-screen
/// list, otherwise the full paged listing behind it.
fn hub_request(section: &str, name: &str, preview: bool) -> Option<Request> {
    let tracks = |sort: &str, filters: &str| {
        let mut params = vec![
            ("ParentId", section),
            ("IncludeItemTypes", "Audio"),
            ("Recursive", "true"),
            ("SortBy", sort),
            ("SortOrder", "Descending"),
        ];
        if !filters.is_empty() {
            params.push(("Filters", filters));
        }
        params.push(("Fields", FIELDS));
        Request::new("/Items", &params)
    };
    let request = match name {
        "recent-added" if preview => Request::new(
            "/Items/Latest",
            &[
                ("ParentId", section),
                ("IncludeItemTypes", "MusicAlbum"),
                ("Limit", "12"),
                ("Fields", FIELDS),
            ],
        ),
        "recent-added" => Request::new(
            "/Items",
            &[
                ("ParentId", section),
                ("IncludeItemTypes", "MusicAlbum"),
                ("Recursive", "true"),
                ("SortBy", "DateCreated"),
                ("SortOrder", "Descending"),
                ("Fields", FIELDS),
            ],
        ),
        "recent-played" => tracks("DatePlayed", "IsPlayed"),
        "favorites" => {
            let mut request = tracks("SortName", "IsFavorite");
            request.params.retain(|(k, _)| k != "SortOrder");
            request
        }
        "most-played" => tracks("PlayCount", "IsPlayed"),
        _ => return None,
    };
    Some(if preview && name != "recent-added" {
        let mut request = request;
        request.params.push(("Limit".into(), "12".into()));
        request
    } else {
        request
    })
}

fn hub_key(section: &str, name: &str) -> String {
    format!("/hubs/jellyfin/{section}/{name}")
}

/// (section, hub name) of a synthesised hub path.
fn parse_hub(path: &str) -> Option<(String, &str)> {
    let rest = path.strip_prefix("/hubs/jellyfin/")?;
    let (section, name) = rest.split_once('/')?;
    let section = normalize_id(section)?;
    HUBS.iter()
        .any(|(hub, ..)| *hub == name)
        .then_some((section, name))
}

/// The seed of an InstantMix path (`/Items/{id}/InstantMix`).
fn parse_mix(path: &str) -> Option<String> {
    let mut parts = path.strip_prefix('/')?.split('/');
    let (kind, id, tail) = (parts.next()?, parts.next()?, parts.next()?);
    (kind == "Items" && tail == "InstantMix" && parts.next().is_none())
        .then(|| normalize_id(id))
        .flatten()
}

fn sort_params(sort: Sort) -> (&'static str, &'static str) {
    match sort {
        Sort::Title => ("SortName", "Ascending"),
        Sort::Added => ("DateCreated", "Descending"),
        Sort::Year => ("ProductionYear,SortName", "Descending,Ascending"),
        Sort::LastPlayed => ("DatePlayed", "Descending"),
        Sort::PlayCount => ("PlayCount", "Descending"),
        Sort::Random => ("Random", "Ascending"),
    }
}

/// Calendar date of a day count since 1970-01-01 (proleptic Gregorian).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// `seconds` since the epoch as an ISO 8601 UTC timestamp.
fn iso8601(seconds: u64) -> String {
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    let rest = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Longest span the `Years` filter is expanded over (the server takes a list
/// of years, not a range).
const MAX_YEAR_SPAN: i32 = 200;

/// The comma-separated `Years` of a year range. An open start is the 199
/// years before the end; an open end is next year.
fn years(from: Option<i32>, to: Option<i32>) -> Result<Option<String>> {
    let (from, to) = match (from, to) {
        (None, None) => return Ok(None),
        (Some(from), Some(to)) => (from, to),
        (Some(from), None) => {
            let now = civil_from_days((crate::unix_seconds() / 86_400) as i64).0 as i32;
            (from, (now + 1).max(from))
        }
        (None, Some(to)) => ((to - MAX_YEAR_SPAN + 1).max(1), to),
    };
    if from > to || to - from + 1 > MAX_YEAR_SPAN {
        return Err(Error::Input("Invalid music filter values"));
    }
    Ok(Some(
        (from..=to)
            .map(|year| year.to_string())
            .collect::<Vec<_>>()
            .join(","),
    ))
}

/// `/Items` (or the artist endpoint) for a listing.
fn browse_request(section: &str, spec: &BrowseSpec, items_endpoint: bool) -> Result<Request> {
    spec.check()?;
    let (path, kind) = match (&spec.kind, items_endpoint) {
        (ItemKind::Artist, false) => ("/Artists/AlbumArtists", None),
        (ItemKind::Artist, true) => ("/Items", Some("MusicArtist")),
        (ItemKind::Album, _) => ("/Items", Some("MusicAlbum")),
        (ItemKind::Track, _) => ("/Items", Some("Audio")),
        _ => return Err(Error::Input("Unknown music type")),
    };
    let (sort_by, sort_order) = sort_params(spec.sort);
    let mut request = Request::new(path, &[("ParentId", section)]);
    if let Some(kind) = kind {
        request
            .params
            .push(("IncludeItemTypes".into(), kind.into()));
        request.params.push(("Recursive".into(), "true".into()));
    }
    request.params.push(("SortBy".into(), sort_by.into()));
    request.params.push(("SortOrder".into(), sort_order.into()));
    let mut filters = Vec::new();
    if spec.favourite {
        filters.push("IsFavorite");
    }
    if spec.played {
        filters.push("IsPlayed");
    }
    if spec.filter.unplayed {
        filters.push("IsUnplayed");
    }
    if !filters.is_empty() {
        request.params.push(("Filters".into(), filters.join(",")));
    }
    if !spec.filter.genre.is_empty() {
        let genre =
            normalize_id(&spec.filter.genre).ok_or(Error::Input("Invalid music filter values"))?;
        request.params.push(("GenreIds".into(), genre));
    }
    if let Some(years) = years(spec.filter.year_from, spec.filter.year_to)? {
        request.params.push(("Years".into(), years));
    }
    let title = spec.filter.title.trim();
    if let Some(term) = spec
        .search
        .as_deref()
        .or((!title.is_empty()).then_some(title))
    {
        request.params.push(("SearchTerm".into(), term.into()));
    }
    request.params.push(("Fields".into(), FIELDS.into()));
    Ok(request.page(None, None))
}

/// The read a query stands for (sections, hubs and the advertised
/// endpoints aside, which [`Jellyfin::fetch`] dispatches itself).
fn request(query: &Query, page: Option<Page>) -> Result<Request> {
    let detail = format!("{FIELDS},Overview");
    let id = |id: &str| normalize_id(id).ok_or(Error::Input("Expected a server identifier"));
    let request = match query {
        Query::Sections => Request::new("/UserViews", &[]),
        Query::Item { id: item, .. } | Query::Playlist { id: item } => Request::new(
            "/Items",
            &[("Ids", &id(item)?), ("Fields", detail.as_str())],
        ),
        Query::Children { id: parent, kind } => {
            let parent = id(parent)?;
            match kind {
                ItemKind::Album => Request::new(
                    "/Items",
                    &[
                        ("ParentId", &parent),
                        ("IncludeItemTypes", "Audio"),
                        ("Recursive", "true"),
                        ("SortBy", "ParentIndexNumber,IndexNumber,SortName"),
                        ("Fields", FIELDS),
                    ],
                ),
                ItemKind::Artist => Request::new(
                    "/Items",
                    &[
                        ("AlbumArtistIds", &parent),
                        ("IncludeItemTypes", "MusicAlbum"),
                        ("Recursive", "true"),
                        ("SortBy", "ProductionYear,SortName"),
                        ("SortOrder", "Descending,Ascending"),
                        ("Fields", FIELDS),
                    ],
                ),
                ItemKind::Playlist => {
                    Request::new(format!("/Playlists/{parent}/Items"), &[("Fields", FIELDS)])
                }
                _ => return Err(Error::Input("Unknown music type")),
            }
        }
        Query::PlaylistItems { id: playlist } => Request::new(
            format!("/Playlists/{}/Items", id(playlist)?),
            &[("Fields", FIELDS)],
        ),
        Query::AllLeaves { id: artist } => Request::new(
            "/Items",
            &[
                ("ArtistIds", &id(artist)?),
                ("IncludeItemTypes", "Audio"),
                ("Recursive", "true"),
                ("SortBy", "Album,ParentIndexNumber,IndexNumber,SortName"),
                ("Fields", FIELDS),
            ],
        ),
        Query::Similar { id: item, count } => Request::new(
            format!("/Items/{}/Similar", id(item)?),
            &[("Limit", &count.to_string()), ("Fields", FIELDS)],
        ),
        Query::Browse { section, spec } => browse_request(&id(section)?, spec, false)?,
        Query::Hubs { section } => Request::new(format!("/hubs/jellyfin/{}", id(section)?), &[]),
        Query::Search { query, limit } => search_requests(query, *limit).remove(0),
        Query::Playlists => Request::new(
            "/Items",
            &[
                ("IncludeItemTypes", "Playlist"),
                ("Recursive", "true"),
                ("SortBy", "SortName"),
                ("Fields", FIELDS),
            ],
        ),
        Query::Advertised { path, params } => advertised_request(path, params, page)?,
        Query::Nearest { .. }
        | Query::FilterSchema { .. }
        | Query::FirstCharacter { .. }
        | Query::ComputePath { .. }
        | Query::PlayQueueWindow { .. } => return Err(unsupported()),
    };
    // Listings that carry their own window already did.
    Ok(match query {
        Query::Browse { spec, .. } => request.page(page, spec.limit),
        Query::Item { .. }
        | Query::Playlist { .. }
        | Query::Similar { .. }
        | Query::Search { .. } => request,
        Query::Advertised { .. } => request,
        _ => request.page(page, None),
    })
}

/// The reads a search stands for, in the order their results are listed:
/// artists (their own endpoint, which matches artist names reliably), then
/// albums and tracks, each with its own limit so one type cannot crowd out
/// the others.
fn search_requests(query: &str, limit: u32) -> Vec<Request> {
    let limit = limit.to_string();
    let mut requests = vec![Request::new(
        "/Artists",
        &[("SearchTerm", query), ("Limit", &limit), ("Fields", FIELDS)],
    )];
    for kind in ["MusicAlbum", "Audio"] {
        requests.push(Request::new(
            "/Items",
            &[
                ("searchTerm", query),
                ("Recursive", "true"),
                ("IncludeItemTypes", kind),
                ("Limit", &limit),
                ("Fields", FIELDS),
            ],
        ));
    }
    requests
}

/// The endpoints a server-advertised key may name: a synthesised hub, an
/// instant mix and the genre list.
fn advertised_request(
    path: &str,
    params: &[(String, String)],
    page: Option<Page>,
) -> Result<Request> {
    let param = |name: &str| {
        params
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };
    if let Some((section, name)) = parse_hub(path) {
        return hub_request(&section, name, false)
            .map(|request| request.page(page, None))
            .ok_or_else(unsupported);
    }
    if let Some(id) = parse_mix(path) {
        let limit = param("limit")
            .and_then(|l| l.parse::<u32>().ok())
            .map_or(50, |l| l.clamp(1, 200));
        // The generic endpoint: stock Jellyfin serves every seed kind from
        // it as from the per-kind ones (`GetInstantMixFromItem`), and mix
        // plugins such as AudioMuse-AI replace only this route.
        // Each field and image type is its own parameter: the AudioMuse-AI
        // action binds plain arrays, which do not split a comma list, while
        // stock Jellyfin accepts both forms. The plugin leaves images and
        // user data out unless asked.
        let mut request = Request::new(
            format!("/Items/{id}/InstantMix"),
            &[("Limit", &limit.to_string())],
        );
        request
            .params
            .extend(FIELDS.split(',').map(|f| ("Fields".into(), f.into())));
        request.params.extend([
            ("EnableImages".into(), "true".into()),
            ("EnableUserData".into(), "true".into()),
            ("EnableImageTypes".into(), "Primary".into()),
            ("EnableImageTypes".into(), "Backdrop".into()),
        ]);
        return Ok(request);
    }
    if path == "/Genres" {
        let parent = param("parentId")
            .and_then(normalize_id)
            .ok_or(Error::Input("Expected a server identifier"))?;
        return Ok(Request::new(
            "/Genres",
            &[("ParentId", &parent), ("SortBy", "SortName")],
        ));
    }
    Err(unsupported())
}

/// Jellyfin's transcode, seek, stop and raw-request rules.
struct JellyfinRules {
    device_id: String,
    user_id: Option<String>,
}
impl JellyfinRules {
    fn transcode(&self, base: &Url, id: &str, kbps: u32, session: &str) -> Result<Url> {
        let id = normalize_id(id).ok_or(Error::Input("Expected a server identifier"))?;
        if !crate::quality::BITRATES.contains(&kbps) || uuid::Uuid::parse_str(session).is_err() {
            return Err(Error::Input("Invalid audio source"));
        }
        let mut url = server_path(base, &format!("/Audio/{id}/stream.mp3"))?;
        url.query_pairs_mut().extend_pairs([
            ("audioCodec", "mp3".to_owned()),
            ("audioBitRate", (u64::from(kbps) * 1000).to_string()),
            ("startTimeTicks", "0".to_owned()),
            ("deviceId", self.device_id.clone()),
            ("playSessionId", session.to_owned()),
            ("static", "false".to_owned()),
        ]);
        Ok(url)
    }
}
impl StreamRules for JellyfinRules {
    fn valid_id(&self, id: &str) -> bool {
        normalize_id(id).is_some()
    }
    fn transcode_url(&self, base: &Url, id: &str, kbps: u32, session: &str) -> Result<Url> {
        self.transcode(base, id, kbps, session)
    }
    fn is_transcode(&self, url: &Url) -> bool {
        let path = url.path();
        path.ends_with("/stream.mp3") && path.contains("/Audio/")
    }
    /// The same request restarted at `milliseconds` under a new session: a
    /// progressive transcode cannot seek, and a late stop for the old
    /// session must not kill the new job.
    fn seek(&self, url: &Url, milliseconds: u64) -> Result<Url> {
        let pairs: Vec<_> = url
            .query_pairs()
            .filter(|(k, _)| k != "startTimeTicks" && k != "playSessionId")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let mut url = url.clone();
        url.set_query(None);
        url.query_pairs_mut()
            .extend_pairs(pairs)
            .append_pair(
                "startTimeTicks",
                &milliseconds.saturating_mul(TICKS_PER_MS).to_string(),
            )
            .append_pair("playSessionId", &uuid::Uuid::new_v4().to_string());
        Ok(url)
    }
    fn stop(&self, url: &Url) -> Option<(Method, Url)> {
        if !self.is_transcode(url) {
            return None;
        }
        let param = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        };
        let (device, session) = (param("deviceId")?, param("playSessionId")?);
        let prefix = url.path().split("/Audio/").next()?;
        let mut stop = url.clone();
        stop.set_path(&format!("{prefix}/Videos/ActiveEncodings"));
        stop.set_query(None);
        stop.query_pairs_mut()
            .append_pair("deviceId", &device)
            .append_pair("playSessionId", &session);
        Some((Method::DELETE, stop))
    }
    fn download_url(&self, base: &Url, part_key: &str) -> Result<Url> {
        advertised_url(base, part_key, is_credential)
    }
    fn artwork_url(&self, base: &Url, path: &str) -> Result<Url> {
        advertised_url(base, path, is_credential)
    }
    fn metadata_url(&self, base: &Url, id: &str) -> Result<Url> {
        let id = normalize_id(id).ok_or(Error::Input("Expected a server identifier"))?;
        let mut url = server_path(base, "/Items")?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("Ids", &id);
            pairs.append_pair("Fields", FIELDS);
            if let Some(user) = &self.user_id {
                pairs.append_pair("userId", user);
            }
        }
        Ok(url)
    }
    fn parse_metadata(&self, body: &[u8]) -> Result<Container> {
        dto::container(body)
    }
    /// The identity travels in the credential header already.
    fn client_headers(&self, _client_identifier: &str) -> HeaderMap {
        HeaderMap::new()
    }
}

/// A signed-in session as a sign-in flow returns it.
pub struct Login {
    pub token: String,
    pub user_id: String,
    pub server_id: String,
}

/// A pending Quick Connect request: the code the user approves on another
/// signed-in device, and the secret that proves it is ours.
pub struct QuickConnect {
    pub secret: String,
    pub code: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Authentication {
    access_token: String,
    #[serde(default)]
    server_id: Option<String>,
    user: AuthUser,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AuthUser {
    id: String,
}

/// `major.minor` of a Jellyfin version string such as `10.11.4`.
fn version(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// The Jellyfin client. `Clone` hands a worker thread its own client with
/// the same connections and identity but a fresh view scope.
pub struct Jellyfin {
    client: Client,
    client_identifier: String,
    device: String,
    /// The server's identity, which scopes the offline namespace.
    server_id: String,
    view: ViewScope,
}
impl Clone for Jellyfin {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            client_identifier: self.client_identifier.clone(),
            device: self.device.clone(),
            server_id: self.server_id.clone(),
            view: ViewScope::default(),
        }
    }
}

impl Jellyfin {
    /// `server_id` is empty until the first sign-in completes.
    pub fn new(client_identifier: &str, server_id: &str) -> Self {
        Self {
            client: client().clone(),
            client_identifier: client_identifier.to_owned(),
            device: device_name(),
            server_id: server_id.to_owned(),
            view: ViewScope::default(),
        }
    }

    fn builder(&self, method: Method, url: Url, token: &str) -> Result<RequestBuilder> {
        let value = authorization(&self.device, &self.client_identifier, Some(token))
            .ok_or(Error::Input("Invalid client identifier"))?;
        Ok(self
            .client
            .request(method, url)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::AUTHORIZATION, value))
    }

    /// A request URL under the server's base path with `params` and, when
    /// signed in, the user.
    fn url(&self, conn: &Conn, path: &str, params: &[(String, String)]) -> Result<Url> {
        let mut url = server_path(&conn.base, path)?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.extend_pairs(params.iter().map(|(k, v)| (k.as_str(), v.as_str())));
            if let Some(user) = &conn.user_id {
                pairs.append_pair("userId", user);
            }
        }
        if url.query() == Some("") {
            url.set_query(None);
        }
        Ok(url)
    }

    /// Send `request` and read its body (at most `limit` bytes) after a
    /// success status.
    fn exchange(&self, request: RequestBuilder, limit: usize, write: bool) -> Result<Vec<u8>> {
        self.view.run(async move {
            let mut response = request.send().await.map_err(|_| Error::Network)?;
            let code = response.status().as_u16();
            // A refused write (403) is a permission of this account, not a
            // sign-in that went stale; only 401 asks to sign in again.
            if write && code == 403 {
                return Err(Error::Http(403));
            }
            status(code)?;
            bounded_body(&mut response, limit)
                .await
                .map_err(|error| error.unwrap_or(Error::Protocol))
        })
    }

    fn get(&self, conn: &Conn, request: &Request) -> Result<Vec<u8>> {
        let url = self.url(conn, &request.path, &request.params)?;
        self.exchange(
            self.builder(Method::GET, url, &conn.token)?,
            JSON_LIMIT,
            false,
        )
    }

    /// A write; `body` is a JSON document. The reply carries nothing Core needs.
    fn write(
        &self,
        conn: &Conn,
        method: Method,
        path: &str,
        params: &[(&str, String)],
        body: Option<Value>,
    ) -> Result<Vec<u8>> {
        let params: Vec<_> = params
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        let url = self.url(conn, path, &params)?;
        let mut request = self.builder(method, url, &conn.token)?;
        if let Some(body) = body {
            request = request.json(&body);
        }
        self.exchange(request, JSON_LIMIT, true)
    }

    /// An unauthenticated or pre-authentication request at `base`.
    fn anonymous(
        &self,
        base: &Url,
        method: Method,
        path: &str,
        params: &[(&str, &str)],
        body: Option<Value>,
    ) -> Result<Vec<u8>> {
        let mut url = server_path(base, path)?;
        url.query_pairs_mut().extend_pairs(params);
        if url.query() == Some("") {
            url.set_query(None);
        }
        let value = authorization(&self.device, &self.client_identifier, None)
            .ok_or(Error::Input("Invalid client identifier"))?;
        let mut request = self
            .client
            .request(method, url)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::AUTHORIZATION, value);
        if let Some(body) = body {
            request = request.json(&body);
        }
        self.exchange(request, JSON_LIMIT, false)
    }

    // ---- sign-in -------------------------------------------------------

    /// The server's identity from the anonymous public info, refusing
    /// anything that is not a Jellyfin server of a supported version.
    /// Returns (server id, name).
    pub fn check_server(&self, base: &Url) -> Result<(String, String)> {
        let body = self.anonymous(base, Method::GET, "/System/Info/Public", &[], None)?;
        let info: Value = serde_json::from_slice(&body).map_err(|_| Error::Protocol)?;
        if info["ProductName"].as_str() != Some(PRODUCT) {
            return Err(Error::Input("This is not a Jellyfin server"));
        }
        let supported = info["Version"]
            .as_str()
            .and_then(version)
            .is_some_and(|v| v >= MIN_VERSION);
        if !supported {
            return Err(Error::Input("Jellyfin 10.9 or newer is required"));
        }
        Ok((
            info["Id"].as_str().unwrap_or_default().to_owned(),
            info["ServerName"].as_str().unwrap_or_default().to_owned(),
        ))
    }

    /// `AuthenticationResult` as a [`Login`]; `server_id` is the public
    /// info's when the reply has none.
    fn login_from(body: &[u8], server_id: &str) -> Result<Login> {
        let result: Authentication = serde_json::from_slice(body).map_err(|_| Error::Protocol)?;
        let user_id = normalize_id(&result.user.id).ok_or(Error::Protocol)?;
        let server_id = result
            .server_id
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| server_id.to_owned());
        if result.access_token.is_empty() || server_id.is_empty() {
            return Err(Error::Protocol);
        }
        Ok(Login {
            token: result.access_token,
            user_id,
            server_id,
        })
    }

    /// Sign in with a user name and password. The password is only in the
    /// request body: never stored, logged or kept after this call.
    pub fn login(&self, base: &Url, username: &str, password: &str) -> Result<Login> {
        let (server_id, _) = self.check_server(base)?;
        let body = self.anonymous(
            base,
            Method::POST,
            "/Users/AuthenticateByName",
            &[],
            Some(json!({"Username": username, "Pw": password})),
        )?;
        Self::login_from(&body, &server_id)
    }

    /// Ask for a Quick Connect code (the server must have the feature on).
    pub fn quick_connect_start(&self, base: &Url) -> Result<QuickConnect> {
        self.check_server(base)?;
        let enabled = self.anonymous(base, Method::GET, "/QuickConnect/Enabled", &[], None)?;
        if !serde_json::from_slice::<bool>(&enabled).map_err(|_| Error::Protocol)? {
            return Err(Error::Input("Quick Connect is not enabled on this server"));
        }
        let body = self.anonymous(base, Method::POST, "/QuickConnect/Initiate", &[], None)?;
        let reply: Value = serde_json::from_slice(&body).map_err(|_| Error::Protocol)?;
        let (secret, code) = (reply["Secret"].as_str(), reply["Code"].as_str());
        match (secret, code) {
            (Some(secret), Some(code)) if !secret.is_empty() && !code.is_empty() => {
                Ok(QuickConnect {
                    secret: secret.to_owned(),
                    code: code.to_owned(),
                })
            }
            _ => Err(Error::Protocol),
        }
    }

    /// Whether the user approved the request. An unknown or expired secret
    /// is `Err(Http(404))`.
    pub fn quick_connect_approved(&self, base: &Url, secret: &str) -> Result<bool> {
        let body = self.anonymous(
            base,
            Method::GET,
            "/QuickConnect/Connect",
            &[("secret", secret)],
            None,
        )?;
        let reply: Value = serde_json::from_slice(&body).map_err(|_| Error::Protocol)?;
        Ok(reply["Authenticated"].as_bool() == Some(true))
    }

    /// Exchange an approved Quick Connect secret for a session.
    pub fn quick_connect_login(&self, base: &Url, secret: &str) -> Result<Login> {
        let (server_id, _) = self.check_server(base)?;
        let body = self.anonymous(
            base,
            Method::POST,
            "/Users/AuthenticateWithQuickConnect",
            &[],
            Some(json!({"Secret": secret})),
        )?;
        Self::login_from(&body, &server_id)
    }

    /// End the session on the server; best effort and bounded, since sign-out
    /// must not wait on an unreachable server.
    pub fn logout(&self, conn: &Conn) {
        let Ok(request) = self
            .url(conn, "/Sessions/Logout", &[])
            .and_then(|url| self.builder(Method::POST, url, &conn.token))
        else {
            return;
        };
        let _ = crate::rt::block_on(async move {
            tokio::time::timeout(Duration::from_secs(3), request.send())
                .await
                .ok()
        });
    }

    // ---- reads ---------------------------------------------------------

    fn id(&self, id: &str) -> Result<String> {
        normalize_id(id).ok_or(Error::Input("Expected a server identifier"))
    }

    /// The kind of item `id`, for a children read whose caller did not know.
    fn kind_of(&self, conn: &Conn, id: &str) -> Result<ItemKind> {
        let c = dto::container(&self.get(
            conn,
            &request(
                &Query::Item {
                    id: id.into(),
                    stations: false,
                },
                None,
            )?,
        )?)?;
        c.items
            .into_iter()
            .next()
            .map(|item| item.kind)
            .ok_or(Error::Http(404))
    }

    fn hubs(&self, conn: &Conn, section: &str) -> Result<Container> {
        let section = self.id(section)?;
        let mut hubs = Vec::new();
        for (name, identifier, title) in HUBS {
            let request = hub_request(&section, name, true).ok_or_else(unsupported)?;
            let items = dto::items(&self.get(conn, &request)?)?.items;
            if !items.is_empty() {
                hubs.push(dto::hub(
                    format!("{identifier}.{section}"),
                    title,
                    hub_key(&section, name),
                    items,
                ));
            }
        }
        Ok(Container {
            size: hubs.len(),
            hubs,
            ..Default::default()
        })
    }

    /// The occurrence ids of a playlist, in order, across all pages (an
    /// entry without an id keeps its place as an empty string).
    fn playlist_entries(&self, conn: &Conn, playlist: &str) -> Result<Vec<String>> {
        let mut entries = Vec::new();
        loop {
            let page = Request::new(format!("/Playlists/{playlist}/Items"), &[]).page(
                Some(Page {
                    start: entries.len(),
                    size: ENTRY_PAGE,
                }),
                None,
            );
            let read = dto::items(&self.get(conn, &page)?)?;
            // Advance by what the server sent, not by what converted.
            entries.extend(read.entry_ids);
            let done = read.raw < ENTRY_PAGE || read.total.is_some_and(|t| entries.len() >= t);
            if done || entries.len() >= MAX_ENTRIES {
                return Ok(entries);
            }
        }
    }

    fn entry<'a>(&self, entry: &'a str) -> Result<&'a str> {
        if entry.is_empty()
            || entry.len() > 64
            || !entry
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(Error::Input("Choose a playlist occurrence"));
        }
        Ok(entry)
    }

    fn report(&self, conn: &Conn, action: &Action) -> Result<()> {
        let Action::Timeline {
            id,
            state,
            position,
            reporting,
            ..
        } = action
        else {
            return Err(unsupported());
        };
        let id = self.id(id)?;
        let session = if reporting.play_session.is_empty() {
            &reporting.occurrence
        } else {
            &reporting.play_session
        };
        let mut body = json!({
            "ItemId": id,
            "MediaSourceId": id,
            "PlaySessionId": session,
            "PositionTicks": position.saturating_mul(TICKS_PER_MS),
            "CanSeek": true,
            "PlayMethod": if reporting.transcode { "Transcode" } else { "DirectPlay" },
        });
        let path = if state == "stopped" {
            "/Sessions/Playing/Stopped"
        } else {
            body["IsPaused"] = json!(state == "paused");
            if reporting.start && state == "playing" {
                "/Sessions/Playing"
            } else {
                "/Sessions/Playing/Progress"
            }
        };
        self.write(conn, Method::POST, path, &[], Some(body))
            .map(drop)
    }
}

impl MediaServer for Jellyfin {
    fn bind_view(&self, scope: Option<(tokio::sync::watch::Receiver<u64>, u64)>) {
        self.view.bind(scope);
    }
    fn cancelled(&self) -> bool {
        self.view.cancelled()
    }
    fn capabilities(&self) -> Caps {
        CAPS
    }
    fn valid_id<'a>(&self, id: &'a str) -> Result<&'a str> {
        normalize_id(id)
            .map(|_| id)
            .ok_or(Error::Input("Expected a server identifier"))
    }
    fn advertised_key(&self, key: &str) -> Result<(String, Vec<(String, String)>)> {
        advertised_parts(key, is_credential)
    }
    fn is_library_key(&self, key: &str) -> bool {
        key.strip_prefix("/Items/")
            .is_some_and(|id| normalize_id(id).is_some())
    }
    fn parse_url(&self, value: &str) -> Result<Url> {
        server_url_with_base_path(value)
    }
    /// The server and the user, not the address or the token: signing in
    /// again, or reaching the server by another address, keeps the downloads
    /// and the history.
    fn namespace(&self, conn: &Conn) -> String {
        crate::sha256_hex(&format!(
            "jellyfin\0{}\0{}",
            self.server_id,
            conn.user_id.as_deref().unwrap_or_default()
        ))
    }
    fn auth_headers(&self, conn: &Conn) -> HeaderMap {
        credential_headers(&self.device, &self.client_identifier, &conn.token)
    }
    fn stream_policy(&self, conn: &Conn) -> StreamPolicy {
        StreamPolicy::new(
            Arc::new(JellyfinRules {
                device_id: self.client_identifier.clone(),
                user_id: conn.user_id.clone(),
            }),
            conn.base.clone(),
            self.auth_headers(conn),
        )
    }
    fn children_query(&self, key: &str, kind: Option<&ItemKind>) -> Result<Query> {
        let id = key.strip_prefix("/Items/").unwrap_or(key);
        let id = self.id(id)?;
        Ok(if kind == Some(&ItemKind::Playlist) {
            Query::PlaylistItems { id }
        } else {
            Query::Children {
                id,
                kind: kind.cloned().unwrap_or_default(),
            }
        })
    }
    /// Artists are not folder parents of their albums, so which listing
    /// answers depends on the parent's kind.
    fn children_need_kind(&self) -> bool {
        true
    }
    fn filter_schema(&self, _section: &str, _kind: &ItemKind) -> Option<Query> {
        None
    }
    /// Genres of the library; Jellyfin has no mood or style tags.
    fn filter_choices(
        &self,
        section: &str,
        _kind: &ItemKind,
        field: crate::server::FilterField,
        _meta: Option<&serde_json::Value>,
    ) -> Result<Option<Query>> {
        if field != crate::server::FilterField::Genre {
            return Ok(None);
        }
        Ok(Some(Query::advertised(
            "/Genres",
            vec![("parentId".into(), self.id(section)?)],
        )))
    }
    fn reports_start(&self) -> bool {
        true
    }
    fn mix_query(&self, id: &str, _kind: RadioKind, limit: u32) -> Option<Query> {
        // The server tells the seed kind from the item itself.
        let id = normalize_id(id)?;
        Some(Query::advertised(
            &format!("/Items/{id}/InstantMix"),
            vec![("limit".into(), limit.to_string())],
        ))
    }
    fn offline_key(&self, query: &Query, page: Option<Page>) -> Result<OfflineKey> {
        let request = match query {
            // Artists are always keyed by the primary endpoint.
            Query::Browse { section, spec } => browse_request(&self.id(section)?, spec, false)?,
            _ => request(query, page)?,
        };
        let mut params = request.params;
        if let (Query::Browse { spec, .. }, Some(page)) = (query, page) {
            let window = Request::new("", &[]).page(Some(page), spec.limit);
            params.extend(window.params);
        }
        Ok((request.path, params))
    }
    fn fetch(&self, conn: &Conn, query: &Query, page: Option<Page>) -> Result<Container> {
        match query {
            Query::Sections => return dto::views(&self.get(conn, &request(query, page)?)?),
            Query::Hubs { section } => return self.hubs(conn, section),
            Query::Children { id, kind } if kind.is_empty() => {
                let id = self.id(id)?;
                let kind = self.kind_of(conn, &id)?;
                return self.fetch(conn, &Query::Children { id, kind }, page);
            }
            Query::Advertised { path, .. } if path == "/Genres" => {
                return dto::named_sections(&self.get(conn, &request(query, page)?)?);
            }
            Query::Search { query, limit } => {
                let mut merged = Container::default();
                for request in search_requests(query, *limit) {
                    let part = dto::container(&self.get(conn, &request)?)?;
                    merged.items.extend(part.items);
                }
                merged.size = merged.items.len();
                return Ok(merged);
            }
            Query::Browse { section, spec } if spec.kind == ItemKind::Artist => {
                let section = self.id(section)?;
                let primary = browse_request(&section, spec, false)?.page(page, spec.limit);
                return match self.get(conn, &primary) {
                    // Servers without the album-artist endpoint list the
                    // artists as ordinary items.
                    Err(Error::Http(404)) => {
                        let fallback = browse_request(&section, spec, true)?.page(page, spec.limit);
                        dto::container(&self.get(conn, &fallback)?)
                    }
                    reply => dto::container(&reply?),
                };
            }
            _ => {}
        }
        dto::container(&self.get(conn, &request(query, page)?)?)
    }
    fn act(&self, conn: &Conn, action: &Action) -> Result<()> {
        let none = || -> Option<Value> { None };
        match action {
            Action::Rate { id, rating } => {
                // Jellyfin has a favourite flag, not a star rating.
                let method = if *rating > 0 {
                    Method::POST
                } else {
                    Method::DELETE
                };
                self.write(
                    conn,
                    method,
                    &format!("/UserFavoriteItems/{}", self.id(id)?),
                    &[],
                    none(),
                )
                .map(drop)
            }
            Action::Scrobble { id, played_at } => self
                .write(
                    conn,
                    Method::POST,
                    &format!("/UserPlayedItems/{}", self.id(id)?),
                    &[("datePlayed", iso8601(*played_at))],
                    none(),
                )
                .map(drop),
            Action::Timeline { .. } => self.report(conn, action),
            Action::PlaylistRename { id, title } => self
                .write(
                    conn,
                    Method::POST,
                    &format!("/Playlists/{}", self.id(id)?),
                    &[],
                    Some(json!({"Name": title})),
                )
                .map(drop),
            Action::PlaylistDelete { id } => self
                .write(
                    conn,
                    Method::DELETE,
                    &format!("/Items/{}", self.id(id)?),
                    &[],
                    none(),
                )
                .map(drop),
            Action::PlaylistAdd {
                id,
                content: PlaylistContent::Tracks(ids),
            } => {
                let playlist = self.id(id)?;
                let ids = ids
                    .iter()
                    .map(|id| self.id(id))
                    .collect::<Result<Vec<_>>>()?;
                for chunk in ids.chunks(ADD_CHUNK) {
                    self.write(
                        conn,
                        Method::POST,
                        &format!("/Playlists/{playlist}/Items"),
                        &[("ids", chunk.join(","))],
                        none(),
                    )?;
                }
                Ok(())
            }
            Action::PlaylistAdd { .. } => Err(unsupported()),
            Action::PlaylistRemove { id, item } => {
                let playlist = self.id(id)?;
                let entry = self.entry(item)?;
                // An entry id that may be shared is never sent (see `unique`).
                let entries = self.playlist_entries(conn, &playlist)?;
                unique(&entries, entry)?;
                self.write(
                    conn,
                    Method::DELETE,
                    &format!("/Playlists/{playlist}/Items"),
                    &[("entryIds", entry.to_owned())],
                    none(),
                )
                .map(drop)
            }
            Action::PlaylistMove {
                id,
                item,
                after,
                index,
            } => {
                let playlist = self.id(id)?;
                let entry = self.entry(item)?;
                let after = after.as_deref().map(|a| self.entry(a)).transpose()?;
                // Every branch reads the list as the server has it now: the
                // entries must exist and be unambiguous before any write. The
                // UI names the entry to go after, not a position, which comes
                // from that same list.
                let entries = self.playlist_entries(conn, &playlist)?;
                unique(&entries, entry)?;
                if let Some(after) = after {
                    unique(&entries, after)?;
                }
                let target = match (after, index) {
                    (None, Some(index)) => *index,
                    (None, None) => 0,
                    (Some(after), _) => {
                        entries
                            .iter()
                            .filter(|e| *e != entry)
                            .position(|e| e == after)
                            .ok_or(Error::Input("The playlist entry was not found"))?
                            + 1
                    }
                };
                self.write(
                    conn,
                    Method::POST,
                    &format!("/Playlists/{playlist}/Items/{entry}/Move/{target}"),
                    &[],
                    none(),
                )?;
                // A move of an unknown entry is a silent success: read the
                // list back and require the entry where it was asked to go.
                let entries = self.playlist_entries(conn, &playlist)?;
                let position = entries.iter().position(|e| e == entry);
                let moved = match (after, position) {
                    (_, None) => false,
                    (Some(after), Some(at)) => at > 0 && entries[at - 1] == after,
                    (None, Some(at)) => at == target.min(entries.len().saturating_sub(1)),
                };
                if moved {
                    Ok(())
                } else {
                    Err(Error::Input("The server did not move the playlist entry"))
                }
            }
        }
    }
    fn create_playlist(
        &self,
        conn: &Conn,
        title: &str,
        content: &PlaylistContent,
    ) -> Result<Container> {
        let PlaylistContent::Tracks(ids) = content else {
            return Err(unsupported());
        };
        let ids = ids
            .iter()
            .map(|id| self.id(id))
            .collect::<Result<Vec<_>>>()?;
        let body = self.write(
            conn,
            Method::POST,
            "/Playlists",
            &[],
            Some(json!({
                "Name": title,
                "Ids": ids,
                "UserId": conn.user_id,
                "MediaType": "Audio",
            })),
        )?;
        let reply: Value = serde_json::from_slice(&body).map_err(|_| Error::Protocol)?;
        let id = reply["Id"]
            .as_str()
            .and_then(normalize_id)
            .ok_or(Error::ProtocolAt("playlist creation"))?;
        Ok(Container {
            size: 1,
            items: vec![Item {
                rating_key: id.clone(),
                key: format!("/Items/{id}"),
                title: title.into(),
                kind: ItemKind::Playlist,
                playlist_type: "audio".into(),
                leaf_count: Some(ids.len()),
                ..Default::default()
            }],
            ..Default::default()
        })
    }
    fn create_radio(&self, _conn: &Conn, _station: &str) -> Result<Container> {
        Err(unsupported())
    }
    fn lyrics(&self, conn: &Conn, stream_key: &str) -> Result<Vec<Line>> {
        let id = stream_key
            .strip_prefix("/Audio/")
            .and_then(|rest| rest.strip_suffix("/Lyrics"))
            .and_then(normalize_id)
            .ok_or(Error::Input("Expected a server-relative path"))?;
        let url = self.url(conn, &format!("/Audio/{id}/Lyrics"), &[])?;
        match self.exchange(
            self.builder(Method::GET, url, &conn.token)?,
            LYRICS_LIMIT,
            false,
        ) {
            Ok(body) => dto::lyrics(&body),
            // The server answers 404 for an item without lyrics.
            Err(Error::Http(404)) => Ok(vec![]),
            Err(error) => Err(error),
        }
    }
    fn stream_url(&self, conn: &Conn, part_key: &str) -> Result<String> {
        Ok(advertised_url(&conn.base, part_key, is_credential)?.into())
    }
    fn transcode_stream(&self, conn: &Conn, id: &str, kbps: u32, session: &str) -> Result<String> {
        let rules = JellyfinRules {
            device_id: self.client_identifier.clone(),
            user_id: conn.user_id.clone(),
        };
        Ok(rules.transcode(&conn.base, id, kbps, session)?.into())
    }
    fn transcode_probe(
        &self,
        conn: &Conn,
        id: &str,
        kbps: u32,
        session: &str,
    ) -> Result<(usize, String, bool)> {
        let rules = JellyfinRules {
            device_id: self.client_identifier.clone(),
            user_id: conn.user_id.clone(),
        };
        let url = rules.transcode(&conn.base, id, kbps, session)?;
        // `headers` replaces the default JSON Accept instead of appending.
        let request = self
            .builder(Method::GET, url.clone(), &conn.token)?
            .headers(HeaderMap::from_iter([
                (reqwest::header::ACCEPT, HeaderValue::from_static("*/*")),
                (
                    reqwest::header::ACCEPT_ENCODING,
                    HeaderValue::from_static("identity"),
                ),
            ]));
        let result = self.view.run(crate::plex::read_audio_probe(request));
        let stopped = rules.stop(&url).is_some_and(|(method, stop)| {
            self.builder(method, stop, &conn.token)
                .and_then(|request| self.exchange(request, JSON_LIMIT, false))
                .is_ok()
        });
        let (bytes, mime) = result?;
        Ok((bytes, mime, stopped))
    }
    fn probe(
        &self,
        conn: &Conn,
        id: &str,
    ) -> Result<impl Future<Output = Probe> + Send + 'static + use<>> {
        let id = self.id(id)?;
        let url = self.url(conn, &format!("/Items/{id}"), &[])?;
        let request = self.builder(Method::GET, url, &conn.token)?;
        Ok(async move {
            match tokio::time::timeout(PROBE_TIMEOUT, request.send()).await {
                // The server's own 404 carries its response-time header; a
                // proxy's does not.
                Ok(Ok(response)) => classify_probe(
                    response.status().as_u16(),
                    response.headers().contains_key("x-response-time-ms"),
                ),
                _ => Probe::Unknown,
            }
        })
    }
}

#[cfg(test)]
mod jellyfin_core_tests;
#[cfg(test)]
mod jellyfin_fixture_tests;
#[cfg(test)]
mod jellyfin_tests;
