//! Media-server boundary. Core speaks semantic [`Query`] and [`Action`]
//! values and asks the backend for everything server-shaped: identifier rules,
//! URLs, credentials in HTTP headers, the offline namespace, capabilities and
//! the transcode/seek/stop rules the audio engine and downloaders follow. Only
//! the `impl MediaServer for Plex` below knows Plex paths, parameters, URIs and
//! headers.
pub use crate::filters::Music;
use crate::{
    jellyfin::Jellyfin,
    lyrics::Line,
    model::{Container, ItemKind, Pin, Resource},
    plex::{Plex, Probe},
    Error, Result,
};
use futures_util::future::Either;
use reqwest::{
    header::{HeaderMap, HeaderValue},
    Method,
};
use serde::Serialize;
use std::{future::Future, sync::Arc};
use url::Url;

/// Where and as whom to talk: the server base URL, its credential and the
/// signed-in user (backends whose credential does not identify one).
#[derive(Clone)]
pub struct Conn {
    pub base: Url,
    pub token: String,
    pub user_id: Option<String>,
}

/// Features a server may lack. Published in the status JSON (`capabilities`)
/// so the UI can hide the matching actions; Core refuses the commands that need
/// a missing one (see [`Command::requires`](crate::Command)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Caps {
    /// Sonic similarity ("nearest" tracks, sonic radio).
    pub sonic: bool,
    pub sonic_adventure: bool,
    /// Radio stations the server advertises for an item.
    pub server_stations: bool,
    pub smart_playlists: bool,
    /// Server-advertised hub keys (`HubItems`).
    pub server_hubs: bool,
    /// Alphabet index (`firstCharacter`).
    pub first_character: bool,
    pub star_rating: bool,
    /// Listing filters by mood, by style and by minimum rating
    /// ([`Music::mood`], [`Music::style`], [`Music::min_rating`]).
    pub mood_filter: bool,
    pub style_filter: bool,
    pub rating_filter: bool,
}
impl Caps {
    /// Everything Plex offers.
    pub const ALL: Self = Self {
        sonic: true,
        sonic_adventure: true,
        server_stations: true,
        smart_playlists: true,
        server_hubs: true,
        first_character: true,
        star_rating: true,
        mood_filter: true,
        style_filter: true,
        rating_filter: true,
    };
    pub fn allows(&self, needed: Capability) -> bool {
        match needed {
            Capability::Sonic => self.sonic,
            Capability::SonicAdventure => self.sonic_adventure,
            Capability::ServerStations => self.server_stations,
            Capability::SmartPlaylists => self.smart_playlists,
            Capability::ServerHubs => self.server_hubs,
            Capability::FirstCharacter => self.first_character,
            Capability::StarRating => self.star_rating,
            Capability::MoodFilter => self.mood_filter,
            Capability::StyleFilter => self.style_filter,
            Capability::RatingFilter => self.rating_filter,
        }
    }
}

/// One field of [`Caps`], named by what a command needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    Sonic,
    SonicAdventure,
    ServerStations,
    SmartPlaylists,
    ServerHubs,
    FirstCharacter,
    StarRating,
    MoodFilter,
    StyleFilter,
    RatingFilter,
}

/// The refusal for a command the server cannot serve.
pub(crate) fn unsupported() -> Error {
    Error::Input("This server does not support this feature")
}

/// Window of a paged read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Page {
    pub start: usize,
    pub size: usize,
}

/// Stable identity of a read for the offline library cache. For Plex this is
/// the exact request path plus ordered query pairs used before the refactor,
/// so saved snapshots stay valid.
pub type OfflineKey = (String, Vec<(String, String)>);

/// Every semantic read Core performs.
// Tests iterate the variants to prove every one has a golden wire row.
#[cfg_attr(
    test,
    derive(strum::EnumDiscriminants),
    strum_discriminants(derive(strum::EnumIter))
)]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Query {
    Sections,
    /// One artist/album/track/playlist metadata record; `stations` also
    /// requests the radio stations the server offers for it.
    Item {
        id: String,
        stations: bool,
    },
    /// The children of an artist (albums), album (tracks) or playlist. `kind`
    /// is the parent's kind, which backends whose artists are not folder
    /// parents of their albums need; Plex ignores it.
    Children {
        id: String,
        kind: ItemKind,
    },
    AllLeaves {
        id: String,
    },
    Similar {
        id: String,
        count: u32,
    },
    Nearest {
        id: String,
        limit: u32,
    },
    /// A section listing (see [`BrowseSpec`]).
    Browse {
        section: String,
        spec: BrowseSpec,
    },
    /// The filter schema of a section's listings of `kind`: the server's
    /// filter field names and choice lists, for backends that have one (see
    /// [`MediaServer::filter_schema`]).
    FilterSchema {
        section: String,
        kind: ItemKind,
    },
    FirstCharacter {
        section: String,
    },
    ComputePath {
        section: String,
        start: String,
        end: String,
    },
    Hubs {
        section: String,
    },
    Search {
        query: String,
        limit: u32,
    },
    Playlists,
    Playlist {
        id: String,
    },
    PlaylistItems {
        id: String,
    },
    /// A window of a server-side play queue (continuous radio).
    PlayQueueWindow {
        id: u64,
        center: u64,
    },
    /// Escape hatch for opaque keys the server itself handed out (hub,
    /// filter-choice keys). Path and query are in the server's vocabulary.
    Advertised {
        path: String,
        params: Vec<(String, String)>,
    },
}

impl Query {
    pub fn browse(section: &str, spec: BrowseSpec) -> Self {
        Self::Browse {
            section: section.into(),
            spec,
        }
    }
    /// A server-handed key (path plus optional query), split for the escape hatch.
    pub fn advertised(path: &str, params: Vec<(String, String)>) -> Self {
        Self::Advertised {
            path: path.into(),
            params,
        }
    }
}

/// How a listing is ordered. Each sort has the one direction the UI offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sort {
    /// By title, A to Z.
    Title,
    /// Most recently added first.
    Added,
    /// Newest release year first.
    Year,
    /// Most recently played first.
    LastPlayed,
    /// Most played first.
    PlayCount,
    Random,
}
impl Sort {
    pub const ALL: [Sort; 6] = [
        Sort::Title,
        Sort::Added,
        Sort::Year,
        Sort::LastPlayed,
        Sort::PlayCount,
        Sort::Random,
    ];
}

/// A filter field whose spelling a server may define in its filter schema
/// (Plex: `album.genre` for tracks), see [`MediaServer::filter_fields`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FilterField {
    Genre,
    Mood,
    Style,
    Year,
    /// The user's rating ([`Music::min_rating`]).
    Rating,
    /// The play count ([`Music::unplayed`]).
    ViewCount,
}

/// A semantic section listing: what to list, in which order and filtered how.
/// Backends map it to their own request; the page window is the separate
/// [`Page`] of the read.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BrowseSpec {
    /// Artist, album or track (playlists are listed by [`Query::Playlists`]).
    pub kind: ItemKind,
    pub sort: Sort,
    /// The user's music filter: genre, mood and style ids, title contains,
    /// year range, minimum rating, unplayed. Validated by Core.
    pub filter: Music,
    /// The server's spelling of filter fields, learned from its filter
    /// schema; fields not listed use the backend's default spelling.
    pub fields: Vec<(FilterField, String)>,
    /// Only favourites.
    pub favourite: bool,
    /// Only items played at least once.
    pub played: bool,
    /// Title contains (the browse search box). Mutually exclusive with
    /// `filter.title`, the same condition from the filtered listing: a spec
    /// carrying both is refused by [`BrowseSpec::check`].
    pub search: Option<String>,
    /// At most this many items (smart playlists).
    pub limit: Option<usize>,
}
impl BrowseSpec {
    /// Refuse a spec Core never builds: two title conditions at once.
    pub(crate) fn check(&self) -> Result<()> {
        if self.search.is_some() && !self.filter.title.trim().is_empty() {
            return Err(Error::Input("Invalid music filter values"));
        }
        Ok(())
    }
    /// Every item of `kind` in `sort` order, unfiltered.
    pub fn new(kind: ItemKind, sort: Sort) -> Self {
        Self {
            kind,
            sort,
            filter: Music::default(),
            fields: vec![],
            favourite: false,
            played: false,
            search: None,
            limit: None,
        }
    }
}

/// A smart playlist's source as the server describes it, for the rule editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmartSource {
    pub section: String,
    pub filter: Music,
    pub sort: Sort,
    pub limit: usize,
    /// Whether every part of the source maps onto [`Music`], a [`Sort`] and a
    /// limit (otherwise the editor would drop rules it cannot show).
    pub editable: bool,
}

/// What a created/extended playlist is made of.
// Tests iterate the variants to prove every one has a golden wire row.
#[cfg_attr(
    test,
    derive(strum::EnumDiscriminants),
    strum_discriminants(derive(strum::EnumIter))
)]
#[derive(Clone, Debug, PartialEq, Eq)]
// Built once per playlist edit: the variant size is irrelevant.
#[allow(clippy::large_enum_variant)]
pub enum PlaylistContent {
    Tracks(Vec<String>),
    /// A smart (filter) playlist: a track listing of `section`.
    Smart {
        section: String,
        spec: BrowseSpec,
    },
}

/// What a playback report needs besides the position: the occurrence it
/// belongs to, the server session id of that occurrence, whether it is the
/// first report of the occurrence and how the audio is delivered. Plex
/// ignores all of it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reporting {
    pub occurrence: String,
    pub play_session: String,
    /// The first `playing` report of the occurrence (servers that count a
    /// play when it starts hear it once).
    pub start: bool,
    /// The audio is a server transcode, not the original file.
    pub transcode: bool,
}

/// Writes whose reply carries nothing Core needs.
// Tests iterate the variants to prove every one has a golden wire row.
#[cfg_attr(
    test,
    derive(strum::EnumDiscriminants),
    strum_discriminants(derive(strum::EnumIter))
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Rate {
        id: String,
        rating: u8,
    },
    Scrobble {
        id: String,
        /// When it was played (Unix seconds), for servers that record it.
        played_at: u64,
    },
    Timeline {
        id: String,
        state: String,
        position: u64,
        duration: u64,
        continuing: bool,
        /// (play queue id, play queue item id) when playing a radio queue.
        queue: Option<(u64, u64)>,
        reporting: Reporting,
    },
    PlaylistRename {
        id: String,
        title: String,
    },
    PlaylistDelete {
        id: String,
    },
    /// Append tracks; for a smart playlist, replace its filter.
    PlaylistAdd {
        id: String,
        content: PlaylistContent,
    },
    PlaylistRemove {
        id: String,
        /// The server's id of the playlist occurrence (opaque string).
        item: String,
    },
    PlaylistMove {
        id: String,
        item: String,
        /// The occurrence the item goes after (None: to the top).
        after: Option<String>,
        /// The 0-based target position, for backends that move by index.
        index: Option<usize>,
    },
}

