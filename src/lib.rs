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
pub mod store;

pub use command::{CollectionView, Command, DownloadAction, PlaylistAction, SmartAction};
pub use daily::{DownloadKind, GroupId};
use model::{Item, ItemKind, Pin, Resource};
use plex::{authenticated_url, server_url, Plex};
use queue::{Queue, Repeat};
use radio::{Radio, RadioKind, Source};
use serde_json::{json, Value};
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
    #[error("Plex authorization failed; sign in again or check server token")]
    Unauthorized,
    #[error("Unexpected Plex response")]
    Protocol,
    #[error("Unexpected Plex response during {0}")]
    ProtocolAt(&'static str),
    #[error("Plex returned HTTP {0}")]
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
    format!("{:x}", sha2::Sha256::digest(value.as_bytes()))
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

pub(crate) fn numeric(value: &str) -> Result<&str> {
    // Plex identifiers fit in u64 (max 20 decimal digits). Reject unbounded
    // digit strings before they become huge URL paths or overflow parsers.
    if value.is_empty() || value.len() > 20 || !value.bytes().all(|c| c.is_ascii_digit()) {
        Err(Error::Input("Expected a numeric Plex identifier"))
    } else {
        Ok(value)
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
    catalogue_epoch: u64,
    dir: PathBuf,
    settings: store::Settings,
    plex: Plex,
    pin: Option<(Pin, Instant)>,
    servers: Vec<Resource>,
    queue: Queue,
    radio: Option<Radio>,
    cache: cache::Cache,
    cached_playback: bool,
    manual_cache: Vec<Item>,
    cache_suspended: bool,
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
    base: url::Url,
    token: String,
    path: String,
    params: Vec<(&'static str, String)>,
    identity: RefillIdentity,
}
#[derive(PartialEq)]
struct RefillIdentity {
    server: String,
    instance: u64,
    version: u64,
    queue: u64,
}
pub(crate) struct RefillJob {
    request: RefillRequest,
    plex: Plex,
}
pub(crate) struct RefillFetched {
    request: RefillRequest,
    result: Result<model::Container>,
}
impl RefillJob {
    /// Blocking fetch; never call it on the shared runtime thread.
    pub(crate) fn fetch(self) -> RefillFetched {
        let RefillJob { request, plex } = self;
        let result = plex.container(
            &request.base,
            &request.token,
            &request.path,
            &request.params,
        );
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
        self.resume_position = 0;
        self.playback_generation = self.playback_generation.wrapping_add(1);
        self.settings.playback = store::Playback {
            queue: self.queue.clone(),
            radio: self.radio.clone(),
            position: 0,
            generation: self.playback_generation,
            occurrence: uuid::Uuid::new_v4().to_string(),
            listened: 0,
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
        Self::build(dir, account_url, true)
    }
    pub fn inspect(dir: PathBuf) -> Result<Self> {
        Self::build(dir, url::Url::parse("https://plex.tv/")?, false)
    }
    fn build(dir: PathBuf, account_url: url::Url, downloads: bool) -> Result<Self> {
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
        let namespace = cache::namespace(&settings.server_url, &settings.server_token);
        if downloads && daily::migrate_station_groups(&mut settings, &namespace) {
            store::save(&dir, &settings)?;
        }
        let plex = Plex::new(&settings.client_identifier, account_url)?;
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
            if numeric(&group.key).is_ok() {
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
            catalogue_epoch: 0,
            network_wifi: false,
            network_cellular: false,
            network_online: false,
            network_known: false,
            health: health::Health::default(),
            playback_generation: settings.playback.generation,
            library: offline::Library::new(dir.join("library-cache"), downloads)?,
            artwork,
            offline_mode: false,
            dir,
            settings,
            plex,
            pin: None,
            servers: vec![],
            queue,
            radio,
            cache,
            cached_playback: false,
            manual_cache,
            cache_suspended: !downloads,
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
                let _ = core.cache.schedule_with_quality(
                    &core.settings.server_url,
                    &core.settings.server_token,
                    pending,
                    core.settings.quality.download_kbps,
                );
            }
        }
        Ok(core)
    }
    fn base(&self) -> Result<url::Url> {
        if self.settings.server_url.is_empty() {
            return Err(Error::Input("Connect to a Plex server first"));
        }
        server_url(&self.settings.server_url)
    }
    fn page(&self, path: &str, params: Vec<(&str, String)>, start: usize) -> Result<Value> {
        self.page_sized(path, params, start, 100)
    }
    fn page_sized(
        &self,
        path: &str,
        mut params: Vec<(&str, String)>,
        start: usize,
        size: usize,
    ) -> Result<Value> {
        params.extend([
            ("X-Plex-Container-Start", start.to_string()),
            ("X-Plex-Container-Size", size.to_string()),
        ]);
        let (c, offline) = self.container_cached(path, &params)?;
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
    fn container_cached(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<(model::Container, bool)> {
        self.check_planning()?;
        let ns = self.cache_namespace();
        if self.offline_mode {
            return self
                .library
                .load(&ns, path, params)
                .map(|c| (c, true))
                .ok_or(Error::Input(
                    "This library page has not been saved offline yet",
                ));
        }
        let fetched = self
            .plex
            .container(&self.base()?, &self.settings.server_token, path, params);
        self.cached_response(path, params, fetched)
    }
    /// Save a fetched library page for offline use, or fall back to the saved
    /// copy when the server is unreachable. Returns whether it is the copy.
    fn cached_response(
        &self,
        path: &str,
        params: &[(&str, String)],
        fetched: Result<model::Container>,
    ) -> Result<(model::Container, bool)> {
        let ns = self.cache_namespace();
        match fetched {
            Ok(c) => {
                self.library.save(&ns, path, params, &c);
                Ok((c, false))
            }
            Err(error @ (Error::Network | Error::Http(500..=599))) => self
                .library
                .load(&ns, path, params)
                .map(|c| (c, true))
                .ok_or(error),
            Err(error) => Err(error),
        }
    }
    fn alphabet(&mut self, section: &str) -> Result<Value> {
        numeric(section)?;
        if let Some(index) = self.artist_indexes.get(section) {
            return Ok(index.clone());
        }
        let (c, offline) = self.container_cached(
            &format!("/library/sections/{section}/firstCharacter"),
            &[("type", "8".into()), ("sort", "titleSort:asc".into())],
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
        let base = self.base().ok()?;
        plex::advertised_path(&base, path).ok()?;
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
            server: self.settings.server_url.clone(),
            token: self.settings.server_token.clone(),
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

    fn detail(&self, key: &str, start: usize) -> Result<Value> {
        let id = numeric(key)?;
        if self.offline_mode {
            let known = self.offline_catalogue();
            let saved = self.library.load(
                &self.cache_namespace(),
                &format!("/library/metadata/{id}"),
                &[("includeStations", "1".into())],
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
            let params = [
                ("X-Plex-Container-Start", start.to_string()),
                ("X-Plex-Container-Size", "100".into()),
            ];
            if let Some(c) = self.library.load(
                &self.cache_namespace(),
                &format!("/library/metadata/{id}/children"),
                &params,
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
        if start == 0 {
            let (c, _) = self.container_cached(
                &format!("/library/metadata/{id}"),
                &[("includeStations", "1".into())],
            )?;
            let item = c
                .items
                .iter()
                .find(|i| {
                    i.rating_key == id
                        && matches!(
                            i.kind,
                            crate::model::ItemKind::Artist | crate::model::ItemKind::Album
                        )
                })
                .ok_or(Error::Input("Artist or album details are unavailable"))?;
            metadata = Some(self.display_item(item));
        }
        let mut result = self.page(&format!("/library/metadata/{id}/children"), vec![], start)?;
        if let Some(item) = metadata {
            result["detail"] = item;
        }
        Ok(result)
    }
    fn similar_artists(&self, key: &str) -> Result<Value> {
        let id = numeric(key)?;
        if self.offline_mode {
            return Ok(json!({"similarKey":id,"similarArtists":[],"similarAvailable":false}));
        }
        let c = match self.plex.container(
            &self.base()?,
            &self.settings.server_token,
            &format!("/library/metadata/{id}/similar"),
            &[("count", "12".into())],
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
                    && numeric(&i.rating_key).is_ok()
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
            let key = numeric(&track.rating_key)?;
            let c = self
                .plex
                .container(
                    &self.base()?,
                    &self.settings.server_token,
                    &format!("/library/metadata/{key}"),
                    &[],
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
            let mut url = quality::transcode_url(
                &self.base()?,
                &track.rating_key,
                if kbps > 0 { kbps } else { 192 },
                &uuid::Uuid::new_v4().to_string(),
            )?;
            url.query_pairs_mut()
                .append_pair("X-Plex-Token", &self.settings.server_token);
            url.into()
        } else {
            authenticated_url(&self.base()?, &part.key, &self.settings.server_token)?
        };
        if let Some(current) = self.queue.track() {
            track.play_queue_item_id = current.play_queue_item_id;
            self.queue.replace_track(track.clone());
        }
        self.cached_playback = false;
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
            && (self
                .radio
                .as_ref()
                .is_some_and(|r| r.kind == RadioKind::Album)
                || self
                    .queue
                    .items()
                    .iter()
                    .all(|i| i.parent_rating_key == album))
    }

    fn cache_namespace(&self) -> String {
        cache::namespace(&self.settings.server_url, &self.settings.server_token)
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
    fn schedule_cache(&self) {
        self.cache
            .protect(&self.cache_namespace(), &self.manual_cache);
        if self.settings.cache.enabled && !self.cache_suspended && self.downloads_allowed() {
            // Do not undermine a mobile bitrate cap with automatic original-file
            // prefetch. Explicit pins still follow the selected download policy.
            let ahead = self.settings.cache.ahead + 1;
            let mut skipped = false;
            let mut items = if self.settings.quality.streaming(self.network_cellular) > 0
                && self.settings.quality.download_kbps == 0
            {
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
            if items.is_empty() && skipped {
                // Nothing left to fetch: stop a batch still retrying gone items.
                self.cache.cancel();
            }
            let _ = self.cache.schedule_with_quality(
                &self.settings.server_url,
                &self.settings.server_token,
                items,
                self.settings.quality.download_kbps,
            );
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

    fn album_tracks(&self, album: &Item) -> Result<Vec<Item>> {
        let id = numeric(&album.rating_key)?;
        let items = self.paged_tracks(
            &format!("/library/metadata/{id}/children"),
            0,
            daily::Paging::Album,
        )?;
        if items.is_empty() {
            return Err(Error::Input("This album contains no playable tracks"));
        }
        Ok(items)
    }

    fn nearest(&self, item: &Item) -> Result<Vec<Item>> {
        let id = numeric(&item.rating_key)?;
        let (c, _) = self.container_cached(
            &format!("/library/metadata/{id}/nearest"),
            &[("limit", "50".into())],
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
        let id = numeric(&key)?;
        let base = self.base()?;
        let c = self.plex.container(
            &base,
            &self.settings.server_token,
            &format!("/library/metadata/{id}"),
            &[("includeStations", "1".into())],
        )?;
        let seed = c
            .items
            .into_iter()
            .find(|i| i.rating_key == key && i.kind == kind.item_kind())
            .ok_or(Error::Input(
                "Radio seed does not match the selected music type",
            ))?;
        let title = seed.title.clone();
        let (items, start, source) = if let Some(station) = radio::station_key(&seed, &base)? {
            let identity =
                self.plex
                    .container(&base, &self.settings.server_token, "/identity", &[])?;
            let uri = radio::station_uri(&identity.machine_identifier, &station)?;
            let queue = self
                .plex
                .create_radio_queue(&base, &self.settings.server_token, &uri)?;
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

    fn extend_radio(&mut self) -> Result<()> {
        let request = self.refill_request()?;
        let fetched = self.plex.container(
            &request.base,
            &request.token,
            &request.path,
            &request.params,
        );
        self.refill_apply(&request, fetched).map(|_| ())
    }
    /// What replenishing the radio by one step asks the server.
    fn refill_request(&self) -> Result<RefillRequest> {
        let radio = self.radio.as_ref().ok_or(Error::Input("No active radio"))?;
        let (path, params) = match &radio.source {
            Source::Station { queue_id } => {
                let center = self
                    .station_center()
                    .ok_or(Error::ProtocolAt("radio queue center"))?;
                (
                    format!("/playQueues/{queue_id}"),
                    vec![
                        ("center", center.to_string()),
                        ("window", "50".into()),
                        ("includeBefore", "0".into()),
                        ("includeAfter", "1".into()),
                    ],
                )
            }
            Source::Sonic { seed, .. } => (
                format!("/library/metadata/{}/nearest", numeric(&seed.rating_key)?),
                vec![("limit", "50".into())],
            ),
        };
        Ok(RefillRequest {
            base: self.base()?,
            token: self.settings.server_token.clone(),
            path,
            params,
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
            Source::Sonic { seed, recent } => {
                let (c, _) = self.cached_response(&request.path, &request.params, fetched)?;
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
        (self.station_radio_active() && self.queue.upcoming(12).len() <= 11)
            || self
                .radio
                .as_ref()
                .is_some_and(|radio| radio.kind == RadioKind::Track)
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
        let album = self
            .radio
            .as_ref()
            .is_some_and(|radio| radio.kind == RadioKind::Album);
        let request = if album || !self.refill_wanted() {
            None
        } else {
            self.refill_request().ok()
        };
        let Some(request) = request else {
            if let Some(radio) = &mut self.radio {
                radio.refill.pending = 0;
            }
            return None;
        };
        let plex = Plex::new(
            &self.settings.client_identifier,
            self.plex.account_url.clone(),
        )
        .ok()?;
        Some(RefillJob { request, plex })
    }
    /// Give up an owed refill that could not be fetched, also in a prepared
    /// successor of the same radio, whose commit would otherwise restore it.
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
                for radio in [Some(&mut self.radio), prepared.map(|p| &mut p.radio)]
                    .into_iter()
                    .flatten()
                    .flatten()
                {
                    radio.refill.pending = 0;
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
            if automatic && self.station_radio_active() {
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
            self.queue = previous_queue;
            self.radio = previous_radio;
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
            let mut params = vec![
                (
                    "key",
                    format!("/library/metadata/{}", crate::numeric(&track.rating_key)?),
                ),
                ("ratingKey", track.rating_key.clone()),
            ];
            if let Some(Radio {
                source: Source::Station { queue_id },
                ..
            }) = &self.radio
            {
                params.extend([
                    ("playQueueID", queue_id.to_string()),
                    (
                        "playQueueItemID",
                        track.play_queue_item_id.ok_or(Error::Protocol)?.to_string(),
                    ),
                ]);
            }
            params.extend([
                ("state", state),
                ("time", position.to_string()),
                ("duration", duration.to_string()),
                ("continuing", u8::from(continuing).to_string()),
            ]);
            self.plex
                .timeline(&self.base()?, &self.settings.server_token, &params)?;
            if self.writable {
                for play in &mut self.settings.history {
                    if play.occurrence == self.settings.playback.occurrence {
                        play.delivered = true;
                    }
                }
                store::save_journals(&self.dir, &self.settings)?;
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
    ) -> Option<impl std::future::Future<Output = plex::Probe> + Send + 'static> {
        let base = self.base().ok()?;
        self.plex
            .probe_metadata(&base, &self.settings.server_token, key)
            .ok()
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
    fn connect(&mut self, url: String, token: String) -> Result<Value> {
        let base = server_url(url.trim())?;
        let c = self
            .plex
            .container(&base, &token, "/library/sections", &[])?;
        let mut settings = self.settings.clone();
        settings.server_url = base.to_string();
        settings.server_token = token;
        // Same URL and token address the same cache namespace: its pins,
        // downloaded audio and queue remain valid and are kept.
        let switched = cache::namespace(&settings.server_url, &settings.server_token)
            != self.cache_namespace();
        if switched {
            settings.playback = store::Playback::default();
            settings.download_groups.clear();
            settings.download_titles.clear();
            settings.downloads.clear();
            settings.download_plans.clear();
        }
        store::save(&self.dir, &settings)?;
        self.settings = settings;
        self.library
            .save(&self.cache_namespace(), "/library/sections", &[], &c);
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
        self.offline_mode = false;
        Ok(
            json!({"libraries": c.sections.into_iter().filter(|s| s.kind == crate::model::ItemKind::Artist).collect::<Vec<_>>(), "serverUrl": self.settings.server_url,"radio":self.radio_summary(),"cache":self.cache_status(),"offlineMode":false,"offline":false}),
        )
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
            } => self.edit_playlist(action, &key, &title, items, item_id, after),
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
                numeric(&key)?;
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
                            self.bounded_tracks(&format!("/playlists/{key}/items"), 0)?
                        }
                        _ => {
                            let (c, _) =
                                self.container_cached(&format!("/library/metadata/{key}"), &[])?;
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
                            self.artwork.schedule(
                                &self.settings.server_url,
                                &self.settings.server_token,
                                path,
                            );
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
                let id = numeric(&key)?;
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
                let (c, _) = self.container_cached(&format!("/library/metadata/{id}"), &[])?;
                let stream = c
                    .items
                    .iter()
                    .filter(|i| i.kind == crate::model::ItemKind::Track && i.rating_key == id)
                    .flat_map(|i| &i.media)
                    .flat_map(|m| &m.parts)
                    .flat_map(|p| &p.streams)
                    .find(|s| s.stream_type == 4 && !s.key.is_empty());
                let lines = if let Some(stream) = stream {
                    lyrics::decode(&self.plex.text(
                        &self.base()?,
                        &self.settings.server_token,
                        &stream.key,
                    )?)?
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
                let (c, _) = self.container_cached(
                    "/hubs/search",
                    &[("query", query), ("limit", "100".into())],
                )?;
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
                        ) && numeric(&i.rating_key).is_ok()
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
                let section = numeric(&section)?;
                let params = match view {
                    CollectionView::Favorites => vec![
                        ("type", "10".into()),
                        ("userRating", "10".into()),
                        ("sort", "titleSort:asc".into()),
                    ],
                    CollectionView::Added => {
                        vec![("type", "9".into()), ("sort", "addedAt:desc".into())]
                    }
                    CollectionView::Played => vec![
                        ("type", "10".into()),
                        ("sort", "lastViewedAt:desc".into()),
                        ("viewCount>>", "0".into()),
                    ],
                };
                self.page(&format!("/library/sections/{section}/all"), params, start)
            }
            Command::Favorite { key, enabled } => {
                if self.offline_mode {
                    return Err(Error::Input("Go online"));
                }
                self.plex.rate(
                    &self.base()?,
                    &self.settings.server_token,
                    numeric(&key)?,
                    if enabled { 10 } else { 0 },
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
                json!({"signedIn": !self.settings.account_token.is_empty(), "serverUrl": self.settings.server_url, "queue": self.queue,"radio":self.radio_summary(),"cache":self.cache_status(),"track":self.queue.track().map(|i|self.display_item(i)),"resumePosition":self.resume_position,"playbackGeneration":self.playback_generation,"playbackOccurrence":self.settings.playback.occurrence,"resumeListened":self.settings.playback.listened,"autoplay":self.settings.autoplay,"history":self.history_status(),"audioConfig":self.settings.audio,"qualityConfig":self.settings.quality}),
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
                        i.kind != crate::model::ItemKind::Track || numeric(&i.rating_key).is_err()
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
                let pin = self.plex.pin()?;
                let query = url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("clientID", &self.settings.client_identifier)
                    .append_pair("code", &pin.code)
                    .append_pair("context[device][product]", "PlexFreq")
                    .finish();
                let result = json!({"url": format!("https://app.plex.tv/auth#?{query}"), "expiresIn": pin.expires_in});
                self.pin = Some((pin, Instant::now()));
                Ok(result)
            }
            Command::PollLogin => {
                let (pin, created) = self.pin.as_ref().ok_or(Error::Input("Start login first"))?;
                if created.elapsed() >= Duration::from_secs(pin.expires_in) {
                    self.pin = None;
                    return Err(Error::Input("Login expired; start again"));
                }
                let pin = self.plex.poll(pin.id, &pin.code)?;
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
                self.servers = self.plex.resources(&self.settings.account_token)?;
                Ok(
                    json!({"servers": self.servers.iter().map(|s| json!({"name": s.name, "id": s.client_identifier})).collect::<Vec<_>>()}),
                )
            }
            Command::Connect { url, token } => self.connect(url, token),
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
                let (c, offline) = self.container_cached("/library/sections", &[])?;
                if offline {
                    self.offline_mode = true;
                }
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
                let section = numeric(&section)?;
                let kind = model::music_kind_code(&kind)?;
                let mut params = vec![("type", kind.into()), ("sort", "titleSort:asc".into())];
                if !query.is_empty() {
                    params.push(("title", query));
                }
                self.page(&format!("/library/sections/{section}/all"), params, start)
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
                    &format!("/library/sections/{}/all", numeric(&section)?),
                    vec![("type", "8".into()), ("sort", "titleSort:asc".into())],
                    0,
                    size,
                )?;
                page["replaceItems"] = json!(true);
                page["selectedLetter"] = json!(letter);
                page["selectedIndex"] = json!(start);
                Ok(page)
            }
            Command::Children { key, start } => {
                // Item keys vary: playlists use /playlists/N/items; artists/albums use metadata children.
                let path = if let Some(id) = key
                    .strip_prefix("/playlists/")
                    .and_then(|rest| rest.strip_suffix("/items"))
                {
                    format!("/playlists/{}/items", numeric(id)?)
                } else {
                    let id = key.strip_prefix("/library/metadata/").unwrap_or(&key);
                    let id = id.strip_suffix("/children").unwrap_or(id);
                    format!("/library/metadata/{}/children", numeric(id)?)
                };
                self.page(&path, vec![], start)
            }
            Command::Detail { key, start } => self.detail(&key, start),
            Command::SimilarArtists { key } => self.similar_artists(&key),
            Command::Playlists { start } => {
                self.page("/playlists", vec![("playlistType", "audio".into())], start)
            }
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
                let settings = store::Settings {
                    client_identifier: self.settings.client_identifier.clone(),
                    ..Default::default()
                };
                store::save(&self.dir, &settings)?;
                self.settings = settings;
                self.pin = None;
                self.servers.clear();
                self.queue = Queue::default();
                self.radio = None;
                self.resume_position = 0;
                self.cache.clear()?;
                self.manual_cache.clear();
                self.artist_indexes.clear();
                self.library.clear()?;
                self.artwork.clear()?;
                Ok(
                    json!({"signedIn": false, "serverUrl": "", "queue": self.queue, "radio":null, "track": null, "stream": "","history":self.history_status(),"cache":self.cache_status(),"autoplay":false}),
                )
            }
        }
    }
}
