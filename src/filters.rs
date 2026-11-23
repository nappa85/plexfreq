//! Validated music filters and server-owned smart playlist sources.
use crate::{
    model::ItemKind,
    server::{
        unsupported, Action, Backend, BrowseSpec, FilterField, MediaServer, Page, PlaylistContent,
        Query, Sort,
    },
    Core, Error, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The user's music filter, as the UI sends it. Empty strings and `None`
/// mean "any".
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Music {
    /// Genre id (a server identifier).
    pub genre: String,
    /// Mood id.
    pub mood: String,
    /// Style id.
    pub style: String,
    /// Title contains.
    pub title: String,
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    /// Minimum user rating, 0-10.
    pub min_rating: Option<u8>,
    pub unplayed: bool,
}
impl Music {
    /// Refuse values out of range, filters the server cannot apply and ids
    /// it would not accept, for a listing of `kind`.
    fn validate(&self, kind: &ItemKind, server: &Backend) -> Result<()> {
        music_kind(kind)?;
        let caps = server.capabilities();
        if !self.mood.is_empty() && !caps.mood_filter
            || !self.style.is_empty() && !caps.style_filter
            || self.min_rating.is_some() && !caps.rating_filter
        {
            return Err(unsupported());
        }
        if self.title.len() > 512
            || *kind == ItemKind::Artist && (self.year_from.is_some() || self.year_to.is_some())
            || self.year_from.is_some_and(|v| !(1..=9999).contains(&v))
            || self.year_to.is_some_and(|v| !(1..=9999).contains(&v))
            || self.year_from.zip(self.year_to).is_some_and(|(a, b)| a > b)
            || self.min_rating.is_some_and(|r| r > 10)
        {
            return Err(Error::Input("Invalid music filter values"));
        }
        for value in [&self.genre, &self.mood, &self.style] {
            if !value.is_empty() {
                server.valid_id(value)?;
            }
        }
        Ok(())
    }
    /// Whether the filter restricts anything.
    fn is_active(&self) -> bool {
        !self.genre.is_empty()
            || !self.mood.is_empty()
            || !self.style.is_empty()
            || !self.title.trim().is_empty()
            || self.year_from.is_some()
            || self.year_to.is_some()
            || self.min_rating.is_some()
            || self.unplayed
    }
}

/// Refuse a listing kind that is not artist, album or track.
pub(crate) fn music_kind(kind: &ItemKind) -> Result<()> {
    if kind.is_music() {
        Ok(())
    } else {
        Err(Error::Input("Unknown music type"))
    }
}

/// UI sort names, shared by plain browsing, filtered browsing and smart
/// playlist rules.
const SORTS: [(&str, Sort); 6] = [
    ("title", Sort::Title),
    ("newest", Sort::Added),
    ("year", Sort::Year),
    ("played", Sort::LastPlayed),
    ("popular", Sort::PlayCount),
    ("random", Sort::Random),
];

/// The sort a UI name stands for, for a listing of `kind`. Artists carry no
/// year, so a year sort is rejected for them.
pub(crate) fn sort(name: &str, kind: &ItemKind) -> Result<Sort> {
    SORTS
        .iter()
        .find(|(n, _)| *n == name && !(name == "year" && *kind == ItemKind::Artist))
        .map(|(_, sort)| *sort)
        .ok_or(Error::Input("Unknown library sort"))
}

fn sort_name(sort: Sort) -> &'static str {
    SORTS
        .iter()
        .find(|(_, s)| *s == sort)
        .map_or("title", |(name, _)| name)
}

/// Borrowed view of owned query pairs for the offline library store.
pub(crate) fn param_refs(params: &[(String, String)]) -> Vec<(&str, String)> {
    params
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect()
}

