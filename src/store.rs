use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub client_identifier: String,
    pub account_token: String,
    pub server_url: String,
    pub server_token: String,
    pub cache: crate::cache::Config,
    pub playback: Playback,
    pub downloads: Vec<crate::model::Item>,
    pub download_groups: std::collections::BTreeMap<String, Vec<String>>,
    pub download_titles: std::collections::BTreeMap<String, String>,
    pub wifi_only: bool,
    pub downloads_paused: bool,
    pub autoplay: bool,
    pub audio: crate::audio::dsp::Config,
    pub history: Vec<crate::history::Play>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Playback {
    pub queue: crate::queue::Queue,
    pub radio: Option<crate::radio::Radio>,
    pub position: u64,
    pub generation: u64,
    pub occurrence: String,
    pub listened: u64,
}

pub fn load(dir: &Path) -> Result<Settings> {
    let path = dir.join("session.json");
    match fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|_| Error::Input("Session file is corrupt"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(e.into()),
    }
}

pub fn save(dir: &Path, settings: &Settings) -> Result<()> {
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    // No shared temp filename or symlink following; one owner per state directory.
    let temp = dir.join(format!(".session-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&serde_json::to_vec(settings)?)?;
        file.sync_all()?;
        fs::rename(&temp, dir.join("session.json"))?;
        fs::File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
