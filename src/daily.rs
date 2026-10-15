//! Daily-use discovery and durable bounded offline plans. No presentation policy.
use crate::{
    model::{Container, Item},
    radio, Command, Core, Error, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Purpose of a bounded track listing; selects offline policy and messages.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Paging {
    /// Durable offline plan: requires fresh server pages.
    Download,
    /// Album contents for playback/queueing/pinning: saved pages are fine.
    Album,
}
impl Paging {
    fn not_track(self) -> Error {
        match self {
            Self::Download => Error::Input("Only audio playlists can be downloaded"),
            Self::Album => Error::ProtocolAt("album radio tracks"),
        }
    }
    fn too_many(self) -> Error {
        match self {
            Self::Download => Error::Input("Download plan exceeds 1000 tracks"),
            Self::Album => Error::Input("Add up to 1000 tracks at a time"),
        }
    }
}

/// Every rating key referenced by any download group.
pub(crate) fn pinned_keys(groups: &BTreeMap<GroupId, Vec<String>>) -> BTreeSet<String> {
    groups.values().flatten().cloned().collect()
}

/// What a pinned download group or durable download plan was made from. The
/// variants are in alphabetical order so `GroupId`'s derived ordering equals
/// the ordering of its string form (persisted maps iterate unchanged).
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Deserialize,
    Serialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
    strum::EnumIter,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum DownloadKind {
    Album,
    AlbumRadio,
    ArtistRadio,
    Playlist,
    /// A Plex station; its group key is the station's hashed namespace.
    Station,
    /// A single pinned track (not a plan).
    Track,
    TrackRadio,
}

impl DownloadKind {
    /// Accepted `minutes` of a download plan of this kind. Radio-like plans
    /// need a duration; bounded listings accept an optional cap.
    pub fn minutes(self) -> std::ops::RangeInclusive<u32> {
        match self {
            Self::Album | Self::Playlist | Self::Track => 0..=480,
            Self::Station | Self::ArtistRadio | Self::AlbumRadio | Self::TrackRadio => 30..=480,
        }
    }
    /// Seed metadata type of a sonic radio download.
    pub fn radio_seed(self) -> Option<crate::model::ItemKind> {
        match self {
            Self::ArtistRadio => Some(crate::model::ItemKind::Artist),
            Self::AlbumRadio => Some(crate::model::ItemKind::Album),
            Self::TrackRadio => Some(crate::model::ItemKind::Track),
            Self::Album | Self::Playlist | Self::Station | Self::Track => None,
        }
    }
    /// The download kind pinning an item of this metadata type.
    pub fn pinned(kind: &crate::model::ItemKind) -> Option<Self> {
        match kind {
            crate::model::ItemKind::Album => Some(Self::Album),
            crate::model::ItemKind::Track => Some(Self::Track),
            crate::model::ItemKind::Playlist => Some(Self::Playlist),
            crate::model::ItemKind::Artist | crate::model::ItemKind::Other(_) => None,
        }
    }
}

/// Key of a pinned download group, persisted and sent to Qt as `kind:key`
/// ("playlist:5", "track:7", "station:<hash>").
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(into = "String", try_from = "String")]
pub struct GroupId {
    pub kind: DownloadKind,
    pub key: String,
}

/// Marker in the serde error of an unparseable group id (mapped to a
/// user-facing message at admission).
pub(crate) const INVALID_GROUP: &str = "invalid download group";

impl GroupId {
    pub fn new(kind: DownloadKind, key: impl Into<String>) -> Self {
        Self {
            kind,
            key: key.into(),
        }
    }
    /// The group of a plan: stations are keyed by their hashed path.
    pub fn plan(kind: DownloadKind, key: &str) -> Self {
        match kind {
            DownloadKind::Station => Self::new(kind, crate::cache::namespace(key, "station")),
            _ => Self::new(kind, key),
        }
    }
}

impl std::fmt::Display for GroupId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.kind, self.key)
    }
}

impl std::str::FromStr for GroupId {
    type Err = &'static str;
    fn from_str(group: &str) -> std::result::Result<Self, Self::Err> {
        let (kind, key) = group.split_once(':').ok_or(INVALID_GROUP)?;
        Ok(Self::new(kind.parse().map_err(|_| INVALID_GROUP)?, key))
    }
}

impl TryFrom<String> for GroupId {
    type Error = &'static str;
    fn try_from(group: String) -> std::result::Result<Self, Self::Error> {
        group.parse()
    }
}

