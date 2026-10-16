use serde::{Deserialize, Serialize};

pub(crate) fn music_kind_code(kind: &ItemKind) -> crate::Result<&'static str> {
    match kind {
        ItemKind::Artist => Ok("8"),
        ItemKind::Album => Ok("9"),
        ItemKind::Track => Ok("10"),
        ItemKind::Playlist | ItemKind::Other(_) => Err(crate::Error::Input("Unknown music type")),
    }
}

/// Plex metadata `type` (also a library section's top-level type). Unknown
/// types keep their exact spelling so items round-trip unchanged through
/// session files, caches and the Qt model; the default is the empty type.
#[derive(
    Clone,
    Debug,
    PartialEq,
    Eq,
    Hash,
    Deserialize,
    Serialize,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
)]
#[serde(from = "String", into = "String")]
#[strum(serialize_all = "lowercase")]
pub enum ItemKind {
    Artist,
    Album,
    Track,
    Playlist,
    #[strum(default)]
    Other(String),
}

impl ItemKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Other(raw) => raw,
            known => known.into(),
        }
    }
    /// Artist, album or track: the kinds music browsing/filters accept.
    pub fn is_music(&self) -> bool {
        matches!(self, Self::Artist | Self::Album | Self::Track)
    }
    /// The empty type (an unset optional kind on the wire).
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Other(raw) if raw.is_empty())
    }
}

impl Default for ItemKind {
    fn default() -> Self {
        Self::Other(String::new())
    }
}

impl From<String> for ItemKind {
    fn from(raw: String) -> Self {
        match raw.parse() {
            Ok(kind) => kind,
            Err(_) => Self::Other(raw),
        }
    }
}

impl From<ItemKind> for String {
    fn from(kind: ItemKind) -> Self {
        match kind {
            ItemKind::Other(raw) => raw,
            known => known.to_string(),
        }
    }
}

impl PartialEq<str> for ItemKind {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

/// Ordered by spelling, as the former string field was (catalogue sort and
/// `(kind, key)` map iteration order are unchanged).
impl Ord for ItemKind {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}
impl PartialOrd for ItemKind {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Default, Deserialize, Serialize, Debug, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Item {
    #[serde(deserialize_with = "null_default")]
    pub rating_key: String,
    #[serde(deserialize_with = "null_default")]
    pub key: String,
    #[serde(deserialize_with = "null_default")]
    pub title: String,
    #[serde(deserialize_with = "null_default")]
    pub summary: String,
    #[serde(deserialize_with = "null_default")]
    pub art: String,
    #[serde(deserialize_with = "lenient")]
    pub year: i32,
    #[serde(deserialize_with = "lenient")]
    pub index: i32,
    #[serde(deserialize_with = "lenient")]
    pub parent_index: i32,
    #[serde(rename = "Image", deserialize_with = "lenient_vec")]
    pub images: Vec<ArtworkImage>,
    #[serde(rename = "type", deserialize_with = "null_default")]
    pub kind: ItemKind,
    #[serde(deserialize_with = "null_default")]
    pub parent_title: String,
    #[serde(deserialize_with = "null_default")]
    pub grandparent_title: String,
    #[serde(deserialize_with = "null_default")]
    pub original_title: String,
    #[serde(deserialize_with = "null_default")]
    pub thumb: String,
    #[serde(deserialize_with = "null_default")]
    pub parent_thumb: String,
    #[serde(deserialize_with = "lenient")]
    pub duration: u64,
    #[serde(deserialize_with = "optional")]
    pub leaf_count: Option<usize>,
    #[serde(deserialize_with = "optional")]
    pub duration_in_seconds: Option<u64>,
    #[serde(
        rename = "playlistItemID",
        alias = "playlistItemId",
        deserialize_with = "optional"
    )]
    pub playlist_item_id: Option<u64>,
    #[serde(deserialize_with = "radio_flag")]
    pub smart: bool,
    #[serde(deserialize_with = "radio_flag")]
    pub radio: bool,
    #[serde(deserialize_with = "null_default")]
    pub playlist_type: String,
    #[serde(deserialize_with = "null_default")]
    pub content: String,
    #[serde(deserialize_with = "null_default")]
    pub subtype: String,
    #[serde(rename = "Format", deserialize_with = "lenient_vec")]
    pub formats: Vec<Tag>,
    #[serde(rename = "Subformat", deserialize_with = "lenient_vec")]
    pub subformats: Vec<Tag>,
    #[serde(deserialize_with = "lenient_f64")]
    pub user_rating: f64,
    #[serde(deserialize_with = "lenient")]
    pub music_analysis_version: u32,
    #[serde(deserialize_with = "null_default")]
    pub parent_rating_key: String,
    #[serde(deserialize_with = "null_default")]
    pub grandparent_rating_key: String,
    #[serde(rename = "playQueueItemID", deserialize_with = "optional")]
    pub play_queue_item_id: Option<u64>,
    #[serde(rename = "Stations")]
    pub stations: serde_json::Value,
    #[serde(rename = "Media", deserialize_with = "lenient_vec")]
    pub media: Vec<Media>,
}

