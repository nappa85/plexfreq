//! Local presentation preferences and measured, credential-scoped listening insights.
use crate::{Core, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Layout {
    pub hidden: Vec<String>,
    pub order: Vec<String>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Listen {
    pub namespace: String,
    pub key: String,
    pub occurrence: String,
    pub played_at: u64,
    pub listened_ms: u64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub artist_key: String,
    pub album_key: String,
}
impl Core {
    pub(crate) fn discovery_layout(&self, section: &str) -> Layout {
        self.settings
            .discovery
            .get(&format!("{}:{section}", self.cache_namespace()))
            .cloned()
            .unwrap_or_default()
    }
    pub(crate) fn discovery_config(
        &mut self,
        section: &str,
        hidden: Vec<String>,
        order: Vec<String>,
    ) -> Result<Value> {
        crate::numeric(section)?;
        if hidden.len() > 64
            || order.len() > 64
            || hidden
                .iter()
                .chain(&order)
                .any(|id| id.is_empty() || id.len() > 512 || id.chars().any(char::is_control))
        {
            return Err(Error::Input("Invalid discovery layout"));
        }
        let layout = Layout { hidden, order };
        self.settings.discovery.insert(
            format!("{}:{section}", self.cache_namespace()),
            layout.clone(),
        );
        if self.writable {
            crate::store::save(&self.dir, &self.settings)?;
        }
        Ok(json!({"discoveryLayout":layout}))
    }
    pub(crate) fn hub_items(&self, key: &str, start: usize) -> Result<Value> {
        let url = crate::plex::server_path(&self.base()?, key)?;
        if !["/hubs/", "/library/", "/playlists/"]
            .iter()
            .any(|prefix| key.starts_with(prefix) && url.path().starts_with(prefix))
        {
            return Err(Error::Input("Expected a server-relative Plex path"));
        }
        self.page(key, Vec::new(), start)
    }
    pub(crate) fn listening_insights(&self, days: u32) -> Result<Value> {
        if ![0, 7, 30, 90].contains(&days) {
            return Err(Error::Input(
                "Choose 7, 30, 90 days or all listening history",
            ));
        }
        let namespace = self.cache_namespace();
        let since = if days == 0 {
            0
        } else {
            crate::history::now().saturating_sub(u64::from(days) * 86400)
        };
        let listens: Vec<_> = self
            .settings
            .listens
            .iter()
            .filter(|listen| listen.namespace == namespace && listen.played_at >= since)
            .collect();
        let aggregate = |kind: &str| {
            let mut groups: BTreeMap<String, (String, u64, u64)> = BTreeMap::new();
            for listen in &listens {
                let (key, title) = match kind {
                    "artist" => (&listen.artist_key, &listen.artist),
                    "album" => (&listen.album_key, &listen.album),
                    _ => (&listen.key, &listen.title),
                };
                let identity = if key.is_empty() {
                    format!("title:{title}")
                } else {
                    key.clone()
                };
                let row = groups.entry(identity).or_insert((title.clone(), 0, 0));
                row.1 += 1;
                row.2 = row.2.saturating_add(listen.listened_ms);
            }
            let mut rows: Vec<_> = groups.into_iter().collect();
            rows.sort_by_key(|(key, (_, plays, _))| (std::cmp::Reverse(*plays), key.clone()));
            rows.into_iter().take(20).map(|(key,(title,plays,listened))|json!({"ratingKey":if key.starts_with("title:"){String::new()}else{key},"type":kind,"title":title,"plays":plays,"listenedMs":listened})).collect::<Vec<_>>()
        };
        Ok(
            json!({"insights":{"days":days,"plays":listens.len(),"listenedMs":listens.iter().map(|l|l.listened_ms).sum::<u64>(),"artists":aggregate("artist"),"albums":aggregate("album"),"tracks":aggregate("track"),"localOnly":true}}),
        )
    }
    pub(crate) fn sonic_journey(&self, section: &str, keys: Vec<String>) -> Result<Value> {
        if !(2..=8).contains(&keys.len()) {
            return Err(Error::Input("Choose 2–8 tracks for a sonic journey"));
        }
        let mut items = Vec::new();
        let mut offline = false;
        for pair in keys.windows(2) {
            let page = self.sonic_adventure(section, &pair[0], &pair[1])?;
            offline |= page["offline"] == true;
            let path = page["items"]
                .as_array()
                .ok_or(Error::ProtocolAt("sonic adventure"))?;
            if path.is_empty() {
                return Err(Error::Input("Plex returned no sonic recommendations for this track. Check the music library's Sonic Analysis setting and completion, or use Artist Radio."));
            }
            for item in path {
                if items
                    .last()
                    .is_some_and(|last: &Value| last["ratingKey"] == item["ratingKey"])
                {
                    continue;
                }
                items.push(item.clone());
            }
            if items.len() > 250 {
                return Err(Error::Input("Sonic journey exceeds 250 tracks"));
            }
        }
        Ok(json!({"items":items,"start":0,"next":0,"hasMore":false,"offline":offline}))
    }
}
