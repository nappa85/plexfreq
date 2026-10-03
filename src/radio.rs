use crate::{model::Item, plex::server_path, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

#[derive(Clone, Copy, Deserialize, Serialize, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RadioKind {
    Artist,
    Album,
    Track,
}

impl RadioKind {
    pub fn item_type(self) -> &'static str {
        match self {
            Self::Artist => "artist",
            Self::Album => "album",
            Self::Track => "track",
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
    fn find(value: &Value) -> Option<&str> {
        match value {
            Value::Array(items) => items.iter().find_map(find),
            Value::Object(fields) => fields
                .get("key")
                .and_then(Value::as_str)
                .or_else(|| fields.values().find_map(find)),
            _ => None,
        }
    }
    let Some(key) = find(&item.stations) else {
        return Ok(None);
    };
    server_path(base, key)?;
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
    Ok(format!(
        "server://{machine_identifier}/com.plexapp.plugins.library{key}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
