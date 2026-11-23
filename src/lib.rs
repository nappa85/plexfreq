mod artwork;
pub mod audio;
mod bluetooth;
pub mod cache;
mod catalogue;
mod command;
mod daily;
mod diagnostics;
mod discovery;
mod features;
mod ffi;
mod filters;
mod health;
#[cfg(test)]
mod health_tests;
pub mod history;
pub mod jellyfin;
pub mod lyrics;
pub mod model;
#[cfg(test)]
mod network_quality_tests;
pub mod offline;
#[cfg(test)]
mod offline_browse_tests;
mod pagination;
pub mod plex;
pub mod quality;
pub mod queue;
pub mod radio;
#[cfg(test)]
mod radio_health_tests;
#[cfg(test)]
mod radio_refill_tests;
mod refresh;
mod rt;
pub mod runtime;
pub mod server;
pub mod store;

pub use command::{CollectionView, Command, DownloadAction, PlaylistAction, SmartAction};
pub use daily::{DownloadKind, GroupId};
use model::{Item, ItemKind, Pin, Resource};
use plex::Plex;
use queue::{Queue, Repeat};
use radio::{Radio, RadioKind, Source};
use serde_json::{json, Value};
use server::{Action, Backend, BrowseSpec, Conn, MediaServer, Page, Query, Sort};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Input(&'static str),
    #[error("Network request failed (check connection and server address)")]
    Network,
    #[error("Authorization failed; sign in again or check server token")]
    Unauthorized,
    #[error("Unexpected server response")]
    Protocol,
    #[error("Unexpected server response ({0})")]
    ProtocolAt(&'static str),
    #[error("Server returned HTTP {0}")]
    Http(u16),
    #[error("Invalid URL")]
    Url(#[from] url::ParseError),
    #[error("Invalid request data")]
    Json(#[from] serde_json::Error),
    #[error("Cannot read or write application state")]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, Error>;

/// Seconds since the Unix epoch (0 if the clock is before it).
pub(crate) fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
/// Lowercase hex SHA-256 of `value`: cache keys, namespaces, file names.
pub(crate) fn sha256_hex(value: &str) -> String {
    use sha2::Digest;
    use std::fmt::Write;
    let digest = sha2::Sha256::digest(value.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Saved items per offline browse page.
const OFFLINE_BROWSE_PAGE: usize = 100;

/// Fault injection for unit tests: panic inside `Core::execute` after a
/// command already changed in-memory state but before it was persisted.
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::path::{Path, PathBuf};
    static PANIC_AFTER: std::sync::Mutex<Vec<(PathBuf, &'static str)>> =
        std::sync::Mutex::new(Vec::new());
    /// Arm one panic for the next `op` executed by the Core owning `dir`.
    pub(crate) fn panic_after(dir: &Path, op: &'static str) {
        crate::mutex_lock(&PANIC_AFTER).push((dir.to_path_buf(), op));
    }
    /// Whether a panic for `op` is still armed (has not fired yet).
    pub(crate) fn armed(dir: &Path, op: &str) -> bool {
        crate::mutex_lock(&PANIC_AFTER)
            .iter()
            .any(|(d, o)| d == dir && *o == op)
    }
    pub(crate) fn check(dir: &Path, op: &str) {
        let mut armed = crate::mutex_lock(&PANIC_AFTER);
        if let Some(index) = armed.iter().position(|(d, o)| d == dir && *o == op) {
            armed.remove(index);
            drop(armed);
            panic!("injected Core panic after {op}");
        }
    }
}

/// Lock a mutex, recovering from poisoning instead of panicking worker threads.
/// A poisoned mutex means another thread panicked while holding it; the data
/// itself is still usable, so continue with it rather than crashing audio,
/// cache or request workers.
pub(crate) fn mutex_lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Shared atomic file write: temp file with 0600, fsync, rename, dir fsync.
/// Used for cache index, offline snapshots and session state.
pub(crate) fn atomic_write_bytes(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::{
        fs::{self, OpenOptions},
        io::Write,
        os::unix::fs::OpenOptionsExt,
    };
    // Append to the full file name instead of replacing the extension:
    // `with_extension("<uuid>.tmp")` turns `index.json` into `index.<uuid>.tmp`
    // whose stem is `index`, invisible to hash-stem GC and quota accounting.
    // `index.json.<uuid>.tmp` always ends with `.tmp` and is reclaimable.
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("tmp");
    let temp = path.with_file_name(format!("{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let result: Result<()> = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        // Ensure the rename itself is durable; a crash between rename and
        // journal commit could otherwise lose the file.
        if let Some(parent) = path.parent() {
            if let Ok(dir) = fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

pub(crate) fn atomic_write_json(
    path: &std::path::Path,
    value: &impl serde::Serialize,
) -> Result<()> {
    atomic_write_bytes(path, &serde_json::to_vec(value)?)
}

/// Preserve malformed state privately before allowing defaults to replace it.
pub(crate) fn quarantine_corrupt(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let backup = path.with_file_name(format!("{name}.corrupt-{}", uuid::Uuid::new_v4()));
    atomic_write_bytes(&backup, bytes)?;
    std::fs::remove_file(path)?;
    Ok(())
}

/// Shown when a queued track no longer exists on the server. Comparisons use
/// this constant; user-facing errors spell the literal inline so that the
/// translation extractor finds it (pinned by a test).
pub(crate) const GONE_TRACK: &str = "This track is no longer on the server";

/// Tracks asked of the server's instant mix per radio batch.
const MIX_BATCH: u32 = 50;
/// Instant-mix requests per refill: the radio's seed, then queued tracks as
/// seeds while the mix brings nothing new.
const MIX_ATTEMPTS: usize = 3;
/// Rotation seeds a mix radio remembers.
const MIX_USED_SEEDS: usize = 64;

/// The reply of a Quick Connect request that ran out: nothing is pending and
/// the user is told why (the message is translated by the adapter).
fn quick_connect_expired() -> Value {
    json!({"quickConnect":null,"notice":"Login expired; start again"})
}
/// The backend `settings` are signed in to.
fn make_backend(settings: &store::Settings, account_url: &url::Url) -> Result<Backend> {
    Ok(match settings.server_kind {
        store::ServerKind::Plex => {
            Backend::Plex(Plex::new(&settings.client_identifier, account_url.clone())?)
        }
        store::ServerKind::Jellyfin => Backend::Jellyfin(jellyfin::Jellyfin::new(
            &settings.client_identifier,
            &settings.server_id,
        )),
    })
}

/// A Quick Connect request: where it was made, the secret that proves it is
/// ours (never shown or logged) and the code the user approves elsewhere.
struct QuickConnectPending {
    base: url::Url,
    secret: String,
    code: String,
    started: Instant,
}

/// The server connection of `settings` for `backend`: base URL, credential
/// and signed-in user.
fn connection(backend: &Backend, settings: &store::Settings) -> Result<Conn> {
    if settings.server_url.is_empty() {
        return Err(Error::Input("Connect to a server first"));
    }
    Ok(Conn {
        base: backend.parse_url(&settings.server_url)?,
        token: settings.server_token.clone(),
        user_id: Some(settings.server_user_id.clone()).filter(|id| !id.is_empty()),
    })
}

/// The account/server scope of everything saved on disk for `settings`. A
/// signed-out or unparsable address keeps the plain URL-and-token hash.
fn settings_namespace(backend: &Backend, settings: &store::Settings) -> String {
    match connection(backend, settings) {
        Ok(conn) => backend.namespace(&conn),
        Err(_) => cache::namespace(&settings.server_url, &settings.server_token),
    }
}

/// Where the current track's audio comes from (`playbackSource` in plans and
/// the presented model).
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    serde::Deserialize,
    serde::Serialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
    strum::EnumIter,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum PlaybackSource {
    /// A completed cache entry (offline-capable).
    Cache,
    /// The original file streamed from the server.
    Stream,
    /// A server transcode at the mobile bitrate.
    Transcode,
}

pub struct Core {
    refresh: refresh::Scheduler,
    planning_guard: Option<(std::sync::Arc<std::sync::atomic::AtomicU64>, u64)>,
    offline_index: std::cell::RefCell<Option<catalogue::Index>>,
    filter_schemas: std::cell::RefCell<std::collections::BTreeMap<String, Value>>,
    /// The artist/album kind of each detail key seen on a first page, so a
    /// later page lists the children of the right kind without refetching.
    detail_kinds: std::cell::RefCell<std::collections::BTreeMap<String, ItemKind>>,
    catalogue_epoch: u64,
    dir: PathBuf,
    settings: store::Settings,
    backend: Backend,
    /// Where Plex sign-in lives, to build a Plex backend after a Jellyfin one.
    account_url: url::Url,
    pin: Option<(Pin, Instant)>,
    /// A Jellyfin Quick Connect request waiting for the user's approval.
    quick_connect: Option<QuickConnectPending>,
    servers: Vec<Resource>,
    queue: Queue,
    radio: Option<Radio>,
    cache: cache::Cache,
    cached_playback: bool,
    /// The current track plays from a server transcode (reported as the play
    /// method, see [`server::Reporting`]).
    transcoding: bool,
    manual_cache: Vec<Item>,
    cache_suspended: bool,
    /// Last cache scheduling outcome reported to diagnostics.
    cache_schedule_seen: std::cell::RefCell<Option<CacheSchedule>>,
    artist_indexes: std::collections::BTreeMap<String, Value>,
    writable: bool,
    resume_position: u64,
    playback_generation: u64,
    library: offline::Library,
    artwork: offline::Artwork,
    offline_mode: bool,
    network_wifi: bool,
    /// The default route is cellular: mobile streaming quality applies.
    network_cellular: bool,
    network_online: bool,
    network_known: bool,
    health: health::Health,
    radio_warning: std::cell::Cell<bool>,
    cached_failures: std::cell::Cell<u64>,
    refill_epoch: uuid::Uuid,
}

/// What `schedule_cache` decided, for change-only diagnostics. Holds counts,
/// fixed names and a rating key only.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CacheSchedule {
    Scheduled {
        items: usize,
        manual: usize,
        quality_kbps: u32,
        first: String,
        cellular_cap: bool,
    },
    Skipped(&'static str),
}
impl CacheSchedule {
    /// The line to log when `self` differs from the previously seen outcome.
    fn changed(&self, seen: &mut Option<Self>) -> Option<String> {
        if seen.as_ref() == Some(self) {
            return None;
        }
        *seen = Some(self.clone());
        Some(match self {
            Self::Scheduled {
                items,
                manual,
                quality_kbps,
                first,
                cellular_cap,
            } => format!(
                "cache schedule items={items} manual={manual} quality_kbps={quality_kbps} first={first} cellular_cap={cellular_cap}"
            ),
            Self::Skipped(reason) => format!("cache schedule skipped reason={reason}"),
        })
    }
}

/// Outcome of a planned automatic skip away from a failed current item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Skip {
    /// A playable item that needs no network to plan.
    Ready,
    /// A playable item whose plan would wait on a metadata fetch.
    NeedsMetadata,
    /// Only gone items (or nothing) ahead: a natural end.
    End,
    /// Nothing playable behind (backward skip).
    Stuck,
}

pub(crate) struct Prepared {
    queue: Queue,
    radio: Option<Radio>,
    cached: bool,
    pub plan: Value,
}
/// A radio refill to fetch off the Core worker. The token lives only in
/// memory and is sent as a header; this type is deliberately not `Debug`.
pub(crate) struct RefillRequest {
    conn: Conn,
    query: Query,
    identity: RefillIdentity,
    /// Mix radio only: the queued tracks to ask next, as (track key, query),
    /// when `query` returns nothing unseen, and the keys queued then.
    mix_fallbacks: Vec<(String, Query)>,
    mix_queued: std::collections::BTreeSet<String>,
    /// Mix radio only: the fallback seeds asked by [`RefillRequest::fetch`].
    mix_attempted: Vec<String>,
}
impl RefillRequest {
    /// Fetch `query`; a mix that holds no unseen playable track is asked
    /// again from the next fallback seed (at most [`MIX_ATTEMPTS`] requests
    /// in all). `query` becomes the one whose reply is returned.
    fn fetch(&mut self, backend: &Backend) -> Result<model::Container> {
        let mut result = backend.fetch(&self.conn, &self.query, None)?;
        let fallbacks = std::mem::take(&mut self.mix_fallbacks);
        for (key, query) in fallbacks.into_iter().take(MIX_ATTEMPTS - 1) {
            if has_unseen_mix(backend, &result, &self.mix_queued) {
                break;
            }
            match backend.fetch(&self.conn, &query, None) {
                Ok(reply) => {
                    self.mix_attempted.push(key);
                    // `query` stays the one whose reply is kept, for its snapshot.
                    result = reply;
                    self.query = query;
                }
                Err(error @ Error::Unauthorized) => return Err(error),
                // A connection or server failure says nothing about the seed:
                // stop rotating and keep it for a later refill.
                Err(error @ (Error::Network | Error::Http(500..=599))) => {
                    crate::diagnostics::event(format_args!(
                        "radio mix fallback seed failed: {error}"
                    ));
                    break;
                }
                // The seed itself failed (gone, refused): it is not asked
                // again, and the reply and query kept so far stand.
                Err(error) => {
                    self.mix_attempted.push(key);
                    crate::diagnostics::event(format_args!(
                        "radio mix fallback seed failed: {error}"
                    ));
                }
            }
        }
        Ok(result)
    }
}
/// Whether an instant-mix reply holds a playable track not in `seen`.
fn has_unseen_mix(
    backend: &Backend,
    c: &model::Container,
    seen: &std::collections::BTreeSet<String>,
) -> bool {
    c.items.iter().any(|i| {
        i.kind == crate::model::ItemKind::Track
            && backend.is_id(&i.rating_key)
            && !seen.contains(&i.rating_key)
    })
}
#[derive(Clone, PartialEq)]
struct RefillIdentity {
    epoch: uuid::Uuid,
    server: String,
    instance: u64,
    version: u64,
    queue: u64,
}
pub(crate) struct RefillJob {
    request: RefillRequest,
    backend: Backend,
}
pub(crate) struct RefillFetched {
    request: RefillRequest,
    result: Result<model::Container>,
}
impl RefillJob {
    /// Blocking fetch; never call it on the shared runtime thread.
    pub(crate) fn fetch(self) -> RefillFetched {
        let RefillJob { request, backend } = self;
        let mut request = request;
        let result = request.fetch(&backend);
        RefillFetched { request, result }
    }
}
impl Prepared {
    /// Rating key of the prepared successor.
    pub(crate) fn key(&self) -> Option<&str> {
        self.queue.track().map(|track| track.rating_key.as_str())
    }
}

impl Core {
    /// A real refresh attempt failed, even if it was made while preparing audio.
    /// This fact is independent of whether that speculative plan becomes audible.
    pub(crate) fn warn_radio(&self) {
        if !self.offline_mode {
            self.radio_warning.set(true);
        }
    }
    pub(crate) fn take_radio_warning(&self) -> bool {
        self.radio_warning.replace(false) && !self.offline_mode
    }
    pub(crate) fn prepare_next(&mut self) -> Result<Option<Prepared>> {
        if self.queue.track().is_none()
            || self.queue.at_end()
                && self.radio.is_none()
                && !self.settings.autoplay
                && self.queue.repeat() == Repeat::Off
        {
            return Ok(None);
        }
        let old = self.queue.clone();
        let radio = self.radio.clone();
        let cached = self.cached_playback;
        let transcoding = self.transcoding;
        let notice = self.health.take_notice();
        let result = self.advance_next(true, false);
        // Speculation never reports: the real transition will.
        self.health.take_notice();
        if notice {
            self.health.set_notice();
        }
        let queue = std::mem::replace(&mut self.queue, old);
        let next_radio = std::mem::replace(&mut self.radio, radio);
        let next_cached = std::mem::replace(&mut self.cached_playback, cached);
        self.transcoding = transcoding;
        let prepared = match result {
            Ok(mut plan) if queue.track().is_some() => {
                plan["queue"] = self.queue_json(&queue);
                Ok(Some(Prepared {
                    queue,
                    radio: next_radio,
                    cached: next_cached,
                    plan,
                }))
            }
            Ok(_) => Ok(None),
            Err(e) => Err(e),
        };
        self.schedule_cache();
        prepared
    }
    pub(crate) fn commit_prepared(&mut self, prepared: Prepared) -> Result<Value> {
        self.queue = prepared.queue;
        self.radio = prepared.radio;
        self.cached_playback = prepared.cached;
        self.transcoding = prepared.plan["playbackSource"] == json!(PlaybackSource::Transcode);
        self.resume_position = 0;
        self.playback_generation = self.playback_generation.wrapping_add(1);
        self.settings.playback = store::Playback {
            queue: self.queue.clone(),
            radio: self.radio.clone(),
            position: 0,
            generation: self.playback_generation,
            occurrence: uuid::Uuid::new_v4().to_string(),
            listened: 0,
            start_reported: String::new(),
        };
        store::save(&self.dir, &self.settings)?;
        let mut plan = prepared.plan;
        if let Some(object) = plan.as_object_mut() {
            object.remove("stream");
        }
        plan["queue"] = self.queue_json(&self.queue);
        plan["playbackOccurrence"] = json!(self.settings.playback.occurrence);
        plan["playbackGeneration"] = json!(self.playback_generation);
        Ok(plan)
    }
    pub(crate) fn audio_config(&self) -> audio::dsp::Config {
        self.settings.audio.clone()
    }
    pub(crate) fn set_audio_config(&mut self, config: audio::dsp::Config) -> Result<()> {
        config.validate()?;
        self.settings.audio = config;
        store::save(&self.dir, &self.settings)
    }
    pub fn new(dir: PathBuf) -> Result<Self> {
        Self::with_account_url(dir, url::Url::parse("https://plex.tv/")?)
    }
    pub fn with_account_url(dir: PathBuf, account_url: url::Url) -> Result<Self> {
        Self::build(dir, account_url, true, None)
    }
    /// Test constructor: Core over an injected backend (e.g. the mock).
    #[cfg(any(test, feature = "mock-backend"))]
    pub fn with_backend(dir: PathBuf, backend: Backend) -> Result<Self> {
        Self::build(
            dir,
            url::Url::parse("https://plex.tv/")?,
            true,
            Some(backend),
        )
    }
    /// Test accessor: how many artist/album kinds Core remembers for later
    /// detail pages.
    #[cfg(any(test, feature = "mock-backend"))]
    pub fn remembered_detail_kinds(&self) -> usize {
        self.detail_kinds.borrow().len()
    }
    pub fn inspect(dir: PathBuf) -> Result<Self> {
        Self::build(dir, url::Url::parse("https://plex.tv/")?, false, None)
    }
    fn build(
        dir: PathBuf,
        account_url: url::Url,
        downloads: bool,
        injected: Option<Backend>,
    ) -> Result<Self> {
        if downloads {
            store::remove_stale_temps(&dir);
        }
        // Read-only inspection (refresh worker) never writes, migrates or
        // compacts session files; only the owner repairs them.
        let mut settings = store::load(&dir)?;
        if settings.client_identifier.is_empty() {
            settings.client_identifier = uuid::Uuid::new_v4().to_string();
            if downloads {
                store::save(&dir, &settings)?;
            }
        }
        // Legacy station download groups were keyed by station path only;
        // scope them to this server/account at startup (owner only).
        let plex = match injected {
            Some(backend) => backend,
            None => make_backend(&settings, &account_url)?,
        };
        let namespace = settings_namespace(&plex, &settings);
        if downloads && daily::migrate_station_groups(&mut settings, &namespace) {
            store::save(&dir, &settings)?;
        }
        if settings.playback.queue.validate().is_err() {
            settings.playback = store::Playback::default();
        }
        if settings.playback.queue.current().is_some() && settings.playback.occurrence.is_empty() {
            settings.playback.occurrence = uuid::Uuid::new_v4().to_string();
        }
        // Older pins predate durable refresh recipes; retain their membership
        // and give audio playlists the same opt-in refresh behavior as new plans.
        let legacy: Vec<_> = settings
            .download_groups
            .keys()
            .filter(|group| {
                group.kind == DownloadKind::Playlist
                    && !settings.download_plans.contains_key(*group)
            })
            .cloned()
            .collect();
        for group in legacy {
            if plex.is_id(&group.key) {
                let plan = daily::DownloadPlan {
                    kind: group.kind,
                    key: group.key.clone(),
                    minutes: 0,
                    refreshed_at: crate::unix_seconds(),
                };
                settings.download_plans.insert(group, plan);
            }
        }
        let queue = settings.playback.queue.clone();
        let radio = settings.playback.radio.clone();
        let resume_position = settings.playback.position;
        let manual_cache = settings.downloads.clone();
        // Corrupt persisted cache config must not brick startup. Validate and
        // fall back to defaults instead of propagating Cache::new's error.
        if settings.cache.validate().is_err() {
            settings.cache = cache::Config::default();
            // Best-effort repair so the next launch starts clean; failures are
            // non-fatal (in-memory fallback already applies).
            if downloads {
                let _ = store::save(&dir, &settings);
            }
        }
        // Corrupt persisted audio/quality configs must not silently misbehave
        // (e.g. headroom 999 muting output). Fall back to defaults.
        if settings.audio.validate().is_err() {
            settings.audio = audio::dsp::Config::default();
            if downloads {
                let _ = store::save(&dir, &settings);
            }
        }
        if settings.quality.validate().is_err() {
            settings.quality = quality::Config::default();
            if downloads {
                let _ = store::save(&dir, &settings);
            }
        }
        // An interval of 0 would make every plan permanently stale.
        if settings.refresh_policy.validate().is_err() {
            settings.refresh_policy = refresh::Policy::default();
            if downloads {
                let _ = store::save(&dir, &settings);
            }
        }
        // Migrate an old all-in-one session and compact damaged/oversized
        // logs once; failures retry at the next checkpoint.
        if downloads && store::needs_maintenance(&settings) {
            if let Err(error) = store::save(&dir, &settings) {
                diagnostics::event(format_args!("session maintenance not saved: {error}"));
            }
        }
        let cache = if downloads {
            cache::Cache::new(
                dir.join("audio-cache"),
                settings.cache.clone(),
                &settings.client_identifier,
            )?
        } else {
            cache::Cache::read_only(
                dir.join("audio-cache"),
                settings.cache.clone(),
                &settings.client_identifier,
            )?
        };
        let mut artwork = offline::Artwork::new(dir.join("artwork-cache"), downloads)?;
        artwork.set_gate(cache.gate());
        cache
            .control(artwork.generation())
            .policy(settings.wifi_only, settings.downloads_paused);
        let core = Self {
            refresh: refresh::Scheduler::default(),
            planning_guard: None,
            offline_index: std::cell::RefCell::new(None),
            filter_schemas: std::cell::RefCell::new(std::collections::BTreeMap::new()),
            detail_kinds: std::cell::RefCell::new(std::collections::BTreeMap::new()),
            catalogue_epoch: 0,
            network_wifi: false,
            network_cellular: false,
            network_online: false,
            network_known: false,
            health: health::Health::default(),
            radio_warning: std::cell::Cell::new(false),
            cached_failures: std::cell::Cell::new(0),
            refill_epoch: uuid::Uuid::new_v4(),
            playback_generation: settings.playback.generation,
            library: offline::Library::new(dir.join("library-cache"), downloads)?,
            artwork,
            offline_mode: false,
            dir,
            settings,
            backend: plex,
            account_url,
            pin: None,
            quick_connect: None,
            servers: vec![],
            queue,
            radio,
            cache,
            cached_playback: false,
            transcoding: false,
            manual_cache,
            cache_suspended: !downloads,
            cache_schedule_seen: std::cell::RefCell::new(None),
            artist_indexes: std::collections::BTreeMap::new(),
            writable: downloads,
            resume_position,
        };
        if downloads && !core.manual_cache.is_empty() && !core.settings.server_url.is_empty() {
            core.schedule_cache();
        } else if downloads
            && core.settings.cache.enabled
            && core.downloads_allowed()
            && !core.settings.server_url.is_empty()
        {
            let pending = core.cache.pending_items(&core.cache_namespace());
            if !pending.is_empty() {
                if let Ok(conn) = core.conn() {
                    let _ = core.cache.schedule_for(
                        &core.backend.namespace(&conn),
                        core.backend.stream_policy(&conn),
                        pending,
                        core.settings.quality.download_kbps,
                    );
                }
            }
        }
        Ok(core)
    }
    fn base(&self) -> Result<url::Url> {
        Ok(self.conn()?.base)
    }
    /// Server address, credential and user for one request.
    fn conn(&self) -> Result<Conn> {
        connection(&self.backend, &self.settings)
    }
    /// How the audio engine, downloaders and artwork loader reach the server
    /// (default, credential-free Plex rules while signed out).
    pub(crate) fn stream_policy(&self) -> server::StreamPolicy {
        self.conn()
            .map(|conn| self.backend.stream_policy(&conn))
            .unwrap_or_default()
    }
    /// Whether the server already counted the play of `occurrence` when its
    /// start was reported (only for servers that count at the start).
    pub(crate) fn start_reported(&self, occurrence: &str) -> bool {
        self.backend.reports_start() && self.settings.playback.start_reported == occurrence
    }
    /// An identifier fit for a request path, by the backend's rule.
    fn id<'a>(&self, value: &'a str) -> Result<&'a str> {
        self.backend.valid_id(value)
    }
    /// Refuse a command the server cannot serve.
    fn require(&self, needed: Option<server::Capability>) -> Result<()> {
        match needed {
            Some(needed) if !self.backend.capabilities().allows(needed) => {
                Err(server::unsupported())
            }
            _ => Ok(()),
        }
    }
    fn page(&self, query: &Query, start: usize) -> Result<Value> {
        self.page_sized(query, start, 100)
    }
    fn page_sized(&self, query: &Query, start: usize, size: usize) -> Result<Value> {
        let (c, offline) = self.container_cached(query, Some(Page { start, size }))?;
        let items: Vec<Value> = c
            .items
            .iter()
            .filter(|i| {
                matches!(
                    i.kind,
                    crate::model::ItemKind::Artist
                        | crate::model::ItemKind::Album
                        | crate::model::ItemKind::Track
                        | crate::model::ItemKind::Playlist
                )
            })
            .map(|i| self.display_item(i))
            .collect();
        Ok(pagination::page(
            items,
            start,
            c.size,
            size,
            c.total_size,
            offline,
        ))
    }
    /// Saved copy of a read, keyed by the server's request identity.
    fn load_saved(&self, query: &Query, page: Option<Page>) -> Option<model::Container> {
        let (path, params) = self.backend.offline_key(query, page).ok()?;
        self.library.load(
            &self.cache_namespace(),
            &path,
            &filters::param_refs(&params),
        )
    }
    fn save_snapshot(&self, query: &Query, page: Option<Page>, c: &model::Container) {
        if let Ok((path, params)) = self.backend.offline_key(query, page) {
            self.library.save(
                &self.cache_namespace(),
                &path,
                &filters::param_refs(&params),
                c,
            );
        }
    }
    fn container_cached(
        &self,
        query: &Query,
        page: Option<Page>,
    ) -> Result<(model::Container, bool)> {
        self.check_planning()?;
        if self.offline_mode {
            return self
                .load_saved(query, page)
                .map(|c| (c, true))
                .ok_or(Error::Input(
                    "This library page has not been saved offline yet",
                ));
        }
        let fetched = self.backend.fetch(&self.conn()?, query, page);
        self.cached_response(query, page, fetched)
    }
    /// Save a fetched library page for offline use, or fall back to the saved
    /// copy when the server is unreachable. Returns whether it is the copy.
    fn cached_response(
        &self,
        query: &Query,
        page: Option<Page>,
        fetched: Result<model::Container>,
    ) -> Result<(model::Container, bool)> {
        if fetched.is_err() {
            self.cached_failures
                .set(self.cached_failures.get().wrapping_add(1));
        }
        match fetched {
            Ok(c) => {
                self.save_snapshot(query, page, &c);
                Ok((c, false))
            }
            Err(error @ (Error::Network | Error::Http(500..=599))) => {
                self.load_saved(query, page).map(|c| (c, true)).ok_or(error)
            }
            Err(error) => Err(error),
        }
    }
    fn alphabet(&mut self, section: &str) -> Result<Value> {
        self.id(section)?;
        if let Some(index) = self.artist_indexes.get(section) {
            return Ok(index.clone());
        }
        let (c, offline) = self.container_cached(
            &Query::FirstCharacter {
                section: section.into(),
            },
            None,
        )?;
        let mut offset = 0usize;
        let mut groups = Vec::new();
        for group in c.sections {
            let letter = if group.title.is_empty() {
                group.key
            } else {
                group.title
            };
            if !letter.is_empty() && group.size > 0 {
                groups.push(json!({"letter":letter,"offset":offset,"count":group.size}));
            }
            offset = offset
                .checked_add(group.size)
                .ok_or(Error::ProtocolAt("artist alphabet index"))?;
        }
        let result = json!({"alphabet":groups,"alphabetSection":section});
        // A saved fallback index is not memoized; refetch once back online.
        if !offline {
            self.artist_indexes.insert(section.into(), result.clone());
        }
        Ok(result)
    }
    /// How the UI shows the image at server-relative `path`: the cached file,
    /// else (online) a credential-free `image://plexfreq` id, else nothing.
    fn image_ref(&self, path: &str) -> Option<String> {
        if path.is_empty() {
            return None;
        }
        let namespace = self.cache_namespace();
        if let Some(local) = self.artwork.local(&namespace, path) {
            return Some(local);
        }
        if self.offline_mode || self.settings.server_token.is_empty() {
            return None;
        }
        self.stream_policy().artwork_url(path).ok()?;
        Some(artwork::id(&namespace, path))
    }
    /// The artwork source for the image provider; None while it cannot fetch.
    pub(crate) fn artwork_snapshot(&self) -> Option<artwork::Snapshot> {
        if self.offline_mode
            || self.settings.server_url.is_empty()
            || self.settings.server_token.is_empty()
        {
            return None;
        }
        Some(artwork::Snapshot {
            policy: self
                .conn()
                .ok()
                .map(|conn| self.backend.stream_policy(&conn))?,
            namespace: self.cache_namespace(),
            store: self.artwork.store().clone(),
        })
    }
    #[cfg(test)]
    pub(crate) fn display_item_for_tests(&self, item: &Item) -> Value {
        self.display_item(item)
    }
    fn display_item(&self, item: &Item) -> Value {
        let mut value = serde_json::to_value(item).unwrap_or_else(|_| serde_json::json!({}));
        let thumb = if item.thumb.is_empty() {
            &item.parent_thumb
        } else {
            &item.thumb
        };
        let artwork = self.image_ref(thumb).unwrap_or_default();
        value["artwork"] = json!(artwork);
        if item.kind == crate::model::ItemKind::Track {
            if let Some(object) = value.as_object_mut() {
                object.remove("playlistItemID");
            }
            value["playlistItemId"] = json!(item.playlist_item_id);
        }
        if item.kind == crate::model::ItemKind::Playlist {
            value["totalDuration"] = json!(if item.duration > 0 || item.leaf_count == Some(0) {
                Some(item.duration)
            } else {
                item.duration_in_seconds
                    .map(|seconds| seconds.saturating_mul(1000))
            });
        }
        let image_url = |path: &str| self.image_ref(path);
        let mut photos = Vec::new();
        for path in std::iter::once(thumb)
            .chain(std::iter::once(&item.art))
            .chain(item.images.iter().map(|image| &image.url))
        {
            if let Some(url) = image_url(path) {
                if !photos.contains(&url) && photos.len() < 8 {
                    photos.push(url);
                }
            }
        }
        value["photos"] = json!(photos);
        value["background"] = json!(image_url(&item.art).unwrap_or_default());
        if item.kind == crate::model::ItemKind::Album {
            value["albumType"] = json!(features::album_type(item));
        }
        if item.kind == crate::model::ItemKind::Track {
            value["albumArtwork"] = json!(image_url(&item.parent_thumb).unwrap_or(artwork));
        }
        value["cached"] = json!(self
            .cache
            .lookup(&self.cache_namespace(), item, false)
            .is_some());
        value["pinned"] = json!(self
            .manual_cache
            .iter()
            .any(|i| i.rating_key == item.rating_key));
        value
    }

    /// Remember the kind of detail item `id` for its later pages (bounded).
    fn remember_detail_kind(&self, id: &str, kind: &ItemKind) {
        let mut kinds = self.detail_kinds.borrow_mut();
        if kinds.len() >= 512 && !kinds.contains_key(id) {
            kinds.clear();
        }
        kinds.insert(id.to_owned(), kind.clone());
    }
    /// The artist or album metadata record of a detail page (with stations).
    fn detail_item(&self, id: &str) -> Result<Item> {
        let (c, _) = self.container_cached(
            &Query::Item {
                id: id.into(),
                stations: true,
            },
            None,
        )?;
        c.items
            .into_iter()
            .find(|i| {
                i.rating_key == id
                    && matches!(
                        i.kind,
                        crate::model::ItemKind::Artist | crate::model::ItemKind::Album
                    )
            })
            .ok_or(Error::Input("Artist or album details are unavailable"))
    }
    /// An artist or album page: its metadata on the first page plus a page of
    /// its children. `kind` is what the caller knows about the item; pages
    /// after the first fall back to the kind remembered for `key`, then (for
    /// backends that list children by kind) to the (cached) metadata record,
    /// so those always get the real one. Other backends list a later page of
    /// an unknown kind directly.
    fn detail(&self, key: &str, kind: Option<ItemKind>, start: usize) -> Result<Value> {
        let id = self.id(key)?;
        let kind = kind.filter(|k| matches!(k, ItemKind::Artist | ItemKind::Album));
        if self.offline_mode {
            let known = self.offline_catalogue();
            let saved = self.load_saved(
                &Query::Item {
                    id: id.into(),
                    stations: true,
                },
                None,
            );
            let item = saved
                .as_ref()
                .and_then(|c| {
                    c.items.iter().find(|i| {
                        i.rating_key == id
                            && matches!(
                                i.kind,
                                crate::model::ItemKind::Artist | crate::model::ItemKind::Album
                            )
                    })
                })
                .cloned()
                .or_else(|| {
                    known
                        .iter()
                        .find(|i| {
                            i.rating_key == id
                                && matches!(
                                    i.kind,
                                    crate::model::ItemKind::Artist | crate::model::ItemKind::Album
                                )
                        })
                        .cloned()
                })
                .or_else(|| {
                    known
                        .iter()
                        .find(|i| {
                            i.kind == crate::model::ItemKind::Track && i.parent_rating_key == id
                        })
                        .map(|i| Item {
                            rating_key: id.into(),
                            kind: crate::model::ItemKind::Album,
                            title: i.parent_title.clone(),
                            thumb: i.parent_thumb.clone(),
                            parent_rating_key: i.grandparent_rating_key.clone(),
                            parent_title: i.grandparent_title.clone(),
                            ..Default::default()
                        })
                })
                .ok_or(Error::Input(
                    "This artist or album has not been saved offline",
                ))?;
            self.remember_detail_kind(id, &item.kind);
            let mut children: Vec<_> = known
                .iter()
                .filter(|i| {
                    i.parent_rating_key == id
                        && (item.kind == crate::model::ItemKind::Artist
                            && i.kind == crate::model::ItemKind::Album
                            || item.kind == crate::model::ItemKind::Album
                                && i.kind == crate::model::ItemKind::Track)
                })
                .cloned()
                .collect();
            // Saved children pages preserve ordering and contain full album lists.
            if let Some(c) = self.load_saved(
                &Query::Children {
                    id: id.into(),
                    kind: item.kind.clone(),
                },
                Some(Page { start, size: 100 }),
            ) {
                let mut value = pagination::page(
                    c.items.iter().map(|i| self.display_item(i)).collect(),
                    start,
                    c.size,
                    100,
                    c.total_size,
                    true,
                );
                if start == 0 {
                    value["detail"] = self.display_item(&item);
                }
                return Ok(value);
            }
            let mut seen = std::collections::BTreeSet::new();
            children.retain(|i| seen.insert(i.rating_key.clone()));
            children.sort_by_cached_key(|i| (i.parent_index, i.index, i.title.clone()));
            let total = children.len();
            let page_items: Vec<_> = children
                .into_iter()
                .skip(start)
                .take(100)
                .map(|i| self.display_item(&i))
                .collect();
            let received = page_items.len();
            let mut value = pagination::page(page_items, start, received, 100, Some(total), true);
            if start == 0 {
                value["detail"] = self.display_item(&item);
            }
            return Ok(value);
        }
        let mut metadata = None;
        let remembered = || self.detail_kinds.borrow().get(id).cloned();
        let kind = match kind.or_else(remembered) {
            Some(kind) if start > 0 => kind,
            None if start > 0 && !self.backend.children_need_kind() => {
                return self.page(
                    &Query::Children {
                        id: id.into(),
                        kind: ItemKind::default(),
                    },
                    start,
                );
            }
            _ => {
                let item = self.detail_item(id)?;
                if start == 0 {
                    metadata = Some(self.display_item(&item));
                }
                item.kind
            }
        };
        self.remember_detail_kind(id, &kind);
        let mut result = self.page(
            &Query::Children {
                id: id.into(),
                kind,
            },
            start,
        )?;
        if let Some(item) = metadata {
            result["detail"] = item;
        }
        Ok(result)
    }
    fn similar_artists(&self, key: &str) -> Result<Value> {
        let id = self.id(key)?;
        if self.offline_mode {
            return Ok(json!({"similarKey":id,"similarArtists":[],"similarAvailable":false}));
        }
        let c = match self.backend.fetch(
            &self.conn()?,
            &Query::Similar {
                id: id.into(),
                count: 12,
            },
            None,
        ) {
            Ok(c) => c,
            Err(Error::Http(400 | 404)) => {
                return Ok(json!({"similarKey":id,"similarArtists":[],"similarAvailable":false}))
            }
            Err(error) => return Err(error),
        };
        let mut seen = std::collections::BTreeSet::new();
        let artists: Vec<_> = c
            .items
            .iter()
            .filter(|i| {
                i.kind == crate::model::ItemKind::Artist
                    && i.rating_key != id
                    && self.id(&i.rating_key).is_ok()
                    && seen.insert(i.rating_key.clone())
            })
            .take(12)
            .map(|i| self.display_item(i))
            .collect();
        Ok(json!({"similarKey":id,"similarArtists":artists,"similarAvailable":true}))
    }
    fn playback(&mut self) -> Result<Value> {
        self.playback_with_cache(true)
    }
    fn playback_with_cache(&mut self, schedule: bool) -> Result<Value> {
        let mut plan = self.playback_plan(schedule, false)?;
        plan["normalizationMode"] = json!(self.normalization_mode());
        plan["albumNormalization"] = json!(self.album_normalization());
        Ok(plan)
    }
    fn normalization_mode(&self) -> &'static str {
        use audio::dsp::NormalizationMode;
        match self.settings.audio.normalization_mode {
            NormalizationMode::Album => "album",
            NormalizationMode::Track => "track",
            NormalizationMode::Auto => {
                if self.album_normalization() {
                    "album"
                } else {
                    "track"
                }
            }
        }
    }
    fn playback_plan(&mut self, schedule: bool, force_transcode: bool) -> Result<Value> {
        let Some(mut track) = self.queue.track().cloned() else {
            return Ok(
                json!({"queue": self.queue, "radio": self.radio_summary(), "track": null, "stream": ""}),
            );
        };
        if let Some((path, mut cached)) = (!force_transcode)
            .then(|| self.cache.lookup(&self.cache_namespace(), &track, true))
            .flatten()
        {
            cached.play_queue_item_id = track.play_queue_item_id;
            self.queue.replace_track(cached.clone());
            self.cached_playback = true;
            self.transcoding = false;
            if schedule {
                self.schedule_cache();
            }
            let stream = url::Url::from_file_path(path)
                .map_err(|_| Error::Input("Invalid cached audio path"))?
                .to_string();
            return Ok(
                json!({"queue":self.queue,"radio":self.radio_summary(),"track":self.display_item(&cached),"stream":stream,"playbackSource":PlaybackSource::Cache,"cache":self.cache_status()}),
            );
        }
        if self.health.is_gone(&track.rating_key) {
            return Err(Error::Input("This track is no longer on the server"));
        }
        if self.offline_mode {
            return Err(Error::Input("This track has not been downloaded"));
        }
        if track.media.first().and_then(|m| m.parts.first()).is_none() {
            let key = self.id(&track.rating_key)?;
            let c = self
                .backend
                .fetch(
                    &self.conn()?,
                    &Query::Item {
                        id: key.into(),
                        stations: false,
                    },
                    None,
                )
                .inspect_err(|error| {
                    // Have the runtime confirm whether the item is gone, so
                    // later navigation can step over it.
                    if matches!(error, Error::Http(404)) {
                        self.health.request_probe(key);
                    }
                })?;
            track = c
                .items
                .into_iter()
                .find(|i| i.kind == crate::model::ItemKind::Track)
                .ok_or(Error::Protocol)?;
        }
        let part = track
            .media
            .first()
            .and_then(|m| m.parts.first())
            .ok_or(Error::Input("This track has no playable media part"))?;
        let kbps = self.settings.quality.streaming(self.network_cellular);
        let transcode = force_transcode
            || kbps > 0
            || (self.settings.quality.codec_fallback && !audio::formats::item_supported(&track));
        let stream = if transcode {
            self.backend.transcode_stream(
                &self.conn()?,
                &track.rating_key,
                if kbps > 0 { kbps } else { 192 },
                &uuid::Uuid::new_v4().to_string(),
            )?
        } else {
            self.backend.stream_url(&self.conn()?, &part.key)?
        };
        if let Some(current) = self.queue.track() {
            track.play_queue_item_id = current.play_queue_item_id;
            self.queue.replace_track(track.clone());
        }
        self.cached_playback = false;
        self.transcoding = transcode;
        if schedule {
            self.schedule_cache();
        }
        Ok(
            json!({"queue": self.queue, "radio":self.radio_summary(), "track": self.display_item(&track), "stream": stream,"playbackSource":if transcode {PlaybackSource::Transcode}else{PlaybackSource::Stream},"streamKbps":if transcode {if kbps>0 {kbps}else{192}}else{0},"cache":self.cache_status()}),
        )
    }

    fn album_normalization(&self) -> bool {
        let album = self
            .queue
            .track()
            .map(|i| i.parent_rating_key.as_str())
            .unwrap_or("");
        !album.is_empty()
            && (self.radio.as_ref().is_some_and(|r| {
                r.kind == RadioKind::Album && !matches!(r.source, Source::Mix { .. })
            }) || self
                .queue
                .items()
                .iter()
                .all(|i| i.parent_rating_key == album))
    }

    fn cache_namespace(&self) -> String {
        settings_namespace(&self.backend, &self.settings)
    }
    fn cache_status(&self) -> Value {
        let mut value = self.cache.status(&self.cache_namespace());
        value["pinnedKeys"] = json!(self
            .manual_cache
            .iter()
            .map(|i| &i.rating_key)
            .collect::<Vec<_>>());
        value["pinnedGroups"] = json!(self.settings.download_groups.keys().collect::<Vec<_>>());
        // Tracks known to be missing from the server, for the track rows.
        value["goneKeys"] = json!(self.health.gone_keys());
        value["jobs"] = json!(self.download_rows_from_status(&value));
        value["autoRefresh"] = json!(self.settings.refresh_policy.enabled);
        value["refreshHours"] = json!(self.settings.refresh_policy.interval_hours);
        value["refreshingGroup"] = json!(self.refresh.pending);
        value["refreshError"] = json!(self.refresh.error);
        // Lyric/snapshot save health, so quota or I/O failures are visible.
        value["offlineStore"] = self.library.store_status();
        value
    }
    fn report_cache_schedule(&self, outcome: CacheSchedule) {
        if let Some(line) = outcome.changed(&mut self.cache_schedule_seen.borrow_mut()) {
            diagnostics::event(format_args!("{line}"));
        }
    }
    /// Why the cache is not scheduled now (a fixed name), if it is not.
    fn cache_block_reason(&self) -> Option<&'static str> {
        if !self.settings.cache.enabled {
            Some("disabled")
        } else if self.cache_suspended {
            Some("cache_suspended")
        } else {
            self.download_block_reason()
        }
    }
    fn schedule_cache(&self) {
        self.cache
            .protect(&self.cache_namespace(), &self.manual_cache);
        if let Some(reason) = self.cache_block_reason() {
            self.report_cache_schedule(CacheSchedule::Skipped(reason));
        } else {
            // Do not undermine a mobile bitrate cap with automatic original-file
            // prefetch. Explicit pins still follow the selected download policy.
            let ahead = self.settings.cache.ahead + 1;
            let mut skipped = false;
            let cellular_cap = self.settings.quality.streaming(self.network_cellular) > 0
                && self.settings.quality.download_kbps == 0;
            let mut items = if cellular_cap {
                Vec::new()
            } else {
                // Gone items would fail again on every reschedule; look past
                // them so the window keeps its size.
                let mut upcoming = self
                    .queue
                    .upcoming(ahead.saturating_add(self.health.gone_len()));
                let before = upcoming.len();
                upcoming.retain(|item| !self.health.is_gone(&item.rating_key));
                skipped = upcoming.len() != before;
                upcoming.truncate(ahead);
                upcoming
            };
            for item in &self.manual_cache {
                if !items.iter().any(|i| i.rating_key == item.rating_key) {
                    items.push(item.clone());
                }
            }
            self.report_cache_schedule(if items.is_empty() {
                CacheSchedule::Skipped(if cellular_cap {
                    "cellular_cap"
                } else {
                    "empty_window"
                })
            } else {
                CacheSchedule::Scheduled {
                    items: items.len(),
                    manual: self.manual_cache.len(),
                    quality_kbps: self.settings.quality.download_kbps,
                    first: items[0].rating_key.clone(),
                    cellular_cap,
                }
            });
            if items.is_empty() && skipped {
                // Nothing left to fetch: stop a batch still retrying gone items.
                self.cache.cancel();
            }
            let scheduled = match self.conn() {
                Ok(conn) => self.cache.schedule_for(
                    &self.backend.namespace(&conn),
                    self.backend.stream_policy(&conn),
                    items,
                    self.settings.quality.download_kbps,
                ),
                // Nothing to fetch needs no server.
                Err(_) if items.is_empty() => Ok(()),
                Err(error) => Err(error),
            };
            if scheduled.is_err() {
                diagnostics::event(format_args!("cache schedule rejected"));
            }
        }
    }

    fn radio_summary(&self) -> Value {
        self.radio.as_ref().map_or(Value::Null, Radio::summary)
    }
    pub(crate) fn radio_active(&self) -> bool {
        self.radio.is_some()
    }
    pub(crate) fn station_radio_active(&self) -> bool {
        matches!(
            &self.radio,
            Some(Radio {
                source: Source::Station { .. },
                ..
            })
        )
    }

    /// Whether the radio is fed by the server's instant mix.
    pub(crate) fn mix_radio_active(&self) -> bool {
        matches!(
            &self.radio,
            Some(Radio {
                source: Source::Mix { .. },
                ..
            })
        )
    }

    fn album_tracks(&self, album: &Item) -> Result<Vec<Item>> {
        let id = self.id(&album.rating_key)?;
        let items = self.paged_tracks(
            &Query::Children {
                id: id.into(),
                kind: ItemKind::Album,
            },
            0,
            daily::Paging::Album,
        )?;
        if items.is_empty() {
            return Err(Error::Input("This album contains no playable tracks"));
        }
        Ok(items)
    }

    fn nearest(&self, item: &Item) -> Result<Vec<Item>> {
        self.require(Some(server::Capability::Sonic))?;
        let id = self.id(&item.rating_key)?;
        let (c, _) = self.container_cached(
            &Query::Nearest {
                id: id.into(),
                limit: 50,
            },
            None,
        )?;
        Ok(c.items
            .into_iter()
            .filter(|i| i.kind == item.kind && i.rating_key != item.rating_key)
            .collect())
    }

    fn start_radio(&mut self, key: String, kind: RadioKind) -> Result<Value> {
        if self.offline_mode {
            return Err(Error::Input("Radio playback requires a connection"));
        }
        let id = self.id(&key)?;
        let base = self.base()?;
        let c = self.backend.fetch(
            &self.conn()?,
            &Query::Item {
                id: id.into(),
                stations: true,
            },
            None,
        )?;
        let seed = c
            .items
            .into_iter()
            .find(|i| i.rating_key == key && i.kind == kind.item_kind())
            .ok_or(Error::Input(
                "Radio seed does not match the selected music type",
            ))?;
        let title = seed.title.clone();
        let (items, start, source) = if let Some(query) =
            self.backend.mix_query(&seed.rating_key, kind, MIX_BATCH)
        {
            let source = Source::Mix {
                seed: seed.rating_key.clone(),
                kind,
                used: Vec::new(),
            };
            let batch = self.backend.fetch(&self.conn()?, &query, None)?;
            // A new radio replaces the queue: nothing in it counts as seen.
            let mut items = self.unseen_mix(batch, &mut std::collections::BTreeSet::new());
            if kind == RadioKind::Track {
                // A track radio starts with its seed.
                items.retain(|i| i.rating_key != seed.rating_key);
                items.insert(0, seed.clone());
            }
            if items.is_empty() {
                return Err(Error::Input("Radio returned no playable tracks"));
            }
            (items, 0, source)
        } else if let Some(station) = radio::station_key(&seed, &base)? {
            let queue = self.backend.create_radio(&self.conn()?, &station)?;
            let queue_id = queue
                .play_queue_id
                .ok_or(Error::ProtocolAt("radio queue identity"))?;
            if queue
                .items
                .iter()
                .any(|i| i.kind != crate::model::ItemKind::Track || i.play_queue_item_id.is_none())
            {
                return Err(Error::ProtocolAt("radio queue tracks"));
            }
            let start = queue
                .items
                .iter()
                .position(|i| i.play_queue_item_id == queue.play_queue_selected_item_id)
                .unwrap_or(0);
            (
                queue.items.into_iter().skip(start).take(11).collect(),
                0,
                Source::Station { queue_id },
            )
        } else {
            if kind == RadioKind::Artist {
                return Err(Error::Input(
                    "Plex does not expose an artist radio station for this artist",
                ));
            }
            // The analysis-version field is optional in Plex responses. Ask the
            // recommendation endpoint; missing metadata is not an entitlement verdict.
            let neighbors = self.nearest(&seed)?;
            if neighbors.is_empty() {
                return Err(Error::Input(match kind {
                    RadioKind::Album => "Plex returned no sonic recommendations for this album. Check the music library's Sonic Analysis setting and completion, or use Artist Radio.",
                    _ => "Plex returned no sonic recommendations for this track. Check the music library's Sonic Analysis setting and completion, or use Artist Radio.",
                }));
            }
            let items = if kind == RadioKind::Album {
                self.album_tracks(&seed)?
            } else {
                vec![seed.clone()]
            };
            let recent = vec![seed.rating_key.clone()];
            (
                items,
                0,
                Source::Sonic {
                    seed: Box::new(seed),
                    recent,
                },
            )
        };
        let mut queue = Queue::default();
        queue.replace(items, start)?;
        let previous_queue = std::mem::replace(&mut self.queue, queue);
        let previous_radio = self.radio.replace(Radio::new(kind, title, source));
        let result = self.playback();
        if result.is_err() {
            self.queue = previous_queue;
            self.radio = previous_radio;
        }
        result
    }

    /// The playable tracks of an instant-mix reply that are not queued yet,
    /// at most [`MIX_BATCH`].
    fn unseen_mix(
        &self,
        c: model::Container,
        seen: &mut std::collections::BTreeSet<String>,
    ) -> Vec<Item> {
        c.items
            .into_iter()
            .filter(|i| {
                i.kind == crate::model::ItemKind::Track
                    && self.backend.is_id(&i.rating_key)
                    && seen.insert(i.rating_key.clone())
            })
            .take(MIX_BATCH as usize)
            .collect()
    }

    fn extend_radio(&mut self) -> Result<()> {
        let request = self.refill_request().inspect_err(|_| self.warn_radio())?;
        let mut request = request;
        let fetched = request.fetch(&self.backend);
        self.refill_apply(&request, fetched).map(|_| ())
    }
    /// What replenishing the radio by one step asks the server.
    fn refill_request(&self) -> Result<RefillRequest> {
        let radio = self.radio.as_ref().ok_or(Error::Input("No active radio"))?;
        let mut mix_fallbacks = Vec::new();
        let mut mix_queued = std::collections::BTreeSet::new();
        let query = match &radio.source {
            Source::Station { queue_id } => {
                let center = self
                    .station_center()
                    .ok_or(Error::ProtocolAt("radio queue center"))?;
                Query::PlayQueueWindow {
                    id: *queue_id,
                    center,
                }
            }
            Source::Sonic { seed, .. } => Query::Nearest {
                id: self.id(&seed.rating_key)?.to_owned(),
                limit: 50,
            },
            Source::Mix { seed, kind, used } => {
                // Seeds for a mix that brings nothing new: the most recently
                // queued tracks not asked yet.
                let items = self.queue.items();
                mix_queued = items.iter().map(|i| i.rating_key.clone()).collect();
                for item in items.iter().rev() {
                    if mix_fallbacks.len() + 1 >= MIX_ATTEMPTS {
                        break;
                    }
                    let key = &item.rating_key;
                    if item.kind != ItemKind::Track
                        || key == seed
                        || used.contains(key)
                        || self.health.is_gone(key)
                        || mix_fallbacks.iter().any(|(k, _)| k == key)
                    {
                        continue;
                    }
                    if let Some(query) = self
                        .id(key)
                        .ok()
                        .and_then(|id| self.backend.mix_query(id, RadioKind::Track, MIX_BATCH))
                    {
                        mix_fallbacks.push((key.clone(), query));
                    }
                }
                self.backend
                    .mix_query(self.id(seed)?, *kind, MIX_BATCH)
                    .ok_or_else(server::unsupported)?
            }
        };
        Ok(RefillRequest {
            conn: self.conn()?,
            query,
            mix_fallbacks,
            mix_queued,
            mix_attempted: Vec::new(),
            identity: self
                .refill_identity()
                .ok_or(Error::Input("No active radio"))?,
        })
    }
    /// The radio instance, its source version and the queue revision: a
    /// refill fetched for any other state is stale.
    fn refill_identity(&self) -> Option<RefillIdentity> {
        let radio = self.radio.as_ref()?;
        Some(RefillIdentity {
            epoch: self.refill_epoch,
            server: self.settings.server_url.clone(),
            instance: radio.refill.instance,
            version: radio.refill.version,
            queue: self.queue.revision(),
        })
    }
    /// Append what a refill request returned and advance the radio source.
    /// Returns the appended items.
    fn refill_apply(
        &mut self,
        request: &RefillRequest,
        fetched: Result<model::Container>,
    ) -> Result<Vec<Item>> {
        let failed = fetched.is_err();
        let cached_failures = self.cached_failures.get();
        let result = self.refill_apply_inner(request, fetched);
        // Includes nested album-children cached fallback, but never ordinary
        // browsing: only failures observed during this refill are relevant.
        if failed
            || self.cached_failures.get() != cached_failures
            || result.as_ref().map_or(true, |items| items.is_empty())
        {
            self.warn_radio();
        }
        result
    }
    fn refill_apply_inner(
        &mut self,
        request: &RefillRequest,
        fetched: Result<model::Container>,
    ) -> Result<Vec<Item>> {
        let mut radio = self.radio.clone().ok_or(Error::Input("No active radio"))?;
        let items: Vec<Item> = match &mut radio.source {
            Source::Station { queue_id } => {
                let c = fetched?;
                if c.play_queue_id.is_some_and(|id| id != *queue_id) {
                    return Err(Error::ProtocolAt("radio queue continuation"));
                }
                if c.items.iter().any(|i| {
                    i.kind != crate::model::ItemKind::Track || i.play_queue_item_id.is_none()
                }) {
                    return Err(Error::ProtocolAt("radio queue continuation"));
                }
                c.items
                    .into_iter()
                    .filter(|i| {
                        !self
                            .queue
                            .items()
                            .iter()
                            .any(|known| known.play_queue_item_id == i.play_queue_item_id)
                    })
                    .take(1)
                    .collect()
            }
            Source::Mix { used, .. } => {
                let reply = self.cached_response(&request.query, None, fetched);
                // The seeds asked count as used whatever the reply held, so
                // the next refill moves on to others.
                used.extend(request.mix_attempted.iter().cloned());
                let excess = used.len().saturating_sub(MIX_USED_SEEDS);
                used.drain(..excess);
                let (c, _) = match reply {
                    Ok(reply) => reply,
                    Err(error) => {
                        self.radio = Some(radio);
                        return Err(error);
                    }
                };
                let mut queued = self
                    .queue
                    .items()
                    .iter()
                    .map(|i| i.rating_key.clone())
                    .collect();
                let items = self.unseen_mix(c, &mut queued);
                if items.is_empty() {
                    self.radio = Some(radio);
                    return Err(Error::Input("Radio returned no playable tracks"));
                }
                items
            }
            Source::Sonic { seed, recent } => {
                let (c, _) = self.cached_response(&request.query, None, fetched)?;
                let neighbors: Vec<Item> = c
                    .items
                    .into_iter()
                    .filter(|i| i.kind == seed.kind && i.rating_key != seed.rating_key)
                    .collect();
                let next = neighbors
                    .iter()
                    .find(|i| !recent.contains(&i.rating_key))
                    .or_else(|| neighbors.first())
                    .ok_or(Error::Input(
                        "Radio has no more sonic recommendations; try another seed",
                    ))?
                    .clone();
                let items = if radio.kind == RadioKind::Album {
                    self.album_tracks(&next)?
                } else {
                    vec![next.clone()]
                };
                recent.push(next.rating_key.clone());
                if recent.len() > 200 {
                    recent.remove(0);
                }
                **seed = next;
                items
            }
        };
        self.queue.append(items.clone())?;
        radio.refill.paid();
        self.radio = Some(radio);
        Ok(items)
    }
    /// Whether this move must replenish the radio: a station keeps a window
    /// of upcoming items, track radio adds one per move, and every radio
    /// refills at its end.
    fn refill_wanted(&self) -> bool {
        ((self.station_radio_active() || self.mix_radio_active())
            && self.queue.upcoming(12).len() <= 11)
            || self.radio.as_ref().is_some_and(|radio| {
                radio.kind == RadioKind::Track && !matches!(radio.source, Source::Mix { .. })
            })
            || self.queue.at_end()
    }
    /// A move can leave the refill to the background when it does not need
    /// it: the queue continues with an item that plays from the cache, so
    /// neither the move nor its playback waits on the server.
    fn refill_can_wait(&self) -> bool {
        !self.queue.at_end()
            && self.queue.upcoming(2).get(1).is_some_and(|next| {
                self.cache
                    .lookup(&self.cache_namespace(), next, false)
                    .is_some()
            })
    }
    /// The refill owed by moves that did not wait, for the runtime to fetch
    /// off the worker. Owed refills that no longer apply are dropped.
    pub(crate) fn refill_job(&mut self) -> Option<RefillJob> {
        let pending = self.radio.as_ref()?.refill.pending;
        if pending == 0 || self.offline_mode || (self.network_known && !self.network_online) {
            return None;
        }
        let album = self.radio.as_ref().is_some_and(|radio| {
            radio.kind == RadioKind::Album && !matches!(radio.source, Source::Mix { .. })
        });
        let request = if album || !self.refill_wanted() {
            None
        } else {
            self.refill_request()
                .inspect_err(|_| self.warn_radio())
                .ok()
        };
        let Some(request) = request else {
            if let Some(radio) = &mut self.radio {
                radio.refill.pending = 0;
            }
            return None;
        };
        // A clone has its own view scope: navigation on Core never cancels it.
        let backend = self.backend.clone();
        Some(RefillJob { request, backend })
    }
    /// Give up an owed refill that could not be fetched, also in a prepared
    /// successor of the same radio, whose commit would otherwise restore it.
    pub(crate) fn refill_worker_failed(
        &mut self,
        identity: &RefillIdentity,
        prepared: Option<&mut Prepared>,
        successor_shown: bool,
    ) {
        if !successor_shown && self.refill_identity().as_ref() == Some(identity) {
            self.warn_radio();
            self.refill_abandon(prepared);
        }
    }
    pub(crate) fn refill_abandon(&mut self, prepared: Option<&mut Prepared>) {
        let instance = self.radio.as_ref().map(|radio| radio.refill.instance);
        let prepared = prepared.and_then(|prepared| prepared.radio.as_mut());
        for radio in [self.radio.as_mut(), prepared].into_iter().flatten() {
            if Some(radio.refill.instance) == instance {
                radio.refill.pending = 0;
            }
        }
    }
    /// [`Core::refill_apply_landed`] followed by [`Core::refill_persist`].
    #[cfg(test)]
    pub(crate) fn refill_landed(
        &mut self,
        fetched: RefillFetched,
        prepared: Option<&mut Prepared>,
    ) -> Option<Value> {
        let landed = self.refill_apply_landed(fetched, prepared);
        if landed.is_some() {
            self.refill_persist();
        }
        landed
    }
    /// Persist and schedule caching after [`Core::refill_apply_landed`]
    /// appended. Kept apart so the runtime can run it outside its
    /// presentation lock.
    pub(crate) fn refill_persist(&mut self) {
        self.schedule_cache();
        self.save_session();
    }
    /// Apply a refill fetched off the worker to the radio and, when it is
    /// the same radio state, to the prepared successor, so its commit keeps
    /// the appended items. Returns the update to publish; the caller then
    /// runs [`Core::refill_persist`].
    pub(crate) fn refill_apply_landed(
        &mut self,
        fetched: RefillFetched,
        prepared: Option<&mut Prepared>,
    ) -> Option<Value> {
        let RefillFetched { request, result } = fetched;
        if self.refill_identity().as_ref() != Some(&request.identity) {
            crate::diagnostics::event(format_args!("radio refill result is stale; fetching again"));
            return None;
        }
        let version = request.identity.version;
        let same = |radio: &Option<Radio>| {
            radio.as_ref().is_some_and(|radio| {
                radio.refill.instance == request.identity.instance
                    && radio.refill.version == version
            })
        };
        let prepared = prepared.filter(|prepared| same(&prepared.radio));
        match self.refill_apply(&request, result) {
            Err(error) => {
                crate::diagnostics::event(format_args!("radio refill failed: {error}"));
                // A failed mix refill still remembers the seeds it asked.
                let source = self.radio.as_ref().map(|radio| radio.source.clone());
                for radio in [Some(&mut self.radio), prepared.map(|p| &mut p.radio)]
                    .into_iter()
                    .flatten()
                    .flatten()
                {
                    radio.refill.pending = 0;
                    if let (Source::Mix { used, .. }, Some(Source::Mix { used: asked, .. })) =
                        (&mut radio.source, &source)
                    {
                        used.clone_from(asked);
                    }
                }
                None
            }
            Ok(items) => {
                if let (Some(prepared), Some(radio)) = (prepared, &self.radio) {
                    // Should it fail, the commit restores the prepared state
                    // and its radio still owes the refill.
                    match prepared.queue.append(items) {
                        Ok(()) => {
                            if let Some(next) = &mut prepared.radio {
                                next.source = radio.source.clone();
                                next.refill.paid();
                            }
                            prepared.plan["queue"] = self.queue_json(&prepared.queue);
                        }
                        Err(error) => crate::diagnostics::event(format_args!(
                            "radio refill not added to the prepared successor: {error}"
                        )),
                    }
                }
                Some(json!({"queue":self.queue_json(&self.queue),"radio":self.radio_summary()}))
            }
        }
    }

    /// The station item to continue after: the current one as long as it is
    /// not gone (PMS answers nothing after a center it no longer holds),
    /// else the last held item, so a stale current item cannot hide a live
    /// station.
    fn station_center(&self) -> Option<u64> {
        if let Some(id) = self
            .queue
            .track()
            .filter(|track| !self.health.is_gone(&track.rating_key))
            .and_then(|track| track.play_queue_item_id)
        {
            return Some(id);
        }
        let items = self.queue.items();
        items
            .iter()
            .rev()
            .find(|item| {
                item.play_queue_item_id.is_some() && !self.health.is_gone(&item.rating_key)
            })
            .or_else(|| {
                items
                    .iter()
                    .rev()
                    .find(|item| item.play_queue_item_id.is_some())
            })
            .and_then(|item| item.play_queue_item_id)
    }
    fn next(&mut self, automatic: bool) -> Result<Value> {
        let previous_queue = self.queue.clone();
        let previous_radio = self.radio.clone();
        let result = (|| {
            if automatic && self.reports_stop_on_transition() {
                let duration = self.queue.track().map(|track| track.duration).unwrap_or(0);
                // Reporting must not prevent local continuation (connection
                // loss, a station emptied on the server); only a rejected
                // session is reported.
                if let Err(error) = self.timeline("stopped".into(), duration, duration, true) {
                    if matches!(error, Error::Unauthorized) {
                        return Err(error);
                    }
                    crate::diagnostics::event(format_args!(
                        "station completion report failed: {error}"
                    ));
                }
            }
            self.advance_next(automatic, true)
        })();
        if result.is_err() {
            // The seeds a failed mix refill asked stay remembered, so a retry
            // moves on to others.
            let asked = match self.radio.as_ref().map(|radio| &radio.source) {
                Some(Source::Mix { used, .. }) => Some(used.clone()),
                _ => None,
            };
            self.queue = previous_queue;
            self.radio = previous_radio;
            if let (
                Some(asked),
                Some(Radio {
                    source: Source::Mix { used, .. },
                    ..
                }),
            ) = (asked, self.radio.as_mut())
            {
                *used = asked;
            }
        }
        result
    }
    fn advance_next(&mut self, automatic: bool, schedule: bool) -> Result<Value> {
        if automatic
            && self.queue.at_end()
            && self.radio.is_none()
            && self.settings.autoplay
            && self.queue.repeat() == Repeat::Off
            && !self.offline_mode
        {
            if let Some(seed) = self.queue.track().cloned() {
                if let Ok(neighbors) = self.nearest(&seed) {
                    let known: std::collections::BTreeSet<_> = self
                        .queue
                        .items()
                        .iter()
                        .rev()
                        .take(200)
                        .map(|i| i.rating_key.clone())
                        .collect();
                    let next: Vec<_> = neighbors
                        .into_iter()
                        .filter(|i| !known.contains(&i.rating_key))
                        .take(20)
                        .collect();
                    if !next.is_empty() {
                        self.queue.append(next)?;
                    }
                }
                if self.queue.at_end() && !seed.grandparent_rating_key.is_empty() {
                    return self.start_radio(seed.grandparent_rating_key, RadioKind::Artist);
                }
            }
        }
        // A radio that could not refill, or had to step over gone items, keeps
        // its whole window: nothing was added, and its cached history is what
        // continuation cycles through.
        let mut refill_failed = false;
        if self.radio.is_some() && !(automatic && self.queue.repeat() == Repeat::One) {
            // An ended station resumes from its last audible item.
            if self.queue.track().is_none() && !self.queue.items().is_empty() {
                let cursor = self.queue.cursor().min(self.queue.items().len() - 1);
                if let Ok(index) = self.queue.physical_index(cursor) {
                    self.queue.select(index)?;
                }
            }
            let disconnected = self.offline_mode || (self.network_known && !self.network_online);
            if disconnected {
                self.warn_radio();
                self.advance_cached_radio()?;
                return self.playback_with_cache(schedule);
            }
            // Station/track radio replenishes as each track is consumed. Album
            // radio retains ordered album groups and requests the next at its end.
            // A move onto a cached successor does not wait for the server: the
            // refill is owed and the runtime fetches it in the background.
            if self.refill_wanted() && self.refill_can_wait() {
                if let Some(radio) = &mut self.radio {
                    radio.refill.owe();
                }
            } else if self.refill_wanted() {
                if let Err(error) = self.extend_radio() {
                    refill_failed = true;
                    let network = matches!(error, Error::Network | Error::Http(500..=599));
                    if network && self.advance_cached_radio().is_ok() {
                        return self.playback_with_cache(schedule);
                    }
                    // A speculative refill must not discard an existing successor.
                    if self.queue.at_end() {
                        if matches!(error, Error::Unauthorized) {
                            return Err(error);
                        }
                        // Nothing left to fetch: continue as offline does.
                        if !network && self.advance_cached_radio().is_ok() {
                            return self.playback_with_cache(schedule);
                        }
                        if self
                            .queue
                            .track()
                            .is_some_and(|track| self.health.is_gone(&track.rating_key))
                        {
                            self.finish_gone(self.queue.cursor());
                            return self.playback_with_cache(schedule);
                        }
                        return Err(error);
                    }
                }
            }
        }
        let audible = self.queue.cursor();
        self.queue.advance(automatic);
        let hopping = self
            .queue
            .track()
            .is_some_and(|track| self.skippable(track));
        if self.settle_forward(audible) && self.radio.is_some() {
            // A cached-loop target or the gone end keeps the radio window,
            // as offline cycling does.
            return self.playback_with_cache(schedule);
        }
        if self.radio.is_some() && !refill_failed && !hopping {
            // Eleven rows: current, upcoming and whatever recent history fits.
            // Whole-album radio can exceed that window to preserve album order.
            let future = self
                .queue
                .upcoming(queue::MAX_QUEUE)
                .len()
                .saturating_sub(1);
            self.queue.trim_history(10usize.saturating_sub(future));
        }
        self.playback_with_cache(schedule)
    }

    fn advance_cached_radio(&mut self) -> Result<()> {
        let ordered = self.queue.presentation();
        let current = ordered
            .current()
            .ok_or(Error::Input("This track has not been downloaded"))?;
        let namespace = self.cache_namespace();
        for offset in 1..=ordered.items().len() {
            let index = (current + offset) % ordered.items().len();
            if self
                .cache
                .lookup(&namespace, &ordered.items()[index], false)
                .is_some()
            {
                self.queue.select(self.queue.physical_index(index)?)?;
                return Ok(());
            }
        }
        Err(Error::Input("This track has not been downloaded"))
    }

    fn timeline(
        &mut self,
        state: String,
        position: u64,
        duration: u64,
        continuing: bool,
    ) -> Result<Value> {
        // Local playback must not wait on an unreachable server. Offline history
        // synchronization will be a separate feature; no pretend events are sent.
        if self.cached_playback || self.offline_mode || (self.network_known && !self.network_online)
        {
            return Ok(json!({}));
        }
        if !matches!(
            state.as_str(),
            "playing" | "paused" | "stopped" | "buffering"
        ) {
            return Err(Error::Input("Unknown playback state"));
        }
        if let Some(track) = self.queue.track() {
            let id = self.id(&track.rating_key)?.to_owned();
            let queue = if let Some(Radio {
                source: Source::Station { queue_id },
                ..
            }) = &self.radio
            {
                Some((*queue_id, track.play_queue_item_id.ok_or(Error::Protocol)?))
            } else {
                None
            };
            let occurrence = self.settings.playback.occurrence.clone();
            // A server that counts a play at its start hears the first
            // `playing` report of each occurrence once, also across restarts
            // that restore the queue.
            let counts_start = self.backend.reports_start();
            let start = counts_start
                && state == "playing"
                && self.settings.playback.start_reported != occurrence;
            self.backend.act(
                &self.conn()?,
                &Action::Timeline {
                    id,
                    state,
                    position,
                    duration,
                    continuing,
                    queue,
                    reporting: server::Reporting {
                        play_session: occurrence.clone(),
                        occurrence: occurrence.clone(),
                        start,
                        transcode: self.transcoding,
                    },
                },
            )?;
            if self.writable {
                if start {
                    self.settings.playback.start_reported = occurrence.clone();
                }
                for play in &mut self.settings.history {
                    if play.occurrence == occurrence && (!counts_start || start) {
                        play.delivered = true;
                        play.start_reported |= counts_start;
                    }
                }
                store::save_journals(&self.dir, &self.settings)?;
                if start {
                    // The session file carries the occurrence that was told.
                    self.save_session();
                }
            }
        }
        Ok(json!({}))
    }
    /// A queued item that navigation steps over: confirmed gone and not
    /// playable from the local cache.
    fn skippable(&self, item: &Item) -> bool {
        self.health.is_gone(&item.rating_key)
            && self
                .cache
                .lookup(&self.cache_namespace(), item, false)
                .is_none()
    }
    /// After a forward move from slot `audible`, step over gone items. When
    /// nothing playable remains ahead, end playback as a natural end does,
    /// back on `audible` (so Previous resumes it even after the marks expire
    /// or a restart), and leave a notice.
    ///
    /// On radio, the fully cached radio items are cycled first, as offline.
    /// Returns whether nothing playable was ahead (cycled or ended).
    fn settle_forward(&mut self, audible: usize) -> bool {
        let mut hopped = false;
        for _ in 0..self.queue.items().len() {
            match self.queue.track() {
                Some(track) if self.skippable(track) => {
                    hopped = true;
                    self.queue.advance(false);
                }
                _ => break,
            }
        }
        if !hopped
            || self
                .queue
                .track()
                .is_some_and(|track| !self.skippable(track))
        {
            return false;
        }
        if self.radio.is_some() {
            let reselected = self
                .queue
                .physical_index(audible)
                .and_then(|index| self.queue.select(index));
            if reselected.is_ok() && self.advance_cached_radio().is_ok() {
                return true;
            }
        }
        self.finish_gone(audible);
        true
    }
    /// End playback at slot `audible` because what follows is gone.
    fn finish_gone(&mut self, audible: usize) {
        self.queue.finish_at(audible);
        self.health.set_notice();
    }
    /// After a backward move, keep stepping back over gone items; fails when
    /// nothing playable lies behind (the caller restores the queue).
    fn settle_back(&mut self) -> Result<()> {
        for _ in 0..self.queue.items().len() {
            match self.queue.track() {
                Some(track) if self.skippable(track) => {
                    let cursor = self.queue.cursor();
                    self.queue.previous();
                    if self.queue.cursor() == cursor {
                        return Err(Error::Input("This track is no longer on the server"));
                    }
                }
                _ => return Ok(()),
            }
        }
        Err(Error::Input("This track is no longer on the server"))
    }
    /// Where an automatic skip from the current item would land, computed on
    /// a copy so that the runtime can decide before anything changes.
    pub(crate) fn peek_skip(&self, back: bool) -> Skip {
        let mut queue = self.queue.clone();
        if back {
            let cursor = queue.cursor();
            queue.previous();
            if queue.cursor() == cursor {
                return Skip::Stuck;
            }
        } else {
            queue.advance(false);
        }
        for _ in 0..queue.items().len() {
            let Some(track) = queue.track() else {
                return if back { Skip::Stuck } else { Skip::End };
            };
            if !self.skippable(track) {
                let ready = track.media.first().and_then(|m| m.parts.first()).is_some()
                    || self
                        .cache
                        .lookup(&self.cache_namespace(), track, false)
                        .is_some();
                return if ready {
                    Skip::Ready
                } else {
                    Skip::NeedsMetadata
                };
            }
            let cursor = queue.cursor();
            if back {
                queue.previous();
                if queue.cursor() == cursor {
                    return Skip::Stuck;
                }
            } else {
                queue.advance(false);
            }
        }
        if back {
            Skip::Stuck
        } else {
            Skip::End
        }
    }
    pub(crate) fn set_health_epoch(&mut self, epoch: std::sync::Arc<std::sync::atomic::AtomicU64>) {
        self.health.set_epoch(epoch);
    }
    pub(crate) fn offline_mode(&self) -> bool {
        self.offline_mode
    }
    /// The platform reported no connection (not merely unknown).
    pub(crate) fn known_offline(&self) -> bool {
        self.network_known && !self.network_online
    }
    /// The logical play occurrence of the current item (stable across
    /// same-item fallbacks).
    pub(crate) fn occurrence(&self) -> &str {
        &self.settings.playback.occurrence
    }
    pub(crate) fn current_key(&self) -> Option<&str> {
        self.queue.track().map(|track| track.rating_key.as_str())
    }
    pub(crate) fn queue_contains(&self, key: &str) -> bool {
        self.queue.items().iter().any(|item| item.rating_key == key)
    }
    pub(crate) fn is_gone(&self, key: &str) -> bool {
        self.health.is_gone(key)
    }
    pub(crate) fn may_probe(&self, key: &str) -> bool {
        !self.offline_mode
            && !(self.network_known && !self.network_online)
            && self.health.may_probe(key)
    }
    /// A probe request plus its owned future, or None when probing is not
    /// possible (no server, invalid key).
    pub(crate) fn probe(
        &self,
        key: &str,
    ) -> Option<impl std::future::Future<Output = plex::Probe> + Send + 'static + use<>> {
        self.backend.probe(&self.conn().ok()?, key).ok()
    }
    pub(crate) fn record_probe(&mut self, key: &str, outcome: plex::Probe) {
        match outcome {
            plex::Probe::Gone => self.health.mark_gone(key),
            plex::Probe::Auth | plex::Probe::Unknown => self.health.mark_unknown(key),
        }
    }
    pub(crate) fn take_cache_suspects(&self) -> Vec<String> {
        self.cache.take_suspects()
    }
    pub(crate) fn take_probe_requests(&mut self) -> Vec<String> {
        self.health.take_pending()
    }
    /// Whether the last command stepped over gone items to a natural end.
    pub(crate) fn take_gone_notice(&mut self) -> bool {
        self.health.take_notice()
    }
    fn change_playback(&mut self, change: impl FnOnce(&mut Queue) -> Result<()>) -> Result<Value> {
        let previous = self.queue.clone();
        change(&mut self.queue)?;
        let result = self.playback();
        if result.is_err() {
            self.queue = previous;
        }
        result
    }
    /// Run an account-level (Plex sign-in, discovery) call. While a Jellyfin
    /// session is active a temporary Plex client serves it, so the account can
    /// be signed in without leaving the session.
    fn with_account<T>(&self, call: impl FnOnce(&dyn server::Account) -> Result<T>) -> Result<T> {
        match &self.backend {
            Backend::Jellyfin(_) => {
                let plex = Plex::new(&self.settings.client_identifier, self.account_url.clone())?;
                call(&plex)
            }
            other => call(other.account()?),
        }
    }
    fn connect(&mut self, url: String, token: String) -> Result<Value> {
        // A Plex connection replaces a Jellyfin backend; every other backend
        // (Plex, an injected test server) stays.
        let replacement = match &self.backend {
            Backend::Jellyfin(_) => Some(Backend::Plex(Plex::new(
                &self.settings.client_identifier,
                self.account_url.clone(),
            )?)),
            _ => None,
        };
        let base = replacement
            .as_ref()
            .unwrap_or(&self.backend)
            .parse_url(url.trim())?;
        let mut settings = self.settings.clone();
        settings.server_kind = store::ServerKind::Plex;
        settings.server_id.clear();
        settings.server_url = base.to_string();
        settings.server_token = token;
        // The user id belongs to the previous sign-in; a backend that needs
        // one stores it after its own login.
        settings.server_user_id.clear();
        self.quick_connect = None;
        self.establish(replacement, settings)
    }
    /// Make `settings` (signed in to `backend`, or to the current backend)
    /// the session: prove the connection with the library listing first, keep
    /// the pins, downloads and queue when the server scope is unchanged and
    /// drop them when it is another one, then save once.
    fn establish(
        &mut self,
        backend: Option<Backend>,
        mut settings: store::Settings,
    ) -> Result<Value> {
        let target = backend.as_ref().unwrap_or(&self.backend);
        let conn = connection(target, &settings)?;
        let c = target.fetch(&conn, &Query::Sections, None)?;
        // Same server scope addresses the same cache namespace: its pins,
        // downloaded audio and queue remain valid and are kept.
        let switched = target.namespace(&conn) != self.cache_namespace();
        if switched {
            settings.playback = store::Playback::default();
            settings.download_groups.clear();
            settings.download_titles.clear();
            settings.downloads.clear();
            settings.download_plans.clear();
        }
        store::save(&self.dir, &settings)?;
        self.settings = settings;
        if let Some(backend) = backend {
            self.backend = backend;
        }
        self.save_snapshot(&Query::Sections, None, &c);
        if switched {
            self.refresh.reset();
            self.queue = Queue::default();
            self.radio = None;
            self.resume_position = 0;
            self.cache.cancel();
            self.artwork.cancel();
            self.manual_cache.clear();
        } else {
            self.refresh.reconnect();
            self.schedule_cache();
        }
        self.artist_indexes.clear();
        self.detail_kinds.borrow_mut().clear();
        self.offline_mode = false;
        Ok(
            json!({"libraries": c.sections.into_iter().filter(|s| s.kind == crate::model::ItemKind::Artist).collect::<Vec<_>>(), "serverUrl": self.settings.server_url,"serverKind":self.settings.server_kind.as_str(),"capabilities":self.backend.capabilities(),"radio":self.radio_summary(),"cache":self.cache_status(),"offlineMode":false,"offline":false}),
        )
    }
    /// The Jellyfin sign-in client: no server identity yet.
    fn jellyfin_client(&self) -> jellyfin::Jellyfin {
        jellyfin::Jellyfin::new(&self.settings.client_identifier, "")
    }
    /// Sign in to Jellyfin with a name and password.
    fn jellyfin_login(&mut self, url: &str, username: &str, password: String) -> Result<Value> {
        if username.trim().is_empty() || username.len() > 256 || password.len() > 1024 {
            return Err(Error::Input("Invalid request data"));
        }
        let client = self.jellyfin_client();
        let base = client.parse_url(url.trim())?;
        // The password lives in this call only.
        let login = client.login(&base, username.trim(), &password);
        drop(password);
        self.adopt_jellyfin(&base, login?)
    }
    /// Make a completed Jellyfin sign-in the session (one save).
    fn adopt_jellyfin(&mut self, base: &url::Url, login: jellyfin::Login) -> Result<Value> {
        let backend = Backend::Jellyfin(jellyfin::Jellyfin::new(
            &self.settings.client_identifier,
            &login.server_id,
        ));
        let mut settings = self.settings.clone();
        settings.server_kind = store::ServerKind::Jellyfin;
        settings.server_url = base.to_string();
        settings.server_token = login.token;
        settings.server_user_id = login.user_id;
        settings.server_id = login.server_id;
        let result = self.establish(Some(backend), settings);
        if result.is_ok() {
            self.quick_connect = None;
        }
        result
    }
    fn jellyfin_quick_connect(&mut self, url: &str) -> Result<Value> {
        let client = self.jellyfin_client();
        let base = client.parse_url(url.trim())?;
        let request = client.quick_connect_start(&base)?;
        let code = request.code.clone();
        self.quick_connect = Some(QuickConnectPending {
            base,
            secret: request.secret,
            code: code.clone(),
            started: Instant::now(),
        });
        Ok(
            json!({"quickConnect":{"code":code,"expiresIn":jellyfin::QUICK_CONNECT_LIFETIME.as_secs()}}),
        )
    }
    fn jellyfin_quick_connect_poll(&mut self) -> Result<Value> {
        // A poll that crosses a cancellation finds nothing pending: that is
        // the login having ended, not an error.
        let Some(pending) = self.quick_connect.as_ref() else {
            return Ok(json!({"quickConnect":null}));
        };
        let (base, secret, code) = (
            pending.base.clone(),
            pending.secret.clone(),
            pending.code.clone(),
        );
        if pending.started.elapsed() >= jellyfin::QUICK_CONNECT_LIFETIME {
            self.quick_connect = None;
            return Ok(quick_connect_expired());
        }
        let client = self.jellyfin_client();
        match client.quick_connect_approved(&base, &secret) {
            // The server forgot the secret: it expired.
            Err(Error::Http(404)) => {
                self.quick_connect = None;
                Ok(quick_connect_expired())
            }
            // A transient failure keeps the request pending; the next poll retries.
            Err(error) => Err(error),
            Ok(false) => Ok(json!({"quickConnect":{"code":code}})),
            Ok(true) => {
                let login = client.quick_connect_login(&base, &secret)?;
                let mut reply = self.adopt_jellyfin(&base, login)?;
                // A new session: stale health marks mean nothing, and the
                // runtime stops the audio of the previous one.
                let saved = self.health.set_aside();
                self.health.commit(saved);
                self.catalogue_epoch = self.catalogue_epoch.wrapping_add(1);
                reply["signedIn"] = json!(true);
                reply["sessionReset"] = json!(true);
                reply["quickConnect"] = Value::Null;
                Ok(reply)
            }
        }
    }
    /// The pending Quick Connect request as the status presents it.
    fn quick_connect_status(&self) -> Value {
        match &self.quick_connect {
            Some(pending) if pending.started.elapsed() < jellyfin::QUICK_CONNECT_LIFETIME => {
                json!({"code":pending.code,"expiresIn":(jellyfin::QUICK_CONNECT_LIFETIME - pending.started.elapsed()).as_secs()})
            }
            _ => Value::Null,
        }
    }
    /// Whether the audible track's end is reported to the server when the
    /// next one starts without a command (a station's play queue, or a
    /// server that counts plays as they start).
    pub(crate) fn reports_stop_on_transition(&self) -> bool {
        self.station_radio_active() || self.backend.reports_start()
    }
    pub fn execute(&mut self, command: Command) -> Result<Value> {
        let catalogue_change = matches!(
            &command,
            Command::Pin { .. }
                | Command::DownloadAction { .. }
                | Command::CacheTracks { .. }
                | Command::ClearCache
                | Command::Connect { .. }
                | Command::SelectServer { .. }
                | Command::JellyfinLogin { .. }
                | Command::Logout
        );
        let persist = matches!(
            &command,
            Command::Play { .. }
                | Command::PlayAlbum { .. }
                | Command::Resume
                | Command::Enqueue { .. }
                | Command::EnqueueAlbum { .. }
                | Command::QueueRemove { .. }
                | Command::QueueMove { .. }
                | Command::SavePlayback { .. }
                | Command::Radio { .. }
                | Command::StopRadio
                | Command::SelectTrack { .. }
                | Command::Next { .. }
                | Command::Previous
                | Command::Shuffle { .. }
                | Command::Repeat { .. }
                | Command::ClearCache
                | Command::CacheTracks { .. }
                | Command::Pin { .. }
                | Command::Favorite { .. }
                | Command::Autoplay { .. }
                | Command::Mix { .. }
                | Command::DownloadPolicy { .. }
                | Command::DownloadAction { .. }
                | Command::QualityConfig { .. }
                | Command::DownloadPlan { .. }
                | Command::DownloadStation { .. }
                | Command::PlayDownload { .. }
                | Command::Station { .. }
        );
        // An explicit download request retries what the server refused.
        if matches!(
            &command,
            Command::Pin { .. }
                | Command::DownloadAction { .. }
                | Command::DownloadPlan { .. }
                | Command::DownloadStation { .. }
                | Command::PlayDownload { .. }
                | Command::CacheTracks { .. }
        ) {
            self.cache.forget_failures();
        }
        let resuming = matches!(
            &command,
            Command::Resume
                | Command::CachedPlayback
                | Command::TranscodedPlayback
                | Command::RestartTranscode
        );
        // A new queue or server makes every health mark meaningless. Clear
        // them before planning (a fresh Play of a once-gone key must work)
        // and put them back if the replacement fails.
        let replacing = matches!(
            &command,
            Command::Play { .. }
                | Command::PlayAlbum { .. }
                | Command::Radio { .. }
                | Command::StopRadio
                | Command::Mix { .. }
                | Command::PlayDownload { .. }
                | Command::Station { .. }
                | Command::Connect { .. }
                | Command::SelectServer { .. }
                | Command::JellyfinLogin { .. }
                | Command::Logout
        );
        let set_aside = replacing.then(|| self.health.set_aside());
        #[cfg(test)]
        let op = command.op();
        let result = self.execute_inner(command);
        if result.is_err() {
            // A failed command rolled its queue change back: nothing ended.
            self.health.take_notice();
        }
        match (set_aside, result.is_ok()) {
            (Some(saved), true) => self.health.commit(saved),
            (Some(saved), false) => self.health.restore(saved),
            (None, _) => {}
        }
        let mut result = result?;
        #[cfg(test)]
        test_hooks::check(&self.dir, op);
        if catalogue_change {
            self.catalogue_epoch = self.catalogue_epoch.wrapping_add(1);
        }
        // An empty stream (e.g. ClearCache returns "") is not a playable plan
        // and must not bump generations or reset occurrences.
        let has_stream = result
            .get("stream")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty());
        if has_stream {
            result["normalizationMode"] = json!(self.normalization_mode());
            result["albumNormalization"] = json!(self.album_normalization());
            self.playback_generation = self.playback_generation.wrapping_add(1);
            result["playbackGeneration"] = json!(self.playback_generation);
        }
        if has_stream && !resuming {
            self.resume_position = 0;
            self.settings.playback.occurrence = uuid::Uuid::new_v4().to_string();
            self.settings.playback.listened = 0;
        }
        if has_stream {
            result["playbackOccurrence"] = json!(self.settings.playback.occurrence);
            result["resumeListened"] = json!(self.settings.playback.listened);
        }
        if persist {
            self.save_session();
        }
        if result.get("queue").is_some() {
            result["queue"] = self.queue_json(&self.queue);
        }
        Ok(result)
    }
    /// Persist the session (queue, radio, position, downloads) after a change
    /// that already took effect in memory.
    fn save_session(&mut self) {
        if !self.writable {
            return;
        }
        self.settings.playback = store::Playback {
            queue: self.queue.clone(),
            radio: self.radio.clone(),
            position: self.resume_position,
            generation: self.playback_generation,
            occurrence: self.settings.playback.occurrence.clone(),
            listened: self.settings.playback.listened,
            start_reported: self.settings.playback.start_reported.clone(),
        };
        self.settings.downloads = self.manual_cache.clone();
        // Failing the change now would hide a plan the UI must follow; keep
        // the consistent in-memory state and retry at the next checkpoint.
        if let Err(error) = store::save(&self.dir, &self.settings) {
            diagnostics::event(format_args!("session checkpoint not saved: {error}"));
        }
    }
    /// The queue as the UI presents it.
    fn queue_json(&self, queue: &Queue) -> Value {
        let q = queue.presentation();
        json!({"items":q.items().iter().map(|i|self.display_item(i)).collect::<Vec<_>>(),"current":q.current(),"repeat":q.repeat(),"shuffled":queue.shuffled()})
    }
    pub fn download_control(&self) -> cache::DownloadControl {
        self.cache.control(self.artwork.generation())
    }
    fn execute_inner(&mut self, command: Command) -> Result<Value> {
        self.require(command.requires())?;
        match command {
            Command::ProbePlayback { key, kbps } => self.probe_playback(&key, kbps),
            Command::SmartRules { key } => self.smart_rules(&key),
            Command::DiscoveryConfig {
                section,
                hidden,
                order,
            } => self.discovery_config(&section, hidden, order),
            Command::HubItems { key, start } => self.hub_items(&key, start),
            Command::ListeningInsights { days } => self.listening_insights(days),
            Command::SonicJourney { section, keys } => self.sonic_journey(&section, keys),
            Command::FilterOptions {
                section,
                kind,
                field,
            } => self.filter_options(&section, &kind, &field),
            Command::FilteredBrowse {
                section,
                kind,
                sort,
                filters,
                start,
            } => self.filtered_browse(&section, &kind, &sort, &filters, start),
            Command::SmartPlaylist {
                action,
                key,
                section,
                title,
                sort,
                limit,
                filters,
            } => self.smart_playlist(action, (&key, &section), &title, &sort, limit, &filters),
            Command::RefreshPolicy {
                enabled,
                interval_hours,
            } => {
                let policy = refresh::Policy {
                    enabled,
                    interval_hours,
                };
                policy.validate()?;
                self.settings.refresh_policy = policy;
                self.refresh.cancel();
                if self.writable {
                    store::save(&self.dir, &self.settings)?;
                }
                Ok(json!({"cache":self.cache_status()}))
            }
            Command::RefreshDownloads => self.maintain_refresh(),
            Command::ProbeQuality { key, kbps } => self.probe_quality(&key, kbps),
            Command::DownloadStation {
                key,
                title,
                minutes,
            } => self.download_station(&key, &title, minutes),
            Command::Station { key, title } => self.start_station(&key, &title),
            Command::DownloadPlan { kind, key, minutes } => self.download_plan(kind, &key, minutes),
            Command::PlayDownload { group } => self.play_download(&group),
            Command::OfflineSearch { query, kind, start } => {
                self.offline_search(&query, &kind, start)
            }
            Command::DiscoveryHome { section } => self.discovery_home(&section),
            Command::SonicNeighbors { key, kind } => self.sonic_neighbors(&key, &kind),
            Command::SonicAdventure {
                section,
                start_key,
                end_key,
            } => self.sonic_adventure(&section, &start_key, &end_key),
            Command::QualityConfig { config } => {
                config.validate()?;
                self.settings.quality = config;
                self.cache.cancel();
                self.schedule_cache();
                Ok(json!({"qualityConfig":self.settings.quality}))
            }
            Command::TranscodedPlayback => {
                if !self.settings.quality.codec_fallback || self.offline_mode {
                    return Err(Error::Input("Audio decoding or streaming failed"));
                }
                self.playback_plan(false, true)
            }
            Command::RestartTranscode => {
                if self.offline_mode || self.known_offline() {
                    return Err(Error::Input("Audio decoding or streaming failed"));
                }
                self.playback_plan(false, true)
            }
            Command::PlaylistChoices => self.playlist_choices(),
            Command::PlaylistEdit {
                action,
                key,
                title,
                items,
                item_id,
                after,
                index,
            } => {
                let entry = features::Entry {
                    item: item_id,
                    after,
                    index,
                };
                self.edit_playlist(action, &key, &title, items, entry)
            }
            Command::PlaylistItems { key, start } => self.playlist_items(&key, start),
            Command::LibraryBrowse {
                section,
                kind,
                sort,
                start,
            } => self.library_browse(&section, &kind, &sort, start),
            Command::ArtistAlbums { key } => self.artist_albums(&key),
            Command::Downloads => {
                Ok(json!({"downloads":self.download_rows(),"cache":self.cache_status()}))
            }
            Command::DownloadPolicy { wifi_only, paused } => {
                self.download_control().policy(wifi_only, paused);
                self.settings.wifi_only = wifi_only;
                self.settings.downloads_paused = paused;
                if paused || wifi_only && !self.network_wifi {
                    self.refresh.cancel();
                }
                self.cache.cancel();
                self.artwork.cancel();
                self.schedule_cache();
                Ok(json!({"cache":self.cache_status(),"downloads":self.download_rows()}))
            }
            Command::DownloadAction { group, action } => self.download_action(&group, action),
            Command::NetworkState {
                wifi,
                online,
                cellular,
                live_hint,
            } => {
                if !live_hint {
                    self.download_control().network(wifi);
                }
                let changed = wifi != self.network_wifi
                    || online != self.network_online
                    || cellular != self.network_cellular;
                if online && !self.network_online {
                    self.refresh.reconnect();
                }
                self.network_wifi = wifi;
                self.network_cellular = cellular;
                self.network_online = online;
                self.network_known = true;
                if !online || !self.downloads_allowed() {
                    self.refresh.cancel();
                }
                if changed {
                    self.cache.cancel();
                    if !self.downloads_allowed() {
                        self.artwork.cancel();
                    }
                    self.schedule_cache();
                }
                Ok(
                    json!({"cache":self.cache_status(),"networkOnline":online,"networkWifi":wifi,"networkCellular":cellular}),
                )
            }
            Command::Autoplay { enabled } => {
                self.settings.autoplay = enabled;
                Ok(json!({"autoplay":enabled}))
            }
            Command::Mix { seeds } => self.play_mix(seeds),
            Command::PlaybackEvent {
                key,
                occurrence,
                listened,
                duration,
            } => self.record_play(&key, &occurrence, listened, duration),
            Command::SyncHistory => self.sync_history(),
            Command::Pin { key, kind, enabled } => {
                self.id(&key)?;
                let Some(pinned) = DownloadKind::pinned(&kind) else {
                    return Err(Error::Input("Choose a track, album or playlist to pin"));
                };
                let group = GroupId::new(pinned, key.clone());
                if enabled {
                    self.refresh.cancel();
                    if !self.settings.cache.enabled {
                        return Err(Error::Input(
                            "Enable audio caching before pinning downloads",
                        ));
                    }
                    let items = match kind {
                        ItemKind::Album => self.album_tracks(&Item {
                            rating_key: key.clone(),
                            ..Default::default()
                        })?,
                        ItemKind::Playlist => {
                            self.bounded_tracks(&Query::PlaylistItems { id: key.clone() }, 0)?
                        }
                        _ => {
                            let (c, _) = self.container_cached(
                                &Query::Item {
                                    id: key.clone(),
                                    stations: false,
                                },
                                None,
                            )?;
                            c.items
                                .into_iter()
                                .filter(|i| i.kind == crate::model::ItemKind::Track)
                                .collect()
                        }
                    };
                    let mut plan = self.manual_cache.clone();
                    for i in &items {
                        if !plan.iter().any(|p| p.rating_key == i.rating_key) {
                            plan.push(i.clone());
                        }
                    }
                    let unique: std::collections::BTreeSet<_> =
                        items.iter().map(|i| i.rating_key.clone()).collect();
                    let mut groups = self.settings.download_groups.clone();
                    groups.insert(group.clone(), unique.into_iter().collect());
                    let keep = daily::pinned_keys(&groups);
                    plan.retain(|i| keep.contains(&i.rating_key));
                    if plan.len() > 1000 || items.is_empty() {
                        return Err(Error::Input("Pin up to 1000 playable tracks"));
                    }
                    let title = if kind == ItemKind::Playlist {
                        self.playlist_metadata(&key)?.title
                    } else {
                        items
                            .first()
                            .map(|i| {
                                if kind == ItemKind::Track {
                                    i.title.clone()
                                } else {
                                    i.parent_title.clone()
                                }
                            })
                            .unwrap_or_default()
                    };
                    self.settings.download_titles.insert(group.clone(), title);
                    self.settings.download_groups = groups;
                    if kind == ItemKind::Playlist {
                        self.settings.download_plans.insert(
                            group.clone(),
                            daily::DownloadPlan {
                                kind: DownloadKind::Playlist,
                                key: key.clone(),
                                minutes: 0,
                                refreshed_at: crate::unix_seconds(),
                            },
                        );
                    }
                    self.manual_cache = plan;
                } else {
                    self.settings.download_groups.remove(&group);
                    self.settings.download_titles.remove(&group);
                    self.settings.download_plans.remove(&group);
                    let keep = daily::pinned_keys(&self.settings.download_groups);
                    self.manual_cache.retain(|i| keep.contains(&i.rating_key));
                }
                self.cache.cancel();
                self.schedule_cache();
                Ok(json!({"cache":self.cache_status()}))
            }
            Command::CacheArtwork { items } => {
                if self.downloads_allowed() {
                    for i in items.into_iter().take(100) {
                        for path in [&i.thumb, &i.parent_thumb, &i.art]
                            .into_iter()
                            .chain(i.images.iter().map(|image| &image.url))
                            .filter(|s| !s.is_empty())
                        {
                            if let Ok(conn) = self.conn() {
                                self.artwork.schedule_with(
                                    &self.backend.namespace(&conn),
                                    &self.backend.stream_policy(&conn),
                                    path,
                                );
                            }
                        }
                    }
                }
                Ok(json!({}))
            }
            Command::OfflineMode { enabled } => {
                self.offline_mode = enabled;
                if enabled {
                    self.refresh.cancel();
                    self.cache.cancel();
                    self.artwork.cancel();
                } else {
                    self.schedule_cache();
                }
                Ok(json!({"offlineMode":enabled}))
            }
            Command::OfflineBrowse { kind, start } => {
                if !kind.is_music() {
                    return Err(Error::Input("Unknown offline music type"));
                }
                let mut items = self.offline_catalogue();
                let mut seen = std::collections::BTreeSet::new();
                items.retain(|i| i.kind == kind && seen.insert(i.rating_key.clone()));
                // The key breaks title ties: pages must agree on one order.
                items.sort_by_cached_key(|i| (i.title.to_lowercase(), i.rating_key.clone()));
                let total = items.len();
                // Only the page is presented (an artwork and a cache lookup each).
                let page: Vec<_> = items
                    .iter()
                    .skip(start)
                    .take(OFFLINE_BROWSE_PAGE)
                    .map(|i| self.display_item(i))
                    .collect();
                let received = page.len();
                Ok(crate::pagination::page(
                    page,
                    start,
                    received,
                    OFFLINE_BROWSE_PAGE,
                    Some(total),
                    true,
                ))
            }
            Command::Lyrics { key } => {
                let id = self.id(&key)?;
                if self.offline_mode {
                    let lines = self
                        .library
                        .lyrics(&self.cache_namespace(), id)
                        .ok_or(Error::Input("Lyrics have not been saved offline"))?;
                    let lines = lyrics::normalize_cached(lines)?;
                    self.library
                        .save_lyrics(&self.cache_namespace(), id, &lines);
                    return Ok(json!({"lyricsKey":id,"lyrics":lines}));
                }
                let (c, _) = self.container_cached(
                    &Query::Item {
                        id: id.into(),
                        stations: false,
                    },
                    None,
                )?;
                let stream = c
                    .items
                    .iter()
                    .filter(|i| i.kind == crate::model::ItemKind::Track && i.rating_key == id)
                    .flat_map(|i| &i.media)
                    .flat_map(|m| &m.parts)
                    .flat_map(|p| &p.streams)
                    .find(|s| s.stream_type == 4 && !s.key.is_empty());
                let lines = if let Some(stream) = stream {
                    self.backend.lyrics(&self.conn()?, &stream.key)?
                } else {
                    vec![]
                };
                self.library
                    .save_lyrics(&self.cache_namespace(), id, &lines);
                Ok(json!({"lyricsKey":id,"lyrics":lines}))
            }
            Command::Search { query } => {
                if query.trim().is_empty() {
                    return Ok(json!({"items":[],"start":0,"next":0,"hasMore":false}));
                }
                if self.offline_mode {
                    return self.offline_search(&query, &ItemKind::default(), 0);
                }
                let (c, _) = match self.container_cached(
                    &Query::Search {
                        query: query.clone(),
                        limit: 100,
                    },
                    None,
                ) {
                    // An unreachable server searches the saved library instead.
                    Err(Error::Network | Error::Http(500..=599)) => {
                        // Online search has no further pages, so neither does this.
                        let mut value = self.offline_search(&query, &ItemKind::default(), 0)?;
                        value["hasMore"] = json!(false);
                        value["next"] = json!(0);
                        return Ok(value);
                    }
                    result => result?,
                };
                let mut seen = std::collections::BTreeSet::new();
                let items: Vec<_> = c
                    .hubs
                    .iter()
                    .flat_map(|h| &h.items)
                    .chain(&c.items)
                    .filter(|i| {
                        matches!(
                            i.kind,
                            crate::model::ItemKind::Artist
                                | crate::model::ItemKind::Album
                                | crate::model::ItemKind::Track
                        ) && self.id(&i.rating_key).is_ok()
                            && seen.insert((i.kind.clone(), i.rating_key.clone()))
                    })
                    .map(|i| self.display_item(i))
                    .collect();
                Ok(json!({"items":items,"start":0,"next":0,"hasMore":false}))
            }
            Command::Collection {
                section,
                view,
                start,
            } => {
                let section = self.id(&section)?;
                let spec = match view {
                    CollectionView::Favorites => BrowseSpec {
                        favourite: true,
                        ..BrowseSpec::new(ItemKind::Track, Sort::Title)
                    },
                    CollectionView::Added => BrowseSpec::new(ItemKind::Album, Sort::Added),
                    CollectionView::Played => BrowseSpec {
                        played: true,
                        ..BrowseSpec::new(ItemKind::Track, Sort::LastPlayed)
                    },
                };
                self.page(&Query::browse(section, spec), start)
            }
            Command::Favorite { key, enabled } => {
                if self.offline_mode {
                    return Err(Error::Input("Go online"));
                }
                self.backend.act(
                    &self.conn()?,
                    &Action::Rate {
                        id: self.id(&key)?.to_owned(),
                        rating: if enabled { 10 } else { 0 },
                    },
                )?;
                self.library.rate(
                    &self.cache_namespace(),
                    &key,
                    if enabled { 10. } else { 0. },
                );
                self.queue.update_items(|item| {
                    if item.rating_key == key {
                        item.user_rating = if enabled { 10. } else { 0. };
                    }
                });
                Ok(
                    json!({"ratedKey":key,"userRating":if enabled{10}else{0},"queue":self.queue,"track":self.queue.track().map(|i|self.display_item(i))}),
                )
            }
            Command::Status => Ok(
                json!({"signedIn": !self.settings.account_token.is_empty() || self.settings.server_kind == store::ServerKind::Jellyfin && !self.settings.server_token.is_empty(), "serverKind": self.settings.server_kind.as_str(), "quickConnect": self.quick_connect_status(), "serverUrl": self.settings.server_url, "queue": self.queue,"radio":self.radio_summary(),"cache":self.cache_status(),"track":self.queue.track().map(|i|self.display_item(i)),"resumePosition":self.resume_position,"playbackGeneration":self.playback_generation,"playbackOccurrence":self.settings.playback.occurrence,"resumeListened":self.settings.playback.listened,"autoplay":self.settings.autoplay,"history":self.history_status(),"audioConfig":self.settings.audio,"qualityConfig":self.settings.quality,"capabilities":self.backend.capabilities()}),
            ),
            Command::Resume => {
                // An explicit play retries an item marked gone.
                if let Some(key) = self.queue.track().map(|track| track.rating_key.clone()) {
                    self.health.forget(&key);
                }
                let mut value = self.playback()?;
                value["resumePosition"] = json!(self.resume_position);
                Ok(value)
            }
            Command::SavePlayback {
                key,
                position,
                generation,
            } => {
                if generation.is_none_or(|g| g == self.playback_generation)
                    && self.queue.track().is_some_and(|i| i.rating_key == key)
                {
                    let duration = self.queue.track().map(|t| t.duration).unwrap_or(0);
                    // Duration may still be unknown (0) while metadata resolves:
                    // don't clamp every save to zero, mirroring audio seek.
                    self.resume_position = if duration > 0 {
                        position.min(duration)
                    } else {
                        position
                    };
                }
                Ok(json!({}))
            }
            Command::Enqueue { items, next } => {
                self.queue.insert(items, next)?;
                self.radio = None;
                self.schedule_cache();
                Ok(json!({"queue":self.queue,"radio":null}))
            }
            Command::EnqueueAlbum { key, next } => {
                let items = self.album_tracks(&Item {
                    rating_key: key,
                    ..Default::default()
                })?;
                self.execute_inner(Command::Enqueue { items, next })
            }
            Command::PlayAlbum { key } => {
                let items = self.album_tracks(&Item {
                    rating_key: key,
                    ..Default::default()
                })?;
                self.execute_inner(Command::Play { items, index: 0 })
            }
            Command::QueueRemove { index } => {
                let physical = self.queue.physical_index(index)?;
                let previous = self.queue.clone();
                let radio = self.radio.take();
                let active = self.queue.remove(physical)?;
                let result = if active {
                    // The removal stands even when only gone items follow; a
                    // later Previous resumes the item before the removed one.
                    let audible = self.queue.cursor().saturating_sub(1);
                    self.settle_forward(audible);
                    self.playback()
                } else {
                    self.schedule_cache();
                    Ok(json!({"queue":self.queue,"radio":null}))
                };
                if result.is_err() {
                    self.queue = previous;
                    self.radio = radio;
                }
                result
            }
            Command::QueueMove { from, to } => {
                self.queue.physical_index(from)?;
                self.queue.physical_index(to)?;
                if from == to {
                    return Ok(json!({"queue":self.queue,"radio":self.radio_summary()}));
                }
                let mut queue = self.queue.presentation();
                queue.move_item(from, to)?;
                self.queue = queue;
                self.radio = None;
                self.schedule_cache();
                Ok(json!({"queue":self.queue,"radio":null}))
            }
            Command::CacheStatus => Ok(
                json!({"cache":self.cache_status(),"localArtwork":self.queue.track().and_then(|i|self.artwork.local(&self.cache_namespace(),if i.thumb.is_empty(){&i.parent_thumb}else{&i.thumb})),"localAlbumArtwork":self.queue.track().and_then(|i|self.artwork.local(&self.cache_namespace(),&i.parent_thumb))}),
            ),
            Command::CacheConfig {
                enabled,
                limit_mb,
                ahead,
            } => {
                let config = cache::Config {
                    enabled,
                    limit_mb,
                    ahead,
                };
                config.validate()?;
                let mut settings = self.settings.clone();
                settings.cache = config.clone();
                let namespace = self.cache_namespace();
                let keep = self
                    .queue
                    .track()
                    .map(|i| (namespace.as_str(), i.rating_key.as_str()));
                self.cache.configure(config, keep)?;
                store::save(&self.dir, &settings)?;
                self.settings = settings;
                if self.settings.cache.enabled
                    && self.manual_cache.is_empty()
                    && self.queue.track().is_none()
                {
                    self.manual_cache = self.cache.pending_items(&self.cache_namespace());
                }
                self.schedule_cache();
                Ok(json!({"cache":self.cache_status()}))
            }
            Command::CacheTracks { items } => {
                if !self.settings.cache.enabled {
                    return Err(Error::Input(
                        "Enable audio caching in Connection settings first",
                    ));
                }
                if items.is_empty()
                    || items.len() > 1000
                    || items.iter().any(|i| {
                        i.kind != crate::model::ItemKind::Track || self.id(&i.rating_key).is_err()
                    })
                {
                    return Err(Error::Input("Choose up to 1000 valid tracks to download"));
                }
                let mut plan = self.manual_cache.clone();
                for item in &items {
                    if !plan.iter().any(|i| i.rating_key == item.rating_key) {
                        plan.push(item.clone());
                    }
                }
                if plan.len() > 1000 {
                    return Err(Error::Input(
                        "Download plan exceeds 1000 tracks; unpin some downloads first",
                    ));
                }
                for item in items {
                    self.settings.download_groups.insert(
                        GroupId::new(DownloadKind::Track, item.rating_key.clone()),
                        vec![item.rating_key.clone()],
                    );
                    if !self
                        .manual_cache
                        .iter()
                        .any(|i| i.rating_key == item.rating_key)
                    {
                        self.manual_cache.push(item);
                    }
                }
                self.schedule_cache();
                Ok(json!({"cache":self.cache_status()}))
            }
            Command::CachedTracks => {
                let items: Vec<_> = self
                    .cache
                    .items(&self.cache_namespace())
                    .iter()
                    .map(|i| self.display_item(i))
                    .collect();
                Ok(
                    json!({"items":items,"start":0,"next":0,"hasMore":false,"cache":self.cache_status()}),
                )
            }
            Command::CachedPlayback => {
                let track = self
                    .queue
                    .track()
                    .ok_or(Error::Input("Nothing is queued"))?;
                if self
                    .cache
                    .lookup(&self.cache_namespace(), track, false)
                    .is_none()
                {
                    return Err(Error::Input(
                        "This track is not fully cached yet; downloaded tracks remain available",
                    ));
                }
                self.playback()
            }
            Command::ClearCache => {
                // Plans and titles go too: an orphaned plan would be refreshed
                // and silently re-download what the user just cleared.
                self.refresh.reset();
                self.cache.clear()?;
                self.manual_cache.clear();
                self.settings.download_groups.clear();
                self.settings.download_titles.clear();
                self.settings.download_plans.clear();
                self.queue = Queue::default();
                self.radio = None;
                self.resume_position = 0;
                Ok(
                    json!({"queue":self.queue,"radio":null,"track":null,"stream":"","cache":self.cache_status()}),
                )
            }
            Command::Login => {
                let identifier = self.settings.client_identifier.clone();
                let (pin, url) = self.with_account(|account| {
                    let pin = account.pin()?;
                    let url = account.login_url(&identifier, &pin);
                    Ok((pin, url))
                })?;
                let result = json!({"url": url, "expiresIn": pin.expires_in});
                self.pin = Some((pin, Instant::now()));
                Ok(result)
            }
            Command::PollLogin => {
                let (pin, created) = self.pin.as_ref().ok_or(Error::Input("Start login first"))?;
                if created.elapsed() >= Duration::from_secs(pin.expires_in) {
                    self.pin = None;
                    return Err(Error::Input("Login expired; start again"));
                }
                let (id, code) = (pin.id, pin.code.clone());
                let pin = self.with_account(|account| account.poll(id, &code))?;
                if let Some(token) = pin.auth_token.filter(|s| !s.is_empty()) {
                    let mut settings = self.settings.clone();
                    settings.account_token = token;
                    store::save(&self.dir, &settings)?;
                    self.settings = settings;
                    self.pin = None;
                    Ok(json!({"signedIn": true}))
                } else {
                    Ok(json!({"signedIn": false}))
                }
            }
            Command::Servers => {
                if self.settings.account_token.is_empty() {
                    return Err(Error::Input("Sign in first"));
                }
                let token = self.settings.account_token.clone();
                self.servers = self.with_account(|account| account.resources(&token))?;
                Ok(
                    json!({"servers": self.servers.iter().map(|s| json!({"name": s.name, "id": s.client_identifier})).collect::<Vec<_>>()}),
                )
            }
            Command::Connect { url, token } => self.connect(url, token),
            Command::JellyfinLogin {
                url,
                username,
                password,
            } => self.jellyfin_login(&url, &username, password),
            Command::JellyfinQuickConnect { url } => self.jellyfin_quick_connect(&url),
            Command::JellyfinQuickConnectPoll => self.jellyfin_quick_connect_poll(),
            Command::JellyfinQuickConnectCancel => {
                self.quick_connect = None;
                Ok(json!({"quickConnect":null}))
            }
            Command::SelectServer { index } => {
                let resource = self
                    .servers
                    .get(index)
                    .ok_or(Error::Input("Invalid server index"))?
                    .clone();
                let mut connections = resource.connections;
                connections.sort_by_key(|c| (c.relay, c.uri.starts_with("http:"), !c.local));
                let mut last = Error::Network;
                for connection in connections {
                    match self.connect(connection.uri, resource.access_token.clone()) {
                        Ok(value) => return Ok(value),
                        Err(error) => last = error,
                    }
                }
                Err(last)
            }
            Command::Libraries => {
                // A saved copy only flags this reply: explicit offline mode stays the
                // user's choice, so the next request contacts the server again.
                let (c, offline) = self.container_cached(&Query::Sections, None)?;
                Ok(
                    json!({"libraries": c.sections.into_iter().filter(|s| s.kind == crate::model::ItemKind::Artist).collect::<Vec<_>>(),"offline":offline,"offlineMode":self.offline_mode}),
                )
            }
            Command::Browse {
                section,
                kind,
                query,
                start,
            } => {
                let section = self.id(&section)?;
                filters::music_kind(&kind)?;
                let spec = BrowseSpec {
                    search: (!query.is_empty()).then_some(query),
                    ..BrowseSpec::new(kind, Sort::Title)
                };
                self.page(&Query::browse(section, spec), start)
            }
            Command::Alphabet { section } => self.alphabet(&section),
            Command::JumpArtist { section, letter } => {
                let index = self.alphabet(&section)?;
                let start = index["alphabet"]
                    .as_array()
                    .and_then(|groups| {
                        groups
                            .iter()
                            .find(|g| g["letter"].as_str() == Some(letter.as_str()))
                    })
                    .and_then(|g| g["offset"].as_u64())
                    .ok_or(Error::Input("No artists indexed under that letter"))?
                    as usize;
                let size = start
                    .checked_add(100)
                    .ok_or(Error::ProtocolAt("artist alphabet index"))?;
                let mut page = self.page_sized(
                    &Query::browse(
                        self.id(&section)?,
                        BrowseSpec::new(ItemKind::Artist, Sort::Title),
                    ),
                    0,
                    size,
                )?;
                page["replaceItems"] = json!(true);
                page["selectedLetter"] = json!(letter);
                page["selectedIndex"] = json!(start);
                Ok(page)
            }
            Command::Children { key, kind, start } => {
                let query = self.backend.children_query(&key, kind.as_ref())?;
                self.page(&query, start)
            }
            Command::Detail { key, kind, start } => self.detail(&key, kind, start),
            Command::SimilarArtists { key } => self.similar_artists(&key),
            Command::Playlists { start } => self.page(&Query::Playlists, start),
            Command::Play { items, index } => {
                let mut queue = Queue::default();
                queue.replace(items, index)?;
                std::mem::swap(&mut queue, &mut self.queue);
                let radio = self.radio.take();
                let result = self.playback();
                if result.is_err() {
                    self.queue = queue;
                    self.radio = radio;
                }
                result
            }
            Command::Radio { key, kind } => self.start_radio(key, kind),
            Command::StopRadio => {
                self.radio = None;
                self.queue = Queue::default();
                self.resume_position = 0;
                self.cache.cancel();
                self.playback()
            }
            Command::Timeline {
                state,
                position,
                duration,
                continuing,
            } => self.timeline(state, position, duration, continuing),
            Command::SelectTrack { index } => {
                let index = self.queue.physical_index(index)?;
                // Choosing an item explicitly retries it even if marked gone.
                if let Some(item) = self.queue.items().get(index) {
                    let key = item.rating_key.clone();
                    self.health.forget(&key);
                }
                self.change_playback(|queue| queue.select(index))
            }
            Command::Next { automatic } => self.next(automatic),
            Command::Previous => {
                let previous = self.queue.clone();
                self.queue.previous();
                let result = self.settle_back().and_then(|()| self.playback());
                if result.is_err() {
                    self.queue = previous;
                }
                result
            }
            Command::Shuffle { enabled } => {
                if self.radio.is_some() {
                    return Err(Error::Input(
                        "Radio keeps its recommended order; stop radio to shuffle",
                    ));
                }
                self.queue.shuffle(enabled);
                self.schedule_cache();
                Ok(json!({"queue": self.queue}))
            }
            Command::Repeat { mode } => {
                if self.radio.is_some() {
                    return Err(Error::Input(
                        "Radio continues automatically; stop radio to repeat",
                    ));
                }
                self.queue.set_repeat(mode);
                self.schedule_cache();
                Ok(json!({"queue": self.queue}))
            }
            Command::AudioConfig { config } => {
                self.set_audio_config(config.clone())?;
                Ok(json!({"audioConfig":config}))
            }
            Command::AudioPause
            | Command::AudioStop
            | Command::AudioSeek { .. }
            | Command::AudioVolume { .. }
            | Command::AudioPlay
            | Command::AudioToggle => Err(Error::Input("Unknown audio command")),
            Command::Logout => {
                self.refresh.reset();
                // Sign-out must not fail or wait on the server: ending a
                // Jellyfin session is best effort.
                if let (Backend::Jellyfin(client), Ok(conn)) = (&self.backend, self.conn()) {
                    client.logout(&conn);
                }
                let settings = store::Settings {
                    client_identifier: self.settings.client_identifier.clone(),
                    ..Default::default()
                };
                store::save(&self.dir, &settings)?;
                self.settings = settings;
                if matches!(self.backend, Backend::Jellyfin(_)) {
                    self.backend = make_backend(&self.settings, &self.account_url)?;
                }
                self.pin = None;
                self.quick_connect = None;
                self.servers.clear();
                self.queue = Queue::default();
                self.radio = None;
                self.resume_position = 0;
                self.cache.clear()?;
                self.manual_cache.clear();
                self.artist_indexes.clear();
                self.detail_kinds.borrow_mut().clear();
                self.library.clear()?;
                self.artwork.clear()?;
                Ok(
                    json!({"signedIn": false, "serverUrl": "", "serverKind": self.settings.server_kind.as_str(), "capabilities": self.backend.capabilities(), "quickConnect": null, "queue": self.queue, "radio":null, "track": null, "stream": "","history":self.history_status(),"cache":self.cache_status(),"autoplay":false}),
                )
            }
        }
    }
}

#[cfg(test)]
mod cache_schedule_diagnostics_tests {
    use super::CacheSchedule;

    fn scheduled(first: &str, items: usize) -> CacheSchedule {
        CacheSchedule::Scheduled {
            items,
            manual: 0,
            quality_kbps: 0,
            first: first.into(),
            cellular_cap: false,
        }
    }

    #[test]
    fn schedule_outcome_is_reported_only_when_it_changes() {
        let mut seen = None;
        let line = scheduled("7", 3).changed(&mut seen).unwrap();
        assert_eq!(
            line,
            "cache schedule items=3 manual=0 quality_kbps=0 first=7 cellular_cap=false"
        );
        assert!(scheduled("7", 3).changed(&mut seen).is_none());
        // A different window logs again.
        assert!(scheduled("8", 3).changed(&mut seen).is_some());
        assert!(scheduled("8", 4).changed(&mut seen).is_some());
        let skipped = CacheSchedule::Skipped("waiting_wifi");
        assert_eq!(
            skipped.changed(&mut seen).unwrap(),
            "cache schedule skipped reason=waiting_wifi"
        );
        assert!(skipped.changed(&mut seen).is_none());
        assert!(CacheSchedule::Skipped("cellular_cap")
            .changed(&mut seen)
            .is_some());
        let empty = CacheSchedule::Skipped("empty_window");
        assert_eq!(
            empty.changed(&mut seen).unwrap(),
            "cache schedule skipped reason=empty_window"
        );
        assert!(empty.changed(&mut seen).is_none());
        assert!(scheduled("8", 4).changed(&mut seen).is_some());
    }
}
