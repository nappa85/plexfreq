use serde::{Deserialize, Serialize};

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
    #[serde(deserialize_with = "lenient_i32")]
    pub year: i32,
    #[serde(deserialize_with = "lenient_i32")]
    pub index: i32,
    #[serde(deserialize_with = "lenient_i32")]
    pub parent_index: i32,
    #[serde(rename = "Image", deserialize_with = "lenient_vec")]
    pub images: Vec<ArtworkImage>,
    #[serde(rename = "type", deserialize_with = "null_default")]
    pub kind: String,
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
    #[serde(deserialize_with = "lenient_u64")]
    pub duration: u64,
    #[serde(deserialize_with = "optional_usize")]
    pub leaf_count: Option<usize>,
    #[serde(deserialize_with = "optional_u64")]
    pub duration_in_seconds: Option<u64>,
    #[serde(
        rename = "playlistItemID",
        alias = "playlistItemId",
        deserialize_with = "optional_u64"
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
    #[serde(deserialize_with = "lenient_u32")]
    pub music_analysis_version: u32,
    #[serde(deserialize_with = "null_default")]
    pub parent_rating_key: String,
    #[serde(deserialize_with = "null_default")]
    pub grandparent_rating_key: String,
    #[serde(rename = "playQueueItemID", deserialize_with = "optional_u64")]
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

fn f64_to_i32(v: f64) -> Option<i32> {
    // `as` saturates floats; out-of-range Plex numbers must default instead
    // of clamping to MAX/MIN.
    if v.is_finite() && v >= i32::MIN as f64 && v <= i32::MAX as f64 {
        Some(v as i32)
    } else {
        None
    }
}

fn f64_to_u32(v: f64) -> Option<u32> {
    if v.is_finite() && v >= 0.0 && v <= u32::MAX as f64 {
        Some(v as u32)
    } else {
        None
    }
}

fn f64_to_u64(v: f64) -> Option<u64> {
    if v.is_finite() && v >= 0.0 && v <= u64::MAX as f64 {
        Some(v as u64)
    } else {
        None
    }
}

fn lenient_i32<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<i32, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Null => 0,
        serde_json::Value::Number(n) => n
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .or_else(|| n.as_u64().and_then(|v| i32::try_from(v).ok()))
            .or_else(|| n.as_f64().and_then(f64_to_i32))
            .unwrap_or_default(),
        serde_json::Value::String(s) => s
            .trim()
            .parse::<i32>()
            .or_else(|_| s.trim().parse::<f64>().ok().and_then(f64_to_i32).ok_or(()))
            .unwrap_or_default(),
        _ => 0,
    })
}

fn lenient_u32<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Null => 0,
        serde_json::Value::Number(n) => n
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .or_else(|| n.as_i64().and_then(|v| u32::try_from(v).ok()))
            .or_else(|| n.as_f64().and_then(f64_to_u32))
            .unwrap_or_default(),
        serde_json::Value::String(s) => {
            let s = s.trim();
            s.parse::<u32>()
                .or_else(|_| s.parse::<f64>().ok().and_then(f64_to_u32).ok_or(()))
                .or_else(|_| {
                    s.parse::<i64>()
                        .ok()
                        .and_then(|v| u32::try_from(v).ok())
                        .ok_or(())
                })
                .unwrap_or_default()
        }
        _ => 0,
    })
}

fn lenient_u64<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Null => 0,
        serde_json::Value::Number(n) => n
            .as_u64()
            .or_else(|| n.as_i64().and_then(|v| u64::try_from(v).ok()))
            .or_else(|| n.as_f64().and_then(f64_to_u64))
            .unwrap_or_default(),
        serde_json::Value::String(s) => {
            let s = s.trim();
            s.parse::<u64>()
                .or_else(|_| s.parse::<f64>().ok().and_then(f64_to_u64).ok_or(()))
                .or_else(|_| {
                    s.parse::<i64>()
                        .ok()
                        .and_then(|v| u64::try_from(v).ok())
                        .ok_or(())
                })
                .unwrap_or_default()
        }
        _ => 0,
    })
}

