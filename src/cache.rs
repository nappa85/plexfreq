use crate::{
    model::{Envelope, Item},
    plex::{server_path, server_url},
    Error, Result,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, watch};
use url::Url;

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    pub enabled: bool,
    pub limit_mb: u64,
    pub ahead: usize,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            limit_mb: 512,
            ahead: 5,
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        if !(64..=8192).contains(&self.limit_mb) || self.ahead > 20 {
            return Err(Error::Input(
                "Cache limit must be 64–8192 MiB and look-ahead 0–20 tracks",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Deserialize, Serialize)]
struct Entry {
    namespace: String,
    item: Item,
    file: String,
    bytes: u64,
    used: u64,
}
#[derive(Default, Deserialize, Serialize)]
struct Manifest {
    entries: BTreeMap<String, Entry>,
}
#[derive(Default, Deserialize, Serialize)]
struct Partial {
    total: Option<u64>,
    validator: Option<String>,
    extension: String,
    #[serde(default)]
    namespace: String,
    #[serde(default)]
    item: Option<Item>,
}
struct State {
    gate: Arc<Gate>,
    manifest: Manifest,
    config: Config,
    pinned: BTreeSet<String>,
    permanent: BTreeSet<String>,
    downloading: bool,
    pending: usize,
    received: u64,
    total: Option<u64>,
    error: String,
    errors: BTreeMap<String, String>,
    active_key: String,
}

#[derive(Default)]
pub struct Gate {
    wifi_only: AtomicBool,
    paused: AtomicBool,
    wifi: AtomicBool,
}
impl Gate {
    pub fn allowed(&self) -> bool {
        !self.paused.load(Ordering::SeqCst)
            && (!self.wifi_only.load(Ordering::SeqCst) || self.wifi.load(Ordering::SeqCst))
    }
}

/// Separate shared cancellation handle; never aliases the thread-confined Core.
pub struct DownloadControl {
    state: Arc<Mutex<State>>,
    epoch: Arc<AtomicU64>,
    cancel: watch::Sender<u64>,
    gate: Arc<Gate>,
    artwork_epoch: Arc<AtomicU64>,
}
impl DownloadControl {
    fn stop_if_blocked(&self) {
        if !self.gate.allowed() {
            let epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = self.cancel.send(epoch);
            self.artwork_epoch.fetch_add(1, Ordering::SeqCst);
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            state.pending = 0;
            state.downloading = false;
            state.active_key.clear();
        }
    }
    pub fn network(&self, wifi: bool) {
        self.gate.wifi.store(wifi, Ordering::SeqCst);
        self.stop_if_blocked();
    }
    pub fn policy(&self, wifi_only: bool, paused: bool) {
        self.gate.wifi_only.store(wifi_only, Ordering::SeqCst);
        self.gate.paused.store(paused, Ordering::SeqCst);
        self.stop_if_blocked();
    }
}
struct Batch {
    epoch: u64,
    base: Url,
    token: String,
    namespace: String,
    items: Vec<Item>,
}

pub struct Cache {
    root: PathBuf,
    state: Arc<Mutex<State>>,
    epoch: Arc<AtomicU64>,
    cancel: watch::Sender<u64>,
    jobs: Option<mpsc::UnboundedSender<Batch>>,
    worker: Option<std::thread::JoinHandle<()>>,
    _lock: Option<fs::File>,
    read_only: bool,
}

pub fn namespace(server: &str, token: &str) -> String {
    hash(&format!("{server}\0{token}"))
}
fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn part_key(item: &Item) -> Option<&str> {
    item.media
        .first()?
        .parts
        .first()
        .map(|p| p.key.as_str())
        .filter(|p| !p.is_empty())
}
fn key(namespace: &str, item: &Item) -> String {
    hash(&format!(
        "{namespace}\0{}\0{}",
        item.rating_key,
        part_key(item).unwrap_or("")
    ))
}
fn safe_file(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && !name.contains('\\') && !name.starts_with('.')
}
fn usable(root: &Path, entry: &Entry) -> bool {
    safe_file(&entry.file)
        && entry.bytes > 0
        && fs::symlink_metadata(root.join(&entry.file))
            .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink() && m.len() == entry.bytes)
}
fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    crate::atomic_write_json(path, value)
}
fn disk_bytes(root: &Path) -> u64 {
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == "index.json" || name == "cache.lock" || name.ends_with(".resume.json") {
                return None;
            }
            e.metadata().ok().filter(|m| m.is_file()).map(|m| m.len())
        })
        .sum()
}
fn save(root: &Path, state: &State) -> Result<()> {
    atomic_json(&root.join("index.json"), &state.manifest)
}

