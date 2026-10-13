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
    #[serde(default)]
    quality_kbps: u32,
    namespace: String,
    item: Item,
    file: String,
    bytes: u64,
    used: u64,
}
#[derive(Default, Deserialize, Serialize)]
struct Manifest {
    #[serde(deserialize_with = "manifest_entries")]
    entries: BTreeMap<String, Entry>,
}

fn manifest_entries<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, Entry>, D::Error> {
    let raw = BTreeMap::<String, Value>::deserialize(deserializer)?;
    let mut entries = BTreeMap::new();
    let mut rejected = 0;
    for (key, value) in raw {
        match serde_json::from_value(value) {
            Ok(entry) => {
                entries.insert(key, entry);
            }
            Err(_) => rejected += 1,
        }
    }
    if rejected > 0 {
        crate::diagnostics::event(format_args!(
            "audio cache index: skipped {rejected} malformed entries"
        ));
    }
    Ok(entries)
}
#[derive(Default, Deserialize, Serialize)]
struct Partial {
    #[serde(default)]
    quality_kbps: u32,
    total: Option<u64>,
    validator: Option<String>,
    extension: String,
    #[serde(default)]
    namespace: String,
    #[serde(default)]
    item: Option<Item>,
}
struct State {
    catalogue_revision: u64,
    gate: Arc<Gate>,
    manifest: Manifest,
    config: Config,
    pinned: BTreeSet<(String, String)>,
    permanent: BTreeSet<(String, String)>,
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
    quality_kbps: u32,
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
            if name == "index.json"
                || name.starts_with("index.json.corrupt-")
                || name == "cache.lock"
                || name.ends_with(".resume.json")
            {
                return None;
            }
            fs::symlink_metadata(e.path())
                .ok()
                .filter(|m| m.is_file())
                .map(|m| m.len())
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
        let mut recovered = false;
        let mut manifest: Manifest = match fs::read(root.join("index.json")) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(manifest) => manifest,
                Err(_) => {
                    if !read_only {
                        crate::quarantine_corrupt(&root.join("index.json"), &bytes)?;
                    }
                    recovered = true;
                    crate::diagnostics::event(format_args!(
                        "corrupt audio cache index preserved; using empty catalogue"
                    ));
                    Manifest::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Manifest::default(),
            Err(error) => return Err(error.into()),
        };
        manifest.entries.retain(|_, e| usable(&root, e));
        // Retain unindexed media evidence across reopen while quarantine exists.
        let preserve_orphans = recovered
            || fs::read_dir(&root)
                .into_iter()
                .flatten()
                .flatten()
                .any(|file| {
                    file.file_name()
                        .to_string_lossy()
                        .starts_with("index.json.corrupt-")
                });
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
                && !preserve_orphans
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
            catalogue_revision: 0,
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
    pub(crate) fn catalogue_revision(&self) -> u64 {
        crate::mutex_lock(&self.state).catalogue_revision
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
            .map(|i| (namespace.to_owned(), i.rating_key.clone()))
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
            state.pinned.insert((namespace.to_owned(), key.to_owned()));
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
        state.catalogue_revision = state.catalogue_revision.wrapping_add(1);
        state.permanent.clear();
        state.error.clear();
        save(&self.root, &state)
    }
    pub fn schedule(&self, server: &str, token: &str, items: Vec<Item>) -> Result<()> {
        self.schedule_with_quality(server, token, items, 0)
    }
    pub fn schedule_with_quality(
        &self,
        server: &str,
        token: &str,
        items: Vec<Item>,
        quality_kbps: u32,
    ) -> Result<()> {
        crate::quality::Config {
            download_kbps: quality_kbps,
            ..Default::default()
        }
        .validate()?;
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
                .map(|i| (ns.clone(), i.rating_key.clone()))
                .collect();
            state.pending = items.len();
            state.error.clear();
            state.errors.clear();
        }
        self.jobs
            .as_ref()
            .ok_or(Error::Input("Cache worker unavailable"))?
            .send(Batch {
                quality_kbps,
                epoch,
                base,
                token: token.into(),
                namespace: ns,
                items,
            })
            .map_err(|_| Error::Input("Cache worker unavailable"))
    }
    pub fn lookup(&self, namespace: &str, item: &Item, touch: bool) -> Option<(PathBuf, Item)> {
        let candidates: Vec<_> = crate::mutex_lock(&self.state)
            .manifest
            .entries
            .iter()
            .filter(|(_, e)| {
                e.namespace == namespace
                    && e.item.rating_key == item.rating_key
                    && part_key(item).is_none_or(|p| part_key(&e.item) == Some(p))
            })
            .map(|(key, entry)| (key.clone(), entry.clone()))
            .collect();
        let found = candidates
            .iter()
            .filter(|(_, e)| usable(&self.root, e))
            .max_by_key(|(_, e)| e.used)
            .map(|(k, _)| k.clone())?;
        let mut state = crate::mutex_lock(&self.state);
        let entry = state.manifest.entries.get_mut(&found)?;
        if touch && !self.read_only {
            entry.used = now();
        }
        let result = (self.root.join(&entry.file), entry.item.clone());
        // Persist LRU with the next manifest mutation or orderly shutdown;
        // playback lookups must not rewrite the index for every hit.
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
                state.catalogue_revision = state.catalogue_revision.wrapping_add(1);
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
        if !self.read_only {
            let _ = save(&self.root, &crate::mutex_lock(&self.state));
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
        let mut terminal = BTreeSet::new();
        for attempt in 0..4 {
            let mut failed = false;
            for (position, item) in batch.items.iter().enumerate() {
                if epoch.load(Ordering::SeqCst) != batch.epoch || !gate.allowed() {
                    break;
                }
                if terminal.contains(&item.rating_key) {
                    continue;
                }
                {
                    let mut s = state.lock().unwrap_or_else(|poison| poison.into_inner());
                    s.pending = batch.items.len() - position;
                    s.active_key = item.rating_key.clone();
                }
                let session = uuid::Uuid::new_v4().to_string();
                let result = tokio::select! {
                    _=changed.changed()=>None,
                    result=download(&client,&root,&state,&epoch,&batch,item.clone(),(client_id,&session))=>Some(result),
                };
                if batch.quality_kbps > 0 {
                    if let Ok(uri) = crate::quality::transcode_url(
                        &batch.base,
                        &item.rating_key,
                        batch.quality_kbps,
                        &session,
                    ) {
                        if let Some(stop) = crate::quality::stop_url(&uri) {
                            let _ = client
                                .get(stop)
                                .header("X-Plex-Token", &batch.token)
                                .timeout(Duration::from_secs(2))
                                .send()
                                .await;
                        }
                    }
                }
                let Some(result) = result else { break };
                if let Err(error) = result {
                    if epoch.load(Ordering::SeqCst) == batch.epoch {
                        let retryable = matches!(error, Error::Network | Error::Http(500..=599))
                            || matches!(&error, Error::Input("Download interrupted; it will retry when the connection improves" | "Audio download is incomplete; waiting to resume" | "Audio changed or server returned the wrong byte range; retrying from the beginning"));
                        failed |= retryable;
                        if !retryable {
                            terminal.insert(item.rating_key.clone());
                        }
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
            if !failed || attempt == 3 || !gate.allowed() {
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
    let mut used = disk_bytes(root);
    while used.saturating_add(extra) > limit {
        let old = state
            .manifest
            .entries
            .iter()
            .filter(|(k, e)| {
                k.as_str() != current_key
                    && !state
                        .pinned
                        .contains(&(e.namespace.clone(), e.item.rating_key.clone()))
                    && !state
                        .permanent
                        .contains(&(e.namespace.clone(), e.item.rating_key.clone()))
            })
            .min_by_key(|(_, e)| e.used)
            .map(|(k, _)| k.clone());
        if let Some(old) = old {
            if let Some(entry) = state.manifest.entries.remove(&old) {
                state.catalogue_revision = state.catalogue_revision.wrapping_add(1);
                if safe_file(&entry.file) {
                    let path = root.join(entry.file);
                    let bytes = fs::metadata(&path).map_or(0, |m| m.len());
                    if fs::remove_file(path).is_ok() {
                        used = used.saturating_sub(bytes);
                    }
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
                            .contains(&(info.namespace.clone(), i.rating_key.clone()))
                            || state
                                .permanent
                                .contains(&(info.namespace.clone(), i.rating_key.clone()))
                    })
                })
                .ok_or(Error::Input(
                    "Cache is full; increase its limit or clear cached audio",
                ))?;
            let _ = fs::remove_file(partial.path().with_extension("resume.json"));
            let bytes = partial.metadata()?.len();
            fs::remove_file(partial.path())?;
            used = used.saturating_sub(bytes);
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
    identity: (&str, &str),
) -> Result<()> {
    let (client_id, session) = identity;
    {
        let s = state.lock().unwrap_or_else(|poison| poison.into_inner());
        if s.manifest.entries.values().any(|e| {
            e.namespace == batch.namespace
                && e.quality_kbps == batch.quality_kbps
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
        let envelope: Envelope =
            crate::plex::bounded_json(response, "cache track metadata").await?;
        item = envelope
            .container
            .items
            .into_iter()
            .find(|i| i.kind == "track" && i.rating_key == item.rating_key)
            .ok_or(Error::ProtocolAt("cache track metadata"))?;
    }
    let part = part_key(&item).ok_or(Error::Input("Track has no downloadable media part"))?;
    let url = if batch.quality_kbps > 0 {
        crate::quality::transcode_url(&batch.base, &item.rating_key, batch.quality_kbps, session)?
    } else {
        server_path(&batch.base, part)?
    };
    let original_id = key(&batch.namespace, &item);
    let id = if batch.quality_kbps > 0 {
        hash(&format!("{original_id}\0mp3:{}", batch.quality_kbps))
    } else {
        original_id
    };
    let partial_path = root.join(format!("{id}.part"));
    let meta_path = root.join(format!("{id}.resume.json"));
    let old: Partial = fs::read(&meta_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let mut offset = fs::metadata(&partial_path).map_or(0, |m| m.len());
    if batch.quality_kbps > 0
        || old.quality_kbps != batch.quality_kbps
        || old.validator.is_none()
        || old.total.is_some_and(|n| offset >= n)
    {
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
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !crate::plex::audio_mime(&mime) {
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
    let extension = if batch.quality_kbps > 0 {
        "mp3".into()
    } else {
        extension(part, mime.as_str())
    };
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
                quality_kbps: batch.quality_kbps,
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
    let mut reserved_until = total.unwrap_or(offset);
    while let Some(bytes) = response.chunk().await.map_err(|_| {
        Error::Input("Download interrupted; it will retry when the connection improves")
    })? {
        if epoch.load(Ordering::SeqCst) != batch.epoch {
            return Err(Error::Input("Cache request cancelled"));
        }
        if total.is_some_and(|n| received.saturating_add(bytes.len() as u64) > n) {
            return Err(Error::Input("Audio response exceeds its declared size"));
        }
        if total.is_none() && received.saturating_add(bytes.len() as u64) > reserved_until {
            let mut s = state.lock().unwrap_or_else(|poison| poison.into_inner());
            // Amortize filesystem accounting across 4 MiB windows. If a
            // protected cache cannot reserve headroom, admit just this chunk.
            let reserve = (4 * 1024 * 1024).max(bytes.len() as u64);
            let admitted = if room(root, &mut s, reserve, &id).is_ok() {
                reserve
            } else {
                room(root, &mut s, bytes.len() as u64, &id)?;
                bytes.len() as u64
            };
            reserved_until = received.saturating_add(admitted);
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
            quality_kbps: batch.quality_kbps,
            namespace: batch.namespace.clone(),
            item,
            file: filename,
            bytes: received,
            used: now(),
        },
    );
    s.catalogue_revision = s.catalogue_revision.wrapping_add(1);
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
                    quality_kbps: 0,
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
        state.permanent.insert(("scope".into(), "2".into()));
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