fn lenient_usize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<usize, D::Error> {
    Ok(usize::try_from(lenient_u64(deserializer)?).unwrap_or_default())
}

fn lenient_f64<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Null => 0.0,
        serde_json::Value::Number(n) => n.as_f64().unwrap_or_default(),
        serde_json::Value::String(s) => s.trim().parse::<f64>().unwrap_or_default(),
        _ => 0.0,
    })
}

fn optional_u64<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    let value =
        Option::<serde_json::Value>::deserialize(deserializer)?.unwrap_or(serde_json::Value::Null);
    Ok(match value {
        serde_json::Value::Null => None,
        serde_json::Value::Number(n) => n
            .as_u64()
            .or_else(|| n.as_i64().and_then(|v| u64::try_from(v).ok()))
            .or_else(|| n.as_f64().and_then(f64_to_u64)),
        serde_json::Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                None
            } else {
                s.parse::<u64>()
                    .or_else(|_| {
                        s.parse::<i64>()
                            .ok()
                            .and_then(|v| u64::try_from(v).ok())
                            .ok_or(())
                    })
                    .or_else(|_| s.parse::<f64>().ok().and_then(f64_to_u64).ok_or(()))
                    .ok()
            }
        }
        _ => None,
    })
}

fn optional_usize<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<usize>, D::Error> {
    Ok(optional_u64(deserializer)?.and_then(|v| usize::try_from(v).ok()))
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
        serde_json::Value::Number(number) => number.as_u64() == Some(1),
        serde_json::Value::String(flag) => flag == "1" || flag.eq_ignore_ascii_case("true"),
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
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
        .filter(|gain| gain.is_finite()))
}

#[derive(Clone, Default, Deserialize, Serialize, Debug, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Media {
    #[serde(deserialize_with = "null_default")]
    pub audio_codec: String,
    #[serde(deserialize_with = "lenient_u32")]
    pub bitrate: u32,
    #[serde(rename = "Part", deserialize_with = "lenient_vec")]
    pub parts: Vec<Part>,
}

#[derive(Clone, Default, Deserialize, Serialize, Debug, PartialEq)]
#[serde(default)]
pub struct Part {
    #[serde(deserialize_with = "null_default")]
    pub key: String,
    #[serde(deserialize_with = "optional_u64")]
    pub size: Option<u64>,
    #[serde(rename = "Stream", deserialize_with = "lenient_vec")]
    pub streams: Vec<Stream>,
}

#[derive(Clone, Default, Deserialize, Serialize, Debug, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Stream {
    #[serde(deserialize_with = "null_default")]
    pub key: String,
    #[serde(deserialize_with = "lenient_u32")]
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
    #[serde(rename = "type", deserialize_with = "null_default")]
    pub kind: String,
    #[serde(deserialize_with = "lenient_usize")]
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
    pub id: u64,
    pub code: String,
    pub expires_in: u64,
    pub auth_token: Option<String>,
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
    #[serde(deserialize_with = "lenient_usize")]
    pub size: usize,
    #[serde(deserialize_with = "optional_usize")]
    pub total_size: Option<usize>,
    #[serde(deserialize_with = "lenient_usize")]
    pub offset: usize,
    #[serde(deserialize_with = "null_default")]
    pub machine_identifier: String,
    #[serde(rename = "playQueueID", deserialize_with = "optional_u64")]
    pub play_queue_id: Option<u64>,
    #[serde(rename = "playQueueSelectedItemID", deserialize_with = "optional_u64")]
    pub play_queue_selected_item_id: Option<u64>,
}

#[derive(Deserialize)]
pub struct Envelope {
    #[serde(rename = "MediaContainer")]
    pub container: Container,
}