/// Plex sign-in and server discovery: account level, not server level. Other
/// backends have their own login methods on their own types (called from their
/// own commands); [`Backend::account`] refuses them here instead of forcing
/// dummy implementations.
pub trait Account {
    fn pin(&self) -> Result<Pin>;
    fn poll(&self, id: u64, code: &str) -> Result<Pin>;
    fn resources(&self, account_token: &str) -> Result<Vec<Resource>>;
    /// URL the user opens in a browser to approve `pin`.
    fn login_url(&self, client_identifier: &str, pin: &Pin) -> String;
}

/// The backend-specific half of [`StreamPolicy`]: how a server spells
/// transcodes, seeks, session teardown and raw media/artwork/metadata requests.
/// Stateless apart from what a backend builds into it (device ids, ...).
pub trait StreamRules: Send + Sync + 'static {
    /// The backend's identifier rule (see [`MediaServer::valid_id`]).
    fn valid_id(&self, id: &str) -> bool;
    /// The progressive transcode URL of item `id` at `kbps` (no credential).
    fn transcode_url(&self, base: &Url, id: &str, kbps: u32, session: &str) -> Result<Url>;
    /// Whether `url` is one of this backend's transcode URLs (not seekable by
    /// byte range).
    fn is_transcode(&self, url: &Url) -> bool;
    /// The transcode `url` restarted at `milliseconds`, under a new session.
    fn seek(&self, url: &Url, milliseconds: u64) -> Result<Url>;
    /// The request that tears down the server session of a transcode `url`.
    fn stop(&self, url: &Url) -> Option<(Method, Url)>;
    /// The URL a downloader fetches for a media part key.
    fn download_url(&self, base: &Url, part_key: &str) -> Result<Url>;
    /// The URL of the image at server-relative `path`.
    fn artwork_url(&self, base: &Url, path: &str) -> Result<Url>;
    /// The URL of one item's metadata, for a downloader that lacks its part.
    fn metadata_url(&self, base: &Url, id: &str) -> Result<Url>;
    /// The metadata reply of [`metadata_url`](Self::metadata_url).
    fn parse_metadata(&self, body: &[u8]) -> Result<Container>;
    /// Headers identifying the client to the server for one downloader.
    fn client_headers(&self, client_identifier: &str) -> HeaderMap;
}

/// What the audio engine, the downloaders and the artwork loader need to talk
/// to the media server without a backend object: credential headers (never a
/// URL parameter) plus the backend's [`StreamRules`]. A clone is cheap and
/// `Send`, so it travels with an audio `Source`, a cache batch or an image
/// request. The default is Plex rules with no credential and no base (tests,
/// local `file:` sources).
#[derive(Clone)]
pub struct StreamPolicy {
    rules: Arc<dyn StreamRules>,
    base: Option<Url>,
    headers: HeaderMap,
}
impl Default for StreamPolicy {
    fn default() -> Self {
        Self {
            rules: Arc::new(PlexRules),
            base: None,
            headers: HeaderMap::new(),
        }
    }
}
impl StreamPolicy {
    pub fn new(rules: Arc<dyn StreamRules>, base: Url, headers: HeaderMap) -> Self {
        Self {
            rules,
            base: Some(base),
            headers,
        }
    }
    /// The Plex policy for `base` and `token` (empty: no credential).
    pub fn plex(base: &Url, token: &str) -> Self {
        Self::new(Arc::new(PlexRules), base.clone(), plex_headers(token))
    }
    /// The credential headers, whatever the request. Requests use
    /// [`headers_for`](Self::headers_for).
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }
    /// Whether `url` is on this policy's server: same origin (scheme, host,
    /// port) and at or below its base path. A policy without a base covers
    /// nothing.
    pub fn covers(&self, url: &Url) -> bool {
        self.base.as_ref().is_some_and(|base| {
            let prefix = base.path().trim_end_matches('/');
            url.origin() == base.origin()
                && (prefix.is_empty()
                    || url.path() == prefix
                    || url
                        .path()
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('/')))
        })
    }
    /// The headers to attach to a request for `url`: the credential only when
    /// [`covers`](Self::covers) holds, so a URL from anywhere else (a stale or
    /// foreign stream, a stop or download URL built from untrusted input)
    /// never receives it.
    pub fn headers_for(&self, url: &Url) -> HeaderMap {
        if self.covers(url) {
            self.headers.clone()
        } else {
            HeaderMap::new()
        }
    }
    pub fn valid_id(&self, id: &str) -> bool {
        self.rules.valid_id(id)
    }
    pub fn base(&self) -> Result<&Url> {
        self.base
            .as_ref()
            .ok_or(Error::Input("Connect to a server first"))
    }
    pub fn transcode_url(&self, id: &str, kbps: u32, session: &str) -> Result<Url> {
        self.rules.transcode_url(self.base()?, id, kbps, session)
    }
    pub fn is_transcode(&self, url: &Url) -> bool {
        self.rules.is_transcode(url)
    }
    /// `uri` positioned at `milliseconds`, and whether it was a transcode
    /// (only those restart; other URLs are returned unchanged and seek by
    /// byte range).
    pub fn seek(&self, uri: &str, milliseconds: u64) -> Result<(String, bool)> {
        let url = Url::parse(uri)?;
        if !self.rules.is_transcode(&url) {
            return Ok((uri.to_owned(), false));
        }
        Ok((self.rules.seek(&url, milliseconds)?.into(), true))
    }
    pub fn stop(&self, url: &Url) -> Option<(Method, Url)> {
        self.rules.stop(url)
    }
    pub fn download_url(&self, part_key: &str) -> Result<Url> {
        self.rules.download_url(self.base()?, part_key)
    }
    pub fn artwork_url(&self, path: &str) -> Result<Url> {
        self.rules.artwork_url(self.base()?, path)
    }
    pub fn metadata_url(&self, id: &str) -> Result<Url> {
        self.rules.metadata_url(self.base()?, id)
    }
    pub fn parse_metadata(&self, body: &[u8]) -> Result<Container> {
        self.rules.parse_metadata(body)
    }
    pub fn client_headers(&self, client_identifier: &str) -> HeaderMap {
        self.rules.client_headers(client_identifier)
    }
    /// Defence in depth for a URL that still carries a legacy `X-Plex-Token`
    /// query: the decoder strips it and passes it here so it travels as a
    /// header. An existing credential header wins; other headers are kept.
    /// A policy without a base (no connection) adopts the origin of `url`,
    /// the server the credential was written for.
    pub(crate) fn with_legacy_token(mut self, url: &Url, token: &str) -> Self {
        if token.is_empty() || self.headers.contains_key(PLEX_TOKEN) {
            return self;
        }
        self.headers.extend(plex_headers(token));
        if self.base.is_none() {
            if let Ok(origin) = Url::parse(&url.origin().ascii_serialization()) {
                self.base = Some(origin);
            }
        }
        self
    }
}

const PLEX_TOKEN: &str = "x-plex-token";

fn plex_headers(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(mut value) = HeaderValue::from_str(token) {
        if !token.is_empty() {
            value.set_sensitive(true);
            headers.insert(PLEX_TOKEN, value);
        }
    }
    headers
}

/// A media server client. `Clone` hands a worker thread its own client: a
/// clone shares connections/credentials-by-value but never the view
/// cancellation scope of the original (see `impl Clone for Plex`).
pub trait MediaServer: Clone + Send + Sync + 'static {
    fn bind_view(&self, scope: Option<(tokio::sync::watch::Receiver<u64>, u64)>);
    fn cancelled(&self) -> bool;

    /// What this server can do; Core refuses commands that need more.
    fn capabilities(&self) -> Caps;
    /// An identifier that may be put into a request path: validated by the
    /// backend that builds the path.
    fn valid_id<'a>(&self, id: &'a str) -> Result<&'a str>;
    /// [`valid_id`](Self::valid_id) as a filter predicate.
    fn is_id(&self, id: &str) -> bool {
        self.valid_id(id).is_ok()
    }
    /// A key the server handed out (hub, filter-choice and station keys),
    /// validated and split into its server-relative path and decoded query
    /// pairs. Rejects anything that is not server-relative and any query
    /// parameter carrying this backend's credential, so a key can never
    /// replace or leak the credential header. Independent of the server's
    /// base path: the path is what [`Query::Advertised`] and the key
    /// predicates below take.
    fn advertised_key(&self, key: &str) -> Result<(String, Vec<(String, String)>)>;
    /// A server-relative key (see [`advertised_key`](Self::advertised_key))
    /// that addresses library content (not a hub or filter-choice key).
    fn is_library_key(&self, key: &str) -> bool;
    /// A stored or typed server address, in this backend's accepted form
    /// (Plex: an origin; others may carry a base path).
    fn parse_url(&self, value: &str) -> Result<Url>;
    /// The account/server identity that scopes downloads, snapshots and
    /// history on disk.
    fn namespace(&self, conn: &Conn) -> String;
    /// The credential as HTTP headers, for requests Core's backend does not
    /// make itself (artwork, downloads, audio).
    fn auth_headers(&self, conn: &Conn) -> HeaderMap;
    /// The [`StreamPolicy`] for audio, downloads and artwork against `conn`.
    fn stream_policy(&self, conn: &Conn) -> StreamPolicy;
    /// The query listing the children of a UI item. `key` is what the server
    /// handed out for the item; `kind` the parent's kind when the UI knows it.
    fn children_query(&self, key: &str, kind: Option<&ItemKind>) -> Result<Query>;
    /// Whether [`Query::Children`] depends on the parent's kind. When it does
    /// not, Core lists a later page of an item whose kind it does not know
    /// without reading the item first.
    fn children_need_kind(&self) -> bool;
    /// The read whose reply describes this server's filter fields and choice
    /// lists for listings of `kind` in `section` (None: the backend has none).
    fn filter_schema(&self, section: &str, kind: &ItemKind) -> Option<Query>;
    /// The server's spelling of filter fields for listings of `kind`, from
    /// the `meta` of a [`filter_schema`](Self::filter_schema) reply. A backend
    /// without a schema has none.
    fn filter_fields(
        &self,
        _meta: &serde_json::Value,
        _kind: &ItemKind,
    ) -> Vec<(FilterField, String)> {
        vec![]
    }
    /// The read listing the choices of `field` (genre, mood or style) for
    /// listings of `kind` in `section`; None when the server offers none.
    /// `meta` is the [`filter_schema`](Self::filter_schema) reply's, if any.
    fn filter_choices(
        &self,
        _section: &str,
        _kind: &ItemKind,
        _field: FilterField,
        _meta: Option<&serde_json::Value>,
    ) -> Result<Option<Query>> {
        Ok(None)
    }
    /// A smart playlist's source, from the `content` the server reports. A
    /// backend without smart playlists reports one the editor cannot show.
    fn smart_source(&self, _content: &str) -> SmartSource {
        SmartSource {
            section: String::new(),
            filter: Music::default(),
            sort: Sort::Title,
            limit: 100,
            editable: false,
        }
    }
    /// Whether the server counts a play when playback starts (not when it
    /// is scrobbled), so that Core reports the start once per occurrence and
    /// replays only plays whose start was never reported.
    fn reports_start(&self) -> bool {
        false
    }
    /// The read that returns an endless-radio batch of tracks for a seed (a
    /// server-side "instant mix"), for backends whose radio is a client-side
    /// source of such batches; None when the server has none.
    fn mix_query(&self, _id: &str, _kind: crate::RadioKind, _limit: u32) -> Option<Query> {
        None
    }

    fn offline_key(&self, query: &Query, page: Option<Page>) -> Result<OfflineKey>;
    fn fetch(&self, conn: &Conn, query: &Query, page: Option<Page>) -> Result<Container>;
    fn act(&self, conn: &Conn, action: &Action) -> Result<()>;
    fn create_playlist(
        &self,
        conn: &Conn,
        title: &str,
        content: &PlaylistContent,
    ) -> Result<Container>;
    /// Start a continuous radio queue from a server-advertised station key.
    fn create_radio(&self, conn: &Conn, station: &str) -> Result<Container>;
    /// The decoded lyric lines of a lyric stream key.
    fn lyrics(&self, conn: &Conn, stream_key: &str) -> Result<Vec<Line>>;
    /// Playable URL of a media part key. It carries no credential: the
    /// reader sends [`auth_headers`](Self::auth_headers).
    fn stream_url(&self, conn: &Conn, part_key: &str) -> Result<String>;
    /// Transcoded stream URL (no credential, like [`stream_url`](Self::stream_url)).
    fn transcode_stream(&self, conn: &Conn, id: &str, kbps: u32, session: &str) -> Result<String>;
    /// Probe a throwaway transcode session and tear it down; (bytes, mime, stopped).
    fn transcode_probe(
        &self,
        conn: &Conn,
        id: &str,
        kbps: u32,
        session: &str,
    ) -> Result<(usize, String, bool)>;
    /// Status-only existence probe, as an owned future for a background task.
    fn probe(
        &self,
        conn: &Conn,
        id: &str,
    ) -> Result<impl Future<Output = Probe> + Send + 'static + use<Self>>;
}

