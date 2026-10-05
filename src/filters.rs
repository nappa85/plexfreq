//! Validated music filters and server-owned smart playlist sources.
use crate::{Core, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Music {
    pub genre: String,
    pub mood: String,
    pub style: String,
    pub title: String,
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    pub min_rating: Option<u8>,
    pub unplayed: bool,
}
impl Music {
    fn params(&self, kind: &str) -> Result<Vec<(String, String)>> {
        if !matches!(kind, "artist" | "album" | "track") {
            return Err(Error::Input("Unknown music type"));
        }
        if self.title.len() > 512
            || kind == "artist" && (self.year_from.is_some() || self.year_to.is_some())
            || self.year_from.is_some_and(|v| !(1..=9999).contains(&v))
            || self.year_to.is_some_and(|v| !(1..=9999).contains(&v))
            || self.year_from.zip(self.year_to).is_some_and(|(a, b)| a > b)
            || self.min_rating.is_some_and(|r| r > 10)
        {
            return Err(Error::Input("Invalid music filter values"));
        }
        let mut params = Vec::new();
        for (name, value) in [
            ("genre", &self.genre),
            ("mood", &self.mood),
            ("style", &self.style),
        ] {
            if !value.is_empty() {
                crate::numeric(value)?;
                params.push((format!("{kind}.{name}"), value.clone()));
            }
        }
        if !self.title.trim().is_empty() {
            params.push(("title".into(), self.title.trim().into()));
        }
        if let Some(year) = self.year_from {
            params.push((format!("{kind}.year>="), year.to_string()));
        }
        if let Some(year) = self.year_to {
            params.push((format!("{kind}.year<="), year.to_string()));
        }
        if let Some(rating) = self.min_rating {
            params.push((format!("{kind}.userRating>="), rating.to_string()));
        }
        if self.unplayed {
            params.push((format!("{kind}.viewCount"), "0".into()));
        }
        Ok(params)
    }
}
fn kind_code(kind: &str) -> Result<&'static str> {
    match kind {
        "artist" => Ok("8"),
        "album" => Ok("9"),
        "track" => Ok("10"),
        _ => Err(Error::Input("Unknown music type")),
    }
}
fn sort_key(sort: &str) -> Result<&'static str> {
    match sort {
        "title" => Ok("titleSort:asc"),
        "newest" => Ok("addedAt:desc"),
        "year" => Ok("year:desc"),
        "played" => Ok("lastViewedAt:desc"),
        "popular" => Ok("viewCount:desc"),
        "random" => Ok("random"),
        _ => Err(Error::Input("Unknown library sort")),
    }
}
impl Core {
    pub(crate) fn smart_rules(&self, key: &str) -> Result<Value> {
        let playlist = self.playlist_metadata(key)?;
        if !playlist.smart {
            return Err(Error::Input(
                "Smart playlist contents are controlled by their Plex filters",
            ));
        }
        let content = if playlist.content.starts_with("server%")
            || playlist.content.starts_with("library%")
        {
            url::form_urlencoded::parse(format!("uri={}", playlist.content).as_bytes())
                .next()
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default()
        } else {
            playlist.content
        };
        let parsed = url::Url::parse(&content).ok();
        let mut editable = true;
        let mut filters = Music::default();
        let mut section = String::new();
        let mut sort = "title".to_string();
        let mut limit = 100usize;
        if let Some(uri) = parsed {
            section = uri
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
                    "year>=" => filters.year_from = value.parse().ok(),
                    "year<=" => filters.year_to = value.parse().ok(),
                    "userRating>=" => filters.min_rating = value.parse().ok(),
                    "viewCount" if value == "0" => filters.unplayed = true,
                    "limit" => limit = value.parse().unwrap_or(0),
                    "sort" => {
                        sort = match value.as_ref() {
                            "titleSort:asc" => "title",
                            "addedAt:desc" => "newest",
                            "year:desc" => "year",
                            "lastViewedAt:desc" => "played",
                            "viewCount:desc" => "popular",
                            "random" => "random",
                            _ => {
                                editable = false;
                                "title"
                            }
                        }
                        .into()
                    }
                    "type" if value == "10" => {}
                    "includeGuids" => {}
                    _ => editable = false,
                }
            }
        } else {
            editable = false;
        }
        editable &= crate::numeric(&section).is_ok()
            && filters.params("track").is_ok()
            && (1..=1000).contains(&limit);
        Ok(
            json!({"smartKey":key,"smartSection":section,"smartFilters":filters,"smartSort":sort,"smartLimit":limit,"smartEditable":editable}),
        )
    }
    fn filter_schema(&self, section: &str, kind: &str) -> Result<Value> {
        crate::numeric(section)?;
        let key = format!("{}:{section}:{kind}", self.cache_namespace());
        if let Some(meta) = self.filter_schemas.borrow().get(&key) {
            return Ok(meta.clone());
        }
        let (container, _) = self.container_cached(
            &format!("/library/sections/{section}/all"),
            &[
                ("type", kind_code(kind)?.into()),
                ("includeMeta", "1".into()),
                ("includeAdvanced", "1".into()),
                ("X-Plex-Container-Start", "0".into()),
                ("X-Plex-Container-Size", "0".into()),
            ],
        )?;
        self.filter_schemas
            .borrow_mut()
            .insert(key, container.meta.clone());
        Ok(container.meta)
    }
    fn filter_params(
        &self,
        section: &str,
        kind: &str,
        filters: &Music,
    ) -> Result<Vec<(String, String)>> {
        let mut params = filters.params(kind)?;
        // Metadata exposes canonical field scopes (e.g. album.genre for tracks).
        // Older servers can omit Meta; their documented type-scoped keys are used.
        if !params.is_empty() {
            let meta = self.filter_schema(section, kind)?;
            if let Some(types) = meta["Type"].as_array() {
                let mut scopes: Vec<_> = types.iter().collect();
                scopes.sort_by_key(|scope| scope["type"] != kind);
                let fields: Vec<_> = scopes
                    .into_iter()
                    .flat_map(|t| t["Field"].as_array().into_iter().flatten())
                    .filter_map(|field| field["key"].as_str())
                    .collect();
                for (key, _) in &mut params {
                    if key == "title" {
                        continue;
                    }
                    let base = key.trim_end_matches(['>', '<', '=']);
                    let operator = &key[base.len()..];
                    let suffix = base.split('.').next_back().unwrap_or(base);
                    if let Some(canonical) =
                        fields.iter().find(|field| **field == base).or_else(|| {
                            fields
                                .iter()
                                .find(|field| field.ends_with(&format!(".{suffix}")))
                        })
                    {
                        *key = format!("{canonical}{operator}");
                    }
                }
            }
        }
        Ok(params)
    }
    pub(crate) fn filter_options(&self, section: &str, kind: &str, field: &str) -> Result<Value> {
        if !matches!(field, "genre" | "mood" | "style") {
            return Err(Error::Input("Invalid music filter values"));
        }
        let meta = self.filter_schema(section, kind)?;
        let key = meta["Type"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["type"] == kind)
            .flat_map(|t| t["Filter"].as_array().into_iter().flatten())
            .find(|f| f["filter"] == field)
            .and_then(|f| f["key"].as_str());
        let Some(key) = key else {
            return Ok(
                json!({"filterSection":section,"filterKind":kind,"filterField":field,"filterChoices":[],"filterAvailable":false}),
            );
        };
        let url = crate::plex::server_path(&self.base()?, key)?;
        if !url
            .path()
            .starts_with(&format!("/library/sections/{section}/"))
        {
            return Err(Error::Input("Expected a server-relative Plex path"));
        }
        let (choices, _) = self.container_cached(key, &[])?;
        Ok(
            json!({"filterSection":section,"filterKind":kind,"filterField":field,"filterChoices":choices.sections.iter().filter(|c|crate::numeric(&c.key).is_ok()).take(1000).map(|c|json!({"key":c.key,"title":c.title})).collect::<Vec<_>>(),"filterAvailable":true}),
        )
    }
    pub(crate) fn filtered_browse(
        &self,
        section: &str,
        kind: &str,
        sort: &str,
        filters: &Music,
        start: usize,
    ) -> Result<Value> {
        crate::numeric(section)?;
        let mut params = self.filter_params(section, kind, filters)?;
        params.extend([
            ("type".into(), kind_code(kind)?.into()),
            ("sort".into(), sort_key(sort)?.into()),
            ("X-Plex-Container-Start".into(), start.to_string()),
            ("X-Plex-Container-Size".into(), "100".into()),
        ]);
        let refs: Vec<_> = params
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        let (container, offline) =
            self.container_cached(&format!("/library/sections/{section}/all"), &refs)?;
        let items: Vec<_> = container
            .items
            .iter()
            .filter(|i| i.kind == kind)
            .map(|i| self.display_item(i))
            .collect();
        let next = start.saturating_add(container.size);
        Ok(
            json!({"items":items,"start":start,"next":next,"hasMore":container.total_size.map_or(container.size==100,|total|next<total),"offline":offline}),
        )
    }
    pub(crate) fn smart_playlist(
        &mut self,
        action: &str,
        target: (&str, &str),
        title: &str,
        sort: &str,
        limit: usize,
        filters: &Music,
    ) -> Result<Value> {
        let (key, section) = target;
        self.online_edit()?;
        crate::numeric(section)?;
        if !(1..=1000).contains(&limit)
            || title.trim().len() > 512
            || action == "create" && title.trim().is_empty()
        {
            return Err(Error::Input("Enter a playlist name (up to 512 bytes)"));
        }
        if !matches!(action, "create" | "update") {
            return Err(Error::Input("Unknown playlist action"));
        }
        if action == "update" && !self.playlist_metadata(key)?.smart {
            return Err(Error::Input(
                "Smart playlist contents are controlled by their Plex filters",
            ));
        }
        let mut params = self.filter_params(section, "track", filters)?;
        params.extend([
            ("type".into(), "10".into()),
            ("sort".into(), sort_key(sort)?.into()),
            ("limit".into(), limit.to_string()),
        ]);
        let encoded = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(params)
            .finish();
        let identity =
            self.plex
                .container(&self.base()?, &self.settings.server_token, "/identity", &[])?;
        let uri = crate::radio::station_uri(
            &identity.machine_identifier,
            &format!("/library/sections/{section}/all?{encoded}"),
        )?;
        if action == "create" {
            let container = self.plex.create_playlist(
                &self.base()?,
                &self.settings.server_token,
                &[
                    ("title", title.trim().into()),
                    ("type", "audio".into()),
                    ("smart", "1".into()),
                    ("uri", uri),
                ],
            )?;
            let item = container
                .items
                .first()
                .filter(|i| i.kind == "playlist" && i.smart)
                .ok_or(Error::ProtocolAt("created playlist"))?;
            self.library
                .invalidate_playlist(&self.cache_namespace(), &item.rating_key);
            Ok(json!({"createdPlaylist":self.display_item(item),"playlistChanged":item.rating_key}))
        } else {
            self.plex.mutate(
                &self.base()?,
                &self.settings.server_token,
                reqwest::Method::PUT,
                &format!("/playlists/{key}/items"),
                &[("uri", uri)],
            )?;
            self.library
                .invalidate_playlist(&self.cache_namespace(), key);
            Ok(json!({"playlistChanged":key}))
        }
    }
}