#[derive(Clone, Default, Deserialize, Serialize, Debug, PartialEq)]
#[serde(default)]
pub struct Tag {
    #[serde(deserialize_with = "null_default")]
    pub tag: String,
}

#[derive(Clone, Default, Deserialize, Serialize, Debug, PartialEq)]
#[serde(default)]
pub struct ArtworkImage {
    #[serde(deserialize_with = "null_default")]
    pub url: String,
}

fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// Integer from a JSON number or numeric string; fractions truncate toward
/// zero. Values outside `T` yield None: `as` would saturate floats, and
/// out-of-range Plex numbers must default instead of clamping to MAX/MIN.
fn lenient_integer<T: TryFrom<i128>>(value: &serde_json::Value) -> Option<T> {
    fn from_float(v: f64) -> Option<i128> {
        // i128::MAX as f64 is 2^127 (exclusive bound); i128::MIN is exact.
        (v.is_finite() && v >= i128::MIN as f64 && v < i128::MAX as f64).then_some(v as i128)
    }
    let wide = match value {
        serde_json::Value::Number(n) => n
            .as_i64()
            .map(i128::from)
            .or_else(|| n.as_u64().map(i128::from))
            .or_else(|| n.as_f64().and_then(from_float)),
        serde_json::Value::String(s) => {
            let s = s.trim();
            s.parse::<i128>()
                .ok()
                .or_else(|| s.parse::<f64>().ok().and_then(from_float))
        }
        _ => None,
    }?;
    T::try_from(wide).ok()
}

/// Lowercased text from a JSON string; any other JSON type (or null) is
/// treated as absent rather than invalidating the surrounding media.
fn lenient_text<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::String(text) => text.trim().to_ascii_lowercase(),
        _ => String::new(),
    })
}

fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: TryFrom<i128> + Default,
{
    Ok(optional(deserializer)?.unwrap_or_default())
}

fn optional<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: TryFrom<i128>,
{
    Ok(lenient_integer(&serde_json::Value::deserialize(
        deserializer,
    )?))
}

fn lenient_f64<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Null => 0.0,
        serde_json::Value::Number(n) => n.as_f64().unwrap_or_default(),
        serde_json::Value::String(s) => s
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .unwrap_or_default(),
        _ => 0.0,
    })
}

fn lenient_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Null => Vec::new(),
        serde_json::Value::Array(items) => items
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect(),
        single => serde_json::from_value(single)
            .map(|one| vec![one])
            .unwrap_or_default(),
    })
}

fn radio_flag<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    // PMS discovery playlists use "1"/"0" even in JSON; other responses use
    // actual booleans. An optional station flag must not poison the entire hub.
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Bool(flag) => flag,
        serde_json::Value::Number(number) => number.as_f64() == Some(1.0),
        serde_json::Value::String(flag) => {
            let flag = flag.trim();
            flag == "1" || flag.eq_ignore_ascii_case("true")
        }
        _ => false,
    })
}

// Optional analysis fields vary between PMS versions (numbers or numeric
// strings). Unusable gain must not invalidate the track or its lyric streams.
fn optional_gain<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<f64>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        .filter(|gain| gain.is_finite()))
}

#[derive(Clone, Default, Deserialize, Serialize, Debug, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Media {
    #[serde(deserialize_with = "null_default")]
    pub audio_codec: String,
    /// Plex `Media.container` (e.g. "mp3", "flac", "mp4", "ogg"). Older or
    /// partial payloads may omit it or use another JSON type.
    #[serde(deserialize_with = "lenient_text")]
    pub container: String,
    #[serde(deserialize_with = "lenient")]
    pub bitrate: u32,
    #[serde(rename = "Part", deserialize_with = "lenient_vec")]
    pub parts: Vec<Part>,
}

#[derive(Clone, Default, Deserialize, Serialize, Debug, PartialEq)]
#[serde(default)]
pub struct Part {
    #[serde(deserialize_with = "null_default")]
    pub key: String,
    #[serde(deserialize_with = "optional")]
    pub size: Option<u64>,
    #[serde(rename = "Stream", deserialize_with = "lenient_vec")]
    pub streams: Vec<Stream>,
}