fn owned(params: &[(&str, &str)]) -> Vec<(String, String)> {
    params
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// The Plex request a query stands for: path plus ordered query pairs.
fn plex_request(query: &Query, page: Option<Page>) -> Result<OfflineKey> {
    let section = |s: &str, tail: &str| format!("/library/sections/{s}{tail}");
    let meta = |id: &str, tail: &str| format!("/library/metadata/{id}{tail}");
    let (path, mut params) = match query {
        Query::Sections => ("/library/sections".to_owned(), vec![]),
        Query::Item { id, stations } => (
            meta(id, ""),
            if *stations {
                owned(&[("includeStations", "1")])
            } else {
                vec![]
            },
        ),
        Query::Children { id, .. } => (meta(id, "/children"), vec![]),
        Query::AllLeaves { id } => (meta(id, "/allLeaves"), vec![]),
        Query::Similar { id, count } => (
            meta(id, "/similar"),
            owned(&[("count", &count.to_string())]),
        ),
        Query::Nearest { id, limit } => (
            meta(id, "/nearest"),
            owned(&[("limit", &limit.to_string())]),
        ),
        Query::Browse { section: s, spec } => (section(s, "/all"), plex_browse(spec)?),
        Query::FilterSchema { section: s, kind } => (
            section(s, "/all"),
            owned(&[
                ("type", plex_type(kind)?),
                ("includeMeta", "1"),
                ("includeAdvanced", "1"),
            ]),
        ),
        Query::FirstCharacter { section: s } => (
            section(s, "/firstCharacter"),
            owned(&[("type", "8"), ("sort", "titleSort:asc")]),
        ),
        Query::ComputePath {
            section: s,
            start,
            end,
        } => (
            section(s, "/computePath"),
            owned(&[("startID", start), ("endID", end)]),
        ),
        Query::Hubs { section: s } => (
            format!("/hubs/sections/{s}"),
            owned(&[
                ("count", "12"),
                ("includeStations", "1"),
                ("includeMyMixes", "1"),
            ]),
        ),
        Query::Search { query, limit } => (
            "/hubs/search".to_owned(),
            owned(&[("query", query), ("limit", &limit.to_string())]),
        ),
        Query::Playlists => ("/playlists".to_owned(), owned(&[("playlistType", "audio")])),
        Query::Playlist { id } => (format!("/playlists/{id}"), vec![]),
        Query::PlaylistItems { id } => (format!("/playlists/{id}/items"), vec![]),
        Query::PlayQueueWindow { id, center } => (
            format!("/playQueues/{id}"),
            owned(&[
                ("center", &center.to_string()),
                ("window", "50"),
                ("includeBefore", "0"),
                ("includeAfter", "1"),
            ]),
        ),
        Query::Advertised { path, params } => (path.clone(), params.clone()),
    };
    if let Some(Page { start, size }) = page {
        params.push(("X-Plex-Container-Start".into(), start.to_string()));
        params.push(("X-Plex-Container-Size".into(), size.to_string()));
    }
    Ok((path, params))
}

/// Plex metadata type code of a listable kind.
fn plex_type(kind: &ItemKind) -> Result<&'static str> {
    match kind {
        ItemKind::Artist => Ok("8"),
        ItemKind::Album => Ok("9"),
        ItemKind::Track => Ok("10"),
        ItemKind::Playlist | ItemKind::Other(_) => Err(Error::Input("Unknown music type")),
    }
}

/// The PMS `sort` value (field:direction) of a [`Sort`].
fn plex_sort(sort: Sort) -> &'static str {
    match sort {
        Sort::Title => "titleSort:asc",
        Sort::Added => "addedAt:desc",
        Sort::Year => "year:desc",
        Sort::LastPlayed => "lastViewedAt:desc",
        Sort::PlayCount => "viewCount:desc",
        Sort::Random => "random",
    }
}

/// The PMS field name suffix of a filter field.
fn plex_field(field: FilterField) -> &'static str {
    match field {
        FilterField::Genre => "genre",
        FilterField::Mood => "mood",
        FilterField::Style => "style",
        FilterField::Year => "year",
        FilterField::Rating => "userRating",
        FilterField::ViewCount => "viewCount",
    }
}

/// The ordered `/library/sections/N/all` pairs of a listing: the filter
/// pairs (genre, mood, style, title, year range, minimum rating, unplayed),
/// then `type`, the favourite rating, `sort`, the search title, the played
/// condition and the limit. That is the order every pre-boundary call site
/// used (browse, collections, filtered browse, smart playlists), so URLs and
/// offline snapshot keys are unchanged.
fn plex_browse(spec: &BrowseSpec) -> Result<Vec<(String, String)>> {
    spec.check()?;
    let code = plex_type(&spec.kind)?;
    let name = |field: FilterField| {
        spec.fields.iter().find(|(f, _)| *f == field).map_or_else(
            || format!("{}.{}", spec.kind, plex_field(field)),
            |(_, name)| name.clone(),
        )
    };
    let filter = &spec.filter;
    let mut params = Vec::new();
    for (field, value) in [
        (FilterField::Genre, &filter.genre),
        (FilterField::Mood, &filter.mood),
        (FilterField::Style, &filter.style),
    ] {
        if !value.is_empty() {
            params.push((name(field), value.clone()));
        }
    }
    if !filter.title.trim().is_empty() {
        params.push(("title".into(), filter.title.trim().into()));
    }
    if let Some(year) = filter.year_from {
        params.push((format!("{}>=", name(FilterField::Year)), year.to_string()));
    }
    if let Some(year) = filter.year_to {
        params.push((format!("{}<=", name(FilterField::Year)), year.to_string()));
    }
    if let Some(rating) = filter.min_rating {
        params.push((
            format!("{}>=", name(FilterField::Rating)),
            rating.to_string(),
        ));
    }
    if filter.unplayed {
        params.push((name(FilterField::ViewCount), "0".into()));
    }
    params.push(("type".into(), code.into()));
    if spec.favourite {
        // Plex favourites are tracks rated 10 (see `Action::Rate`).
        params.push(("userRating".into(), "10".into()));
    }
    params.push(("sort".into(), plex_sort(spec.sort).into()));
    if let Some(search) = &spec.search {
        params.push(("title".into(), search.clone()));
    }
    if spec.played {
        params.push(("viewCount>>".into(), "0".into()));
    }
    if let Some(limit) = spec.limit {
        params.push(("limit".into(), limit.to_string()));
    }
    Ok(params)
}

/// The canonical field scopes a PMS filter schema (`Meta.Type[].Field[]`)
/// declares for `kind` (tracks filter on `album.genre`, ...): for each field
/// the exact `{kind}.{field}` key if listed, else the first key with the
/// same field name, own type's scope first. Older servers omit Meta; their
/// documented type-scoped keys are the default spelling.
fn plex_filter_fields(meta: &serde_json::Value, kind: &ItemKind) -> Vec<(FilterField, String)> {
    let Some(types) = meta["Type"].as_array() else {
        return vec![];
    };
    let mut scopes: Vec<_> = types.iter().collect();
    scopes.sort_by_key(|scope| scope["type"] != kind.as_str());
    let fields: Vec<_> = scopes
        .into_iter()
        .flat_map(|t| t["Field"].as_array().into_iter().flatten())
        .filter_map(|field| field["key"].as_str())
        .collect();
    [
        FilterField::Genre,
        FilterField::Mood,
        FilterField::Style,
        FilterField::Year,
        FilterField::Rating,
        FilterField::ViewCount,
    ]
    .into_iter()
    .filter_map(|field| {
        let suffix = plex_field(field);
        let exact = format!("{kind}.{suffix}");
        fields
            .iter()
            .find(|key| **key == exact)
            .or_else(|| {
                fields
                    .iter()
                    .find(|key| key.ends_with(&format!(".{suffix}")))
            })
            .map(|key| (field, (*key).to_owned()))
    })
    .collect()
}

