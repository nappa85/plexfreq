use crate::{model::Item, plex::advertised_path, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

#[derive(
    Clone,
    Copy,
    Deserialize,
    Serialize,
    Debug,
    PartialEq,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
    strum::EnumIter,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum RadioKind {
    Artist,
    Album,
    Track,
}

impl RadioKind {
    /// The metadata type of the radio's seed item.
    pub fn item_kind(self) -> crate::model::ItemKind {
        match self {
            Self::Artist => crate::model::ItemKind::Artist,
            Self::Album => crate::model::ItemKind::Album,
            Self::Track => crate::model::ItemKind::Track,
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub enum Source {
    Station {
        queue_id: u64,
    },
    Sonic {
        seed: Box<Item>,
        recent: Vec<String>,
    },
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Radio {
    pub kind: RadioKind,
    pub title: String,
    pub source: Source,
}

impl Radio {
    pub fn summary(&self) -> Value {
        serde_json::json!({"kind":self.kind,"title":self.title,"source":match self.source { Source::Station{..} => "station", Source::Sonic{..} => "sonic" }})
    }
}

// Plex JSON may represent the optional Stations child as an array directly,
// or as an object containing a named child array (the XML API calls it Playlist).
pub fn station_key(item: &Item, base: &Url) -> Result<Option<String>> {
    // `container` is true for wrapper objects ({"size":1,"Metadata":[...]},
    // {"key":"/x","Playlist":[...]}): an outer wrapper may carry an unrelated
    // "key" while the real stations live in its child arrays. Array elements
    // are station entries whose own key wins over any nested child (Image,
    // Genre...) that happens to carry a "key" too.
    fn find(value: &Value, container: bool) -> Option<&str> {
        match value {
            Value::Array(items) => items.iter().find_map(|entry| find(entry, false)),
            Value::Object(fields) => {
                let own = || fields.get("key").and_then(Value::as_str);
                let nested = || fields.values().find_map(|child| find(child, true));
                if container {
                    nested().or_else(own)
                } else {
                    own().or_else(nested)
                }
            }
            _ => None,
        }
    }
    let Some(key) = find(&item.stations, true) else {
        return Ok(None);
    };
    // An empty key means "no station", not a protocol error: fall back to
    // sonic radio instead of failing the whole request.
    if key.is_empty() {
        return Ok(None);
    }
    advertised_path(base, key)?;
    Ok(Some(key.to_owned()))
}

pub fn station_uri(machine_identifier: &str, key: &str) -> Result<String> {
    if machine_identifier.is_empty()
        || !machine_identifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(Error::ProtocolAt("radio server identity"));
    }
    // Reuse the advertised-key rules (including legitimate station queries).
    let base = Url::parse("https://station.invalid/")?;
    advertised_path(&base, key)?;
    Ok(format!(
        "server://{machine_identifier}/com.plexapp.plugins.library{key}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn radio_kind_strum_and_serde_spellings_agree() {
        use strum::IntoEnumIterator;
        for kind in RadioKind::iter() {
            let name = kind.to_string();
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(name));
            assert_eq!(
                serde_json::from_value::<RadioKind>(json!(name)).unwrap(),
                kind
            );
            assert_eq!(name.parse::<RadioKind>().unwrap(), kind);
            assert_eq!(kind.item_kind().as_str(), name);
        }
    }

    #[test]
    fn advertised_station_shapes_preserve_query_and_reject_external_keys() {
        let base = Url::parse("https://music.example/").unwrap();
        let key = "/library/metadata/42/station/example?type=10";
        for stations in [json!([{"key":key}]), json!({"Playlist":[{"key":key}]})] {
            let item = Item {
                stations,
                ..Default::default()
            };
            assert_eq!(station_key(&item, &base).unwrap().as_deref(), Some(key));
        }
        assert!(station_key(&Item::default(), &base).unwrap().is_none());
        let external = Item {
            stations: json!([{"key":"//evil.example/station"}]),
            ..Default::default()
        };
        assert!(station_key(&external, &base).is_err());
        assert_eq!(
            station_uri("server-123", key).unwrap(),
            format!("server://server-123/com.plexapp.plugins.library{key}")
        );
        assert!(station_uri("server/another", key).is_err());
    }

    #[test]
    fn station_entry_key_wins_over_nested_children() {
        let base = Url::parse("https://music.example/").unwrap();
        let key = "/library/metadata/42/station/example";
        for stations in [
            json!([{"Genre":[{"key":"/library/sections/1/genre/7"}],"key":key}]),
            json!({"key":"/bad","Metadata":[{"Image":{"key":"/photo/x"},"key":key}]}),
        ] {
            let item = Item {
                stations,
                ..Default::default()
            };
            assert_eq!(station_key(&item, &base).unwrap().as_deref(), Some(key));
        }
    }
}
