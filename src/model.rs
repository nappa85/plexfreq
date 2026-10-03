use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Deserialize, Serialize, Debug, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Item {
    pub rating_key: String,
    pub key: String,
    pub title: String,
    #[serde(deserialize_with = "null_default")]
    pub summary: String,
    #[serde(deserialize_with = "null_default")]
    pub art: String,
    #[serde(deserialize_with = "null_default")]
    pub year: i32,
    #[serde(deserialize_with = "null_default")]
    pub index: i32,
    #[serde(deserialize_with = "null_default")]
    pub parent_index: i32,
    #[serde(rename = "Image", deserialize_with = "null_default")]
    pub images: Vec<ArtworkImage>,
    #[serde(rename = "type")]
    pub kind: String,
    pub parent_title: String,
    pub grandparent_title: String,
    pub original_title: String,
    pub thumb: String,
    pub parent_thumb: String,
    #[serde(deserialize_with = "null_default")]
    pub duration: u64,
    pub leaf_count: Option<usize>,
    pub duration_in_seconds: Option<u64>,
    #[serde(rename = "playlistItemID", alias = "playlistItemId")]
    pub playlist_item_id: Option<u64>,
    #[serde(deserialize_with = "null_default")]
    pub smart: bool,
    #[serde(deserialize_with = "null_default")]
    pub playlist_type: String,
    #[serde(deserialize_with = "null_default")]
    pub subtype: String,
    #[serde(rename = "Format", deserialize_with = "null_default")]
    pub formats: Vec<Tag>,
    #[serde(rename = "Subformat", deserialize_with = "null_default")]
    pub subformats: Vec<Tag>,
    #[serde(deserialize_with = "null_default")]
    pub user_rating: f64,
    pub music_analysis_version: u32,
    pub parent_rating_key: String,
    pub grandparent_rating_key: String,
    #[serde(rename = "playQueueItemID")]
    pub play_queue_item_id: Option<u64>,
    #[serde(rename = "Stations")]
    pub stations: serde_json::Value,
    #[serde(rename = "Media")]
    pub media: Vec<Media>,
}

#[derive(Clone, Default, Deserialize, Serialize, Debug, PartialEq)]
#[serde(default)]
pub struct Tag {
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

#[derive(Clone, Default, Deserialize, Serialize, Debug, PartialEq)]
#[serde(default)]
pub struct Media {
    #[serde(rename = "Part")]
    pub parts: Vec<Part>,
}

#[derive(Clone, Default, Deserialize, Serialize, Debug, PartialEq)]
#[serde(default)]
pub struct Part {
    pub key: String,
    pub size: Option<u64>,
    #[serde(rename = "Stream", deserialize_with = "null_default")]
    pub streams: Vec<Stream>,
}

#[derive(Clone, Default, Deserialize, Serialize, Debug, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Stream {
    pub key: String,
    pub stream_type: u32,
    pub format: String,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Hub {
    #[serde(rename = "Metadata")]
    pub items: Vec<Item>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Section {
    pub key: String,
    pub title: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub size: usize,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resource {
    pub name: String,
    pub client_identifier: String,
    pub provides: String,
    pub access_token: String,
    pub connections: Vec<Connection>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Connection {
    pub uri: String,
    #[serde(default)]
    pub local: bool,
    #[serde(default)]
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
    #[serde(rename = "Hub")]
    pub hubs: Vec<Hub>,
    #[serde(rename = "Metadata")]
    pub items: Vec<Item>,
    #[serde(rename = "Directory")]
    pub sections: Vec<Section>,
    pub size: usize,
    pub total_size: Option<usize>,
    pub offset: usize,
    pub machine_identifier: String,
    #[serde(rename = "playQueueID")]
    pub play_queue_id: Option<u64>,
    #[serde(rename = "playQueueSelectedItemID")]
    pub play_queue_selected_item_id: Option<u64>,
}

#[derive(Deserialize)]
pub struct Envelope {
    #[serde(rename = "MediaContainer")]
    pub container: Container,
}
