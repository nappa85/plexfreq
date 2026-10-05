use crate::{model::Item, store, Command, Core, Error, Result};
use rand::seq::SliceRandom;
use reqwest::Method;
use serde_json::{json, Value};

pub fn album_type(item: &Item) -> String {
    let tags: Vec<_> = item
        .subformats
        .iter()
        .map(|t| t.tag.to_lowercase())
        .collect();
    if tags.iter().any(|t| t == "live") {
        return "Live".into();
    }
    if tags.iter().any(|t| t == "compilation") {
        return "Compilations".into();
    }
    let format = item
        .formats
        .first()
        .map_or(item.subtype.as_str(), |t| t.tag.as_str())
        .to_lowercase();
    match format.as_str() {
        "single" => "Singles",
        "ep" => "EPs",
        _ => "Albums",
    }
    .into()
}

impl Core {
    pub(crate) fn downloads_allowed(&self) -> bool {
        !self.offline_mode
            && !self.settings.downloads_paused
            && (!self.settings.wifi_only || self.network_wifi)
    }
    pub(crate) fn online_edit(&self) -> Result<()> {
        if !self.writable {
            return Err(Error::Input(
                "Playlist edits are disabled in read-only inspection",
            ));
        }
        if self.offline_mode {
            Err(Error::Input("Go online to edit Plex playlists"))
        } else {
            Ok(())
        }
    }
    pub(crate) fn playlist_metadata(&self, key: &str) -> Result<Item> {
        crate::numeric(key)?;
        let (c, _) = self.container_cached(&format!("/playlists/{key}"), &[])?;
        c.items
            .into_iter()
            .find(|i| i.kind == "playlist" && i.rating_key == key)
            .ok_or(Error::ProtocolAt("playlist metadata"))
    }
    pub(crate) fn playlist_items(&self, key: &str, start: usize) -> Result<Value> {
        let playlist = self.playlist_metadata(key)?;
        let mut page = self.page(&format!("/playlists/{key}/items"), vec![], start)?;
        page["playlist"] = self.display_item(&playlist);
        Ok(page)
    }
    pub(crate) fn playlist_choices(&self) -> Result<Value> {
        let mut items = Vec::new();
        let mut start = 0;
        loop {
            let page = self.page("/playlists", vec![("playlistType", "audio".into())], start)?;
            let count = page["items"].as_array().map_or(0, Vec::len);
            items.extend(page["items"].as_array().cloned().unwrap_or_default());
            if items.len() > 1000 {
                return Err(Error::Input(
                    "Playlist chooser is limited to 1000 playlists",
                ));
            }
            if count == 0 || page["hasMore"] != true {
                break;
            }
            start = page["next"].as_u64().unwrap_or(0) as usize;
        }
        Ok(json!({"playlistChoices":items}))
    }
    fn playlist_uri(&self, items: Vec<Item>) -> Result<String> {
        let mut keys = Vec::new();
        for item in items {
            let tracks = if item.kind == "album" {
                self.album_tracks(&item)?
            } else if item.kind == "track" {
                vec![item]
            } else {
                return Err(Error::Input("Add tracks or albums to a playlist"));
            };
            for track in tracks {
                keys.push(crate::numeric(&track.rating_key)?.to_string());
            }
            if keys.len() > 1000 {
                return Err(Error::Input("Add up to 1000 tracks at a time"));
            }
        }
        if keys.is_empty() {
            return Err(Error::Input("Choose tracks or an album for the playlist"));
        }
        let identity =
            self.plex
                .container(&self.base()?, &self.settings.server_token, "/identity", &[])?;
        crate::radio::station_uri(
            &identity.machine_identifier,
            &format!("/library/metadata/{}", keys.join(",")),
        )
    }
    pub(crate) fn edit_playlist(
        &mut self,
        action: &str,
        key: &str,
        title: &str,
        items: Vec<Item>,
        item_id: Option<u64>,
        after: Option<u64>,
    ) -> Result<Value> {
        self.online_edit()?;
        let title = title.trim();
        if matches!(action, "create" | "rename") && (title.is_empty() || title.len() > 512) {
            return Err(Error::Input("Enter a playlist name (up to 512 bytes)"));
        }
        if action == "create" {
            let uri = self.playlist_uri(items)?;
            let c = self.plex.create_playlist(
                &self.base()?,
                &self.settings.server_token,
                &[
                    ("title", title.into()),
                    ("type", "audio".into()),
                    ("smart", "0".into()),
                    ("uri", uri),
                ],
            )?;
            let item = c
                .items
                .first()
                .filter(|i| i.kind == "playlist")
                .ok_or(Error::ProtocolAt("created playlist"))?;
            self.library
                .invalidate_playlist(&self.cache_namespace(), &item.rating_key);
            return Ok(
                json!({"playlistChanged":item.rating_key,"createdPlaylist":self.display_item(item)}),
            );
        }
        let metadata = self.playlist_metadata(key)?;
        let path = format!("/playlists/{key}");
        let (method, path, params) = match action {
            "rename" => (Method::PUT, path, vec![("title", title.into())]),
            "delete" => (Method::DELETE, path, vec![]),
            "add" | "remove" | "move" if metadata.smart => {
                return Err(Error::Input(
                    "Smart playlist contents are controlled by their Plex filters",
                ))
            }
            "add" => (
                Method::PUT,
                format!("{path}/items"),
                vec![("uri", self.playlist_uri(items)?)],
            ),
            "remove" => (
                Method::DELETE,
                format!(
                    "{path}/items/{}",
                    item_id.ok_or(Error::Input("Choose a playlist occurrence"))?
                ),
                vec![],
            ),
            "move" => (
                Method::PUT,
                format!(
                    "{path}/items/{}/move",
                    item_id.ok_or(Error::Input("Choose a playlist occurrence"))?
                ),
                after
                    .map(|id| vec![("after", id.to_string())])
                    .unwrap_or_default(),
            ),
            _ => return Err(Error::Input("Unknown playlist action")),
        };
        self.plex.mutate(
            &self.base()?,
            &self.settings.server_token,
            method,
            &path,
            &params,
        )?;
        self.library
            .invalidate_playlist(&self.cache_namespace(), key);
        Ok(json!({"playlistChanged":key,"playlistDeleted":action=="delete"}))
    }
    pub(crate) fn library_browse(
        &self,
        section: &str,
        kind: &str,
        sort: &str,
        start: usize,
    ) -> Result<Value> {
        crate::numeric(section)?;
        let code = match kind {
            "artist" => "8",
            "album" => "9",
            "track" => "10",
            _ => return Err(Error::Input("Unknown music type")),
        };
        let sort = match sort {
            "title" => "titleSort:asc",
            "newest" => "addedAt:desc",
            "year" if kind != "artist" => "year:desc",
            _ => return Err(Error::Input("Unknown library sort")),
        };
        self.page(
            &format!("/library/sections/{section}/all"),
            vec![("type", code.into()), ("sort", sort.into())],
            start,
        )
    }
    pub(crate) fn artist_albums(&self, key: &str) -> Result<Value> {
        let mut page = self.detail(key, 0)?;
        let mut items = page["items"].as_array().cloned().unwrap_or_default();
        loop {
            if items.len() > 1000 {
                return Err(Error::Input(
                    "Grouping is limited to 1000 albums per artist",
                ));
            }
            if page["hasMore"] != true {
                break;
            }
            page = self.page(
                &format!("/library/metadata/{}/children", crate::numeric(key)?),
                vec![],
                page["next"].as_u64().unwrap_or(0) as usize,
            )?;
            items.extend(page["items"].as_array().cloned().unwrap_or_default());
        }
        items.sort_by(|a, b| {
            a["albumType"]
                .as_str()
                .cmp(&b["albumType"].as_str())
                .then_with(|| b["year"].as_i64().cmp(&a["year"].as_i64()))
                .then_with(|| a["title"].as_str().cmp(&b["title"].as_str()))
        });
        Ok(json!({"items":items,"start":0,"next":0,"hasMore":false,"groupedAlbums":true}))
    }
    pub(crate) fn play_mix(&mut self, seeds: Vec<Item>) -> Result<Value> {
        if seeds.is_empty() || seeds.len() > 10 {
            return Err(Error::Input("Choose 1–10 artists or albums for a mix"));
        }
        let mut groups = Vec::new();
        for seed in seeds {
            let mut items = match seed.kind.as_str() {
                "album" => self.album_tracks(&seed)?,
                "artist" => {
                    // Intentional bounded sampling: up to 200 tracks per artist
                    // seed (2 pages). Full discographies would make mixes slow
                    // and unbounded; roadmap documents this sampling policy.
                    let mut all = Vec::new();
                    for start in [0, 100] {
                        let (c, _) = self.container_cached(
                            &format!(
                                "/library/metadata/{}/allLeaves",
                                crate::numeric(&seed.rating_key)?
                            ),
                            &[
                                ("X-Plex-Container-Start", start.to_string()),
                                ("X-Plex-Container-Size", "100".into()),
                            ],
                        )?;
                        let size = c.items.len();
                        all.extend(c.items.into_iter().filter(|i| i.kind == "track"));
                        if size < 100 {
                            break;
                        }
                    }
                    all
                }
                _ => return Err(Error::Input("Mix seeds must be artists or albums")),
            };
            items.shuffle(&mut rand::thread_rng());
            groups.push(items.into_iter());
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut tracks = Vec::new();
        loop {
            let mut advanced = false;
            for group in &mut groups {
                if let Some(item) = group.next() {
                    advanced = true;
                    if seen.insert(item.rating_key.clone()) {
                        tracks.push(item);
                    }
                }
            }
            if !advanced || tracks.len() >= 1000 {
                break;
            }
        }
        let mut result = self.execute_inner(Command::Play {
            items: tracks,
            index: 0,
        })?;
        result["mix"] = json!(true);
        Ok(result)
    }
    pub(crate) fn download_rows(&self) -> Vec<Value> {
        self.download_rows_from_status(&self.cache.status(&self.cache_namespace()))
    }
    pub(crate) fn download_rows_from_status(&self, status: &Value) -> Vec<Value> {
        let planned: std::collections::BTreeMap<_, _> = self
            .manual_cache
            .iter()
            .map(|i| (&i.rating_key, i))
            .collect();
        self.settings.download_groups.iter().map(|(group,keys)| {
            let items:Vec<_>=keys.iter().filter_map(|key|planned.get(key).copied()).collect();
            let mut ready=0;let mut bytes=0;
            for item in &items{if let Some(size)=status["readyBytes"][&item.rating_key].as_u64(){ready+=1;bytes+=size;}}
            let title=self.settings.download_titles.get(group).cloned().or_else(||items.first().map(|i|if group.starts_with("track:"){i.title.clone()}else{i.parent_title.clone()})).filter(|s|!s.is_empty()).unwrap_or_else(||group.split(':').next().unwrap_or("Download").into());
            let error=keys.iter().find_map(|key|status["errors"][key].as_str()).unwrap_or("");
            json!({"group":group,"title":title,"ready":ready,"total":keys.len(),"bytes":bytes,"duration":items.iter().map(|i|i.duration).sum::<u64>(),"refreshable":self.settings.download_plans.contains_key(group) || group.starts_with("playlist:") || group.starts_with("album:"),"minutes":self.settings.download_plans.get(group).map_or(0,|p|p.minutes),"active":keys.iter().any(|k|Some(k.as_str())==status["activeKey"].as_str()),"error":error})
        }).collect()
    }
    pub(crate) fn download_action(&mut self, group: &str, action: &str) -> Result<Value> {
        if !self.settings.download_groups.contains_key(group) {
            return Err(Error::Input("Download group no longer exists"));
        }
        match action {
            "refresh" => return self.refresh_download(group),
            "retry" => {
                self.settings.downloads_paused = false;
                self.cache.cancel();
                self.schedule_cache();
            }
            "cancel" | "remove" => {
                let keys = self
                    .settings
                    .download_groups
                    .remove(group)
                    .unwrap_or_default();
                self.settings.download_titles.remove(group);
                self.settings.download_plans.remove(group);
                let keep: std::collections::BTreeSet<_> = self
                    .settings
                    .download_groups
                    .values()
                    .flatten()
                    .cloned()
                    .collect();
                self.manual_cache.retain(|i| keep.contains(&i.rating_key));
                self.cache.cancel();
                self.schedule_cache();
                if action == "remove" {
                    for key in keys {
                        if !keep.contains(&key)
                            && self.queue.track().is_none_or(|i| i.rating_key != key)
                        {
                            self.cache.remove(&self.cache_namespace(), &key)?;
                        }
                    }
                }
            }
            _ => return Err(Error::Input("Unknown download action")),
        }
        Ok(json!({"cache":self.cache_status(),"downloads":self.download_rows()}))
    }
    pub(crate) fn history_status(&self) -> Value {
        json!({"pending":self.settings.history.iter().filter(|e|!e.delivered && e.namespace==self.cache_namespace()).count()})
    }
    pub(crate) fn record_play(
        &mut self,
        key: &str,
        occurrence: &str,
        listened: u64,
        duration: u64,
    ) -> Result<Value> {
        if !self.writable
            || occurrence != self.settings.playback.occurrence
            || self.queue.track().is_none_or(|i| i.rating_key != key)
        {
            return Ok(json!({}));
        }
        let listened = listened.min(duration);
        self.settings.playback.listened = self.settings.playback.listened.max(listened);
        let previously_qualified = self.settings.listens.iter().any(|listen| {
            listen.occurrence == occurrence && listen.namespace == self.cache_namespace()
        });
        if crate::history::qualifies(listened, duration) {
            if let Some(listen) = self
                .settings
                .listens
                .iter_mut()
                .find(|listen| listen.occurrence == occurrence)
            {
                listen.listened_ms = listen.listened_ms.max(listened);
            } else if let Some(track) = self.queue.track() {
                let artist = if track.original_title.is_empty() {
                    track.grandparent_title.clone()
                } else {
                    track.original_title.clone()
                };
                let listen = crate::discovery::Listen {
                    namespace: self.cache_namespace(),
                    key: key.into(),
                    occurrence: occurrence.into(),
                    played_at: crate::history::now(),
                    listened_ms: listened,
                    title: track.title.clone(),
                    artist,
                    album: track.parent_title.clone(),
                    artist_key: track.grandparent_rating_key.clone(),
                    album_key: track.parent_rating_key.clone(),
                };
                if self.settings.listens.len() >= 10000 {
                    self.settings.listens.remove(0);
                }
                self.settings.listens.push(listen);
            }
        }
        if crate::history::qualifies(listened, duration)
            && !previously_qualified
            && !self
                .settings
                .history
                .iter()
                .any(|e| e.occurrence == occurrence)
        {
            if self.settings.history.len() >= 2000 {
                self.settings.history.retain(|e| !e.delivered);
            }
            if self.settings.history.len() >= 2000 {
                return Err(Error::Input(
                    "Listening history sync is needed before recording more plays",
                ));
            }
            self.settings.history.push(crate::history::Play {
                namespace: self.cache_namespace(),
                key: crate::numeric(key)?.into(),
                occurrence: occurrence.into(),
                played_at: crate::history::now(),
                delivered: false,
            });
        }
        store::save(&self.dir, &self.settings)?;
        Ok(json!({"history":self.history_status()}))
    }
    pub(crate) fn sync_history(&mut self) -> Result<Value> {
        if !self.writable || self.offline_mode {
            return Ok(json!({"history":self.history_status()}));
        }
        let ns = self.cache_namespace();
        let mut error = String::new();
        let pending: Vec<_> = self
            .settings
            .history
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.delivered && e.namespace == ns)
            .map(|(i, _)| i)
            .take(20)
            .collect();
        for index in pending {
            let key = self.settings.history[index].key.clone();
            match self.plex.mutate(
                &self.base()?,
                &self.settings.server_token,
                Method::GET,
                "/:/scrobble",
                &[
                    ("key", key),
                    ("identifier", "com.plexapp.plugins.library".into()),
                ],
            ) {
                Ok(()) => {
                    self.settings.history[index].delivered = true;
                    store::save(&self.dir, &self.settings)?;
                }
                Err(e) => {
                    error = e.to_string();
                    break;
                }
            }
        }
        let mut status = self.history_status();
        status["error"] = json!(error);
        Ok(json!({"history":status}))
    }
}