impl Cache {
    pub fn new(root: PathBuf, config: Config, client_id: &str) -> Result<Self> {
        Self::build(root, config, client_id, false)
    }
    pub fn read_only(root: PathBuf, config: Config, client_id: &str) -> Result<Self> {
        Self::build(root, config, client_id, true)
    }
    fn build(root: PathBuf, config: Config, client_id: &str, read_only: bool) -> Result<Self> {
        config.validate()?;
        let lock = if !read_only {
            fs::create_dir_all(&root)?;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(root.join("cache.lock"))?;
            file.try_lock_exclusive().map_err(|_| {
                Error::Input("Audio cache is already in use by another PlexFreq instance")
            })?;
            Some(file)
        } else {
            None
        };
        let mut manifest: Manifest = fs::read(root.join("index.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        manifest.entries.retain(|_, e| usable(&root, e));
        // A crash between temp write and rename can leave orphan `*.tmp` files
        // (old `index.<uuid>.tmp` naming included) plus unindexed hash files.
        // Unindexed files are not playable without metadata; reclaim our hash
        // names and any temp leftovers. Be resilient to transient entries
        // disappearing mid-scan instead of aborting cache open.
        for file in fs::read_dir(&root).into_iter().flatten().flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            if !read_only && name.ends_with(".tmp") {
                let _ = fs::remove_file(file.path());
                continue;
            }
            let Ok(file_type) = file.file_type() else {
                continue;
            };
            let stem = name.split('.').next().unwrap_or("");
            if !read_only
                && stem.len() == 64
                && stem.bytes().all(|b| b.is_ascii_hexdigit())
                && !name.ends_with(".part")
                && !name.ends_with(".resume.json")
                && !manifest.entries.values().any(|e| e.file == name)
                && file_type.is_file()
            {
                let _ = fs::remove_file(file.path());
            }
        }
        let state = Arc::new(Mutex::new(State {
            gate: Arc::new(Gate::default()),
            manifest,
            config,
            pinned: BTreeSet::new(),
            permanent: BTreeSet::new(),
            downloading: false,
            pending: 0,
            received: 0,
            total: None,
            error: String::new(),
            errors: BTreeMap::new(),
            active_key: String::new(),
        }));
        let epoch = Arc::new(AtomicU64::new(0));
        let (cancel, changed) = watch::channel(0);
        let (jobs, receiver) = mpsc::unbounded_channel();
        let worker_root = root.clone();
        let worker_state = state.clone();
        let worker_epoch = epoch.clone();
        let client_id = client_id.to_owned();
        let worker = if read_only {
            None
        } else {
            Some(
                std::thread::Builder::new()
                    .name("plexfreq-cache".into())
                    .spawn(move || {
                        if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        {
                            runtime.block_on(worker(
                                worker_root,
                                worker_state,
                                worker_epoch,
                                receiver,
                                changed,
                                &client_id,
                            ));
                        }
                    })?,
            )
        };
        Ok(Self {
            root,
            state,
            epoch,
            cancel,
            jobs: Some(jobs),
            worker,
            _lock: lock,
            read_only,
        })
    }

    pub fn config(&self) -> Config {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .config
            .clone()
    }
    pub fn gate(&self) -> Arc<Gate> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .gate
            .clone()
    }
    pub fn control(&self, artwork_epoch: Arc<AtomicU64>) -> DownloadControl {
        DownloadControl {
            state: self.state.clone(),
            epoch: self.epoch.clone(),
            cancel: self.cancel.clone(),
            gate: self.gate(),
            artwork_epoch,
        }
    }
    pub fn protect(&self, namespace: &str, items: &[Item]) {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .permanent = items
            .iter()
            .map(|i| format!("{namespace}\0{}", i.rating_key))
            .collect();
    }
    pub fn configure(&self, config: Config, keep: Option<(&str, &str)>) -> Result<()> {
        config.validate()?;
        self.cancel();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let previous = state.config.clone();
        state.config = config;
        if let Some((namespace, key)) = keep {
            state.pinned.insert(format!("{namespace}\0{key}"));
        }
        let result = room(&self.root, &mut state, 0, "");
        if result.is_err() {
            state.config = previous;
        }
        result
    }
    pub fn cancel(&self) {
        let epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.cancel.send(epoch);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.pending = 0;
        state.downloading = false;
        state.active_key.clear();
        state.pinned.clear();
    }
    pub fn clear(&self) -> Result<()> {
        self.cancel();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        for file in fs::read_dir(&self.root)?.flatten() {
            let Ok(file_type) = file.file_type() else {
                continue;
            };
            if file.file_name() != "cache.lock" && file_type.is_file() {
                let _ = fs::remove_file(file.path());
            }
        }
        state.manifest.entries.clear();
        state.permanent.clear();
        state.error.clear();
        save(&self.root, &state)
    }
    pub fn schedule(&self, server: &str, token: &str, items: Vec<Item>) -> Result<()> {
        if !self.config().enabled || !self.gate().allowed() || items.is_empty() {
            return Ok(());
        }
        if items.len() > 1000
            || items
                .iter()
                .any(|i| i.kind != "track" || crate::numeric(&i.rating_key).is_err())
        {
            return Err(Error::Input(
                "Cache requests require up to 1000 valid music tracks",
            ));
        }
        let base = server_url(server)?;
        let ns = namespace(server, token);
        let epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.cancel.send(epoch);
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            state.pinned = items
                .iter()
                .map(|i| format!("{ns}\0{}", i.rating_key))
                .collect();
            state.pending = items.len();
            state.error.clear();
            state.errors.clear();
        }
        self.jobs
            .as_ref()
            .ok_or(Error::Input("Cache worker unavailable"))?
            .send(Batch {
                epoch,
                base,
                token: token.into(),
                namespace: ns,
                items,
            })
            .map_err(|_| Error::Input("Cache worker unavailable"))
    }
    pub fn lookup(&self, namespace: &str, item: &Item, touch: bool) -> Option<(PathBuf, Item)> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let found = state
            .manifest
            .entries
            .iter()
            .filter(|(_, e)| {
                e.namespace == namespace
                    && e.item.rating_key == item.rating_key
                    && part_key(item).is_none_or(|p| part_key(&e.item) == Some(p))
                    && usable(&self.root, e)
            })
            .max_by_key(|(_, e)| e.used)
            .map(|(k, _)| k.clone())?;
        let entry = state.manifest.entries.get_mut(&found)?;
        if touch && !self.read_only {
            entry.used = now();
        }
        let result = (self.root.join(&entry.file), entry.item.clone());
        if touch && !self.read_only {
            let _ = save(&self.root, &state);
        }
        Some(result)
    }
    pub fn items(&self, namespace: &str) -> Vec<Item> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut entries: Vec<_> = state
            .manifest
            .entries
            .values()
            .filter(|e| e.namespace == namespace && usable(&self.root, e))
            .collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.used));
        let mut seen = BTreeSet::new();
        entries
            .into_iter()
            .filter(|e| seen.insert(e.item.rating_key.clone()))
            .map(|e| e.item.clone())
            .collect()
    }
    pub fn remove(&self, namespace: &str, rating_key: &str) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let keys: Vec<_> = state
            .manifest
            .entries
            .iter()
            .filter(|(_, e)| e.namespace == namespace && e.item.rating_key == rating_key)
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            if let Some(entry) = state.manifest.entries.remove(&key) {
                if safe_file(&entry.file) {
                    let _ = fs::remove_file(self.root.join(entry.file));
                }
            }
        }
        save(&self.root, &state)
    }
    pub fn pending_items(&self, namespace: &str) -> Vec<Item> {
        fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".resume.json"))
            .filter_map(|e| fs::read(e.path()).ok())
            .filter_map(|b| serde_json::from_slice::<Partial>(&b).ok())
            .filter(|p| p.namespace == namespace)
            .filter_map(|p| p.item)
            .collect()
    }
    pub fn status(&self, namespace: &str) -> Value {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let ready_bytes: BTreeMap<_, _> = state
            .manifest
            .entries
            .values()
            .filter(|e| e.namespace == namespace && usable(&self.root, e))
            .map(|e| (e.item.rating_key.clone(), e.bytes))
            .collect();
        let ready: Vec<_> = ready_bytes.keys().collect();
        json!({"enabled":state.config.enabled,"limitMb":state.config.limit_mb,"ahead":state.config.ahead,"bytes":disk_bytes(&self.root),"tracks":ready.len(),"readyKeys":ready,
            "readyBytes":ready_bytes,
            "wifiOnly":state.gate.wifi_only.load(Ordering::SeqCst),"paused":state.gate.paused.load(Ordering::SeqCst),"waitingForWifi":state.gate.wifi_only.load(Ordering::SeqCst) && !state.gate.wifi.load(Ordering::SeqCst),
            "downloading":state.downloading,"pending":state.pending,"received":state.received,"total":state.total,"error":state.error,"errors":state.errors,"activeKey":state.active_key})
    }
}
impl Drop for Cache {
    fn drop(&mut self) {
        self.cancel();
        self.jobs.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

async fn worker(
    root: PathBuf,
    state: Arc<Mutex<State>>,
    epoch: Arc<AtomicU64>,
    mut jobs: mpsc::UnboundedReceiver<Batch>,
    mut changed: watch::Receiver<u64>,
    client_id: &str,
) {
    let gate = state
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .gate
        .clone();
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .user_agent("PlexFreq cache")
        .build()
    {
        Ok(client) => client,
        Err(_) => return,
    };
    while let Some(batch) = jobs.recv().await {
        if epoch.load(Ordering::SeqCst) != batch.epoch || !gate.allowed() {
            continue;
        }
        changed.borrow_and_update();
        loop {
            let mut failed = false;
            for (position, item) in batch.items.iter().enumerate() {
                if epoch.load(Ordering::SeqCst) != batch.epoch || !gate.allowed() {
                    break;
                }
                {
                    let mut s = state.lock().unwrap_or_else(|poison| poison.into_inner());
                    s.pending = batch.items.len() - position;
                    s.active_key = item.rating_key.clone();
                }
                let result = tokio::select! {
                    _=changed.changed()=>break,
                    result=download(&client,&root,&state,&epoch,&batch,item.clone(),client_id)=>result,
                };
                if let Err(error) = result {
                    if epoch.load(Ordering::SeqCst) == batch.epoch {
                        failed = true;
                        let mut s = state.lock().unwrap_or_else(|poison| poison.into_inner());
                        s.error = error.to_string();
                        s.errors.insert(item.rating_key.clone(), error.to_string());
                    }
                } else {
                    state
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .errors
                        .remove(&item.rating_key);
                }
            }
            if epoch.load(Ordering::SeqCst) != batch.epoch {
                break;
            }
            {
                let mut s = state.lock().unwrap_or_else(|poison| poison.into_inner());
                s.downloading = false;
                s.pending = 0;
                s.active_key.clear();
            }
            if !failed {
                break;
            }
            // Retry stalled/interrupted downloads while the same queue is active.
            tokio::select! { _=changed.changed()=>break, _=tokio::time::sleep(Duration::from_secs(30))=>{} }
        }
    }
}

fn room(root: &Path, state: &mut State, extra: u64, current_key: &str) -> Result<()> {
    let limit = state.config.limit_mb * 1024 * 1024;
    if extra > limit {
        return Err(Error::Input("Audio file exceeds the cache limit"));
    }
    let mut evicted = false;
    while disk_bytes(root).saturating_add(extra) > limit {
        let old = state
            .manifest
            .entries
            .iter()
            .filter(|(k, e)| {
                k.as_str() != current_key
                    && !state
                        .pinned
                        .contains(&format!("{}\0{}", e.namespace, e.item.rating_key))
                    && !state
                        .permanent
                        .contains(&format!("{}\0{}", e.namespace, e.item.rating_key))
            })
            .min_by_key(|(_, e)| e.used)
            .map(|(k, _)| k.clone());
        if let Some(old) = old {
            if let Some(entry) = state.manifest.entries.remove(&old) {
                if safe_file(&entry.file) {
                    let _ = fs::remove_file(root.join(entry.file));
                }
            }
        } else {
            let partial = fs::read_dir(root)?
                .filter_map(|e| e.ok())
                .find(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if !name.ends_with(".part") || name == format!("{current_key}.part") {
                        return false;
                    }
                    let meta = root.join(name.replace(".part", ".resume.json"));
                    let info: Partial = fs::read(meta)
                        .ok()
                        .and_then(|b| serde_json::from_slice(&b).ok())
                        .unwrap_or_default();
                    !info.item.is_some_and(|i| {
                        state
                            .pinned
                            .contains(&format!("{}\0{}", info.namespace, i.rating_key))
                            || state
                                .permanent
                                .contains(&format!("{}\0{}", info.namespace, i.rating_key))
                    })
                })
                .ok_or(Error::Input(
                    "Cache is full; increase its limit or clear cached audio",
                ))?;
            let _ = fs::remove_file(partial.path().with_extension("resume.json"));
            fs::remove_file(partial.path())?;
        }
        evicted = true;
    }
    // Persist only when eviction changed the manifest. The per-chunk path with
    // unknown total calls room() for every 64KB; rewriting index.json there
    // wastes flash and slows downloads with no benefit when nothing was freed.
    if evicted {
        save(root, state)
    } else {
        Ok(())
    }
}

async fn download(
    client: &reqwest::Client,
    root: &Path,
    state: &Arc<Mutex<State>>,
    epoch: &AtomicU64,
    batch: &Batch,
    mut item: Item,
    client_id: &str,
) -> Result<()> {
    {
        let s = state.lock().unwrap_or_else(|poison| poison.into_inner());
        if s.manifest.entries.values().any(|e| {
            e.namespace == batch.namespace
                && e.item.rating_key == item.rating_key
                && part_key(&item).is_none_or(|p| part_key(&e.item) == Some(p))
                && usable(root, e)
        }) {
            return Ok(());
        }
    }
    if part_key(&item).is_none() {
        let url = server_path(
            &batch.base,
            &format!("/library/metadata/{}", item.rating_key),
        )?;
        let response = client
            .get(url)
            .header("Accept", "application/json")
            .header("X-Plex-Token", &batch.token)
            .header("X-Plex-Client-Identifier", client_id)
            .send()
            .await
            .map_err(|_| Error::Network)?;
        if !response.status().is_success() {
            return Err(Error::Http(response.status().as_u16()));
        }
        let envelope: Envelope = response
            .json()
            .await
            .map_err(|_| Error::ProtocolAt("cache track metadata"))?;
        item = envelope
            .container
            .items
            .into_iter()
            .find(|i| i.kind == "track" && i.rating_key == item.rating_key)
            .ok_or(Error::ProtocolAt("cache track metadata"))?;
    }
    let part = part_key(&item).ok_or(Error::Input("Track has no downloadable media part"))?;
    let url = server_path(&batch.base, part)?;
    let id = key(&batch.namespace, &item);
    let partial_path = root.join(format!("{id}.part"));
    let meta_path = root.join(format!("{id}.resume.json"));
    let old: Partial = fs::read(&meta_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let mut offset = fs::metadata(&partial_path).map_or(0, |m| m.len());
    if old.validator.is_none() || old.total.is_some_and(|n| offset >= n) {
        offset = 0;
    }
    let mut request = client
        .get(url)
        .header("Accept", "*/*")
        .header("Accept-Encoding", "identity")
        .header("X-Plex-Token", &batch.token)
        .header("X-Plex-Client-Identifier", client_id);
    if offset > 0 {
        // offset>0 implies old.validator.is_some() via the reset above, but
        // don't panic the worker if that invariant ever breaks: fall back to
        // a full download instead of resurrecting an unwrap.
        if let Some(validator) = old.validator.as_deref() {
            request = request
                .header("Range", format!("bytes={offset}-"))
                .header("If-Range", validator);
        } else {
            offset = 0;
        }
    }
    let mut response = request.send().await.map_err(|_| Error::Network)?;
    let status = response.status().as_u16();
    if !matches!(status, 200 | 206) {
        return Err(Error::Http(status));
    }
    let headers = response.headers();
    if headers
        .get("Content-Encoding")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| !h.eq_ignore_ascii_case("identity"))
    {
        return Err(Error::Input("Cache requires an unencoded audio response"));
    }
    let mime = headers
        .get("Content-Type")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !(mime.is_empty()
        || mime.starts_with("audio/")
        || mime.starts_with("application/octet-stream")
        || mime.starts_with("application/binary")
        || mime.starts_with("video/mp4"))
    {
        return Err(Error::Input(
            "Server returned non-audio content for this track",
        ));
    }
    let validator = response_validator(headers);
    let total = if status == 206 {
        let range = headers
            .get("Content-Range")
            .and_then(|h| h.to_str().ok())
            .ok_or(Error::Input("Resumed download has no Content-Range"))?;
        let (start, end, total) =
            parse_range(range).ok_or(Error::Input("Invalid resumed download range"))?;
        if start != offset
            || end + 1 > total
            || old.total.is_some_and(|n| offset > 0 && n != total)
            || old
                .validator
                .as_ref()
                .is_some_and(|v| offset > 0 && validator.as_ref().is_some_and(|new| new != v))
        {
            let _ = fs::remove_file(&partial_path);
            let _ = fs::remove_file(&meta_path);
            return Err(Error::Input("Audio changed or server returned the wrong byte range; retrying from the beginning"));
        }
        if response
            .content_length()
            .is_some_and(|length| length != end - start + 1)
        {
            return Err(Error::Input("Resumed audio length is inconsistent"));
        }
        Some(total)
    } else {
        offset = 0;
        response.content_length()
    };
    let extension = extension(part, mime);
    let mut file = {
        let mut s = state.lock().unwrap_or_else(|poison| poison.into_inner());
        if epoch.load(Ordering::SeqCst) != batch.epoch {
            return Err(Error::Input("Cache request cancelled"));
        }
        if offset == 0 {
            let _ = fs::remove_file(&partial_path);
        }
        if let Some(total) = total {
            room(root, &mut s, total.saturating_sub(offset), &id)?;
        }
        let mut saved = item.clone();
        saved.stations = Value::Null;
        atomic_json(
            &meta_path,
            &Partial {
                total,
                validator,
                extension: extension.clone(),
                namespace: batch.namespace.clone(),
                item: Some(saved),
            },
        )?;
        s.downloading = true;
        s.received = offset;
        s.total = total;
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(offset == 0)
            .append(offset > 0)
            .mode(0o600)
            .open(&partial_path)?
    };
    let mut received = offset;
    while let Some(bytes) = response.chunk().await.map_err(|_| {
        Error::Input("Download interrupted; it will retry when the connection improves")
    })? {
        if epoch.load(Ordering::SeqCst) != batch.epoch {
            return Err(Error::Input("Cache request cancelled"));
        }
        if total.is_some_and(|n| received.saturating_add(bytes.len() as u64) > n) {
            return Err(Error::Input("Audio response exceeds its declared size"));
        }
        if total.is_none() {
            let mut s = state.lock().unwrap_or_else(|poison| poison.into_inner());
            room(root, &mut s, bytes.len() as u64, &id)?;
        }
        file.write_all(&bytes)?;
        received += bytes.len() as u64;
        state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .received = received;
    }
    if received == 0 || total.is_some_and(|n| received != n) {
        return Err(Error::Input(
            "Audio download is incomplete; waiting to resume",
        ));
    }
    file.sync_all()?;
    drop(file);
    let mut s = state.lock().unwrap_or_else(|poison| poison.into_inner());
    if epoch.load(Ordering::SeqCst) != batch.epoch {
        return Err(Error::Input("Cache request cancelled"));
    }
    let filename = format!("{id}.{extension}");
    fs::rename(&partial_path, root.join(&filename))?;
    let _ = fs::remove_file(meta_path);
    // PMS database sizes can differ from the actual served representation.
    // Framing/validators determine completion; retain that actual size locally.
    if let Some(part) = item.media.first_mut().and_then(|m| m.parts.first_mut()) {
        part.size = Some(received);
    }
    item.stations = Value::Null;
    s.manifest.entries.insert(
        id,
        Entry {
            namespace: batch.namespace.clone(),
            item,
            file: filename,
            bytes: received,
            used: now(),
        },
    );
    s.error.clear();
    s.downloading = false;
    save(root, &s)?;
    fs::File::open(root)?.sync_all()?;
    Ok(())
}

pub(crate) fn parse_range(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.strip_prefix("bytes ")?;
    let (span, total) = value.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    let start = start.parse().ok()?;
    let end = end.parse().ok()?;
    let total = total.parse().ok()?;
    (end >= start && end < total).then_some((start, end, total))
}
pub(crate) fn response_validator(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get("ETag")
        .and_then(|h| h.to_str().ok())
        .filter(|h| !h.starts_with("W/"))
        .or_else(|| headers.get("Last-Modified").and_then(|h| h.to_str().ok()))
        .map(str::to_owned)
}
fn extension(part: &str, mime: &str) -> String {
    let candidate = part
        .split('?')
        .next()
        .unwrap_or(part)
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if [
        "mp3", "flac", "ogg", "opus", "m4a", "aac", "wav", "wma", "aiff", "alac",
    ]
    .contains(&candidate.as_str())
    {
        return candidate;
    }
    if mime.contains("mpeg") {
        "mp3"
    } else if mime.contains("flac") {
        "flac"
    } else if mime.contains("wav") {
        "wav"
    } else if mime.contains("mp4") {
        "m4a"
    } else {
        "audio"
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_ranges_and_scopes_without_credentials() {
        assert_eq!(parse_range("bytes 4-9/10"), Some((4, 9, 10)));
        for value in ["bytes 9-4/10", "bytes 4-10/10", "bytes 0-9/*", "garbage"] {
            assert!(parse_range(value).is_none());
        }
        assert_ne!(
            namespace("https://server/", "one"),
            namespace("https://server/", "two")
        );
        assert!(!namespace("https://server/", "one").contains("one"));
    }
    #[test]
    fn eviction_respects_active_tracks_and_enforces_total_audio_budget() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(
            dir.path().into(),
            Config {
                enabled: true,
                limit_mb: 64,
                ahead: 5,
            },
            "fixture",
        )
        .unwrap();
        let mut state = cache
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        for (id, used) in [("1", 1), ("2", 2)] {
            let filename = format!("{}.wav", hash(id));
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(dir.path().join(&filename))
                .unwrap();
            file.set_len(40 * 1024 * 1024).unwrap();
            state.manifest.entries.insert(
                hash(id),
                Entry {
                    namespace: "scope".into(),
                    item: Item {
                        rating_key: id.into(),
                        kind: "track".into(),
                        ..Default::default()
                    },
                    file: filename,
                    bytes: 40 * 1024 * 1024,
                    used,
                },
            );
        }
        state.permanent.insert(format!("scope\0{}", "2"));
        room(dir.path(), &mut state, 1024 * 1024, "new").unwrap();
        assert!(!state.manifest.entries.contains_key(&hash("1")));
        assert!(state.manifest.entries.contains_key(&hash("2")));
        assert!(disk_bytes(dir.path()) + 1024 * 1024 <= 64 * 1024 * 1024);
        drop(state);
        cache.cancel();
        let mut state = cache
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        assert!(room(dir.path(), &mut state, 40 * 1024 * 1024, "new").is_err());
        assert!(state.manifest.entries.contains_key(&hash("2")));
    }
}
