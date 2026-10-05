//! Daily-use discovery and durable bounded offline plans. No presentation policy.
use crate::{
    model::{Container, Item},
    radio, Command, Core, Error, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadPlan {
    pub kind: String,
    pub key: String,
    pub minutes: u32,
    pub refreshed_at: u64,
}

impl Core {
    pub(crate) fn probe_quality(&self, key: &str, kbps: u32) -> Result<Value> {
        if self.offline_mode {
            return Err(Error::Input("Go online to refresh downloads"));
        }
        crate::quality::Config {
            mobile_kbps: kbps,
            ..Default::default()
        }
        .validate()?;
        let seed = self.fresh_metadata_item(key, "track")?;
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
            self.plex
                .mutate(
                    &base,
                    &self.settings.server_token,
                    reqwest::Method::GET,
                    &format!("{}?{}", stop.path(), stop.query().unwrap_or("")),
                    &[],
                )
                .is_ok()
        });
        let (bytes, mime) = result?;
        Ok(json!({"transcodeProbe":{"bytes":bytes,"mime":mime,"stopped":stopped}}))
    }
    fn fresh_metadata_item(&self, key: &str, kind: &str) -> Result<Item> {
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
            .find(|i| i.rating_key == key && i.kind == kind)
            .ok_or(Error::Input("Choose a music item"))
    }
    fn metadata_item(&self, key: &str, kind: &str) -> Result<Item> {
        crate::numeric(key)?;
        self.container_cached(
            &format!("/library/metadata/{key}"),
            &[("includeStations", "1".into())],
        )?
        .0
        .items
        .into_iter()
        .find(|i| i.rating_key == key && i.kind == kind)
        .ok_or(Error::Input("Choose a music item"))
    }
    fn bounded_tracks(&self, path: &str, minutes: u32) -> Result<Vec<Item>> {
        let mut tracks = Vec::new();
        let mut start = 0;
        let mut duration = 0u64;
        for _ in 0..11 {
            let (page, offline) = self.container_cached(
                path,
                &[
                    ("X-Plex-Container-Start", start.to_string()),
                    ("X-Plex-Container-Size", "100".into()),
                ],
            )?;
            if offline {
                return Err(Error::Input("Go online to refresh downloads"));
            }
            let count = page.items.len();
            for item in page.items {
                if item.kind != "track" || crate::numeric(&item.rating_key).is_err() {
                    return Err(Error::Input("Only audio playlists can be downloaded"));
                }
                if minutes > 0 && item.duration == 0 {
                    return Err(Error::Input(
                        "Track duration is needed for a timed download",
                    ));
                }
                duration = duration.saturating_add(item.duration);
                tracks.push(item);
                if tracks.len() > 1000 {
                    return Err(Error::Input("Download plan exceeds 1000 tracks"));
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
        Err(Error::Input("Download plan exceeds 1000 tracks"))
    }
    fn radio_download_tracks(&self, seed: Item, minutes: u32) -> Result<Vec<Item>> {
        let base = self.base()?;
        let mut tracks = Vec::new();
        let mut seen = BTreeSet::new();
        let mut duration = 0u64;
        let mut add = |items: Vec<Item>| -> Result<bool> {
            let before = tracks.len();
            for mut item in items {
                if item.kind != "track" || crate::numeric(&item.rating_key).is_err() {
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
            if seed.kind == "artist" {
                return Err(Error::Input(
                    "Plex does not expose an artist radio station for this artist",
                ));
            }
            let mut seed = seed;
            let mut seeds = BTreeSet::new();
            for _ in 0..20 {
                if !seeds.insert(seed.rating_key.clone()) {
                    break;
                }
                let items = if seed.kind == "album" {
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
    pub(crate) fn download_plan(&mut self, kind: &str, key: &str, minutes: u32) -> Result<Value> {
        if kind == "station" {
            if !key.starts_with("/library/")
                || !crate::plex::server_path(&self.base()?, key)?
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
        if minutes > 480
            || ((kind == "station" || kind.ends_with("_radio")) && !(30..=480).contains(&minutes))
        {
            return Err(Error::Input("Choose 30–480 minutes for a radio download"));
        }
        let (title, mut tracks) = match kind {
            "station" => {
                let group = format!("station:{}", crate::cache::namespace(key, "station"));
                let title = self
                    .settings
                    .download_titles
                    .get(&group)
                    .cloned()
                    .unwrap_or_else(|| "Radio".into());
                let seed = Item {
                    kind: "artist".into(),
                    title: title.clone(),
                    stations: json!([{ "key":key }]),
                    ..Default::default()
                };
                (title, self.radio_download_tracks(seed, minutes)?)
            }
            "playlist" => {
                let item = self.playlist_metadata(key)?;
                (
                    item.title,
                    self.bounded_tracks(&format!("/playlists/{key}/items"), minutes)?,
                )
            }
            "album" => {
                let seed = self.fresh_metadata_item(key, "album")?;
                (
                    seed.title.clone(),
                    self.bounded_tracks(&format!("/library/metadata/{key}/children"), 0)?,
                )
            }
            "artist_radio" | "album_radio" | "track_radio" => {
                let seed = self.fresh_metadata_item(key, kind.trim_end_matches("_radio"))?;
                (
                    seed.title.clone(),
                    self.radio_download_tracks(seed, minutes)?,
                )
            }
            _ => return Err(Error::Input("Choose a playlist, album or radio download")),
        };
        let mut seen = BTreeSet::new();
        tracks.retain(|i| seen.insert(i.rating_key.clone()));
        if tracks.is_empty() {
            return Err(Error::Input("Pin up to 1000 playable tracks"));
        }
        let group = if kind == "station" {
            format!("station:{}", crate::cache::namespace(key, "station"))
        } else {
            format!("{kind}:{key}")
        };
        let mut groups = self.settings.download_groups.clone();
        groups.insert(
            group.clone(),
            tracks.iter().map(|i| i.rating_key.clone()).collect(),
        );
        let keep: BTreeSet<_> = groups.values().flatten().cloned().collect();
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
                kind: kind.into(),
                key: key.into(),
                minutes,
                refreshed_at: crate::history::now(),
            },
        );
        self.manual_cache = plan.into_values().collect();
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
        self.download_plan("station", key, minutes)?;
        let group = format!("station:{}", crate::cache::namespace(key, "station"));
        self.settings
            .download_titles
            .insert(group, title.chars().take(512).collect());
        Ok(json!({"cache":self.cache_status(),"downloads":self.download_rows()}))
    }
    pub(crate) fn refresh_download(&mut self, group: &str) -> Result<Value> {
        if let Some(plan) = self.settings.download_plans.get(group).cloned() {
            return self.download_plan(&plan.kind, &plan.key, plan.minutes);
        }
        let (kind, key) = group
            .split_once(':')
            .ok_or(Error::Input("Download group no longer exists"))?;
        self.download_plan(kind, key, 0)
    }
    pub(crate) fn play_download(&mut self, group: &str) -> Result<Value> {
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
    pub(crate) fn offline_catalogue(&self) -> Vec<Item> {
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
                    .entry(("album".into(), track.parent_rating_key.clone()))
                    .or_insert_with(|| Item {
                        kind: "album".into(),
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
                    .entry(("artist".into(), track.grandparent_rating_key.clone()))
                    .or_insert_with(|| Item {
                        kind: "artist".into(),
                        rating_key: track.grandparent_rating_key.clone(),
                        title: track.grandparent_title.clone(),
                        ..Default::default()
                    });
            }
            catalogue.insert((track.kind.clone(), track.rating_key.clone()), track);
        }
        catalogue.into_values().collect()
    }
    pub(crate) fn offline_search(&self, query: &str, kind: &str, start: usize) -> Result<Value> {
        if !matches!(kind, "" | "artist" | "album" | "track") || query.len() > 512 {
            return Err(Error::Input("Choose a music item"));
        }
        let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut items: Vec<_> = self
            .offline_catalogue()
            .into_iter()
            .filter(|i| kind.is_empty() || i.kind == kind)
            .filter(|i| {
                let text = format!("{} {} {}", i.title, i.parent_title, i.grandparent_title)
                    .to_lowercase();
                terms.iter().all(|term| text.contains(term))
            })
            .collect();
        items
            .sort_by_cached_key(|i| (i.title.to_lowercase(), i.kind.clone(), i.rating_key.clone()));
        let total = items.len();
        let page: Vec<_> = items
            .iter()
            .skip(start)
            .take(100)
            .map(|i| self.display_item(i))
            .collect();
        let next = start.saturating_add(page.len());
        Ok(json!({"items":page,"start":start,"next":next,"hasMore":next<total,"offline":true}))
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
        let mut items = Vec::new();
        for hub in container.hubs.into_iter().take(24) {
            for item in hub.items.into_iter().take(12) {
                let station = item.kind == "playlist"
                    && item.radio
                    && item.key.starts_with("/library/")
                    && crate::plex::server_path(&self.base()?, &item.key).is_ok();
                if !matches!(
                    item.kind.as_str(),
                    "artist" | "album" | "track" | "playlist"
                ) || (!station && crate::numeric(&item.rating_key).is_err())
                {
                    continue;
                }
                let mut value = self.display_item(&item);
                value["station"] = json!(station);
                value["discoveryGroup"] = json!(hub.title);
                value["hubIdentifier"] = json!(hub.hub_identifier);
                items.push(value);
            }
        }
        Ok(
            json!({"items":items,"start":0,"next":0,"hasMore":false,"offline":offline,"discoveryHome":true}),
        )
    }
    pub(crate) fn sonic_neighbors(&self, key: &str, kind: &str) -> Result<Value> {
        if !matches!(kind, "artist" | "album" | "track") {
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
        crate::plex::server_path(&base, key)?;
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
            .any(|i| i.kind != "track" || i.play_queue_item_id.is_none())
        {
            return Err(Error::ProtocolAt("radio queue tracks"));
        }
        let start = page
            .items
            .iter()
            .position(|i| i.play_queue_item_id == page.play_queue_selected_item_id)
            .unwrap_or(0);
        let mut queue = crate::queue::Queue::default();
        queue.replace(page.items, start)?;
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
            || container
                .items
                .iter()
                .any(|i| i.kind != "track" || crate::numeric(&i.rating_key).is_err())
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
