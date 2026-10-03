//! Private, credential-scoped raw metadata snapshots and bounded artwork storage.
use crate::{
    model::{Container, Item},
    plex::{server_path, server_url},
    Error, Result,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
};

fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    write_bytes(path, &serde_json::to_vec(value)?)
}
fn write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
fn prune(root: &Path, extra: u64, limit: u64) -> Result<()> {
    let mut files: Vec<_> = fs::read_dir(root)?
        .flatten()
        .filter_map(|e| {
            e.metadata()
                .ok()
                .filter(|m| m.is_file())
                .map(|m| (m.modified().ok(), m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort_by_key(|f| f.0);
    for (_, size, path) in files {
        if total + extra <= limit {
            break;
        }
        fs::remove_file(path)?;
        total = total.saturating_sub(size);
    }
    Ok(())
}

pub struct Library {
    root: PathBuf,
    writable: bool,
}
impl Library {
    pub fn new(root: PathBuf, writable: bool) -> Result<Self> {
        if writable {
            fs::create_dir_all(&root)?;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self { root, writable })
    }
    fn path(&self, namespace: &str, path: &str, params: &[(&str, String)]) -> PathBuf {
        self.root.join(format!(
            "{namespace}-{}.json",
            hash(&format!(
                "{path}\0{}",
                serde_json::to_string(params).unwrap()
            ))
        ))
    }
    pub fn save(
        &self,
        namespace: &str,
        path: &str,
        params: &[(&str, String)],
        container: &Container,
    ) {
        if self.writable {
            if prune(
                &self.root,
                serde_json::to_vec(container).map_or(0, |b| b.len()) as u64,
                64 * 1024 * 1024,
            )
            .is_err()
            {
                return;
            }
            let _ = write_json(&self.path(namespace, path, params), container);
        }
    }
    pub fn load(
        &self,
        namespace: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Option<Container> {
        serde_json::from_slice(&fs::read(self.path(namespace, path, params)).ok()?).ok()
    }
    pub fn items(&self, namespace: &str) -> Vec<Item> {
        let mut items = std::collections::BTreeMap::new();
        for entry in fs::read_dir(&self.root).into_iter().flatten().flatten() {
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("{namespace}-"))
            {
                continue;
            }
            if let Some(c) = fs::read(entry.path())
                .ok()
                .and_then(|b| serde_json::from_slice::<Container>(&b).ok())
            {
                for i in c
                    .items
                    .into_iter()
                    .chain(c.hubs.into_iter().flat_map(|h| h.items))
                {
                    if matches!(i.kind.as_str(), "artist" | "album" | "track") {
                        items
                            .entry((i.kind.clone(), i.rating_key.clone()))
                            .or_insert(i);
                    }
                }
            }
        }
        items.into_values().collect()
    }
    pub fn clear(&self) -> Result<()> {
        if self.writable {
            for e in fs::read_dir(&self.root)? {
                let e = e?;
                if e.file_type()?.is_file() {
                    fs::remove_file(e.path())?;
                }
            }
        }
        Ok(())
    }
    pub fn invalidate_playlist(&self, namespace: &str, key: &str) {
        if !self.writable {
            return;
        }
        let _ = fs::remove_file(self.path(namespace, &format!("/playlists/{key}"), &[]));
        for start in (0..10000).step_by(100) {
            let page = [
                ("X-Plex-Container-Start", start.to_string()),
                ("X-Plex-Container-Size", "100".into()),
            ];
            let _ =
                fs::remove_file(self.path(namespace, &format!("/playlists/{key}/items"), &page));
            let mut list = vec![("playlistType", "audio".into())];
            list.extend(page);
            let _ = fs::remove_file(self.path(namespace, "/playlists", &list));
        }
    }
    pub fn rate(&self, namespace: &str, key: &str, rating: f64) {
        if !self.writable {
            return;
        }
        for entry in fs::read_dir(&self.root).into_iter().flatten().flatten() {
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("{namespace}-"))
            {
                continue;
            }
            if let Some(mut c) = fs::read(entry.path())
                .ok()
                .and_then(|b| serde_json::from_slice::<Container>(&b).ok())
            {
                for i in c
                    .items
                    .iter_mut()
                    .chain(c.hubs.iter_mut().flat_map(|h| &mut h.items))
                {
                    if i.rating_key == key {
                        i.user_rating = rating;
                    }
                }
                let _ = write_json(&entry.path(), &c);
            }
        }
    }
    pub fn lyrics(&self, namespace: &str, key: &str) -> Option<Vec<crate::lyrics::Line>> {
        serde_json::from_slice(
            &fs::read(self.root.join(format!("{namespace}-lyrics-{key}.json"))).ok()?,
        )
        .ok()
    }
    pub fn save_lyrics(&self, namespace: &str, key: &str, lines: &[crate::lyrics::Line]) {
        if self.writable {
            if prune(&self.root, 512 * 1024, 64 * 1024 * 1024).is_err() {
                return;
            }
            let _ = write_json(
                &self.root.join(format!("{namespace}-lyrics-{key}.json")),
                &lines,
            );
        }
    }
}

struct Job {
    epoch: u64,
    server: String,
    token: String,
    path: String,
    file: PathBuf,
}
pub struct Artwork {
    gate: Option<Arc<crate::cache::Gate>>,
    root: PathBuf,
    jobs: Option<mpsc::SyncSender<Job>>,
    pending: Arc<Mutex<BTreeSet<PathBuf>>>,
    worker: Option<thread::JoinHandle<()>>,
    stopping: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
}
impl Artwork {
    pub fn new(root: PathBuf, writable: bool) -> Result<Self> {
        if writable {
            fs::create_dir_all(&root)?;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        }
        let pending = Arc::new(Mutex::new(BTreeSet::new()));
        let stopping = Arc::new(AtomicBool::new(false));
        let generation = Arc::new(AtomicU64::new(0));
        let gen = generation.clone();
        let stop = stopping.clone();
        let (tx, rx) = mpsc::sync_channel::<Job>(128);
        let active = pending.clone();
        let worker = if writable {
            Some(thread::spawn(move || {
                let client = match reqwest::blocking::Client::builder()
                    .connect_timeout(std::time::Duration::from_secs(3))
                    .timeout(std::time::Duration::from_secs(5))
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                {
                    Ok(c) => c,
                    Err(_) => return,
                };
                while let Ok(job) = rx.recv() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if job.epoch != gen.load(Ordering::SeqCst) {
                        active.lock().unwrap().remove(&job.file);
                        continue;
                    }
                    let result = (|| -> Result<()> {
                        let url = server_path(&server_url(&job.server)?, &job.path)?;
                        let response = client
                            .get(url)
                            .header("X-Plex-Token", &job.token)
                            .send()
                            .map_err(|_| Error::Network)?;
                        if !response.status().is_success()
                            || !response
                                .headers()
                                .get("content-type")
                                .and_then(|h| h.to_str().ok())
                                .is_some_and(|s| s.starts_with("image/"))
                        {
                            return Err(Error::ProtocolAt("artwork"));
                        }
                        let mut bytes = Vec::new();
                        response.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
                        if bytes.is_empty() || bytes.len() > 4 * 1024 * 1024 {
                            return Err(Error::Input("Artwork exceeds size limit"));
                        }
                        let root = job.file.parent().unwrap();
                        let mut files: Vec<_> = fs::read_dir(root)?
                            .flatten()
                            .filter_map(|e| {
                                e.metadata()
                                    .ok()
                                    .filter(|m| m.is_file())
                                    .map(|m| (m.modified().ok(), m.len(), e.path()))
                            })
                            .collect();
                        let mut total: u64 = files.iter().map(|f| f.1).sum();
                        files.sort_by_key(|f| f.0);
                        for (_, size, path) in files {
                            if total + bytes.len() as u64 <= 128 * 1024 * 1024 {
                                break;
                            }
                            fs::remove_file(path)?;
                            total = total.saturating_sub(size);
                        }
                        let _guard = active.lock().unwrap();
                        if job.epoch != gen.load(Ordering::SeqCst) {
                            return Ok(());
                        }
                        write_bytes(&job.file, &bytes)
                    })();
                    let _ = result;
                    active.lock().unwrap().remove(&job.file);
                }
            }))
        } else {
            None
        };
        Ok(Self {
            gate: None,
            root,
            jobs: if writable { Some(tx) } else { None },
            pending,
            worker,
            stopping,
            generation,
        })
    }
    pub fn local(&self, namespace: &str, path: &str) -> Option<String> {
        let file = self.root.join(hash(&format!("{namespace}\0{path}")));
        fs::symlink_metadata(&file)
            .ok()
            .filter(|m| m.is_file() && !m.file_type().is_symlink() && m.len() > 0)?;
        url::Url::from_file_path(file).ok().map(String::from)
    }
    pub fn schedule(&self, server: &str, token: &str, path: &str) {
        if self.gate.as_ref().is_some_and(|gate| !gate.allowed()) {
            return;
        }
        if server_url(server)
            .and_then(|base| server_path(&base, path))
            .is_err()
        {
            return;
        }
        let ns = crate::cache::namespace(server, token);
        if self.local(&ns, path).is_some() {
            return;
        }
        let file = self.root.join(hash(&format!("{ns}\0{path}")));
        if let Some(tx) = &self.jobs {
            if self.pending.lock().unwrap().insert(file.clone())
                && tx
                    .try_send(Job {
                        epoch: self.generation.load(Ordering::SeqCst),
                        server: server.into(),
                        token: token.into(),
                        path: path.into(),
                        file: file.clone(),
                    })
                    .is_err()
            {
                self.pending.lock().unwrap().remove(&file);
            }
        }
    }
    pub fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
    pub fn generation(&self) -> Arc<AtomicU64> {
        self.generation.clone()
    }
    pub fn set_gate(&mut self, gate: Arc<crate::cache::Gate>) {
        self.gate = Some(gate);
    }
    pub fn clear(&self) -> Result<()> {
        self.cancel();
        let _guard = self.pending.lock().unwrap();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }
}
impl Drop for Artwork {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.jobs.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