/// Percent-decodes a whole string. form_urlencoded decodes `%XX`, but its
/// form syntax (`+` as space, `&` as separator) is escaped first so those
/// characters survive literally.
fn percent_decode(text: &str) -> String {
    let escaped = text.replace('+', "%2B").replace('&', "%26");
    url::form_urlencoded::parse(format!("v={escaped}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

/// A Plex smart playlist's `content`: a (possibly percent-encoded)
/// `server://` or `library://` URI of a section listing with filter pairs.
fn plex_smart_source(content: &str) -> SmartSource {
    let content = if content.starts_with("server%") || content.starts_with("library%") {
        percent_decode(content)
    } else {
        content.to_owned()
    };
    let mut source = SmartSource {
        section: String::new(),
        filter: Music::default(),
        sort: Sort::Title,
        limit: 100,
        editable: true,
    };
    let Ok(uri) = Url::parse(&content) else {
        source.editable = false;
        return source;
    };
    let (filters, editable) = (&mut source.filter, &mut source.editable);
    source.section = uri
        .path()
        .split("/library/sections/")
        .nth(1)
        .and_then(|path| path.split('/').next())
        .unwrap_or("")
        .into();
    for (key, value) in uri.query_pairs() {
        let field = key.split('.').next_back().unwrap_or(&key);
        match field {
            "genre" => filters.genre = value.into_owned(),
            "mood" => filters.mood = value.into_owned(),
            "style" => filters.style = value.into_owned(),
            "title" => filters.title = value.into_owned(),
            "year>=" => {
                filters.year_from = value.parse().ok();
                *editable &= filters.year_from.is_some();
            }
            "year<=" => {
                filters.year_to = value.parse().ok();
                *editable &= filters.year_to.is_some();
            }
            "userRating>=" => {
                filters.min_rating = value.parse().ok();
                *editable &= filters.min_rating.is_some();
            }
            "viewCount" if value == "0" => filters.unplayed = true,
            "limit" => match value.parse().ok().filter(|n| (1..=1000).contains(n)) {
                Some(parsed) => source.limit = parsed,
                None => {
                    *editable = false;
                    source.limit = 100;
                }
            },
            "sort" => {
                source.sort = match Sort::ALL.into_iter().find(|s| plex_sort(*s) == value) {
                    Some(sort) => sort,
                    None => {
                        *editable = false;
                        Sort::Title
                    }
                }
            }
            "type" if value == "10" => {}
            "includeGuids" => {}
            _ => *editable = false,
        }
    }
    source
}

/// The advertised choice key of `field` in a PMS filter schema
/// (`Meta.Type[type == kind].Filter[filter == field].key`), as a read
/// confined to `section`.
fn plex_filter_choices(
    server: &impl MediaServer,
    section: &str,
    kind: &ItemKind,
    field: FilterField,
    meta: Option<&serde_json::Value>,
) -> Result<Option<Query>> {
    let field = plex_field(field);
    let key = meta
        .and_then(|meta| meta["Type"].as_array())
        .into_iter()
        .flatten()
        .filter(|t| t["type"] == kind.as_str())
        .flat_map(|t| t["Filter"].as_array().into_iter().flatten())
        .find(|f| f["filter"] == field)
        .and_then(|f| f["key"].as_str());
    let Some(key) = key else {
        return Ok(None);
    };
    let (path, params) = server.advertised_key(key)?;
    if !path.starts_with(&format!("/library/sections/{section}/")) {
        return Err(Error::Input("Expected a server-relative path"));
    }
    Ok(Some(Query::advertised(&path, params)))
}

/// Item keys vary: playlists use /playlists/N/items; artists and albums use
/// /library/metadata/N/children. Plex needs no kind.
fn plex_children_query(key: &str, kind: Option<&ItemKind>) -> Result<Query> {
    if let Some(id) = key
        .strip_prefix("/playlists/")
        .and_then(|rest| rest.strip_suffix("/items"))
    {
        return Ok(Query::PlaylistItems {
            id: crate::plex::valid_id(id)?.to_owned(),
        });
    }
    let id = key.strip_prefix("/library/metadata/").unwrap_or(key);
    let id = id.strip_suffix("/children").unwrap_or(id);
    Ok(Query::Children {
        id: crate::plex::valid_id(id)?.to_owned(),
        kind: kind.cloned().unwrap_or_default(),
    })
}

fn refs(params: &[(String, String)]) -> Vec<(&str, String)> {
    params
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect()
}

impl Plex {
    /// `server://<machine id>/com.plexapp.plugins.library<key>` for a content key.
    fn content_uri(&self, conn: &Conn, content: &PlaylistContent) -> Result<String> {
        let key = match content {
            PlaylistContent::Tracks(ids) => format!("/library/metadata/{}", ids.join(",")),
            PlaylistContent::Smart { section, spec } => {
                let encoded = url::form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(plex_browse(spec)?)
                    .finish();
                format!("/library/sections/{section}/all?{encoded}")
            }
        };
        self.station_uri(conn, &key)
    }
    fn station_uri(&self, conn: &Conn, key: &str) -> Result<String> {
        let identity = self.container(&conn.base, &conn.token, "/identity", &[])?;
        crate::radio::station_uri(&identity.machine_identifier, key)
    }
}

/// Plex's transcode, seek, stop and raw-request rules.
struct PlexRules;
impl StreamRules for PlexRules {
    fn valid_id(&self, id: &str) -> bool {
        crate::plex::valid_id(id).is_ok()
    }
    fn transcode_url(&self, base: &Url, id: &str, kbps: u32, session: &str) -> Result<Url> {
        crate::quality::transcode_url(base, id, kbps, session)
    }
    fn is_transcode(&self, url: &Url) -> bool {
        crate::quality::is_transcode(url)
    }
    fn seek(&self, url: &Url, milliseconds: u64) -> Result<Url> {
        Ok(crate::quality::seek(url, milliseconds))
    }
    fn stop(&self, url: &Url) -> Option<(Method, Url)> {
        crate::quality::stop_url(url).map(|stop| (Method::GET, stop))
    }
    fn download_url(&self, base: &Url, part_key: &str) -> Result<Url> {
        crate::plex::server_path(base, part_key)
    }
    fn artwork_url(&self, base: &Url, path: &str) -> Result<Url> {
        crate::plex::advertised_path(base, path)
    }
    fn metadata_url(&self, base: &Url, id: &str) -> Result<Url> {
        crate::plex::server_path(
            base,
            &format!("/library/metadata/{}", crate::plex::valid_id(id)?),
        )
    }
    fn parse_metadata(&self, body: &[u8]) -> Result<Container> {
        Ok(
            crate::plex::plex_json::<crate::model::Envelope>(body, "cache track metadata")?
                .container,
        )
    }
    fn client_headers(&self, client_identifier: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Ok(value) = HeaderValue::from_str(client_identifier) {
            headers.insert("x-plex-client-identifier", value);
        }
        headers
    }
}

impl Account for Plex {
    fn pin(&self) -> Result<Pin> {
        Plex::pin(self)
    }
    fn poll(&self, id: u64, code: &str) -> Result<Pin> {
        Plex::poll(self, id, code)
    }
    fn resources(&self, account_token: &str) -> Result<Vec<Resource>> {
        Plex::resources(self, account_token)
    }
    fn login_url(&self, client_identifier: &str, pin: &Pin) -> String {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("clientID", client_identifier)
            .append_pair("code", &pin.code)
            .append_pair("context[device][product]", "PlexFreq")
            .finish();
        format!("https://app.plex.tv/auth#?{query}")
    }
}

impl MediaServer for Plex {
    fn bind_view(&self, scope: Option<(tokio::sync::watch::Receiver<u64>, u64)>) {
        Plex::bind_view(self, scope);
    }
    fn cancelled(&self) -> bool {
        Plex::cancelled(self)
    }
    fn capabilities(&self) -> Caps {
        Caps::ALL
    }
    fn valid_id<'a>(&self, id: &'a str) -> Result<&'a str> {
        crate::plex::valid_id(id)
    }
    fn advertised_key(&self, key: &str) -> Result<(String, Vec<(String, String)>)> {
        crate::plex::advertised_parts(key, crate::plex::is_plex_credential)
    }
    fn is_library_key(&self, key: &str) -> bool {
        key.starts_with("/library/")
    }
    fn parse_url(&self, value: &str) -> Result<Url> {
        crate::plex::server_url(value)
    }
    fn namespace(&self, conn: &Conn) -> String {
        crate::cache::namespace(conn.base.as_str(), &conn.token)
    }
    fn auth_headers(&self, conn: &Conn) -> HeaderMap {
        plex_headers(&conn.token)
    }
    fn stream_policy(&self, conn: &Conn) -> StreamPolicy {
        StreamPolicy::plex(&conn.base, &conn.token)
    }
    fn children_query(&self, key: &str, kind: Option<&ItemKind>) -> Result<Query> {
        plex_children_query(key, kind)
    }
    fn children_need_kind(&self) -> bool {
        false
    }
    fn filter_schema(&self, section: &str, kind: &ItemKind) -> Option<Query> {
        Some(Query::FilterSchema {
            section: section.into(),
            kind: kind.clone(),
        })
    }
    fn filter_fields(
        &self,
        meta: &serde_json::Value,
        kind: &ItemKind,
    ) -> Vec<(FilterField, String)> {
        plex_filter_fields(meta, kind)
    }
    fn filter_choices(
        &self,
        section: &str,
        kind: &ItemKind,
        field: FilterField,
        meta: Option<&serde_json::Value>,
    ) -> Result<Option<Query>> {
        plex_filter_choices(self, section, kind, field, meta)
    }
    fn smart_source(&self, content: &str) -> SmartSource {
        plex_smart_source(content)
    }
    fn offline_key(&self, query: &Query, page: Option<Page>) -> Result<OfflineKey> {
        plex_request(query, page)
    }
    fn fetch(&self, conn: &Conn, query: &Query, page: Option<Page>) -> Result<Container> {
        let (path, params) = plex_request(query, page)?;
        self.container(&conn.base, &conn.token, &path, &refs(&params))
    }
    fn act(&self, conn: &Conn, action: &Action) -> Result<()> {
        let (base, token) = (&conn.base, conn.token.as_str());
        match action {
            Action::Rate { id, rating } => self.rate(base, token, id, *rating),
            Action::Scrobble { id, .. } => self.mutate(
                base,
                token,
                Method::GET,
                "/:/scrobble",
                &[
                    ("key", id.clone()),
                    ("identifier", "com.plexapp.plugins.library".into()),
                ],
            ),
            Action::Timeline {
                id,
                state,
                position,
                duration,
                continuing,
                queue,
                ..
            } => {
                let mut params = vec![
                    ("key", format!("/library/metadata/{id}")),
                    ("ratingKey", id.clone()),
                ];
                if let Some((queue_id, item_id)) = queue {
                    params.extend([
                        ("playQueueID", queue_id.to_string()),
                        ("playQueueItemID", item_id.to_string()),
                    ]);
                }
                params.extend([
                    ("state", state.clone()),
                    ("time", position.to_string()),
                    ("duration", duration.to_string()),
                    ("continuing", u8::from(*continuing).to_string()),
                ]);
                self.timeline(base, token, &params)
            }
            Action::PlaylistRename { id, title } => self.mutate(
                base,
                token,
                Method::PUT,
                &format!("/playlists/{id}"),
                &[("title", title.clone())],
            ),
            Action::PlaylistDelete { id } => self.mutate(
                base,
                token,
                Method::DELETE,
                &format!("/playlists/{id}"),
                &[],
            ),
            Action::PlaylistAdd { id, content } => {
                let uri = self.content_uri(conn, content)?;
                self.mutate(
                    base,
                    token,
                    Method::PUT,
                    &format!("/playlists/{id}/items"),
                    &[("uri", uri)],
                )
            }
            Action::PlaylistRemove { id, item } => self.mutate(
                base,
                token,
                Method::DELETE,
                &format!("/playlists/{id}/items/{}", crate::plex::valid_id(item)?),
                &[],
            ),
            // Plex orders by the occurrence it goes after; `index` is for
            // backends that move by position.
            Action::PlaylistMove {
                id, item, after, ..
            } => {
                let after = after
                    .as_deref()
                    .map(|a| crate::plex::valid_id(a).map(str::to_owned))
                    .transpose()?;
                self.mutate(
                    base,
                    token,
                    Method::PUT,
                    &format!(
                        "/playlists/{id}/items/{}/move",
                        crate::plex::valid_id(item)?
                    ),
                    &after.map(|a| vec![("after", a)]).unwrap_or_default(),
                )
            }
        }
    }
    fn create_playlist(
        &self,
        conn: &Conn,
        title: &str,
        content: &PlaylistContent,
    ) -> Result<Container> {
        let uri = self.content_uri(conn, content)?;
        let smart = matches!(content, PlaylistContent::Smart { .. });
        Plex::create_playlist(
            self,
            &conn.base,
            &conn.token,
            &[
                ("title", title.into()),
                ("type", "audio".into()),
                ("smart", if smart { "1" } else { "0" }.into()),
                ("uri", uri),
            ],
        )
    }
    fn create_radio(&self, conn: &Conn, station: &str) -> Result<Container> {
        let uri = self.station_uri(conn, station)?;
        self.create_radio_queue(&conn.base, &conn.token, &uri)
    }
    fn lyrics(&self, conn: &Conn, stream_key: &str) -> Result<Vec<Line>> {
        crate::lyrics::decode(&self.text(&conn.base, &conn.token, stream_key)?)
    }
    fn stream_url(&self, conn: &Conn, part_key: &str) -> Result<String> {
        Ok(crate::plex::advertised_path(&conn.base, part_key)?.into())
    }
    fn transcode_stream(&self, conn: &Conn, id: &str, kbps: u32, session: &str) -> Result<String> {
        Ok(crate::quality::transcode_url(&conn.base, id, kbps, session)?.into())
    }
    fn transcode_probe(
        &self,
        conn: &Conn,
        id: &str,
        kbps: u32,
        session: &str,
    ) -> Result<(usize, String, bool)> {
        let uri = crate::quality::transcode_url(&conn.base, id, kbps, session)?;
        let result = self.probe_audio(uri.clone(), &conn.token);
        let stopped = crate::quality::stop_url(&uri).is_some_and(|stop| {
            let params: Vec<_> = stop
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            self.mutate(
                &conn.base,
                &conn.token,
                Method::GET,
                stop.path(),
                &refs(&params),
            )
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
        self.probe_metadata(&conn.base, &conn.token, id)
    }
}

/// The server Core talks to. An enum instead of `dyn`: static dispatch, `Clone`
/// for worker threads, and the mock variant compiles out of release builds.
#[derive(Clone)]
// One long-lived value per Core and one clone per refill: size is irrelevant.
#[allow(clippy::large_enum_variant)]
pub enum Backend {
    Plex(Plex),
    Jellyfin(Jellyfin),
    #[cfg(any(test, feature = "mock-backend"))]
    Mock(MockServer),
}

macro_rules! forward {
    ($tr:ident, $self:ident . $method:ident ( $($arg:expr),* )) => {
        match $self {
            Backend::Plex(server) => $tr::$method(server, $($arg),*),
            Backend::Jellyfin(server) => $tr::$method(server, $($arg),*),
            #[cfg(any(test, feature = "mock-backend"))]
            Backend::Mock(server) => $tr::$method(server, $($arg),*),
        }
    };
}

impl Backend {
    /// The sign-in/discovery interface, for backends that have one (Plex PIN
    /// login and resources); the others refuse instead of faking it.
    pub fn account(&self) -> Result<&dyn Account> {
        match self {
            Backend::Plex(server) => Ok(server),
            Backend::Jellyfin(_) => Err(unsupported()),
            #[cfg(any(test, feature = "mock-backend"))]
            Backend::Mock(server) => Ok(server),
        }
    }
}

impl MediaServer for Backend {
    fn bind_view(&self, scope: Option<(tokio::sync::watch::Receiver<u64>, u64)>) {
        forward!(MediaServer, self.bind_view(scope));
    }
    fn cancelled(&self) -> bool {
        forward!(MediaServer, self.cancelled())
    }
    fn capabilities(&self) -> Caps {
        forward!(MediaServer, self.capabilities())
    }
    fn valid_id<'a>(&self, id: &'a str) -> Result<&'a str> {
        forward!(MediaServer, self.valid_id(id))
    }
    fn is_id(&self, id: &str) -> bool {
        forward!(MediaServer, self.is_id(id))
    }
    fn advertised_key(&self, key: &str) -> Result<(String, Vec<(String, String)>)> {
        forward!(MediaServer, self.advertised_key(key))
    }
    fn is_library_key(&self, key: &str) -> bool {
        forward!(MediaServer, self.is_library_key(key))
    }
    fn parse_url(&self, value: &str) -> Result<Url> {
        forward!(MediaServer, self.parse_url(value))
    }
    fn namespace(&self, conn: &Conn) -> String {
        forward!(MediaServer, self.namespace(conn))
    }
    fn auth_headers(&self, conn: &Conn) -> HeaderMap {
        forward!(MediaServer, self.auth_headers(conn))
    }
    fn stream_policy(&self, conn: &Conn) -> StreamPolicy {
        forward!(MediaServer, self.stream_policy(conn))
    }
    fn children_query(&self, key: &str, kind: Option<&ItemKind>) -> Result<Query> {
        forward!(MediaServer, self.children_query(key, kind))
    }
    fn children_need_kind(&self) -> bool {
        forward!(MediaServer, self.children_need_kind())
    }
    fn filter_schema(&self, section: &str, kind: &ItemKind) -> Option<Query> {
        forward!(MediaServer, self.filter_schema(section, kind))
    }
    fn filter_fields(
        &self,
        meta: &serde_json::Value,
        kind: &ItemKind,
    ) -> Vec<(FilterField, String)> {
        forward!(MediaServer, self.filter_fields(meta, kind))
    }
    fn filter_choices(
        &self,
        section: &str,
        kind: &ItemKind,
        field: FilterField,
        meta: Option<&serde_json::Value>,
    ) -> Result<Option<Query>> {
        forward!(MediaServer, self.filter_choices(section, kind, field, meta))
    }
    fn smart_source(&self, content: &str) -> SmartSource {
        forward!(MediaServer, self.smart_source(content))
    }
    fn reports_start(&self) -> bool {
        forward!(MediaServer, self.reports_start())
    }
    fn mix_query(&self, id: &str, kind: crate::RadioKind, limit: u32) -> Option<Query> {
        forward!(MediaServer, self.mix_query(id, kind, limit))
    }
    fn offline_key(&self, query: &Query, page: Option<Page>) -> Result<OfflineKey> {
        forward!(MediaServer, self.offline_key(query, page))
    }
    fn fetch(&self, conn: &Conn, query: &Query, page: Option<Page>) -> Result<Container> {
        forward!(MediaServer, self.fetch(conn, query, page))
    }
    fn act(&self, conn: &Conn, action: &Action) -> Result<()> {
        forward!(MediaServer, self.act(conn, action))
    }
    fn create_playlist(
        &self,
        conn: &Conn,
        title: &str,
        content: &PlaylistContent,
    ) -> Result<Container> {
        forward!(MediaServer, self.create_playlist(conn, title, content))
    }
    fn create_radio(&self, conn: &Conn, station: &str) -> Result<Container> {
        forward!(MediaServer, self.create_radio(conn, station))
    }
    fn lyrics(&self, conn: &Conn, stream_key: &str) -> Result<Vec<Line>> {
        forward!(MediaServer, self.lyrics(conn, stream_key))
    }
    fn stream_url(&self, conn: &Conn, part_key: &str) -> Result<String> {
        forward!(MediaServer, self.stream_url(conn, part_key))
    }
    fn transcode_stream(&self, conn: &Conn, id: &str, kbps: u32, session: &str) -> Result<String> {
        forward!(MediaServer, self.transcode_stream(conn, id, kbps, session))
    }
    fn transcode_probe(
        &self,
        conn: &Conn,
        id: &str,
        kbps: u32,
        session: &str,
    ) -> Result<(usize, String, bool)> {
        forward!(MediaServer, self.transcode_probe(conn, id, kbps, session))
    }
    /// The variants return different future types; `Either` unifies them.
    /// Without the mock, the innermost right side is a never-built `Ready`.
    fn probe(
        &self,
        conn: &Conn,
        id: &str,
    ) -> Result<impl Future<Output = Probe> + Send + 'static + use<>> {
        Ok(match self {
            Backend::Plex(server) => Either::<_, Either<_, std::future::Ready<Probe>>>::Left(
                MediaServer::probe(server, conn, id)?,
            ),
            Backend::Jellyfin(server) => {
                Either::Right(Either::Left(MediaServer::probe(server, conn, id)?))
            }
            #[cfg(any(test, feature = "mock-backend"))]
            Backend::Mock(server) => {
                Either::Right(Either::Right(MediaServer::probe(server, conn, id)?))
            }
        })
    }
}

