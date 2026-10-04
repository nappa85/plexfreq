pub mod audio;
pub mod cache;
mod features;
mod ffi;
pub mod history;
pub mod lyrics;
pub mod model;
pub mod offline;
pub mod plex;
pub mod queue;
pub mod radio;
pub mod runtime;
pub mod store;

use model::{Item, Pin, Resource};
use plex::{authenticated_url, server_url, Plex};
use queue::{Queue, Repeat};
use radio::{Radio, RadioKind, Source};
use serde::Deserialize;
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

/// Lock a mutex, recovering from poisoning instead of panicking worker threads.
/// A poisoned mutex means another thread panicked while holding it; the data
/// itself is still usable, so continue with it rather than crashing audio,
/// cache or request workers.
pub(crate) fn mutex_lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Shared atomic file write: temp file with 0600, fsync, rename. Used for
/// cache index, offline snapshots and session state (session adds dir fsync).
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

pub(crate) fn numeric(value: &str) -> Result<&str> {
    if value.is_empty() || !value.bytes().all(|c| c.is_ascii_digit()) {
        Err(Error::Input("Expected a numeric Plex identifier"))
    } else {
        Ok(value)
    }
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Command {
    Status,
    PlaylistChoices,
    PlaylistEdit {
        action: String,
        #[serde(default)]
        key: String,
        #[serde(default)]
        title: String,
        #[serde(default)]
        items: Vec<Item>,
        #[serde(default)]
        item_id: Option<u64>,
        #[serde(default)]
        after: Option<u64>,
    },
    PlaylistItems {
        key: String,
        #[serde(default)]
        start: usize,
    },
    LibraryBrowse {
        section: String,
        kind: String,
        sort: String,
        #[serde(default)]
        start: usize,
    },
    ArtistAlbums {
        key: String,
    },
    Downloads,
    DownloadPolicy {
        wifi_only: bool,
        paused: bool,
    },
    DownloadAction {
        group: String,
        action: String,
    },
    NetworkState {
        wifi: bool,
        online: bool,
        #[serde(default)]
        live_hint: bool,
    },
    Autoplay {
        enabled: bool,
    },
    Mix {
        seeds: Vec<Item>,
    },
    PlaybackEvent {
        key: String,
        occurrence: String,
        listened: u64,
        duration: u64,
    },
    SyncHistory,
    OfflineMode {
        enabled: bool,
    },
    OfflineBrowse {
        kind: String,
    },
    CacheArtwork {
        items: Vec<Item>,
    },
    Pin {
        key: String,
        kind: String,
        enabled: bool,
    },
    Lyrics {
        key: String,
    },
    Search {
        query: String,
    },
    Collection {
        section: String,
        view: String,
        #[serde(default)]
        start: usize,
    },
    Favorite {
        key: String,
        enabled: bool,
    },
    Login,
    PollLogin,
    Servers,
    Connect {
        url: String,
        token: String,
    },
    SelectServer {
        index: usize,
    },
    Libraries,
    Browse {
        section: String,
        kind: String,
        #[serde(default)]
        query: String,
        #[serde(default)]
        start: usize,
    },
    Alphabet {
        section: String,
    },
    JumpArtist {
        section: String,
        letter: String,
    },
    Children {
        key: String,
        #[serde(default)]
        start: usize,
    },
    Detail {
        key: String,
        #[serde(default)]
        start: usize,
    },
    SimilarArtists {
        key: String,
    },
    Playlists {
        #[serde(default)]
        start: usize,
    },
    Play {
        items: Vec<Item>,
        index: usize,
    },
    Enqueue {
        items: Vec<Item>,
        #[serde(default)]
        next: bool,
    },
    EnqueueAlbum {
        key: String,
        #[serde(default)]
        next: bool,
    },
    PlayAlbum {
        key: String,
    },
    QueueRemove {
        index: usize,
    },
    QueueMove {
        from: usize,
        to: usize,
    },
    Resume,
    SavePlayback {
        key: String,
        position: u64,
        #[serde(default)]
        generation: Option<u64>,
    },
    Radio {
        key: String,
        kind: RadioKind,
    },
    StopRadio,
    Timeline {
        state: String,
        position: u64,
        duration: u64,
        #[serde(default)]
        continuing: bool,
    },
    CacheStatus,
    CacheConfig {
        enabled: bool,
        limit_mb: u64,
        ahead: usize,
    },
    CacheTracks {
        items: Vec<Item>,
    },
    CachedTracks,
    CachedPlayback,
    ClearCache,
    SelectTrack {
        index: usize,
    },
    Next {
        #[serde(default)]
        automatic: bool,
    },
    Previous,
    Shuffle {
        enabled: bool,
    },
    Repeat {
        mode: String,
    },
    Logout,
}

pub struct Core {
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
    network_online: bool,
}

pub(crate) struct Prepared {
    queue: Queue,
    radio: Option<Radio>,
    cached: bool,
    pub plan: Value,
}

impl Core {
    pub(crate) fn prepare_next(&mut self) -> Result<Option<Prepared>> {
        if self.queue.track().is_none()
            || self.queue.at_end()
                && self.radio.is_none()
                && !self.settings.autoplay
                && self.queue.repeat == Repeat::Off
        {
            return Ok(None);
        }
        let old = self.queue.clone();
        let radio = self.radio.clone();
        let cached = self.cached_playback;
        let result = self.advance_next(true, false);
        let queue = std::mem::replace(&mut self.queue, old);
        let next_radio = std::mem::replace(&mut self.radio, radio);
        let next_cached = std::mem::replace(&mut self.cached_playback, cached);
        let prepared = match result {
            Ok(mut plan) if queue.track().is_some() => {
                let q = queue.presentation();
                plan["queue"] = json!({"items":q.items.iter().map(|i|self.display_item(i)).collect::<Vec<_>>(),"current":q.current,"repeat":q.repeat,"shuffled":q.shuffled});
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
        let mut settings = store::load(&dir)?;
        if settings.client_identifier.is_empty() {
            settings.client_identifier = uuid::Uuid::new_v4().to_string();
            store::save(&dir, &settings)?;
        }
        let plex = Plex::new(&settings.client_identifier, account_url)?;
        if settings.playback.queue.validate().is_err() {
            settings.playback = store::Playback::default();
        }
        if settings.playback.queue.current.is_some() && settings.playback.occurrence.is_empty() {
            settings.playback.occurrence = uuid::Uuid::new_v4().to_string();
        }
        let queue = settings.playback.queue.clone();
        let radio = settings.playback.radio.clone();
        let resume_position = settings.playback.position;
        let manual_cache = settings.downloads.clone();
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
            network_wifi: false,
            network_online: false,
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
                let _ = core.cache.schedule(
                    &core.settings.server_url,
                    &core.settings.server_token,
                    pending,
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
    fn page(&self, path: &str, mut params: Vec<(&str, String)>, start: usize) -> Result<Value> {
        params.extend([
            ("X-Plex-Container-Start", start.to_string()),
            ("X-Plex-Container-Size", "100".into()),
        ]);
        let (c, offline) = self.container_cached(path, &params)?;
        let items: Vec<Value> = c
            .items
            .iter()
            .filter(|i| matches!(i.kind.as_str(), "artist" | "album" | "track" | "playlist"))
            .map(|i| self.display_item(i))
            .collect();
        let next = start + c.size;
        Ok(json!({"items": items, "start": start, "next": next,
            "hasMore": c.total_size.map_or(c.size == 100, |total| next < total),"offline":offline}))
    }
    fn container_cached(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<(model::Container, bool)> {
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
        match self
            .plex
            .container(&self.base()?, &self.settings.server_token, path, params)
        {
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
        let c = self.plex.container(
            &self.base()?,
            &self.settings.server_token,
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
        self.artist_indexes.insert(section.into(), result.clone());
        Ok(result)
    }
    fn display_item(&self, item: &Item) -> Value {
        let mut value = serde_json::to_value(item).unwrap_or_else(|_| serde_json::json!({}));
        let thumb = if item.thumb.is_empty() {
            &item.parent_thumb
        } else {
            &item.thumb
        };
        let artwork = if thumb.is_empty()
            || self.offline_mode && self.artwork.local(&self.cache_namespace(), thumb).is_none()
        {
            String::new()
        } else {
            if let Some(local) = self.artwork.local(&self.cache_namespace(), thumb) {
                local
            } else {
                self.base()
                    .and_then(|base| authenticated_url(&base, thumb, &self.settings.server_token))
                    .unwrap_or_default()
            }
        };
        value["artwork"] = json!(artwork);
        if item.kind == "track" {
            if let Some(object) = value.as_object_mut() {
                object.remove("playlistItemID");
            }
            value["playlistItemId"] = json!(item.playlist_item_id);
        }
        if item.kind == "playlist" {
            value["totalDuration"] = json!(if item.duration > 0 || item.leaf_count == Some(0) {
                Some(item.duration)
            } else {
                item.duration_in_seconds
                    .map(|seconds| seconds.saturating_mul(1000))
            });
        }
        let image_url = |path: &str| {
            if path.is_empty() {
                return None;
            }
            if let Some(local) = self.artwork.local(&self.cache_namespace(), path) {
                return Some(local);
            }
            if self.offline_mode {
                return None;
            }
            self.base()
                .and_then(|base| authenticated_url(&base, path, &self.settings.server_token))
                .ok()
        };
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
        if item.kind == "album" {
            value["albumType"] = json!(features::album_type(item));
        }
        if item.kind == "track" {
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
            let mut known = self.library.items(&self.cache_namespace());
            known.extend(self.cache.items(&self.cache_namespace()));
            let saved = self.library.load(
                &self.cache_namespace(),
                &format!("/library/metadata/{id}"),
                &[("includeStations", "1".into())],
            );
            let item = saved
                .as_ref()
                .and_then(|c| {
                    c.items.iter().find(|i| {
                        i.rating_key == id && matches!(i.kind.as_str(), "artist" | "album")
                    })
                })
                .cloned()
                .or_else(|| {
                    known
                        .iter()
                        .find(|i| {
                            i.rating_key == id && matches!(i.kind.as_str(), "artist" | "album")
                        })
                        .cloned()
                })
                .or_else(|| {
                    known
                        .iter()
                        .find(|i| i.kind == "track" && i.parent_rating_key == id)
                        .map(|i| Item {
                            rating_key: id.into(),
                            kind: "album".into(),
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
                        && (item.kind == "artist" && i.kind == "album"
                            || item.kind == "album" && i.kind == "track")
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
                let next = start + c.size;
                let has_more = c.total_size.map_or(c.size == 100, |total| next < total);
                let mut value = json!({"items":c.items.iter().map(|i|self.display_item(i)).collect::<Vec<_>>(),"start":start,"next":next,"hasMore":has_more,"offline":true});
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
            let next = start.saturating_add(page_items.len());
            let has_more = next < total;
            let mut value = json!({"items":page_items,"start":start,"next":next,"hasMore":has_more,"offline":true});
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
                .find(|i| i.rating_key == id && matches!(i.kind.as_str(), "artist" | "album"))
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
                i.kind == "artist"
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
        let Some(mut track) = self.queue.track().cloned() else {
            return Ok(
                json!({"queue": self.queue, "radio": self.radio_summary(), "track": null, "stream": ""}),
            );
        };
        if let Some((path, mut cached)) = self.cache.lookup(&self.cache_namespace(), &track, true) {
            cached.play_queue_item_id = track.play_queue_item_id;
            if let Some(index) = self.queue.current {
                self.queue.items[index] = cached.clone();
            }
            self.cached_playback = true;
            if schedule {
                self.schedule_cache();
            }
            let stream = url::Url::from_file_path(path)
                .map_err(|_| Error::Input("Invalid cached audio path"))?
                .to_string();
            return Ok(
                json!({"queue":self.queue,"radio":self.radio_summary(),"track":self.display_item(&cached),"stream":stream,"playbackSource":"cache","cache":self.cache_status()}),
            );
        }
        if track.media.first().and_then(|m| m.parts.first()).is_none() {
            let key = numeric(&track.rating_key)?;
            let c = self.plex.container(
                &self.base()?,
                &self.settings.server_token,
                &format!("/library/metadata/{key}"),
                &[],
            )?;
            track = c
                .items
                .into_iter()
                .find(|i| i.kind == "track")
                .ok_or(Error::Protocol)?;
        }
        let part = track
            .media
            .first()
            .and_then(|m| m.parts.first())
            .ok_or(Error::Input("This track has no playable media part"))?;
        let stream = authenticated_url(&self.base()?, &part.key, &self.settings.server_token)?;
        if let Some(index) = self.queue.current {
            track.play_queue_item_id = self.queue.items[index].play_queue_item_id;
            self.queue.items[index] = track.clone();
        }
        self.cached_playback = false;
        if schedule {
            self.schedule_cache();
        }
        Ok(
            json!({"queue": self.queue, "radio":self.radio_summary(), "track": self.display_item(&track), "stream": stream,"playbackSource":"stream","cache":self.cache_status()}),
        )
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
        value["jobs"] = json!(self.download_rows_from_status(&value));
        value
    }
    fn schedule_cache(&self) {
        self.cache
            .protect(&self.cache_namespace(), &self.manual_cache);
        if self.settings.cache.enabled && !self.cache_suspended && self.downloads_allowed() {
            let mut items = self.queue.upcoming(self.settings.cache.ahead + 1);
            for item in &self.manual_cache {
                if !items.iter().any(|i| i.rating_key == item.rating_key) {
                    items.push(item.clone());
                }
            }
            let _ = self.cache.schedule(
                &self.settings.server_url,
                &self.settings.server_token,
                items,
            );
        }
    }

    fn radio_summary(&self) -> Value {
        self.radio.as_ref().map_or(Value::Null, Radio::summary)
    }

    fn album_tracks(&self, album: &Item) -> Result<Vec<Item>> {
        let id = numeric(&album.rating_key)?;
        let mut items = Vec::new();
        loop {
            let start = items.len();
            let (c, _) = self.container_cached(
                &format!("/library/metadata/{id}/children"),
                &[
                    ("X-Plex-Container-Start", start.to_string()),
                    ("X-Plex-Container-Size", "100".into()),
                ],
            )?;
            let count = c.items.len();
            if c.items.iter().any(|i| i.kind != "track") {
                return Err(Error::ProtocolAt("album radio tracks"));
            }
            items.extend(c.items);
            if items.len() > 1000 {
                return Err(Error::Input("Album exceeds the radio track limit"));
            }
            if count == 0
                || c.total_size
                    .map_or(count < 100, |total| items.len() >= total)
            {
                break;
            }
        }
        if items.is_empty() {
            return Err(Error::Input("This album contains no playable tracks"));
        }
        Ok(items)
    }

    fn nearest(&self, item: &Item) -> Result<Vec<Item>> {
        let id = numeric(&item.rating_key)?;
        let c = self.plex.container(
            &self.base()?,
            &self.settings.server_token,
            &format!("/library/metadata/{id}/nearest"),
            &[("limit", "50".into())],
        )?;
        Ok(c.items
            .into_iter()
            .filter(|i| i.kind == item.kind && i.rating_key != item.rating_key)
            .collect())
    }

    fn start_radio(&mut self, key: String, kind: RadioKind) -> Result<Value> {
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
            .find(|i| i.rating_key == key && i.kind == kind.item_type())
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
                .any(|i| i.kind != "track" || i.play_queue_item_id.is_none())
            {
                return Err(Error::ProtocolAt("radio queue tracks"));
            }
            let start = queue
                .items
                .iter()
                .position(|i| i.play_queue_item_id == queue.play_queue_selected_item_id)
                .unwrap_or(0);
            (queue.items, start, Source::Station { queue_id })
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
        let previous_radio = self.radio.replace(Radio {
            kind,
            title,
            source,
        });
        let result = self.playback();
        if result.is_err() {
            self.queue = previous_queue;
            self.radio = previous_radio;
        }
        result
    }

    fn extend_radio(&mut self) -> Result<()> {
        let mut radio = self.radio.clone().ok_or(Error::Input("No active radio"))?;
        let items = match &mut radio.source {
            Source::Station { queue_id } => {
                let center = self
                    .queue
                    .track()
                    .and_then(|i| i.play_queue_item_id)
                    .ok_or(Error::ProtocolAt("radio queue center"))?;
                let c = self.plex.container(
                    &self.base()?,
                    &self.settings.server_token,
                    &format!("/playQueues/{queue_id}"),
                    &[
                        ("center", center.to_string()),
                        ("window", "50".into()),
                        ("includeBefore", "0".into()),
                        ("includeAfter", "1".into()),
                    ],
                )?;
                if c.play_queue_id.is_some_and(|id| id != *queue_id) {
                    return Err(Error::ProtocolAt("radio queue continuation"));
                }
                if c.items
                    .iter()
                    .any(|i| i.kind != "track" || i.play_queue_item_id.is_none())
                {
                    return Err(Error::ProtocolAt("radio queue continuation"));
                }
                c.items
                    .into_iter()
                    .filter(|i| {
                        !self
                            .queue
                            .items
                            .iter()
                            .any(|known| known.play_queue_item_id == i.play_queue_item_id)
                    })
                    .collect()
            }
            Source::Sonic { seed, recent } => {
                let neighbors = self.nearest(seed)?;
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
        self.queue.append(items)?;
        self.radio = Some(radio);
        Ok(())
    }

    fn next(&mut self, automatic: bool) -> Result<Value> {
        let previous_queue = self.queue.clone();
        let previous_radio = self.radio.clone();
        let result = self.advance_next(automatic, true);
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
            && self.queue.repeat == Repeat::Off
            && !self.offline_mode
        {
            if let Some(seed) = self.queue.track().cloned() {
                if let Ok(neighbors) = self.nearest(&seed) {
                    let known: std::collections::BTreeSet<_> = self
                        .queue
                        .items
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
        if self.radio.is_some() && self.queue.at_end() {
            self.extend_radio()?;
        }
        self.queue.advance(automatic);
        if self.radio.is_some() {
            self.queue.trim_history(100);
        }
        self.playback_with_cache(schedule)
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
        if self.cached_playback || self.offline_mode {
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
                store::save(&self.dir, &self.settings)?;
            }
        }
        Ok(json!({}))
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
        settings.playback = store::Playback::default();
        settings.download_groups.clear();
        settings.download_titles.clear();
        settings.downloads.clear();
        store::save(&self.dir, &settings)?;
        self.settings = settings;
        self.library
            .save(&self.cache_namespace(), "/library/sections", &[], &c);
        self.queue = Queue::default();
        self.radio = None;
        self.cache.cancel();
        self.artwork.cancel();
        self.manual_cache.clear();
        self.artist_indexes.clear();
        self.offline_mode = false;
        Ok(
            json!({"libraries": c.sections.into_iter().filter(|s| s.kind == "artist").collect::<Vec<_>>(), "serverUrl": self.settings.server_url,"radio":null,"cache":self.cache_status(),"offlineMode":false,"offline":false}),
        )
    }
    pub fn execute(&mut self, command: Command) -> Result<Value> {
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
        );
        let resuming = matches!(&command, Command::Resume | Command::CachedPlayback);
        let mut result = self.execute_inner(command)?;
        if result.get("stream").is_some() {
            self.playback_generation = self.playback_generation.wrapping_add(1);
            result["playbackGeneration"] = json!(self.playback_generation);
        }
        if result.get("stream").is_some() && !resuming {
            self.resume_position = 0;
            self.settings.playback.occurrence = uuid::Uuid::new_v4().to_string();
            self.settings.playback.listened = 0;
        }
        if result.get("stream").is_some() {
            result["playbackOccurrence"] = json!(self.settings.playback.occurrence);
            result["resumeListened"] = json!(self.settings.playback.listened);
        }
        if persist && self.writable {
            self.settings.playback = store::Playback {
                queue: self.queue.clone(),
                radio: self.radio.clone(),
                position: self.resume_position,
                generation: self.playback_generation,
                occurrence: self.settings.playback.occurrence.clone(),
                listened: self.settings.playback.listened,
            };
            self.settings.downloads = self.manual_cache.clone();
            store::save(&self.dir, &self.settings)?;
        }
        if result.get("queue").is_some() {
            let q = self.queue.presentation();
            result["queue"] = json!({"items":q.items.iter().map(|i|self.display_item(i)).collect::<Vec<_>>(),"current":q.current,"repeat":q.repeat,"shuffled":q.shuffled});
        }
        Ok(result)
    }
    pub fn download_control(&self) -> cache::DownloadControl {
        self.cache.control(self.artwork.generation())
    }
    fn execute_inner(&mut self, command: Command) -> Result<Value> {
        match command {
            Command::PlaylistChoices => self.playlist_choices(),
            Command::PlaylistEdit {
                action,
                key,
                title,
                items,
                item_id,
                after,
            } => self.edit_playlist(&action, &key, &title, items, item_id, after),
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
                self.cache.cancel();
                self.artwork.cancel();
                self.schedule_cache();
                Ok(json!({"cache":self.cache_status(),"downloads":self.download_rows()}))
            }
            Command::DownloadAction { group, action } => self.download_action(&group, &action),
            Command::NetworkState {
                wifi,
                online,
                live_hint,
            } => {
                if !live_hint {
                    self.download_control().network(wifi);
                }
                let changed = wifi != self.network_wifi || online != self.network_online;
                self.network_wifi = wifi;
                self.network_online = online;
                if changed {
                    self.cache.cancel();
                    if !self.downloads_allowed() {
                        self.artwork.cancel();
                    }
                    self.schedule_cache();
                }
                Ok(json!({"cache":self.cache_status(),"networkOnline":online,"networkWifi":wifi}))
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
                if !matches!(kind.as_str(), "album" | "track" | "playlist") {
                    return Err(Error::Input("Choose a track, album or playlist to pin"));
                }
                let group = format!("{kind}:{key}");
                if enabled {
                    if !self.settings.cache.enabled {
                        return Err(Error::Input(
                            "Enable audio caching before pinning downloads",
                        ));
                    }
                    let items = match kind.as_str() {
                        "album" => self.album_tracks(&Item {
                            rating_key: key.clone(),
                            ..Default::default()
                        })?,
                        "playlist" => {
                            let mut tracks = Vec::new();
                            loop {
                                let start = tracks.len();
                                let (c, _) = self.container_cached(
                                    &format!("/playlists/{key}/items"),
                                    &[
                                        ("X-Plex-Container-Start", start.to_string()),
                                        ("X-Plex-Container-Size", "100".into()),
                                    ],
                                )?;
                                let count = c.items.len();
                                if c.items.iter().any(|i| i.kind != "track") {
                                    return Err(Error::Input(
                                        "Only audio playlists can be downloaded",
                                    ));
                                }
                                tracks.extend(c.items.into_iter().filter(|i| i.kind == "track"));
                                if count < 100
                                    || c.total_size.is_some_and(|total| start + count >= total)
                                {
                                    break;
                                }
                                if tracks.len() >= 1000 {
                                    return Err(Error::Input("Download plan exceeds 1000 tracks"));
                                }
                            }
                            tracks
                        }
                        _ => {
                            let (c, _) =
                                self.container_cached(&format!("/library/metadata/{key}"), &[])?;
                            c.items.into_iter().filter(|i| i.kind == "track").collect()
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
                    let keep: std::collections::BTreeSet<_> =
                        groups.values().flatten().cloned().collect();
                    plan.retain(|i| keep.contains(&i.rating_key));
                    if plan.len() > 1000 || items.is_empty() {
                        return Err(Error::Input("Pin up to 1000 playable tracks"));
                    }
                    let title = if kind == "playlist" {
                        self.playlist_metadata(&key)?.title
                    } else {
                        items
                            .first()
                            .map(|i| {
                                if kind == "track" {
                                    i.title.clone()
                                } else {
                                    i.parent_title.clone()
                                }
                            })
                            .unwrap_or_default()
                    };
                    self.settings
                        .download_titles
                        .insert(format!("{kind}:{key}"), title);
                    self.settings.download_groups = groups;
                    self.manual_cache = plan;
                } else {
                    self.settings.download_groups.remove(&group);
                    self.settings.download_titles.remove(&group);
                    let keep: std::collections::BTreeSet<_> = self
                        .settings
                        .download_groups
                        .values()
                        .flatten()
                        .cloned()
                        .collect();
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
                    self.cache.cancel();
                    self.artwork.cancel();
                } else {
                    self.schedule_cache();
                }
                Ok(json!({"offlineMode":enabled}))
            }
            Command::OfflineBrowse { kind } => {
                if !matches!(kind.as_str(), "artist" | "album" | "track") {
                    return Err(Error::Input("Unknown offline music type"));
                }
                let mut items = self.library.items(&self.cache_namespace());
                items.extend(self.cache.items(&self.cache_namespace()));
                let mut seen = std::collections::BTreeSet::new();
                items.retain(|i| i.kind == kind && seen.insert(i.rating_key.clone()));
                items.sort_by_cached_key(|i| i.title.to_lowercase());
                Ok(
                    json!({"items":items.iter().map(|i|self.display_item(i)).collect::<Vec<_>>(),"start":0,"next":0,"hasMore":false,"offline":true}),
                )
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
                    .filter(|i| i.kind == "track" && i.rating_key == id)
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
                    let query = query.to_lowercase();
                    let items: Vec<_> = self
                        .library
                        .items(&self.cache_namespace())
                        .iter()
                        .filter(|i| {
                            format!("{} {} {}", i.title, i.parent_title, i.grandparent_title)
                                .to_lowercase()
                                .contains(&query)
                        })
                        .map(|i| self.display_item(i))
                        .collect();
                    return Ok(
                        json!({"items":items,"start":0,"next":0,"hasMore":false,"offline":true}),
                    );
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
                        matches!(i.kind.as_str(), "artist" | "album" | "track")
                            && numeric(&i.rating_key).is_ok()
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
                let params = match view.as_str() {
                    "favorites" => vec![
                        ("type", "10".into()),
                        ("userRating", "10".into()),
                        ("sort", "titleSort:asc".into()),
                    ],
                    "added" => vec![("type", "9".into()), ("sort", "addedAt:desc".into())],
                    "played" => vec![
                        ("type", "10".into()),
                        ("sort", "lastViewedAt:desc".into()),
                        ("viewCount>>", "0".into()),
                    ],
                    _ => return Err(Error::Input("Unknown discovery view")),
                };
                self.page(&format!("/library/sections/{section}/all"), params, start)
            }
            Command::Favorite { key, enabled } => {
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
                for item in &mut self.queue.items {
                    if item.rating_key == key {
                        item.user_rating = if enabled { 10. } else { 0. };
                    }
                }
                Ok(
                    json!({"ratedKey":key,"userRating":if enabled{10}else{0},"queue":self.queue,"track":self.queue.track().map(|i|self.display_item(i))}),
                )
            }
            Command::Status => Ok(
                json!({"signedIn": !self.settings.account_token.is_empty(), "serverUrl": self.settings.server_url, "queue": self.queue,"radio":self.radio_summary(),"cache":self.cache_status(),"track":self.queue.track().map(|i|self.display_item(i)),"resumePosition":self.resume_position,"playbackGeneration":self.playback_generation,"playbackOccurrence":self.settings.playback.occurrence,"resumeListened":self.settings.playback.listened,"autoplay":self.settings.autoplay,"history":self.history_status(),"audioConfig":self.settings.audio}),
            ),
            Command::Resume => {
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
                    || items
                        .iter()
                        .any(|i| i.kind != "track" || numeric(&i.rating_key).is_err())
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
                        format!("track:{}", item.rating_key),
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
                self.cache.clear()?;
                self.manual_cache.clear();
                self.settings.download_groups.clear();
                self.queue = Queue::default();
                self.radio = None;
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
                    json!({"libraries": c.sections.into_iter().filter(|s| s.kind == "artist").collect::<Vec<_>>(),"offline":offline,"offlineMode":self.offline_mode}),
                )
            }
            Command::Browse {
                section,
                kind,
                query,
                start,
            } => {
                let section = numeric(&section)?;
                let kind = match kind.as_str() {
                    "artist" => "8",
                    "album" => "9",
                    "track" => "10",
                    _ => return Err(Error::Input("Unknown music type")),
                };
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
                let mut page = self.page(
                    &format!("/library/sections/{}/all", numeric(&section)?),
                    vec![("type", "8".into()), ("sort", "titleSort:asc".into())],
                    start,
                )?;
                page["replaceItems"] = json!(true);
                page["selectedLetter"] = json!(letter);
                Ok(page)
            }
            Command::Children { key, start } => {
                // Item keys vary: playlists use /playlists/N/items; artists/albums use metadata children.
                let path = if key.starts_with("/playlists/") && key.ends_with("/items") {
                    numeric(
                        key.trim_start_matches("/playlists/")
                            .trim_end_matches("/items"),
                    )?;
                    key
                } else {
                    let id = key
                        .trim_start_matches("/library/metadata/")
                        .trim_end_matches("/children");
                    numeric(id)?;
                    format!("/library/metadata/{id}/children")
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
                self.change_playback(|queue| queue.select(index))
            }
            Command::Next { automatic } => self.next(automatic),
            Command::Previous => self.change_playback(|queue| {
                queue.previous();
                Ok(())
            }),
            Command::Shuffle { enabled } => {
                if self.radio.is_some() {
                    return Err(Error::Input(
                        "Radio keeps its recommended order; stop radio to shuffle",
                    ));
                }
                self.queue.shuffle(enabled);
                Ok(json!({"queue": self.queue}))
            }
            Command::Repeat { mode } => {
                if self.radio.is_some() {
                    return Err(Error::Input(
                        "Radio continues automatically; stop radio to repeat",
                    ));
                }
                self.queue.repeat = match mode.as_str() {
                    "off" => Repeat::Off,
                    "all" => Repeat::All,
                    "one" => Repeat::One,
                    _ => return Err(Error::Input("Unknown repeat mode")),
                };
                Ok(json!({"queue": self.queue}))
            }
            Command::Logout => {
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