impl From<GroupId> for String {
    fn from(group: GroupId) -> Self {
        group.to_string()
    }
}

/// Persisted group maps: drop entries whose key is not a group id instead of
/// rejecting the whole session document.
pub(crate) fn lenient_groups<'de, D, V>(
    deserializer: D,
) -> std::result::Result<BTreeMap<GroupId, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    V: serde::de::DeserializeOwned,
{
    Ok(BTreeMap::<String, Value>::deserialize(deserializer)?
        .into_iter()
        .filter_map(|(group, value)| {
            Some((group.parse().ok()?, serde_json::from_value(value).ok()?))
        })
        .collect())
}

#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DownloadPlan {
    pub kind: DownloadKind,
    pub key: String,
    pub minutes: u32,
    pub refreshed_at: u64,
}

impl Core {
    pub(crate) fn probe_playback(&self, key: &str, kbps: u32) -> Result<Value> {
        if self.offline_mode {
            return Err(Error::Input("Go online to refresh downloads"));
        }
        crate::quality::Config {
            mobile_kbps: kbps,
            ..Default::default()
        }
        .validate()?;
        let track = self.fresh_metadata_item(key, &crate::model::ItemKind::Track)?;
        if track.duration < 10000 || track.duration > 600000 {
            return Err(Error::Input(
                "Track duration is needed for a timed download",
            ));
        }
        let mut uri = crate::quality::transcode_url(
            &self.base()?,
            key,
            kbps,
            &uuid::Uuid::new_v4().to_string(),
        )?;
        uri.query_pairs_mut()
            .append_pair("X-Plex-Token", &self.settings.server_token);
        let engine = crate::audio::Engine::new(crate::audio::Sink::Fake)?;
        engine.handle.load(
            crate::audio::Source {
                id: 1,
                url: uri.into(),
                duration: track.duration,
                album: track.parent_rating_key,
                ..Default::default()
            },
            false,
        )?;
        let wait = |predicate: &dyn Fn(&crate::audio::Snapshot) -> bool| -> Result<crate::audio::Snapshot> {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let state = engine.handle.snapshot();
                if !state.error.is_empty() { return Err(Error::Input("Audio playback failed")); }
                if predicate(&state) { return Ok(state); }
                if std::time::Instant::now() >= end { return Err(Error::Input("Audio playback failed")); }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        wait(&|state| state.playing && state.position >= 150)?;
        engine.handle.pause()?;
        let paused = wait(&|state| state.paused)?;
        let target = (track.duration / 2).min(60000);
        engine.handle.seek(target)?;
        engine.handle.play()?;
        let state = wait(&|state| state.playing && state.position >= target + 100)?;
        if state.listened > 2000 {
            return Err(Error::Input("Audio playback failed"));
        }
        let result = json!({"playbackProbe":{"played":true,"paused":paused.paused,"seeked":true,"position":state.position,"listened":state.listened}});
        engine.handle.stop()?;
        drop(engine);
        Ok(result)
    }
    pub(crate) fn check_planning(&self) -> Result<()> {
        if self.planning_guard.as_ref().is_some_and(|(epoch, wanted)| {
            epoch.load(std::sync::atomic::Ordering::SeqCst) != *wanted
        }) {
            return Err(Error::Input("Cache request cancelled"));
        }
        Ok(())
    }
    pub(crate) fn maintain_refresh(&mut self) -> Result<Value> {
        let allowed = self.writable
            && self.network_online
            && self.downloads_allowed()
            && self.cache.gate().allowed()
            && self.settings.refresh_policy.enabled;
        if !allowed {
            self.refresh.cancel();
        }
        if let Some(outcome) = self.refresh.poll() {
            if allowed
                && outcome.job.namespace == self.cache_namespace()
                && self.settings.download_plans.get(&outcome.job.group) == Some(&outcome.job.plan)
            {
                match outcome.result {
                    Ok((title, tracks)) => {
                        match self.install_download_plan(
                            outcome.job.plan.kind,
                            &outcome.job.plan.key,
                            outcome.job.plan.minutes,
                            title,
                            tracks,
                        ) {
                            Ok(_) => {
                                self.settings.downloads = self.manual_cache.clone();
                                crate::store::save(&self.dir, &self.settings)?;
                            }
                            Err(error) => self.refresh.error = error.to_string(),
                        }
                    }
                    Err(error) => self.refresh.error = error.to_string(),
                }
            }
        }
        if allowed && self.refresh.pending.is_none() {
            let now = crate::history::now();
            let due = self
                .settings
                .download_plans
                .iter()
                .find(|(group, plan)| {
                    plan.kind != DownloadKind::Album
                        && self
                            .refresh
                            .due(group, plan, &self.settings.refresh_policy, now)
                })
                .map(|(group, plan)| (group.clone(), plan.clone()));
            if let Some((group, plan)) = due {
                self.refresh
                    .submit(self.dir.clone(), self.cache_namespace(), group, plan, now)?;
            }
        }
        Ok(json!({"cache":self.cache_status()}))
    }
    pub(crate) fn probe_quality(&self, key: &str, kbps: u32) -> Result<Value> {
        if self.offline_mode {
            return Err(Error::Input("Go online to refresh downloads"));
        }
        crate::quality::Config {
            mobile_kbps: kbps,
            ..Default::default()
        }
        .validate()?;
        let seed = self.fresh_metadata_item(key, &crate::model::ItemKind::Track)?;
        if seed.duration == 0 || seed.duration > 600000 {
            return Err(Error::Input(
                "Track duration is needed for a timed download",
            ));
        }
        let base = self.base()?;
        let uri =
            crate::quality::transcode_url(&base, key, kbps, &uuid::Uuid::new_v4().to_string())?;
        let result = self
            .plex
            .probe_audio(uri.clone(), &self.settings.server_token);
        let stopped = crate::quality::stop_url(&uri).is_some_and(|stop| {
            let params: Vec<_> = stop
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            let refs: Vec<_> = params
                .iter()
                .map(|(k, v)| (k.as_str(), v.clone()))
                .collect();
            self.plex
                .mutate(
                    &base,
                    &self.settings.server_token,
                    reqwest::Method::GET,
                    stop.path(),
                    &refs,
                )
                .is_ok()
        });
        let (bytes, mime) = result?;
        Ok(json!({"transcodeProbe":{"bytes":bytes,"mime":mime,"stopped":stopped}}))
    }
    fn fresh_metadata_item(&self, key: &str, kind: &crate::model::ItemKind) -> Result<Item> {
        self.check_planning()?;
        crate::numeric(key)?;
        let path = format!("/library/metadata/{key}");
        let params = [("includeStations", "1".into())];
        let container =
            self.plex
                .container(&self.base()?, &self.settings.server_token, &path, &params)?;
        self.library
            .save(&self.cache_namespace(), &path, &params, &container);
        container
            .items
            .into_iter()
            .find(|i| i.rating_key == key && i.kind == *kind)
            .ok_or(Error::Input("Choose a music item"))
    }
    fn metadata_item(&self, key: &str, kind: &crate::model::ItemKind) -> Result<Item> {
        crate::numeric(key)?;
        self.container_cached(
            &format!("/library/metadata/{key}"),
            &[("includeStations", "1".into())],
        )?
        .0
        .items
        .into_iter()
        .find(|i| i.rating_key == key && i.kind == *kind)
        .ok_or(Error::Input("Choose a music item"))
    }
    /// Download recipes page fresh server data only; never a stale offline page.
    pub(crate) fn bounded_tracks(&self, path: &str, minutes: u32) -> Result<Vec<Item>> {
        self.paged_tracks(path, minutes, Paging::Download)
    }
    /// The one bounded track pager: 100-row pages, at most 1000 tracks. Stops
    /// on an empty page, a short page without `totalSize`, the advertised total
    /// or, for timed plans, once `minutes` of audio are collected.
    pub(crate) fn paged_tracks(
        &self,
        path: &str,
        minutes: u32,
        paging: Paging,
    ) -> Result<Vec<Item>> {
        let mut tracks = Vec::new();
        let mut start = 0;
        let mut duration = 0u64;
        // 10 full pages reach the limit; the 11th confirms an unknown total ended.
        for _ in 0..11 {
            let (page, offline) = self.container_cached(
                path,
                &[
                    ("X-Plex-Container-Start", start.to_string()),
                    ("X-Plex-Container-Size", "100".into()),
                ],
            )?;
            if offline && paging == Paging::Download {
                return Err(Error::Input("Go online to refresh downloads"));
            }
            let count = page.items.len();
            for item in page.items {
                if item.kind != crate::model::ItemKind::Track
                    || crate::numeric(&item.rating_key).is_err()
                {
                    return Err(paging.not_track());
                }
                if minutes > 0 && item.duration == 0 {
                    return Err(Error::Input(
                        "Track duration is needed for a timed download",
                    ));
                }
                duration = duration.saturating_add(item.duration);
                tracks.push(item);
                if tracks.len() > 1000 {
                    return Err(paging.too_many());
                }
                if minutes > 0 && duration >= u64::from(minutes) * 60000 {
                    return Ok(tracks);
                }
            }
            start += count;
            if count == 0 || page.total_size.map_or(count < 100, |total| start >= total) {
                return Ok(tracks);
            }
        }
        Err(paging.too_many())
    }
    fn radio_download_tracks(&self, seed: Item, minutes: u32) -> Result<Vec<Item>> {
        let base = self.base()?;
        let mut tracks = Vec::new();
        let mut seen = BTreeSet::new();
        let mut duration = 0u64;
        let mut add = |items: Vec<Item>| -> Result<bool> {
            let before = tracks.len();
            for mut item in items {
                if item.kind != crate::model::ItemKind::Track
                    || crate::numeric(&item.rating_key).is_err()
                {
                    return Err(Error::ProtocolAt("radio download tracks"));
                }
                if seen.insert(item.rating_key.clone()) {
                    if item.duration == 0 {
                        return Err(Error::Input(
                            "Track duration is needed for a timed download",
                        ));
                    }
                    item.play_queue_item_id = None;
                    duration = duration.saturating_add(item.duration);
                    tracks.push(item);
                    if duration >= u64::from(minutes) * 60000 || tracks.len() >= 1000 {
                        break;
                    }
                }
            }
            Ok(tracks.len() > before
                && duration < u64::from(minutes) * 60000
                && tracks.len() < 1000)
        };
        if let Some(key) = radio::station_key(&seed, &base)? {
            self.check_planning()?;
            let identity =
                self.plex
                    .container(&base, &self.settings.server_token, "/identity", &[])?;
            let uri = radio::station_uri(&identity.machine_identifier, &key)?;
            let mut page =
                self.plex
                    .create_radio_queue(&base, &self.settings.server_token, &uri)?;
            let id = page
                .play_queue_id
                .ok_or(Error::ProtocolAt("radio queue identity"))?;
            let mut centers = BTreeSet::new();
            for _ in 0..20 {
                self.check_planning()?;
                let center = page.items.last().and_then(|i| i.play_queue_item_id);
                if !add(page.items)? {
                    break;
                }
                let Some(center) = center.filter(|center| centers.insert(*center)) else {
                    break;
                };
                page = self.plex.container(
                    &base,
                    &self.settings.server_token,
                    &format!("/playQueues/{id}"),
                    &[
                        ("center", center.to_string()),
                        ("window", "50".into()),
                        ("includeBefore", "0".into()),
                        ("includeAfter", "1".into()),
                    ],
                )?;
            }
        } else {
            if seed.kind == crate::model::ItemKind::Artist {
                return Err(Error::Input(
                    "Plex does not expose an artist radio station for this artist",
                ));
            }
            let mut seed = seed;
            let mut seeds = BTreeSet::new();
            for _ in 0..20 {
                self.check_planning()?;
                if !seeds.insert(seed.rating_key.clone()) {
                    break;
                }
                let items = if seed.kind == crate::model::ItemKind::Album {
                    self.album_tracks(&seed)?
                } else {
                    vec![seed.clone()]
                };
                if !add(items)? {
                    break;
                }
                let next = self
                    .nearest(&seed)?
                    .into_iter()
                    .find(|i| !seeds.contains(&i.rating_key));
                let Some(next) = next else { break };
                seed = next;
            }
        }
        if tracks.is_empty() {
            return Err(Error::Input("Radio returned no playable tracks"));
        }
        Ok(tracks)
    }
    pub(crate) fn download_plan(
        &mut self,
        kind: DownloadKind,
        key: &str,
        minutes: u32,
    ) -> Result<Value> {
        if kind == DownloadKind::Station {
            if !key.starts_with("/library/")
                || !crate::plex::advertised_path(&self.base()?, key)?
                    .path()
                    .starts_with("/library/")
            {
                return Err(Error::Input("Choose a music item"));
            }
        } else {
            crate::numeric(key)?;
        }
        if !self.writable || self.offline_mode {
            return Err(Error::Input("Go online to refresh downloads"));
        }
        if !self.settings.cache.enabled {
            return Err(Error::Input(
                "Enable audio caching before pinning downloads",
            ));
        }
        if !kind.minutes().contains(&minutes) {
            return Err(Error::Input("Choose 30–480 minutes for a radio download"));
        }
        let (title, tracks) = self.collect_download(kind, key, minutes)?;
        self.install_download_plan(kind, key, minutes, title, tracks)
    }
    pub(crate) fn collect_download(
        &self,
        kind: DownloadKind,
        key: &str,
        minutes: u32,
    ) -> Result<(String, Vec<Item>)> {
        self.check_planning()?;
        let collected = match kind {
            DownloadKind::Station => {
                let group = GroupId::plan(kind, key);
                let title = self
                    .settings
                    .download_titles
                    .get(&group)
                    .cloned()
                    .unwrap_or_else(|| "Radio".into());
                let seed = Item {
                    kind: crate::model::ItemKind::Artist,
                    title: title.clone(),
                    stations: json!([{ "key":key }]),
                    ..Default::default()
                };
                (title, self.radio_download_tracks(seed, minutes)?)
            }
            DownloadKind::Playlist => {
                let item = self.playlist_metadata(key)?;
                (
                    item.title,
                    self.bounded_tracks(&format!("/playlists/{key}/items"), minutes)?,
                )
            }
            DownloadKind::Album => {
                let seed = self.fresh_metadata_item(key, &crate::model::ItemKind::Album)?;
                (
                    seed.title.clone(),
                    self.bounded_tracks(&format!("/library/metadata/{key}/children"), 0)?,
                )
            }
            DownloadKind::ArtistRadio | DownloadKind::AlbumRadio | DownloadKind::TrackRadio => {
                let seed_kind = kind
                    .radio_seed()
                    .ok_or(Error::Input("Choose a playlist, album or radio download"))?;
                let seed = self.fresh_metadata_item(key, &seed_kind)?;
                (
                    seed.title.clone(),
                    self.radio_download_tracks(seed, minutes)?,
                )
            }
            DownloadKind::Track => {
                return Err(Error::Input("Choose a playlist, album or radio download"))
            }
        };
        self.check_planning()?;
        Ok(collected)
    }
    fn install_download_plan(
        &mut self,
        kind: DownloadKind,
        key: &str,
        minutes: u32,
        title: String,
        mut tracks: Vec<Item>,
    ) -> Result<Value> {
        let mut seen = BTreeSet::new();
        tracks.retain(|i| seen.insert(i.rating_key.clone()));
        if tracks.is_empty() {
            return Err(Error::Input("Pin up to 1000 playable tracks"));
        }
        let group = GroupId::plan(kind, key);
        let mut groups = self.settings.download_groups.clone();
        groups.insert(
            group.clone(),
            tracks.iter().map(|i| i.rating_key.clone()).collect(),
        );
        let keep = pinned_keys(&groups);
        let mut plan: BTreeMap<_, _> = self
            .manual_cache
            .iter()
            .filter(|i| keep.contains(&i.rating_key))
            .map(|i| (i.rating_key.clone(), i.clone()))
            .collect();
        for track in &tracks {
            plan.insert(track.rating_key.clone(), track.clone());
        }
        if plan.len() > 1000 {
            return Err(Error::Input("Pin up to 1000 playable tracks"));
        }
        // Prepare everything first: failed refresh leaves the previous plan intact.
        self.settings.download_groups = groups;
        self.settings.download_titles.insert(group.clone(), title);
        self.settings.download_plans.insert(
            group.clone(),
            DownloadPlan {
                kind,
                key: key.into(),
                minutes,
                refreshed_at: crate::history::now(),
            },
        );
        self.manual_cache = plan.into_values().collect();
        self.catalogue_epoch = self.catalogue_epoch.wrapping_add(1);
        self.library.save(
            &self.cache_namespace(),
            &format!("/plexfreq/downloads/{group}"),
            &[],
            &Container {
                items: tracks.clone(),
                size: tracks.len(),
                ..Default::default()
            },
        );
        let _ = self.execute_inner(Command::CacheArtwork { items: tracks });
        self.cache.cancel();
        self.schedule_cache();
        Ok(json!({"cache":self.cache_status(),"downloads":self.download_rows()}))
    }
    pub(crate) fn download_station(
        &mut self,
        key: &str,
        title: &str,
        minutes: u32,
    ) -> Result<Value> {
        self.download_plan(DownloadKind::Station, key, minutes)?;
        let group = GroupId::plan(DownloadKind::Station, key);
        self.settings
            .download_titles
            .insert(group, title.chars().take(512).collect());
        Ok(json!({"cache":self.cache_status(),"downloads":self.download_rows()}))
    }
    pub(crate) fn refresh_download(&mut self, group: &GroupId) -> Result<Value> {
        if let Some(plan) = self.settings.download_plans.get(group).cloned() {
            return self.download_plan(plan.kind, &plan.key, plan.minutes);
        }
        self.download_plan(group.kind, &group.key, 0)
    }
    pub(crate) fn play_download(&mut self, group: &GroupId) -> Result<Value> {
        let keys = self
            .settings
            .download_groups
            .get(group)
            .ok_or(Error::Input("Download group no longer exists"))?;
        let items: Vec<_> = keys
            .iter()
            .filter_map(|key| self.manual_cache.iter().find(|i| &i.rating_key == key))
            .filter_map(|item| {
                self.cache
                    .lookup(&self.cache_namespace(), item, false)
                    .map(|(_, saved)| saved)
            })
            .collect();
        if items.is_empty() {
            return Err(Error::Input("This track has not been downloaded"));
        }
        self.execute_inner(Command::Play { items, index: 0 })
    }
    fn build_offline_catalogue(&self) -> Vec<Item> {
        let mut catalogue: BTreeMap<_, _> = self
            .library
            .items(&self.cache_namespace())
            .into_iter()
            .map(|i| ((i.kind.clone(), i.rating_key.clone()), i))
            .collect();
        for track in self
            .manual_cache
            .iter()
            .cloned()
            .chain(self.cache.items(&self.cache_namespace()))
        {
            if !track.parent_rating_key.is_empty() {
                catalogue
                    .entry((
                        crate::model::ItemKind::Album,
                        track.parent_rating_key.clone(),
                    ))
                    .or_insert_with(|| Item {
                        kind: crate::model::ItemKind::Album,
                        rating_key: track.parent_rating_key.clone(),
                        title: track.parent_title.clone(),
                        parent_rating_key: track.grandparent_rating_key.clone(),
                        parent_title: track.grandparent_title.clone(),
                        thumb: track.parent_thumb.clone(),
                        ..Default::default()
                    });
            }
            if !track.grandparent_rating_key.is_empty() {
                catalogue
                    .entry((
                        crate::model::ItemKind::Artist,
                        track.grandparent_rating_key.clone(),
                    ))
                    .or_insert_with(|| Item {
                        kind: crate::model::ItemKind::Artist,
                        rating_key: track.grandparent_rating_key.clone(),
                        title: track.grandparent_title.clone(),
                        ..Default::default()
                    });
            }
            catalogue.insert((track.kind.clone(), track.rating_key.clone()), track);
        }
        catalogue.into_values().collect()
    }
    fn ensure_offline_index(&self) {
        let stamp = crate::catalogue::Stamp {
            namespace: self.cache_namespace(),
            library: self.library.catalogue_stamp(),
            audio: self.cache.catalogue_revision(),
            plans: self.catalogue_epoch,
        };
        if self
            .offline_index
            .borrow()
            .as_ref()
            .is_some_and(|index| index.stamp == stamp)
        {
            return;
        }
        let items = self.build_offline_catalogue();
        *self.offline_index.borrow_mut() = Some(crate::catalogue::Index::new(stamp, items));
    }
    pub(crate) fn offline_catalogue(&self) -> Vec<Item> {
        self.ensure_offline_index();
        self.offline_index
            .borrow()
            .as_ref()
            .map_or_else(Vec::new, |index| index.items.clone())
    }
    pub(crate) fn offline_search(
        &self,
        query: &str,
        kind: &crate::model::ItemKind,
        start: usize,
    ) -> Result<Value> {
        if !(kind.is_empty() || kind.is_music()) || query.len() > 512 {
            return Err(Error::Input("Choose a music item"));
        }
        self.ensure_offline_index();
        let index = self.offline_index.borrow();
        let index = index.as_ref().ok_or(Error::Input("Choose a music item"))?;
        let mut total = 0;
        let mut page = Vec::new();
        for item in index.search(query, kind) {
            if total >= start && page.len() < 100 {
                page.push(self.display_item(item));
            }
            total += 1;
        }
        let received = page.len();
        Ok(crate::pagination::page(
            page,
            start,
            received,
            100,
            Some(total),
            true,
        ))
    }
    pub(crate) fn discovery_home(&self, section: &str) -> Result<Value> {
        crate::numeric(section)?;
        let (container, offline) = self.container_cached(
            &format!("/hubs/sections/{section}"),
            &[
                ("count", "12".into()),
                ("includeStations", "1".into()),
                ("includeMyMixes", "1".into()),
            ],
        )?;
        let layout = self.discovery_layout(section);
        let mut hubs = container.hubs;
        hubs.sort_by_cached_key(|hub| {
            layout
                .order
                .iter()
                .position(|id| id == &hub.hub_identifier)
                .unwrap_or(usize::MAX)
        });
        let options:Vec<_>=hubs.iter().map(|hub|json!({"identifier":hub.hub_identifier,"title":hub.title,"hidden":layout.hidden.contains(&hub.hub_identifier)})).collect();
        let mut items = Vec::new();
        for hub in hubs
            .into_iter()
            .filter(|hub| !layout.hidden.contains(&hub.hub_identifier))
            .take(24)
        {
            let recent = hub
                .hub_identifier
                .strip_prefix("music.recent.added.")
                .or_else(|| hub.hub_identifier.strip_prefix("music.recent.played."));
            let grid = recent.is_some_and(|section| crate::numeric(section).is_ok())
                || matches!(
                    hub.hub_identifier.as_str(),
                    "music.recentlyAdded"
                        | "music.recentlyAddedAlbums"
                        | "music.recentlyPlayed"
                        | "music.recentlyPlayedAlbums"
                        | "music.recentlyPlayedTracks"
                        | "home.recentlyAdded"
                        | "home.recentlyPlayed"
                );
            for item in hub.items.into_iter().take(12) {
                let station = item.kind == crate::model::ItemKind::Playlist
                    && item.radio
                    && item.key.starts_with("/library/")
                    && crate::plex::advertised_path(&self.base()?, &item.key).is_ok();
                if !(item.kind.is_music() || item.kind == crate::model::ItemKind::Playlist)
                    || (!station && crate::numeric(&item.rating_key).is_err())
                {
                    continue;
                }
                let mut value = self.display_item(&item);
                value["station"] = json!(station);
                value["discoveryGroup"] = json!(hub.title);
                value["hubIdentifier"] = json!(hub.hub_identifier);
                value["hubKey"] = json!(hub.key);
                value["hubMore"] = json!(hub.more);
                value["discoveryGrid"] = json!(grid);
                items.push(value);
            }
        }
        Ok(
            json!({"items":items,"start":0,"next":0,"hasMore":false,"offline":offline,"discoveryHome":true,"discoveryHubs":options,"discoveryLayout":layout}),
        )
    }
    pub(crate) fn sonic_neighbors(
        &self,
        key: &str,
        kind: &crate::model::ItemKind,
    ) -> Result<Value> {
        if !kind.is_music() {
            return Err(Error::Input("Choose a music item"));
        }
        let seed = self.metadata_item(key, kind)?;
        let items: Vec<_> = self
            .nearest(&seed)?
            .into_iter()
            .take(100)
            .map(|i| self.display_item(&i))
            .collect();
        Ok(json!({"items":items,"start":0,"next":0,"hasMore":false}))
    }
    pub(crate) fn start_station(&mut self, key: &str, title: &str) -> Result<Value> {
        if self.offline_mode || !key.starts_with("/library/") {
            return Err(Error::Input("Choose a music item"));
        }
        let base = self.base()?;
        crate::plex::advertised_path(&base, key)?;
        let identity = self
            .plex
            .container(&base, &self.settings.server_token, "/identity", &[])?;
        let uri = radio::station_uri(&identity.machine_identifier, key)?;
        let page = self
            .plex
            .create_radio_queue(&base, &self.settings.server_token, &uri)?;
        let id = page
            .play_queue_id
            .ok_or(Error::ProtocolAt("radio queue identity"))?;
        if page
            .items
            .iter()
            .any(|i| i.kind != crate::model::ItemKind::Track || i.play_queue_item_id.is_none())
        {
            return Err(Error::ProtocolAt("radio queue tracks"));
        }
        let start = page
            .items
            .iter()
            .position(|i| i.play_queue_item_id == page.play_queue_selected_item_id)
            .unwrap_or(0);
        let mut queue = crate::queue::Queue::default();
        queue.replace(page.items.into_iter().skip(start).take(11).collect(), 0)?;
        let old = std::mem::replace(&mut self.queue, queue);
        let old_radio = self.radio.replace(radio::Radio {
            kind: radio::RadioKind::Artist,
            title: title.chars().take(512).collect(),
            source: radio::Source::Station { queue_id: id },
        });
        let result = self.playback();
        if result.is_err() {
            self.queue = old;
            self.radio = old_radio;
        }
        result
    }
    pub(crate) fn sonic_adventure(&self, section: &str, start: &str, end: &str) -> Result<Value> {
        for key in [section, start, end] {
            crate::numeric(key)?;
        }
        let (container, offline) = self.container_cached(
            &format!("/library/sections/{section}/computePath"),
            &[
                ("startID", start.into()),
                ("endID", end.into()),
                ("X-Plex-Container-Start", "0".into()),
                ("X-Plex-Container-Size", "100".into()),
            ],
        )?;
        if container.items.len() > 100
            || container.total_size.is_some_and(|n| n > 100)
            || container.items.iter().any(|i| {
                i.kind != crate::model::ItemKind::Track || crate::numeric(&i.rating_key).is_err()
            })
        {
            return Err(Error::ProtocolAt("sonic adventure"));
        }
        let items: Vec<_> = container
            .items
            .iter()
            .map(|i| self.display_item(i))
            .collect();
        Ok(json!({"items":items,"start":0,"next":0,"hasMore":false,"offline":offline}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoEnumIterator;

    #[test]
    fn group_ids_keep_their_string_form_and_order() {
        let raw = [
            "album:3",
            "album_radio:4",
            "artist_radio:1",
            "playlist:10",
            "playlist:5",
            "station:0123abcd",
            "track:7",
            "track_radio:2",
        ];
        let mut sorted = raw.to_vec();
        sorted.sort();
        let mut groups: Vec<GroupId> = raw.iter().map(|g| g.parse().unwrap()).collect();
        groups.reverse();
        groups.sort();
        assert_eq!(
            groups.iter().map(GroupId::to_string).collect::<Vec<_>>(),
            sorted
        );
        for group in &groups {
            let value = serde_json::to_value(group).unwrap();
            assert_eq!(value, json!(group.to_string()));
            assert_eq!(serde_json::from_value::<GroupId>(value).unwrap(), *group);
        }
        for bad in ["", "playlist", "genre:1", ":5"] {
            assert!(bad.parse::<GroupId>().is_err(), "{bad}");
        }
        assert_eq!(
            GroupId::plan(DownloadKind::Station, "/library/metadata/1/station/x").to_string(),
            format!(
                "station:{}",
                crate::cache::namespace("/library/metadata/1/station/x", "station")
            )
        );
        // Settings maps keep their on-disk keys; an unparseable legacy entry
        // is dropped instead of discarding the whole session document.
        let settings: crate::store::Settings = serde_json::from_value(json!({
            "downloadGroups": {"playlist:5": ["1"], "bogus": ["2"]},
            "downloadTitles": {"playlist:5": "Mix"},
            "downloadPlans": {
                "playlist:5": {"kind":"playlist","key":"5","minutes":0,"refreshedAt":1},
                "album:9": {"kind":"nonsense","key":"9","minutes":0,"refreshedAt":1}
            }
        }))
        .unwrap();
        let saved = serde_json::to_value(&settings).unwrap();
        assert_eq!(saved["downloadGroups"], json!({"playlist:5": ["1"]}));
        assert_eq!(saved["downloadTitles"], json!({"playlist:5": "Mix"}));
        assert_eq!(
            saved["downloadPlans"],
            json!({"playlist:5": {"kind":"playlist","key":"5","minutes":0,"refreshedAt":1}})
        );
    }

    #[test]
    fn download_kind_minute_rules() {
        for kind in DownloadKind::iter() {
            let radio = kind.radio_seed().is_some() || kind == DownloadKind::Station;
            assert_eq!(kind.minutes().contains(&0), !radio, "{kind}");
            assert!(kind.minutes().contains(&480), "{kind}");
            assert!(!kind.minutes().contains(&481), "{kind}");
            assert_eq!(kind.minutes().contains(&29), !radio, "{kind}");
            assert!(kind.minutes().contains(&30), "{kind}");
            let name = kind.to_string();
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(name));
            assert_eq!(name.parse::<DownloadKind>().unwrap(), kind);
        }
        assert_eq!(
            DownloadKind::TrackRadio.radio_seed(),
            Some(crate::model::ItemKind::Track)
        );
        assert_eq!(DownloadKind::Station.radio_seed(), None);
    }
}