#[derive(Clone, Default, Deserialize, Serialize, Debug, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Stream {
    #[serde(deserialize_with = "null_default")]
    pub key: String,
    #[serde(deserialize_with = "lenient")]
    pub stream_type: u32,
    #[serde(deserialize_with = "null_default")]
    pub format: String,
    #[serde(deserialize_with = "optional_gain")]
    pub gain: Option<f64>,
    #[serde(deserialize_with = "optional_gain")]
    pub album_gain: Option<f64>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Hub {
    #[serde(deserialize_with = "null_default")]
    pub key: String,
    #[serde(deserialize_with = "radio_flag")]
    pub more: bool,
    #[serde(deserialize_with = "null_default")]
    pub title: String,
    #[serde(rename = "hubIdentifier", deserialize_with = "null_default")]
    pub hub_identifier: String,
    #[serde(rename = "Metadata", deserialize_with = "lenient_vec")]
    pub items: Vec<Item>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Section {
    #[serde(deserialize_with = "null_default")]
    pub key: String,
    #[serde(deserialize_with = "null_default")]
    pub title: String,
    /// The section's top-level item type ("artist" for music libraries).
    #[serde(rename = "type", deserialize_with = "null_default")]
    pub kind: ItemKind,
    #[serde(deserialize_with = "lenient")]
    pub size: usize,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Resource {
    #[serde(deserialize_with = "null_default")]
    pub name: String,
    #[serde(deserialize_with = "null_default")]
    pub client_identifier: String,
    #[serde(deserialize_with = "null_default")]
    pub provides: String,
    #[serde(deserialize_with = "null_default")]
    pub access_token: String,
    #[serde(deserialize_with = "lenient_vec")]
    pub connections: Vec<Connection>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Connection {
    #[serde(deserialize_with = "null_default")]
    pub uri: String,
    #[serde(deserialize_with = "radio_flag")]
    pub local: bool,
    #[serde(deserialize_with = "radio_flag")]
    pub relay: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pin {
    #[serde(deserialize_with = "required_u64")]
    pub id: u64,
    pub code: String,
    #[serde(deserialize_with = "required_u64")]
    pub expires_in: u64,
    pub auth_token: Option<String>,
}

fn required_u64<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
        .ok_or_else(|| serde::de::Error::custom("Expected an unsigned integer"))
}

#[cfg(test)]
mod pin_tests {
    use super::*;

    #[test]
    fn pin_integer_strings_are_supported_but_invalid_required_values_fail() {
        let pin =
            serde_json::json!({"id":"42","code":"fixture","expiresIn":"3600","authToken":null});
        let decoded: Pin = serde_json::from_value(pin.clone()).unwrap();
        assert_eq!(decoded.id, 42);
        assert_eq!(decoded.expires_in, 3600);
        for value in [
            serde_json::json!(null),
            serde_json::json!(-1),
            serde_json::json!("bad"),
            serde_json::json!(1.5),
        ] {
            let mut invalid = pin.clone();
            invalid["expiresIn"] = value;
            assert!(serde_json::from_value::<Pin>(invalid).is_err());
        }
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Container {
    #[serde(rename = "Meta")]
    pub meta: serde_json::Value,
    #[serde(rename = "Hub", deserialize_with = "lenient_vec")]
    pub hubs: Vec<Hub>,
    #[serde(rename = "Metadata", deserialize_with = "lenient_vec")]
    pub items: Vec<Item>,
    #[serde(rename = "Directory", deserialize_with = "lenient_vec")]
    pub sections: Vec<Section>,
    #[serde(deserialize_with = "lenient")]
    pub size: usize,
    #[serde(deserialize_with = "optional")]
    pub total_size: Option<usize>,
    #[serde(deserialize_with = "lenient")]
    pub offset: usize,
    #[serde(deserialize_with = "null_default")]
    pub machine_identifier: String,
    #[serde(rename = "playQueueID", deserialize_with = "optional")]
    pub play_queue_id: Option<u64>,
    #[serde(rename = "playQueueSelectedItemID", deserialize_with = "optional")]
    pub play_queue_selected_item_id: Option<u64>,
}

#[derive(Deserialize)]
pub struct Envelope {
    #[serde(rename = "MediaContainer")]
    pub container: Container,
}

#[cfg(test)]
mod kind_tests {
    use super::*;

    #[test]
    fn item_kind_strum_and_serde_spellings_agree() {
        for raw in ["artist", "album", "track", "playlist", "", "photo", "Track"] {
            let kind = ItemKind::from(raw.to_owned());
            assert_eq!(kind.to_string(), raw);
            assert_eq!(kind.as_str(), raw);
            assert_eq!(serde_json::to_value(&kind).unwrap(), raw);
            assert_eq!(
                serde_json::from_value::<ItemKind>(serde_json::json!(raw)).unwrap(),
                kind
            );
        }
        assert_eq!("track".parse::<ItemKind>().unwrap(), ItemKind::Track);
        assert_eq!(
            ItemKind::from("photo".to_owned()),
            ItemKind::Other("photo".into())
        );
        assert_eq!(ItemKind::default().as_str(), "");
        let mut kinds: Vec<_> = ["track", "album", "playlist", "artist"]
            .map(|raw| ItemKind::from(raw.to_owned()))
            .into();
        kinds.sort();
        assert_eq!(
            kinds.iter().map(ItemKind::as_str).collect::<Vec<_>>(),
            ["album", "artist", "playlist", "track"]
        );
        // Unknown Plex types and null deserialize leniently inside an item.
        let item: Item = serde_json::from_value(serde_json::json!({"type":"clip"})).unwrap();
        assert_eq!(item.kind, ItemKind::Other("clip".into()));
        let item: Item = serde_json::from_value(serde_json::json!({"type":null})).unwrap();
        assert!(item.kind.is_empty());
        assert_eq!(serde_json::to_value(&item).unwrap()["type"], "");
    }
}