#[cfg(any(test, feature = "mock-backend"))]
pub use mock::{MockError, MockServer, Recorded};
#[cfg(any(test, feature = "mock-backend"))]
pub use mock::{MockIds, MockRules};

/// In-memory scripted server for tests: no HTTP, shared state across clones.
#[cfg(any(test, feature = "mock-backend"))]
mod mock {
    use super::*;
    use crate::Error;
    use std::{
        collections::{HashMap, VecDeque},
        sync::{Arc, Mutex},
    };

    /// Injectable failures (`crate::Error` is not `Clone`).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum MockError {
        Unauthorized,
        Network,
        Http(u16),
    }
    impl From<MockError> for Error {
        fn from(error: MockError) -> Self {
            match error {
                MockError::Unauthorized => Error::Unauthorized,
                MockError::Network => Error::Network,
                MockError::Http(code) => Error::Http(code),
            }
        }
    }

    /// Everything the mock was asked to do, in order.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Recorded {
        Fetch(Query, Option<Page>),
        Act(Action),
        CreatePlaylist(String, PlaylistContent),
        CreateRadio(String),
    }

    type Reply = std::result::Result<Container, MockError>;

    /// The identifier family the mock accepts. `Plex` is numeric; `Guid`
    /// stands in for a backend with 32-hex ids and `/Items/<id>` keys, so
    /// tests can prove Core does not assume numeric ids.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum MockIds {
        Plex,
        Guid,
    }

    fn guid(value: &str) -> bool {
        value.len() == 32
            && value
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }

    struct State {
        caps: Caps,
        ids: MockIds,
        replies: HashMap<(Query, Option<Page>), VecDeque<Reply>>,
        creates: VecDeque<Reply>,
        act_error: Option<MockError>,
        probe: Probe,
        lyrics: Option<String>,
        pin: Option<Pin>,
        resources: Vec<Resource>,
        log: Vec<Recorded>,
    }

    /// Clones share the script and log (a refill thread's clone sees the same
    /// script) but have no view scope of their own to cancel.
    #[derive(Clone)]
    pub struct MockServer {
        state: Arc<Mutex<State>>,
    }

    impl Default for MockServer {
        fn default() -> Self {
            Self {
                state: Arc::new(Mutex::new(State {
                    caps: Caps::ALL,
                    ids: MockIds::Plex,
                    replies: HashMap::new(),
                    creates: VecDeque::new(),
                    act_error: None,
                    probe: Probe::Unknown,
                    lyrics: None,
                    pin: None,
                    resources: vec![],
                    log: vec![],
                })),
            }
        }
    }

    impl MockServer {
        fn state(&self) -> std::sync::MutexGuard<'_, State> {
            crate::mutex_lock(&self.state)
        }
        /// Queue a reply for a read. The last queued reply repeats.
        pub fn script(&self, query: Query, page: Option<Page>, reply: Reply) -> &Self {
            self.state()
                .replies
                .entry((query, page))
                .or_default()
                .push_back(reply);
            self
        }
        /// Queue a reply for the next playlist/radio creation.
        pub fn script_create(&self, reply: Reply) -> &Self {
            self.state().creates.push_back(reply);
            self
        }
        /// Make every following `act` fail (None clears).
        pub fn fail_actions(&self, error: Option<MockError>) -> &Self {
            self.state().act_error = error;
            self
        }
        pub fn script_probe(&self, probe: Probe) -> &Self {
            self.state().probe = probe;
            self
        }
        pub fn script_lyrics(&self, text: &str) -> &Self {
            self.state().lyrics = Some(text.into());
            self
        }
        /// What the mock reports it can do (default: everything).
        pub fn script_caps(&self, caps: Caps) -> &Self {
            self.state().caps = caps;
            self
        }
        /// The identifier family it validates and namespaces by.
        pub fn script_ids(&self, ids: MockIds) -> &Self {
            self.state().ids = ids;
            self
        }
        fn ids(&self) -> MockIds {
            self.state().ids
        }
        pub fn script_account(&self, pin: Pin, resources: Vec<Resource>) -> &Self {
            let mut state = self.state();
            state.pin = Some(pin);
            state.resources = resources;
            drop(state);
            self
        }
        pub fn log(&self) -> Vec<Recorded> {
            self.state().log.clone()
        }
        pub fn actions(&self) -> Vec<Action> {
            self.log()
                .into_iter()
                .filter_map(|r| match r {
                    Recorded::Act(action) => Some(action),
                    _ => None,
                })
                .collect()
        }
    }

    fn next(queue: &mut VecDeque<Reply>) -> Option<Reply> {
        if queue.len() > 1 {
            queue.pop_front()
        } else {
            queue.front().cloned()
        }
    }

    impl Account for MockServer {
        fn pin(&self) -> Result<Pin> {
            self.state().pin.clone().ok_or(Error::Unauthorized)
        }
        fn poll(&self, _id: u64, _code: &str) -> Result<Pin> {
            self.pin()
        }
        fn resources(&self, _account_token: &str) -> Result<Vec<Resource>> {
            Ok(self.state().resources.clone())
        }
        fn login_url(&self, _client_identifier: &str, pin: &Pin) -> String {
            format!("mock://login/{}", pin.code)
        }
    }

    /// Transcode rules in a vocabulary no real server uses, so tests can tell
    /// that Core went through the policy: DELETE stops, `/mock/` paths.
    pub struct MockRules(pub MockIds);
    impl StreamRules for MockRules {
        fn valid_id(&self, id: &str) -> bool {
            match self.0 {
                MockIds::Plex => crate::plex::valid_id(id).is_ok(),
                MockIds::Guid => guid(id),
            }
        }
        fn transcode_url(&self, base: &Url, id: &str, kbps: u32, session: &str) -> Result<Url> {
            let mut url = crate::plex::server_path(base, &format!("/mock/transcode/{id}"))?;
            url.query_pairs_mut()
                .append_pair("kbps", &kbps.to_string())
                .append_pair("session", session)
                .append_pair("offset", "0");
            Ok(url)
        }
        fn is_transcode(&self, url: &Url) -> bool {
            url.path().contains("/mock/transcode/")
        }
        fn seek(&self, url: &Url, milliseconds: u64) -> Result<Url> {
            let pairs: Vec<_> = url
                .query_pairs()
                .filter(|(k, _)| k != "offset" && k != "session")
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            let mut url = url.clone();
            url.set_query(None);
            url.query_pairs_mut()
                .extend_pairs(pairs)
                .append_pair("session", &uuid::Uuid::new_v4().to_string())
                .append_pair("offset", &milliseconds.to_string());
            Ok(url)
        }
        fn stop(&self, url: &Url) -> Option<(Method, Url)> {
            let session = url.query_pairs().find(|(k, _)| k == "session")?.1;
            let prefix = url.path().split("/mock/transcode/").next()?;
            let mut stop = url.clone();
            stop.set_path(&format!("{prefix}/mock/stop"));
            stop.set_query(None);
            stop.query_pairs_mut().append_pair("session", &session);
            Some((Method::DELETE, stop))
        }
        fn download_url(&self, base: &Url, part_key: &str) -> Result<Url> {
            crate::plex::server_path(base, part_key)
        }
        fn artwork_url(&self, base: &Url, path: &str) -> Result<Url> {
            crate::plex::server_path(base, path)
        }
        fn metadata_url(&self, base: &Url, id: &str) -> Result<Url> {
            crate::plex::server_path(base, &format!("/mock/items/{id}"))
        }
        fn parse_metadata(&self, body: &[u8]) -> Result<Container> {
            Ok(serde_json::from_slice(body)?)
        }
        fn client_headers(&self, _client_identifier: &str) -> HeaderMap {
            HeaderMap::new()
        }
    }

    impl MediaServer for MockServer {
        fn bind_view(&self, _scope: Option<(tokio::sync::watch::Receiver<u64>, u64)>) {}
        fn cancelled(&self) -> bool {
            false
        }
        fn capabilities(&self) -> Caps {
            self.state().caps
        }
        fn valid_id<'a>(&self, id: &'a str) -> Result<&'a str> {
            match self.ids() {
                MockIds::Plex => crate::plex::valid_id(id),
                MockIds::Guid if guid(id) => Ok(id),
                MockIds::Guid => Err(Error::Input("Expected a server identifier")),
            }
        }
        /// The GUID family filters a Jellyfin-style `api_key`/`ApiKey`
        /// credential parameter instead of `X-Plex-*`.
        fn advertised_key(&self, key: &str) -> Result<(String, Vec<(String, String)>)> {
            match self.ids() {
                MockIds::Plex => {
                    crate::plex::advertised_parts(key, crate::plex::is_plex_credential)
                }
                MockIds::Guid => crate::plex::advertised_parts(key, |name| {
                    name.eq_ignore_ascii_case("api_key") || name.eq_ignore_ascii_case("apikey")
                }),
            }
        }
        fn is_library_key(&self, key: &str) -> bool {
            match self.ids() {
                MockIds::Plex => key.starts_with("/library/"),
                MockIds::Guid => key.strip_prefix("/Items/").is_some_and(guid),
            }
        }
        fn parse_url(&self, value: &str) -> Result<Url> {
            match self.ids() {
                MockIds::Plex => crate::plex::server_url(value),
                MockIds::Guid => crate::plex::server_url_with_base_path(value),
            }
        }
        /// Plex shape by default; the GUID family keys by user, not by URL or
        /// token, like a backend whose re-login must keep downloads.
        fn namespace(&self, conn: &Conn) -> String {
            match (self.ids(), &conn.user_id) {
                (MockIds::Guid, Some(user)) => crate::sha256_hex(&format!("mock\0{user}")),
                _ => crate::cache::namespace(conn.base.as_str(), &conn.token),
            }
        }
        fn auth_headers(&self, conn: &Conn) -> HeaderMap {
            let mut headers = HeaderMap::new();
            if let Ok(value) = HeaderValue::from_str(&conn.token) {
                headers.insert("x-mock-token", value);
            }
            headers
        }
        fn stream_policy(&self, conn: &Conn) -> StreamPolicy {
            StreamPolicy::new(
                Arc::new(MockRules(self.ids())),
                conn.base.clone(),
                self.auth_headers(conn),
            )
        }
        fn children_query(&self, key: &str, kind: Option<&ItemKind>) -> Result<Query> {
            match self.ids() {
                MockIds::Plex => plex_children_query(key, kind),
                MockIds::Guid => {
                    let id = key.strip_prefix("/Items/").unwrap_or(key);
                    let id = self.valid_id(id)?.to_owned();
                    if kind == Some(&ItemKind::Playlist) {
                        Ok(Query::PlaylistItems { id })
                    } else {
                        Ok(Query::Children {
                            id,
                            kind: kind.cloned().unwrap_or_default(),
                        })
                    }
                }
            }
        }
        /// The GUID family lists children by kind, like a backend whose
        /// artists are not folder parents of their albums.
        fn children_need_kind(&self) -> bool {
            self.ids() == MockIds::Guid
        }
        /// Plex's schema vocabulary, so filter tests can script the replies.
        fn filter_schema(&self, section: &str, kind: &ItemKind) -> Option<Query> {
            Some(Query::FilterSchema {
                section: section.into(),
                kind: kind.clone(),
            })
        }
        fn filter_fields(
            &self,
            meta: &serde_json::Value,
            kind: &ItemKind,
        ) -> Vec<(FilterField, String)> {
            plex_filter_fields(meta, kind)
        }
        fn filter_choices(
            &self,
            section: &str,
            kind: &ItemKind,
            field: FilterField,
            meta: Option<&serde_json::Value>,
        ) -> Result<Option<Query>> {
            plex_filter_choices(self, section, kind, field, meta)
        }
        fn smart_source(&self, content: &str) -> SmartSource {
            plex_smart_source(content)
        }
        fn offline_key(&self, query: &Query, page: Option<Page>) -> Result<OfflineKey> {
            plex_request(query, page)
        }
        fn fetch(&self, _conn: &Conn, query: &Query, page: Option<Page>) -> Result<Container> {
            let mut state = self.state();
            state.log.push(Recorded::Fetch(query.clone(), page));
            let reply = state
                .replies
                .get_mut(&(query.clone(), page))
                .and_then(next)
                .ok_or(Error::Input("Mock has no scripted reply"))?;
            Ok(reply?)
        }
        fn act(&self, _conn: &Conn, action: &Action) -> Result<()> {
            let mut state = self.state();
            state.log.push(Recorded::Act(action.clone()));
            state.act_error.map_or(Ok(()), |error| Err(error.into()))
        }
        fn create_playlist(
            &self,
            _conn: &Conn,
            title: &str,
            content: &PlaylistContent,
        ) -> Result<Container> {
            let mut state = self.state();
            state
                .log
                .push(Recorded::CreatePlaylist(title.into(), content.clone()));
            Ok(next(&mut state.creates).ok_or(Error::Input("Mock has no scripted creation"))??)
        }
        fn create_radio(&self, _conn: &Conn, station: &str) -> Result<Container> {
            let mut state = self.state();
            state.log.push(Recorded::CreateRadio(station.into()));
            Ok(next(&mut state.creates).ok_or(Error::Input("Mock has no scripted creation"))??)
        }
        fn lyrics(&self, _conn: &Conn, _stream_key: &str) -> Result<Vec<Line>> {
            crate::lyrics::decode(&self.state().lyrics.clone().ok_or(Error::Http(404))?)
        }
        fn stream_url(&self, conn: &Conn, part_key: &str) -> Result<String> {
            Ok(crate::plex::server_path(&conn.base, part_key)?.into())
        }
        fn transcode_stream(
            &self,
            conn: &Conn,
            id: &str,
            kbps: u32,
            session: &str,
        ) -> Result<String> {
            Ok(MockRules(self.ids())
                .transcode_url(&conn.base, id, kbps, session)?
                .into())
        }
        fn transcode_probe(
            &self,
            _conn: &Conn,
            _id: &str,
            _kbps: u32,
            _session: &str,
        ) -> Result<(usize, String, bool)> {
            Ok((1, "audio/mpeg".into(), true))
        }
        #[allow(refining_impl_trait)]
        fn probe(&self, _conn: &Conn, _id: &str) -> Result<std::future::Ready<Probe>> {
            Ok(std::future::ready(self.state().probe))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, Core, Error};

    #[test]
    fn backend_dispatches_probe_through_either_and_clones_share_the_script() {
        let mock = MockServer::default();
        mock.script_probe(Probe::Gone);
        let backend = Backend::Mock(mock.clone());
        let conn = Conn {
            base: Url::parse("http://127.0.0.1/").unwrap(),
            token: String::new(),
            user_id: None,
        };
        let future = backend.clone().probe(&conn, "1").unwrap();
        assert_eq!(crate::rt::block_on(future), Probe::Gone);
        backend
            .act(
                &conn,
                &Action::Scrobble {
                    id: "1".into(),
                    played_at: 0,
                },
            )
            .unwrap();
        assert_eq!(
            mock.actions(),
            [Action::Scrobble {
                id: "1".into(),
                played_at: 0
            }]
        );
    }

    fn plex_conn() -> Conn {
        Conn {
            base: Url::parse("http://192.0.2.1:32400/").unwrap(),
            token: "fixture-secret".into(),
            user_id: None,
        }
    }
    fn plex() -> Plex {
        Plex::new("fixture-client", Url::parse("https://plex.tv/").unwrap()).unwrap()
    }

    #[test]
    fn the_plex_namespace_is_the_url_and_token_hash_it_always_was() {
        let conn = plex_conn();
        assert_eq!(
            plex().namespace(&conn),
            crate::sha256_hex("http://192.0.2.1:32400/\0fixture-secret")
        );
        // The user id plays no part for Plex.
        let with_user = Conn {
            user_id: Some("u".into()),
            ..conn.clone()
        };
        assert_eq!(plex().namespace(&with_user), plex().namespace(&conn));
        let other = Conn {
            token: "other".into(),
            ..conn.clone()
        };
        assert_ne!(plex().namespace(&other), plex().namespace(&conn));
    }

    #[test]
    fn a_listing_with_two_title_conditions_is_refused_by_plex_too() {
        let mut spec = BrowseSpec::new(ItemKind::Track, Sort::Title);
        spec.search = Some("a".into());
        assert!(plex()
            .offline_key(&Query::browse("3", spec.clone()), None)
            .is_ok());
        spec.filter.title = "b".into();
        assert!(plex().offline_key(&Query::browse("3", spec), None).is_err());
    }

    #[test]
    fn plex_owns_its_identifier_and_key_rules() {
        let plex = plex();
        assert_eq!(plex.valid_id("12345").unwrap(), "12345");
        for bad in [
            "",
            "1a",
            "../1",
            "-1",
            &"9".repeat(21),
            "a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4",
        ] {
            assert!(plex.valid_id(bad).is_err(), "{bad}");
            assert!(!plex.is_id(bad), "{bad}");
        }
        assert!(plex.is_id(&"9".repeat(20)));
        assert!(plex.is_library_key("/library/metadata/1/station/x"));
        assert!(!plex.is_library_key("/hubs/sections/1"));
        assert!(!plex.is_library_key("/Items/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(plex.parse_url("https://plex.example:32400/").is_ok());
        assert!(plex
            .parse_url("https://plex.example:32400/prefix/")
            .is_err());
    }

    #[test]
    fn plex_filter_choices_follow_the_schema_key_inside_its_section() {
        let plex = plex();
        let meta = |key: &str| {
            serde_json::json!({"Type":[
                {"type":"album","Filter":[{"filter":"genre","key":"/library/sections/3/album-genre"}]},
                {"type":"track","Filter":[{"filter":"genre","key":key}]}
            ]})
        };
        let choices = |section: &str, key: &str, field: FilterField| {
            plex.filter_choices(section, &ItemKind::Track, field, Some(&meta(key)))
        };
        assert_eq!(
            choices("3", "/library/sections/3/genre?type=10", FilterField::Genre).unwrap(),
            Some(Query::advertised(
                "/library/sections/3/genre",
                vec![("type".into(), "10".into())]
            ))
        );
        // Another section's key, a credential parameter: refused.
        assert!(choices("3", "/library/sections/33/genre", FilterField::Genre).is_err());
        assert!(choices(
            "3",
            "/library/sections/3/genre?X-Plex-Token=x",
            FilterField::Genre
        )
        .is_err());
        // No advertised filter for the field or no schema: unavailable.
        assert_eq!(choices("3", "/x", FilterField::Mood).unwrap(), None);
        assert_eq!(
            plex.filter_choices("3", &ItemKind::Track, FilterField::Genre, None)
                .unwrap(),
            None
        );
    }

    #[test]
    fn plex_smart_sources_decode_the_content_uri() {
        assert_eq!(
            percent_decode("server%3A%2F%2Fid%2Fall%3Ftitle%3Da+b&c%2Bd"),
            "server://id/all?title=a+b&c+d"
        );
        let source = plex_smart_source(
            "server%3A%2F%2Fm%2Fcom.plexapp.plugins.library%2Flibrary%2Fsections%2F3%2Fall%3Ftype%3D10%26album.genre%3D12%26track.year%253E%253D%3D1990%26sort%3DviewCount%253Adesc%26limit%3D50",
        );
        assert_eq!(
            source,
            SmartSource {
                section: "3".into(),
                filter: Music {
                    genre: "12".into(),
                    year_from: Some(1990),
                    ..Music::default()
                },
                sort: Sort::PlayCount,
                limit: 50,
                editable: true,
            }
        );
        // Unknown pairs, sorts or limits make the source read-only.
        for content in [
            "server://m/library/sections/3/all?type=10&other=1",
            "server://m/library/sections/3/all?sort=rating",
            "server://m/library/sections/3/all?limit=0",
            "not a uri",
        ] {
            assert!(!plex_smart_source(content).editable, "{content}");
        }
    }

    #[test]
    fn plex_children_keys_map_to_queries_and_reject_foreign_ids() {
        let plex = plex();
        assert_eq!(
            plex.children_query("/library/metadata/7/children", Some(&ItemKind::Album))
                .unwrap(),
            Query::Children {
                id: "7".into(),
                kind: ItemKind::Album
            }
        );
        assert_eq!(
            plex.children_query("7", None).unwrap(),
            Query::Children {
                id: "7".into(),
                kind: ItemKind::default()
            }
        );
        assert_eq!(
            plex.children_query("/playlists/9/items", None).unwrap(),
            Query::PlaylistItems { id: "9".into() }
        );
        for bad in [
            "/library/metadata/x/children",
            "/playlists/../items",
            "guid",
        ] {
            assert!(plex.children_query(bad, None).is_err(), "{bad}");
        }
    }

    #[test]
    fn plex_advertised_keys_split_and_refuse_plex_credentials() {
        let plex = plex();
        assert_eq!(
            plex.advertised_key("/library/sections/3/all?type=9&sort=addedAt:desc")
                .unwrap(),
            (
                "/library/sections/3/all".to_owned(),
                vec![
                    ("type".to_owned(), "9".to_owned()),
                    ("sort".to_owned(), "addedAt:desc".to_owned())
                ]
            )
        );
        assert_eq!(
            plex.advertised_key("/hubs/sections/3").unwrap(),
            ("/hubs/sections/3".to_owned(), vec![])
        );
        for bad in [
            "/hubs?X-Plex-Token=x",
            "/hubs?x-plex-client-identifier=y",
            "https://evil.example/hubs",
            "//evil.example/hubs",
            "/library/../hubs",
            "hubs",
            "/hubs#x",
        ] {
            assert!(plex.advertised_key(bad).is_err(), "{bad}");
        }
        // Only Plex's own credential vocabulary is filtered.
        assert!(plex.advertised_key("/hubs?api_key=x").is_ok());
    }

    #[test]
    fn plex_playlist_entry_ids_are_validated_before_they_reach_a_path() {
        let conn = plex_conn();
        for action in [
            Action::PlaylistRemove {
                id: "3".into(),
                item: "../x".into(),
            },
            Action::PlaylistMove {
                id: "3".into(),
                item: "4".into(),
                after: Some("a b".into()),
                index: Some(0),
            },
            Action::PlaylistMove {
                id: "3".into(),
                item: "entry-guid".into(),
                after: None,
                index: None,
            },
        ] {
            assert!(
                matches!(plex().act(&conn, &action), Err(Error::Input(_))),
                "{action:?}"
            );
        }
    }

    #[test]
    fn credentials_are_sensitive_headers_and_never_part_of_a_stream_policy_url() {
        let conn = plex_conn();
        let headers = plex().auth_headers(&conn);
        assert_eq!(headers.len(), 1);
        let token = &headers["x-plex-token"];
        assert_eq!(token, "fixture-secret");
        assert!(token.is_sensitive());
        assert!(plex()
            .auth_headers(&Conn {
                token: String::new(),
                ..conn.clone()
            })
            .is_empty());

        let policy = plex().stream_policy(&conn);
        assert_eq!(policy.headers(), &headers);
        let url = policy.transcode_url("7", 128, SESSION_ID).unwrap();
        assert!(!url.as_str().contains("fixture-secret"));
        assert!(policy.is_transcode(&url));
        let direct = Url::parse("http://192.0.2.1:32400/library/parts/5/1/file.mp3").unwrap();
        assert!(!policy.is_transcode(&direct));
        assert!(policy.download_url("/library/parts/5/1/file.mp3").is_ok());
        assert!(policy
            .download_url("/library/parts/5/1/file.mp3?x=1")
            .is_err());
        assert!(policy
            .artwork_url("/library/metadata/7/thumb/1?x=1")
            .is_ok());
        assert!(policy.artwork_url("/p?X-Plex-Token=other").is_err());
        assert!(policy.valid_id("12") && !policy.valid_id("a"));
        assert_eq!(
            policy.metadata_url("7").unwrap().as_str(),
            "http://192.0.2.1:32400/library/metadata/7"
        );
        assert!(policy.metadata_url("7/../8").is_err());
        assert_eq!(
            policy.client_headers("cid")["x-plex-client-identifier"],
            "cid"
        );
    }

    const SESSION_ID: &str = "123e4567-e89b-12d3-a456-426614174000";

    #[test]
    fn the_policy_seeks_only_transcodes_and_names_the_stop_request() {
        let policy = plex().stream_policy(&plex_conn());
        let transcode = policy.transcode_url("7", 128, SESSION_ID).unwrap();
        let (seek, transcoded) = policy.seek(transcode.as_str(), 12_345).unwrap();
        assert!(transcoded);
        let seek = Url::parse(&seek).unwrap();
        assert!(seek
            .query_pairs()
            .any(|(k, v)| k == "offset" && v == "12.345"));
        assert!(
            !seek.query_pairs().any(|(_, v)| v == SESSION_ID),
            "new session"
        );
        let (method, stop) = policy.stop(&seek).unwrap();
        assert_eq!(method, Method::GET);
        assert_eq!(stop.path(), "/video/:/transcode/universal/stop");
        let direct = "http://192.0.2.1:32400/library/parts/5/1/file.mp3?x=1";
        assert_eq!(
            policy.seek(direct, 5000).unwrap(),
            (direct.to_owned(), false)
        );
        assert!(policy.stop(&Url::parse(direct).unwrap()).is_none());

        // The default policy (a `file:` source, tests) has Plex rules, no
        // credential and no base.
        let default = StreamPolicy::default();
        assert!(default.headers().is_empty());
        assert!(default.transcode_url("7", 128, SESSION_ID).is_err());
        assert!(default.is_transcode(&transcode));
    }

    #[test]
    fn a_policy_from_another_backend_seeks_and_stops_in_its_own_vocabulary() {
        let mock = MockServer::default();
        mock.script_ids(MockIds::Guid);
        let conn = Conn {
            base: Url::parse("https://media.example/jf/").unwrap(),
            token: "t".into(),
            user_id: Some("u".into()),
        };
        let id = "a".repeat(32);
        let policy = mock.stream_policy(&conn);
        assert!(policy.valid_id(&id) && !policy.valid_id("12"));
        let url = policy.transcode_url(&id, 128, SESSION_ID).unwrap();
        assert!(url.path().starts_with("/jf/mock/transcode/"), "{url}");
        let (seek, transcoded) = policy.seek(url.as_str(), 2500).unwrap();
        assert!(transcoded);
        assert!(seek.contains("offset=2500"), "{seek}");
        let (method, stop) = policy.stop(&Url::parse(&seek).unwrap()).unwrap();
        assert_eq!(method, Method::DELETE);
        assert_eq!(stop.path(), "/jf/mock/stop");
        // The stop request stays on the server, so it carries the credential.
        assert_eq!(policy.headers_for(&stop)["x-mock-token"], "t");
        assert_eq!(policy.headers()["x-mock-token"], "t");
        // A Plex transcode is not this backend's transcode.
        let plex_transcode =
            crate::quality::transcode_url(&conn.base, "7", 128, SESSION_ID).unwrap();
        assert!(!policy.is_transcode(&plex_transcode));
    }

    #[test]
    fn a_legacy_url_credential_only_fills_a_missing_header() {
        let legacy = Url::parse("http://192.0.2.9:32400/library/parts/1/a.mp3").unwrap();
        let policy = StreamPolicy::default().with_legacy_token(&legacy, "legacy");
        assert_eq!(policy.headers()["x-plex-token"], "legacy");
        // Without a connection the credential is scoped to the URL's origin.
        assert_eq!(policy.headers_for(&legacy)["x-plex-token"], "legacy");
        let elsewhere = Url::parse("http://192.0.2.10:32400/library/parts/1/a.mp3").unwrap();
        assert!(policy.headers_for(&elsewhere).is_empty());
        let own = plex()
            .stream_policy(&plex_conn())
            .with_legacy_token(&legacy, "legacy");
        assert_eq!(own.headers()["x-plex-token"], "fixture-secret");
        assert!(StreamPolicy::default()
            .with_legacy_token(&legacy, "")
            .headers()
            .is_empty());
        // Another backend's headers survive; the legacy credential is added.
        let mut mock_headers = HeaderMap::new();
        mock_headers.insert("x-mock-token", HeaderValue::from_static("own"));
        let policy = StreamPolicy::new(
            Arc::new(MockRules(MockIds::Plex)),
            Url::parse("http://192.0.2.1:32400/").unwrap(),
            mock_headers,
        )
        .with_legacy_token(&legacy, "legacy");
        assert_eq!(policy.headers().len(), 2);
        assert_eq!(policy.headers()["x-mock-token"], "own");
        assert_eq!(policy.headers()["x-plex-token"], "legacy");
        assert!(policy.headers()["x-plex-token"].is_sensitive());
    }

    #[test]
    fn credentials_go_only_to_the_policys_origin_and_base_path() {
        let plex_policy = plex().stream_policy(&plex_conn());
        let url = |value: &str| Url::parse(value).unwrap();
        for covered in [
            "http://192.0.2.1:32400/library/parts/5/1/file.mp3",
            "http://192.0.2.1:32400/video/:/transcode/universal/stop?session=x",
        ] {
            assert_eq!(
                plex_policy.headers_for(&url(covered))["x-plex-token"],
                "fixture-secret",
                "{covered}"
            );
        }
        for foreign in [
            "http://192.0.2.2:32400/library/parts/5/1/file.mp3",
            "https://192.0.2.1:32400/library/parts/5/1/file.mp3",
            "http://192.0.2.1:32401/library/parts/5/1/file.mp3",
            "http://evil.example/library/parts/5/1/file.mp3",
        ] {
            assert!(
                plex_policy.headers_for(&url(foreign)).is_empty(),
                "{foreign}"
            );
        }
        let mock = MockServer::default();
        mock.script_ids(MockIds::Guid);
        let prefixed = mock.stream_policy(&Conn {
            base: url("https://media.example/jf/"),
            token: "t".into(),
            user_id: None,
        });
        for covered in [
            "https://media.example/jf/",
            "https://media.example/jf/Audio/x",
        ] {
            assert!(prefixed.covers(&url(covered)), "{covered}");
        }
        for foreign in [
            "https://media.example/",
            "https://media.example/jfx/Audio/x",
            "https://media.example/other/jf/Audio/x",
            "https://media.example:8443/jf/Audio/x",
        ] {
            assert!(prefixed.headers_for(&url(foreign)).is_empty(), "{foreign}");
        }
        assert!(StreamPolicy::default()
            .headers_for(&url("http://192.0.2.1:32400/x"))
            .is_empty());
    }

    #[test]
    fn lyrics_arrive_as_lines_from_the_trait() {
        let mock = MockServer::default();
        mock.script_lyrics("[00:01.00]one\n[00:02.50]two");
        let lines = mock.lyrics(&plex_conn(), "/x").unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].time, Some(2500));
    }

    #[test]
    fn an_injected_unauthorized_reaches_core_and_a_5xx_uses_the_saved_copy() {
        for (error, offline) in [
            (MockError::Http(503), true),
            (MockError::Unauthorized, false),
        ] {
            let mock = MockServer::default();
            mock.script(Query::Sections, None, Ok(Container::default()));
            mock.script(Query::Sections, None, Err(error));
            let dir = tempfile::tempdir().unwrap();
            let mut core =
                Core::with_backend(dir.path().into(), Backend::Mock(mock.clone())).unwrap();
            core.execute(Command::Connect {
                url: "http://127.0.0.1:32400/".into(),
                token: "synthetic".into(),
            })
            .unwrap();
            let result = core.execute(Command::Libraries);
            if offline {
                assert_eq!(result.unwrap()["offline"], true);
            } else {
                assert!(matches!(result, Err(Error::Unauthorized)));
            }
        }
    }
}

#[cfg(test)]
#[path = "server_wire_tests.rs"]
mod plex_wire_tests;
