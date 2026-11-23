//! Jellyfin wire shapes (`BaseItemDto`, `QueryResult`, lyrics, authentication
//! replies) and their conversion to the neutral [`model`](crate::model).
//! Every field is optional on the wire: a missing or null value defaults, and
//! one malformed item is skipped instead of failing its page.
use crate::{
    lyrics::Line,
    model::{Container, EntryId, Hub, Item, ItemKind, Media, Part, Section, Stream},
    Error, Result,
};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::collections::HashMap;

/// A null JSON value is the default, like a missing one.
fn nullable<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// 100-nanosecond ticks per millisecond.
const TICKS_PER_MS: u64 = 10_000;

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct NameId {
    #[serde(deserialize_with = "nullable")]
    id: String,
    #[serde(deserialize_with = "nullable")]
    name: String,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct UserData {
    #[serde(deserialize_with = "nullable")]
    is_favorite: bool,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct MediaStream {
    #[serde(rename = "Type", deserialize_with = "nullable")]
    kind: String,
    codec: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct MediaSource {
    container: Option<String>,
    bitrate: Option<f64>,
    size: Option<f64>,
    #[serde(deserialize_with = "nullable")]
    media_streams: Vec<MediaStream>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
struct BaseItem {
    #[serde(deserialize_with = "nullable")]
    id: String,
    name: Option<String>,
    #[serde(rename = "Type", deserialize_with = "nullable")]
    kind: String,
    overview: Option<String>,
    album: Option<String>,
    album_id: Option<String>,
    album_primary_image_tag: Option<String>,
    album_artist: Option<String>,
    #[serde(deserialize_with = "nullable")]
    album_artists: Vec<NameId>,
    #[serde(deserialize_with = "nullable")]
    artists: Vec<String>,
    #[serde(deserialize_with = "nullable")]
    artist_items: Vec<NameId>,
    index_number: Option<f64>,
    parent_index_number: Option<f64>,
    production_year: Option<f64>,
    run_time_ticks: Option<f64>,
    child_count: Option<f64>,
    #[serde(deserialize_with = "nullable")]
    image_tags: HashMap<String, String>,
    #[serde(deserialize_with = "nullable")]
    backdrop_image_tags: Vec<String>,
    user_data: Option<UserData>,
    #[serde(deserialize_with = "nullable")]
    media_sources: Vec<MediaSource>,
    normalization_gain: Option<f64>,
    album_normalization_gain: Option<f64>,
    #[serde(deserialize_with = "nullable")]
    has_lyrics: bool,
    playlist_item_id: Option<String>,
    media_type: Option<String>,
}

/// A non-negative whole number out of a JSON number (fractions truncate).
fn whole(value: Option<f64>) -> Option<u64> {
    value
        .filter(|n| n.is_finite() && *n >= 0.0 && *n < u64::MAX as f64)
        .map(|n| n as u64)
}

/// The image request path of image `tag` of item `id`: the size and quality
/// every request carries, so a stored path is the whole identity of the image.
fn image(id: &str, kind: &str, tag: &str) -> String {
    let tag = url::form_urlencoded::byte_serialize(tag.as_bytes()).collect::<String>();
    format!("/Items/{id}/Images/{kind}?tag={tag}&maxWidth=1000&quality=90")
}

fn item_kind(kind: &str) -> ItemKind {
    match kind {
        "MusicArtist" => ItemKind::Artist,
        "MusicAlbum" => ItemKind::Album,
        "Audio" => ItemKind::Track,
        "Playlist" => ItemKind::Playlist,
        other => ItemKind::Other(other.to_owned()),
    }
}

/// The `Container` of the first of `container`'s comma-separated format names
/// that PlexFreq decodes itself, else the first (ffprobe names such as
/// `mov,mp4,m4a,3gp,3g2,mj2` list the family).
fn container_name(container: &str) -> String {
    let names: Vec<_> = container
        .split(',')
        .map(|n| n.trim().to_ascii_lowercase())
        .filter(|n| !n.is_empty())
        .collect();
    names
        .iter()
        .find(|n| {
            matches!(
                n.as_str(),
                "mp3" | "flac" | "mp4" | "m4a" | "m4b" | "aac" | "ogg" | "wav"
            )
        })
        .or_else(|| names.first())
        .cloned()
        .unwrap_or_default()
}

impl BaseItem {
    /// The neutral item, or None when the id is not a Jellyfin id.
    fn convert(self) -> Option<Item> {
        let id = super::normalize_id(&self.id)?;
        let kind = item_kind(&self.kind);
        let first_artist = |people: &[NameId]| {
            people
                .iter()
                .find_map(|p| super::normalize_id(&p.id).map(|id| (id, p.name.clone())))
        };
        let artist = first_artist(&self.album_artists).or_else(|| first_artist(&self.artist_items));
        let artist_name = self
            .album_artist
            .clone()
            .filter(|n| !n.is_empty())
            .or_else(|| artist.as_ref().map(|(_, name)| name.clone()))
            .or_else(|| self.artists.first().cloned())
            .unwrap_or_default();
        let mut item = Item {
            key: format!("/Items/{id}"),
            title: self.name.unwrap_or_default(),
            summary: self.overview.unwrap_or_default(),
            year: self.production_year.map_or(0, |n| n as i32),
            index: self.index_number.map_or(0, |n| n as i32),
            parent_index: self.parent_index_number.map_or(0, |n| n as i32),
            duration: whole(self.run_time_ticks).map_or(0, |ticks| ticks / TICKS_PER_MS),
            kind: kind.clone(),
            ..Default::default()
        };
        let own_image = self
            .image_tags
            .get("Primary")
            .filter(|t| !t.is_empty())
            .map(|tag| image(&id, "Primary", tag));
        let album = self
            .album_id
            .as_deref()
            .and_then(super::normalize_id)
            .zip(self.album_primary_image_tag.filter(|t| !t.is_empty()))
            .map(|(album, tag)| image(&album, "Primary", &tag));
        item.thumb = own_image.clone().unwrap_or_default();
        item.art = self
            .backdrop_image_tags
            .first()
            .map(|tag| image(&id, "Backdrop/0", tag))
            .unwrap_or_default();
        if let Some(data) = &self.user_data {
            item.user_rating = if data.is_favorite { 10. } else { 0. };
        }
        match kind {
            ItemKind::Track => {
                item.parent_title = self.album.unwrap_or_default();
                item.parent_rating_key = self
                    .album_id
                    .as_deref()
                    .and_then(super::normalize_id)
                    .unwrap_or_default();
                item.grandparent_title = artist_name.clone();
                item.grandparent_rating_key = artist.map(|(id, _)| id).unwrap_or_default();
                // Plex's `originalTitle`: the track artist when it differs
                // from the album artist.
                let track_artists = self.artists.join(", ");
                if !track_artists.is_empty() && track_artists != artist_name {
                    item.original_title = track_artists;
                }
                // A track without art of its own shows its album's.
                item.parent_thumb = album.unwrap_or_default();
                item.media = self
                    .media_sources
                    .first()
                    .map(|source| {
                        let codec = source
                            .media_streams
                            .iter()
                            .find(|s| s.kind == "Audio")
                            .and_then(|s| s.codec.clone())
                            .unwrap_or_default()
                            .to_ascii_lowercase();
                        let mut streams = vec![Stream {
                            stream_type: 2,
                            gain: self.normalization_gain.filter(|g| g.is_finite()),
                            album_gain: self.album_normalization_gain.filter(|g| g.is_finite()),
                            ..Default::default()
                        }];
                        if self.has_lyrics {
                            streams.push(Stream {
                                stream_type: 4,
                                key: format!("/Audio/{id}/Lyrics"),
                                format: "json".into(),
                                ..Default::default()
                            });
                        }
                        Media {
                            audio_codec: codec,
                            container: container_name(source.container.as_deref().unwrap_or("")),
                            bitrate: whole(source.bitrate).map_or(0, |bps| (bps / 1000) as u32),
                            parts: vec![Part {
                                key: format!("/Audio/{id}/stream?static=true"),
                                size: whole(source.size),
                                streams,
                            }],
                        }
                    })
                    .into_iter()
                    .collect();
            }
            ItemKind::Album => {
                // Like Plex, an album's parent is its artist.
                item.parent_title = artist_name;
                item.parent_rating_key = artist.map(|(id, _)| id).unwrap_or_default();
                item.leaf_count = whole(self.child_count).map(|n| n as usize);
            }
            ItemKind::Playlist => {
                item.leaf_count = whole(self.child_count).map(|n| n as usize);
                item.playlist_type = "audio".into();
            }
            ItemKind::Artist | ItemKind::Other(_) => {}
        }
        item.playlist_item_id = self
            .playlist_item_id
            .filter(|id| !id.is_empty())
            .map(|id| EntryId::from(id.as_str()));
        item.rating_key = id;
        Some(item)
    }
}

/// A raw item as a neutral one; None for a malformed item or a playlist that
/// is not an audio playlist.
fn convert(raw: Value) -> Option<Item> {
    let item: BaseItem = serde_json::from_value(raw).ok()?;
    if item.kind == "Playlist"
        && item
            .media_type
            .as_deref()
            .is_some_and(|t| !t.eq_ignore_ascii_case("audio"))
    {
        return None;
    }
    item.convert()
}

/// The items of a reply and its total: a `QueryResult` object or the bare
/// array `/Items/Latest` returns. `raw` is how many entries the server sent
/// (the page's size, whatever was skipped).
pub(super) struct Items {
    pub items: Vec<Item>,
    /// The `PlaylistItemId` of every raw entry, in order; an empty string for
    /// an entry that is malformed or has none, so positions stay right.
    pub entry_ids: Vec<String>,
    pub raw: usize,
    pub total: Option<usize>,
    pub start: usize,
}

pub(super) fn items(body: &[u8]) -> Result<Items> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| Error::ProtocolAt("server library request"))?;
    let (entries, total, start) = match value {
        Value::Array(entries) => (entries, None, 0),
        Value::Object(mut object) => {
            let Some(Value::Array(entries)) = object.remove("Items") else {
                return Err(Error::ProtocolAt("server library request"));
            };
            (
                entries,
                whole(object.get("TotalRecordCount").and_then(Value::as_f64)).map(|n| n as usize),
                whole(object.get("StartIndex").and_then(Value::as_f64)).map_or(0, |n| n as usize),
            )
        }
        _ => return Err(Error::ProtocolAt("server library request")),
    };
    let raw = entries.len();
    let entry_ids = entries
        .iter()
        .map(|e| {
            e.get("PlaylistItemId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    Ok(Items {
        entry_ids,
        items: entries.into_iter().filter_map(convert).collect(),
        raw,
        total,
        start,
    })
}

/// A page of items as the neutral container.
pub(super) fn container(body: &[u8]) -> Result<Container> {
    let Items {
        items,
        raw,
        entry_ids: _,
        total,
        start,
    } = items(body)?;
    Ok(Container {
        items,
        size: raw,
        total_size: total,
        offset: start,
        ..Default::default()
    })
}

/// A listing of named things (genres) as the container's sections.
pub(super) fn named_sections(body: &[u8]) -> Result<Container> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| Error::ProtocolAt("server library request"))?;
    let entries = value["Items"]
        .as_array()
        .ok_or(Error::ProtocolAt("server library request"))?;
    let sections: Vec<Section> = entries
        .iter()
        .filter_map(|entry| {
            let key = super::normalize_id(entry["Id"].as_str()?)?;
            Some(Section {
                key,
                title: entry["Name"].as_str().unwrap_or_default().to_owned(),
                kind: ItemKind::Other("genre".into()),
                size: 0,
            })
        })
        .collect();
    Ok(Container {
        size: entries.len(),
        sections,
        ..Default::default()
    })
}

/// The music libraries of `/UserViews` as sections (a music library is an
/// "artist" section, like Plex's).
pub(super) fn views(body: &[u8]) -> Result<Container> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| Error::ProtocolAt("server library request"))?;
    let entries = value["Items"]
        .as_array()
        .ok_or(Error::ProtocolAt("server library request"))?;
    let sections: Vec<Section> = entries
        .iter()
        .filter(|view| {
            view["CollectionType"]
                .as_str()
                .is_some_and(|t| t.eq_ignore_ascii_case("music"))
        })
        .filter_map(|view| {
            Some(Section {
                key: super::normalize_id(view["Id"].as_str()?)?,
                title: view["Name"].as_str().unwrap_or_default().to_owned(),
                kind: ItemKind::Artist,
                size: 0,
            })
        })
        .collect();
    Ok(Container {
        size: entries.len(),
        sections,
        ..Default::default()
    })
}

/// A synthesised hub of items.
pub(super) fn hub(identifier: String, title: &str, key: String, items: Vec<Item>) -> Hub {
    Hub {
        key,
        more: true,
        title: title.into(),
        hub_identifier: identifier,
        items,
    }
}

/// `Lyrics[].{Text, Start}` as lyric lines: `Start` is in ticks and absent
/// for unsynchronised lyrics. The document's `Metadata.Offset` is not
/// applied: whether `Start` already includes it is not documented, and
/// applying it to a value that has it would shift every line twice.
pub(super) fn lyrics(body: &[u8]) -> Result<Vec<Line>> {
    let value: Value =
        serde_json::from_slice(body).map_err(|_| Error::ProtocolAt("lyrics JSON"))?;
    let lines = value["Lyrics"]
        .as_array()
        .ok_or(Error::ProtocolAt("lyric lines"))?;
    Ok(lines
        .iter()
        .take(10_000)
        .map(|line| Line {
            time: whole(line["Start"].as_f64()).map(|ticks| ticks / TICKS_PER_MS),
            text: line["Text"].as_str().unwrap_or_default().to_owned(),
        })
        .collect())
}