impl Core {
    pub(crate) fn smart_rules(&self, key: &str) -> Result<Value> {
        let playlist = self.playlist_metadata(key)?;
        if !playlist.smart {
            return Err(Error::Input(
                "Smart playlist contents are controlled by their Plex filters",
            ));
        }
        let source = self.backend.smart_source(&playlist.content);
        let editable = source.editable
            && self.id(&source.section).is_ok()
            && source
                .filter
                .validate(&ItemKind::Track, &self.backend)
                .is_ok()
            && (1..=1000).contains(&source.limit);
        Ok(
            json!({"smartKey":key,"smartSection":source.section,"smartFilters":source.filter,"smartSort":sort_name(source.sort),"smartLimit":source.limit,"smartEditable":editable}),
        )
    }
    /// The `meta` of the server's filter schema for listings of `kind`
    /// (memoized per server), or None when the backend has no schema.
    fn filter_schema(&self, section: &str, kind: &ItemKind) -> Result<Option<Value>> {
        self.id(section)?;
        let Some(query) = self.backend.filter_schema(section, kind) else {
            return Ok(None);
        };
        let key = format!("{}:{section}:{kind}", self.cache_namespace());
        if let Some(meta) = self.filter_schemas.borrow().get(&key) {
            return Ok(Some(meta.clone()));
        }
        let (container, _) = self.container_cached(&query, Some(Page { start: 0, size: 0 }))?;
        self.filter_schemas
            .borrow_mut()
            .insert(key, container.meta.clone());
        Ok(Some(container.meta))
    }
    /// Validate `filters` for a listing of `kind` and learn the server's
    /// spelling of the fields they use (one schema read per server, section
    /// and kind; none for an inactive filter).
    fn filter_fields(
        &self,
        section: &str,
        kind: &ItemKind,
        filters: &Music,
    ) -> Result<Vec<(FilterField, String)>> {
        filters.validate(kind, &self.backend)?;
        if !filters.is_active() {
            return Ok(vec![]);
        }
        Ok(match self.filter_schema(section, kind)? {
            Some(meta) => self.backend.filter_fields(&meta, kind),
            None => vec![],
        })
    }
    pub(crate) fn filter_options(
        &self,
        section: &str,
        kind: &ItemKind,
        field: &str,
    ) -> Result<Value> {
        music_kind(kind)?;
        let caps = self.backend.capabilities();
        let choice = match field {
            "genre" => FilterField::Genre,
            "mood" if caps.mood_filter => FilterField::Mood,
            "style" if caps.style_filter => FilterField::Style,
            "mood" | "style" => return Err(unsupported()),
            _ => return Err(Error::Input("Invalid music filter values")),
        };
        let meta = self.filter_schema(section, kind)?;
        let Some(query) = self
            .backend
            .filter_choices(section, kind, choice, meta.as_ref())?
        else {
            return Ok(
                json!({"filterSection":section,"filterKind":kind,"filterField":field,"filterChoices":[],"filterAvailable":false}),
            );
        };
        let (choices, _) = self.container_cached(&query, None)?;
        Ok(
            json!({"filterSection":section,"filterKind":kind,"filterField":field,"filterChoices":choices.sections.iter().filter(|c|self.id(&c.key).is_ok()).take(1000).map(|c|json!({"key":c.key,"title":c.title})).collect::<Vec<_>>(),"filterAvailable":true}),
        )
    }
    pub(crate) fn filtered_browse(
        &self,
        section: &str,
        kind: &ItemKind,
        sort_name: &str,
        filters: &Music,
        start: usize,
    ) -> Result<Value> {
        self.id(section)?;
        // Validation and the schema read come before the sort check, as
        // they always have.
        let fields = self.filter_fields(section, kind, filters)?;
        let spec = BrowseSpec {
            filter: filters.clone(),
            fields,
            ..BrowseSpec::new(kind.clone(), sort(sort_name, kind)?)
        };
        let (container, offline) = self.container_cached(
            &Query::browse(section, spec),
            Some(Page { start, size: 100 }),
        )?;
        let items: Vec<_> = container
            .items
            .iter()
            .filter(|i| i.kind == *kind)
            .map(|i| self.display_item(i))
            .collect();
        Ok(crate::pagination::page(
            items,
            start,
            container.size,
            100,
            container.total_size,
            offline,
        ))
    }
    pub(crate) fn smart_playlist(
        &mut self,
        action: crate::SmartAction,
        target: (&str, &str),
        title: &str,
        sort_name: &str,
        limit: usize,
        filters: &Music,
    ) -> Result<Value> {
        let (key, section) = target;
        self.online_edit()?;
        self.id(section)?;
        if !(1..=1000).contains(&limit)
            || title.trim().len() > 512
            || action == crate::SmartAction::Create && title.trim().is_empty()
        {
            return Err(Error::Input("Enter a playlist name (up to 512 bytes)"));
        }
        // Refuse what the server cannot apply before reading anything.
        filters.validate(&ItemKind::Track, &self.backend)?;
        if action == crate::SmartAction::Update && !self.playlist_metadata(key)?.smart {
            return Err(Error::Input(
                "Smart playlist contents are controlled by their Plex filters",
            ));
        }
        let fields = self.filter_fields(section, &ItemKind::Track, filters)?;
        let spec = BrowseSpec {
            filter: filters.clone(),
            fields,
            limit: Some(limit),
            ..BrowseSpec::new(ItemKind::Track, sort(sort_name, &ItemKind::Track)?)
        };
        let content = PlaylistContent::Smart {
            section: section.into(),
            spec,
        };
        if action == crate::SmartAction::Create {
            let container = self
                .backend
                .create_playlist(&self.conn()?, title.trim(), &content)?;
            let item = container
                .items
                .first()
                .filter(|i| i.kind == crate::model::ItemKind::Playlist && i.smart)
                .ok_or(Error::ProtocolAt("created playlist"))?;
            self.library.invalidate_playlist(
                &self.backend,
                &self.cache_namespace(),
                &item.rating_key,
            );
            Ok(json!({"createdPlaylist":self.display_item(item),"playlistChanged":item.rating_key}))
        } else {
            self.backend.act(
                &self.conn()?,
                &Action::PlaylistAdd {
                    id: key.into(),
                    content,
                },
            )?;
            self.library
                .invalidate_playlist(&self.backend, &self.cache_namespace(), key);
            Ok(json!({"playlistChanged":key}))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_names_round_trip_and_artist_year_is_rejected() {
        for (name, value) in SORTS {
            assert_eq!(sort(name, &ItemKind::Album).unwrap(), value);
            assert_eq!(sort_name(value), name);
        }
        for value in Sort::ALL {
            assert!(SORTS.iter().any(|(_, s)| *s == value), "{value:?}");
        }
        assert!(sort("year", &ItemKind::Artist).is_err());
        assert!(sort("title", &ItemKind::Artist).is_ok());
        assert!(sort("unknown", &ItemKind::Track).is_err());
    }
}
