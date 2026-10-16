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
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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
    // Older records also carry an unused `extension`; serde ignores it.
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
    lru_saved: Instant,
}

/// Playback lookups update LRU timestamps in memory; persist them at most this
/// often so a killed process keeps recent usage without rewriting the index
/// on every hit.
const LRU_PERSIST_INTERVAL: Duration = Duration::from_secs(60);
/// Keep unindexed media after an index corruption for this long so it can be
/// inspected/recovered, then resume normal orphan reclamation.
const CORRUPT_PRESERVE_WINDOW: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Download failure classification, mapped to the user-facing [`Error`] at
/// the worker boundary. Retryability must not depend on message text.
#[derive(Debug)]
enum Failure {
    /// Transient: the connection dropped mid-body.
    Interrupted,
    /// Transient: the body ended before the declared size.
    Incomplete,
    /// Transient: the partial was discarded and must restart from zero.
    Restart,
    /// A newer schedule/cancel superseded the batch while awaiting the network.
    Cancelled,
    Other(Error),
}
impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self::Other(error)
    }
}
impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Self::Other(error.into())
    }
}
impl Failure {
    fn retryable(&self) -> bool {
        match self {
            Self::Interrupted | Self::Incomplete | Self::Restart => true,
            Self::Cancelled => false,
            Self::Other(error) => matches!(error, Error::Network | Error::Http(500..=599)),
        }
    }
    fn into_error(self) -> Error {
        match self {
            Self::Interrupted => {
                Error::Input("Download interrupted; it will retry when the connection improves")
            }
            Self::Incomplete => Error::Input("Audio download is incomplete; waiting to resume"),
            Self::Restart => Error::Input(
                "Audio changed or server returned the wrong byte range; retrying from the beginning",
            ),
            Self::Cancelled => Error::Input("Cache request cancelled"),
            Self::Other(error) => error,
        }
    }
}

impl State {
    fn new(config: Config) -> Self {
        Self {
            catalogue_revision: 0,
            gate: Arc::new(Gate::default()),
            manifest: Manifest::default(),
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
            lru_saved: Instant::now(),
        }
    }
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
    worker: Option<crate::rt::Task>,
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
/// Resumable downloads on disk: `(<id>.part path, its bytes, resume record)`.
/// A `.part` without a readable record yields a default (unowned) record.
fn partials(root: &Path) -> Vec<(PathBuf, u64, Partial)> {
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
        .filter_map(|e| {
            let meta = fs::symlink_metadata(e.path())
                .ok()
                .filter(|m| m.is_file())?;
            let info = fs::read(resume_path(&e.path()))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            Some((e.path(), meta.len(), info))
        })
        .collect()
}
/// Resume records, including those whose `.part` bytes were discarded.
fn resume_records(root: &Path) -> Vec<(PathBuf, Partial)> {
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".resume.json"))
        .filter_map(|e| {
            let info = serde_json::from_slice(&fs::read(e.path()).ok()?).ok()?;
            Some((e.path(), info))
        })
        .collect()
}
fn resume_path(part: &Path) -> PathBuf {
    part.with_extension("resume.json")
}
fn part_path(resume: &Path) -> PathBuf {
    let name = resume.file_name().unwrap_or_default().to_string_lossy();
    resume.with_file_name(name.replace(".resume.json", ".part"))
}
fn discard_partial(resume: &Path) {
    let _ = fs::remove_file(part_path(resume));
    let _ = fs::remove_file(resume);
}
fn protected(state: &State, namespace: &str, rating_key: &str) -> bool {
    let key = (namespace.to_owned(), rating_key.to_owned());
    state.pinned.contains(&key) || state.permanent.contains(&key)
}
fn partial_protected(state: &State, info: &Partial) -> bool {
    info.item
        .as_ref()
        .is_some_and(|i| protected(state, &info.namespace, &i.rating_key))
}
/// True when a completed, usable entry already satisfies this partial.
fn superseded(root: &Path, manifest: &Manifest, info: &Partial) -> bool {
    info.item.as_ref().is_some_and(|item| {
        manifest.entries.values().any(|e| {
            e.namespace == info.namespace
                && e.quality_kbps == info.quality_kbps
                && e.item.rating_key == item.rating_key
                && part_key(&e.item) == part_key(item)
                && usable(root, e)
        })
    })
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
        // Retain unindexed media evidence across reopen while a recent
        // quarantine exists. Bounded in time: a permanent flag would disable
        // orphan reclamation forever and leave the quota stuck at "full".
        let preserve_orphans = recovered
            || fs::read_dir(&root)
                .into_iter()
                .flatten()
                .flatten()
                .any(|file| {
                    file.file_name()
                        .to_string_lossy()
                        .starts_with("index.json.corrupt-")
                        && file
                            .metadata()
                            .and_then(|m| m.modified())
                            .ok()
                            .and_then(|t| t.elapsed().ok())
                            .is_none_or(|age| age < CORRUPT_PRESERVE_WINDOW)
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
        if !read_only {
            for (resume, info) in resume_records(&root) {
                if superseded(&root, &manifest, &info) {
                    // Completed elsewhere; the partial only consumes quota.
                    discard_partial(&resume);
                } else if info.quality_kbps > 0 {
                    // Transcodes always restart from zero; keep the record so
                    // the track stays pending, but release the dead bytes.
                    let _ = fs::remove_file(part_path(&resume));
                }
            }
        }
        let state = Arc::new(Mutex::new(State {
            manifest,
            ..State::new(config)
        }));
        let epoch = Arc::new(AtomicU64::new(0));
        let (cancel, changed) = watch::channel(0);
        let (jobs, receiver) = mpsc::unbounded_channel();
        let worker_root = root.clone();
        let worker_state = state.clone();
        let worker_epoch = epoch.clone();
        let client_id = client_id.to_owned();
        // A task on the shared runtime; filesystem work inside it is handed
        // to the blocking pool so it cannot stall audio/D-Bus I/O.
        let worker = (!read_only).then(|| {
            crate::rt::spawn(worker(
                worker_root,
                worker_state,
                worker_epoch,
                receiver,
                changed,
                client_id,
            ))
        });
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
            || items.iter().any(|i| {
                i.kind != crate::model::ItemKind::Track || crate::numeric(&i.rating_key).is_err()
            })
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
        // Persist LRU with manifest mutations, orderly shutdown and at most
        // once per interval here, so playback hits don't rewrite the index
        // each time yet a killed process keeps recent usage.
        if touch && !self.read_only && state.lru_saved.elapsed() >= LRU_PERSIST_INTERVAL {
            state.lru_saved = Instant::now();
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
                state.catalogue_revision = state.catalogue_revision.wrapping_add(1);
                if safe_file(&entry.file) {
                    let _ = fs::remove_file(self.root.join(entry.file));
                }
            }
        }
        // A resumable partial would otherwise resurrect the removed download
        // via pending_items() on the next start or cache reconfiguration.
        for (resume, info) in resume_records(&self.root) {
            if info.namespace == namespace
                && info
                    .item
                    .as_ref()
                    .is_some_and(|i| i.rating_key == rating_key)
            {
                discard_partial(&resume);
            }
        }
        save(&self.root, &state)
    }
    pub fn pending_items(&self, namespace: &str) -> Vec<Item> {
        let state = crate::mutex_lock(&self.state);
        let mut seen = BTreeSet::new();
        resume_records(&self.root)
            .into_iter()
            .map(|(_, info)| info)
            .filter(|p| p.namespace == namespace && !superseded(&self.root, &state.manifest, p))
            .filter_map(|p| p.item)
            .filter(|i| seen.insert(i.rating_key.clone()))
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
            // Cancellation is cooperative (network awaits observe `cancel`);
            // in-flight filesystem steps complete before the task ends.
            worker.join(None);
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
    client_id: String,
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
                let mut transcoded = 0;
                // Cancellation interrupts network awaits only. Filesystem steps
                // run on the blocking pool and are never abandoned mid-way, so
                // a superseding batch cannot race a detached partial write.
                let result = match download(
                    &client,
                    &root,
                    &state,
                    &epoch,
                    &mut changed,
                    &batch,
                    item.clone(),
                    (&client_id, &session),
                    &mut transcoded,
                )
                .await
                {
                    Err(Failure::Cancelled) => None,
                    result => Some(result),
                };
                if transcoded > 0 {
                    if let Ok(uri) = crate::quality::transcode_url(
                        &batch.base,
                        &item.rating_key,
                        transcoded,
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
                        let retryable = error.retryable();
                        let error = error.into_error();
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

const CACHE_FULL: Error = Error::Input("Cache is full; increase its limit or clear cached audio");

fn evictable_entry(state: &State, key: &str, entry: &Entry, current_key: &str) -> bool {
    key != current_key && !protected(state, &entry.namespace, &entry.item.rating_key)
}
fn evictable_partial(state: &State, part: &Path, info: &Partial, current_key: &str) -> bool {
    part.file_name()
        .is_some_and(|n| n.to_string_lossy() != format!("{current_key}.part"))
        && !partial_protected(state, info)
}
/// Bytes room() could reclaim without touching protected or active audio.
fn evictable_bytes(root: &Path, state: &State, current_key: &str) -> u64 {
    let entries: u64 = state
        .manifest
        .entries
        .iter()
        .filter(|(k, e)| evictable_entry(state, k, e, current_key) && safe_file(&e.file))
        .filter_map(|(_, e)| fs::symlink_metadata(root.join(&e.file)).ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum();
    let partials: u64 = partials(root)
        .iter()
        .filter(|(path, _, info)| evictable_partial(state, path, info, current_key))
        .map(|(_, bytes, _)| bytes)
        .sum();
    entries.saturating_add(partials)
}

fn room(root: &Path, state: &mut State, extra: u64, current_key: &str) -> Result<()> {
    let limit = state.config.limit_mb * 1024 * 1024;
    if extra > limit {
        return Err(Error::Input("Audio file exceeds the cache limit"));
    }
    let mut used = disk_bytes(root);
    if used.saturating_add(extra) <= limit {
        return Ok(());
    }
    // Reject before deleting anything when protected/active data alone cannot
    // fit; otherwise a doomed request would evict every unprotected track.
    let fixed = used.saturating_sub(evictable_bytes(root, state, current_key));
    if fixed.saturating_add(extra) > limit {
        return Err(CACHE_FULL);
    }
    let mut evicted = false;
    while used.saturating_add(extra) > limit {
        let old = state
            .manifest
            .entries
            .iter()
            .filter(|(k, e)| evictable_entry(state, k, e, current_key))
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
            let (part, bytes, _) = partials(root)
                .into_iter()
                .find(|(path, _, info)| evictable_partial(state, path, info, current_key))
                .ok_or(CACHE_FULL)?;
            let _ = fs::remove_file(resume_path(&part));
            fs::remove_file(&part)?;
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

/// Await a network step unless a newer schedule/cancel supersedes the batch.
async fn cancellable<T>(
    changed: &mut watch::Receiver<u64>,
    step: impl std::future::Future<Output = T>,
) -> std::result::Result<T, Failure> {
    tokio::select! {
        _ = changed.changed() => Err(Failure::Cancelled),
        value = step => Ok(value),
    }
}

/// Delete a partial download and its resume record off the I/O thread.
async fn remove_partial(partial: &Path, meta: &Path) {
    let (partial, meta) = (partial.to_path_buf(), meta.to_path_buf());
    crate::rt::unblock(move || {
        let _ = fs::remove_file(partial);
        let _ = fs::remove_file(meta);
    })
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn download(
    client: &reqwest::Client,
    root: &Path,
    state: &Arc<Mutex<State>>,
    epoch: &Arc<AtomicU64>,
    changed: &mut watch::Receiver<u64>,
    batch: &Batch,
    mut item: Item,
    identity: (&str, &str),
    transcoded: &mut u32,
) -> std::result::Result<(), Failure> {
    let (client_id, session) = identity;
    let complete = |item: &Item| {
        let quality = download_quality(batch.quality_kbps, item);
        let s = state.lock().unwrap_or_else(|poison| poison.into_inner());
        s.manifest.entries.values().any(|e| {
            e.namespace == batch.namespace
                && e.quality_kbps == quality
                && e.item.rating_key == item.rating_key
                && part_key(item).is_none_or(|p| part_key(&e.item) == Some(p))
                && usable(root, e)
        })
    };
    if complete(&item) {
        return Ok(());
    }
    if part_key(&item).is_none() {
        let url = server_path(
            &batch.base,
            &format!("/library/metadata/{}", item.rating_key),
        )?;
        let response = cancellable(
            changed,
            client
                .get(url)
                .header("Accept", "application/json")
                .header("X-Plex-Token", &batch.token)
                .header("X-Plex-Client-Identifier", client_id)
                .send(),
        )
        .await?
        .map_err(|_| Error::Network)?;
        if !response.status().is_success() {
            return Err(Error::Http(response.status().as_u16()).into());
        }
        let envelope: Envelope = cancellable(
            changed,
            crate::plex::bounded_json(response, "cache track metadata"),
        )
        .await??;
        item = envelope
            .container
            .items
            .into_iter()
            .find(|i| i.kind == crate::model::ItemKind::Track && i.rating_key == item.rating_key)
            .ok_or(Error::ProtocolAt("cache track metadata"))?;
    }
    let part = part_key(&item).ok_or(Error::Input("Track has no downloadable media part"))?;
    // Metadata may have been completed above: check for a finished copy again.
    if complete(&item) {
        return Ok(());
    }
    let quality = download_quality(batch.quality_kbps, &item);
    *transcoded = quality;
    let url = if quality > 0 {
        crate::quality::transcode_url(&batch.base, &item.rating_key, quality, session)?
    } else {
        server_path(&batch.base, part)?
    };
    let original_id = key(&batch.namespace, &item);
    let id = if quality > 0 {
        hash(&format!("{original_id}\0mp3:{quality}"))
    } else {
        original_id
    };
    let partial_path = root.join(format!("{id}.part"));
    let meta_path = root.join(format!("{id}.resume.json"));
    // Partials of this track at another quality/part can never complete this
    // request and would otherwise count toward the quota indefinitely. The
    // directory scan and record reads stay off the shared I/O thread: on a
    // large cache on phone flash they could otherwise stall audio streaming.
    let (old, mut offset): (Partial, u64) = {
        let (root, namespace, rating_key) = (
            root.to_path_buf(),
            batch.namespace.clone(),
            item.rating_key.clone(),
        );
        let (partial_path, meta_path) = (partial_path.clone(), meta_path.clone());
        crate::rt::unblock(move || {
            for (resume, info) in resume_records(&root) {
                if resume != meta_path
                    && info.namespace == namespace
                    && info
                        .item
                        .as_ref()
                        .is_some_and(|i| i.rating_key == rating_key)
                {
                    discard_partial(&resume);
                }
            }
            let old: Partial = fs::read(&meta_path)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            (old, fs::metadata(&partial_path).map_or(0, |m| m.len()))
        })
        .await
    };
    if quality > 0
        || old.quality_kbps != quality
        || old.validator.is_none()
        || old.total.is_some_and(|n| offset >= n)
    {
        offset = 0;
    }
    let mut response = loop {
        let mut request = client
            .get(url.clone())
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
        let response = cancellable(changed, request.send())
            .await?
            .map_err(|_| Error::Network)?;
        if offset > 0 && response.status().as_u16() == 416 {
            // The partial can never be extended (e.g. an unknown-length
            // download that got every byte but died before the rename).
            // Repeating the range would fail forever: restart from zero.
            remove_partial(&partial_path, &meta_path).await;
            offset = 0;
            continue;
        }
        break response;
    };
    let status = response.status().as_u16();
    if !matches!(status, 200 | 206) {
        return Err(Error::Http(status).into());
    }
    let headers = response.headers();
    if headers
        .get("Content-Encoding")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| !h.eq_ignore_ascii_case("identity"))
    {
        return Err(Error::Input("Cache requires an unencoded audio response").into());
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
        return Err(Error::Input("Server returned non-audio content for this track").into());
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
            remove_partial(&partial_path, &meta_path).await;
            return Err(Failure::Restart);
        }
        if response
            .content_length()
            .is_some_and(|length| length != end - start + 1)
        {
            return Err(Error::Input("Resumed audio length is inconsistent").into());
        }
        Some(total)
    } else {
        offset = 0;
        response.content_length()
    };
    let extension = if quality > 0 {
        "mp3".into()
    } else {
        extension(part, mime.as_str())
    };
    let mut file = {
        let (root, state, epoch, id) =
            (root.to_path_buf(), state.clone(), epoch.clone(), id.clone());
        let (batch_epoch, quality_kbps, namespace) =
            (batch.epoch, quality, batch.namespace.clone());
        let (partial_path, meta_path) = (partial_path.clone(), meta_path.clone());
        let mut saved = item.clone();
        saved.stations = Value::Null;
        crate::rt::unblock(move || -> std::result::Result<fs::File, Failure> {
            let mut s = state.lock().unwrap_or_else(|poison| poison.into_inner());
            if epoch.load(Ordering::SeqCst) != batch_epoch {
                return Err(Error::Input("Cache request cancelled").into());
            }
            if offset == 0 {
                let _ = fs::remove_file(&partial_path);
            }
            if let Some(total) = total {
                room(&root, &mut s, total.saturating_sub(offset), &id)?;
            }
            atomic_json(
                &meta_path,
                &Partial {
                    quality_kbps,
                    total,
                    validator,
                    namespace,
                    item: Some(saved),
                },
            )?;
            s.downloading = true;
            s.received = offset;
            s.total = total;
            Ok(OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(offset == 0)
                .append(offset > 0)
                .mode(0o600)
                .open(&partial_path)?)
        })
        .await?
    };
    let mut received = offset;
    let mut reserved_until = total.unwrap_or(offset);
    while let Some(bytes) = cancellable(changed, response.chunk())
        .await?
        .map_err(|_| Failure::Interrupted)?
    {
        if epoch.load(Ordering::SeqCst) != batch.epoch {
            return Err(Error::Input("Cache request cancelled").into());
        }
        let length = bytes.len() as u64;
        if total.is_some_and(|n| received.saturating_add(length) > n) {
            return Err(Error::Input("Audio response exceeds its declared size").into());
        }
        let reserve = total.is_none() && received.saturating_add(length) > reserved_until;
        let (job_root, job_state, job_id) = (root.to_path_buf(), state.clone(), id.clone());
        // Each chunk is written before the next is read, as before; only the
        // thread changes. A write is never abandoned half-way.
        let (returned, written) = crate::rt::unblock(move || {
            let written = (|| -> std::result::Result<Option<u64>, Failure> {
                let mut admitted = None;
                if reserve {
                    let mut s = job_state
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner());
                    // Amortize filesystem accounting across 4 MiB windows. If a
                    // protected cache cannot reserve headroom, admit just this chunk.
                    let window = (4 * 1024 * 1024).max(length);
                    admitted = Some(if room(&job_root, &mut s, window, &job_id).is_ok() {
                        window
                    } else {
                        room(&job_root, &mut s, length, &job_id)?;
                        length
                    });
                }
                file.write_all(&bytes)?;
                Ok(admitted)
            })();
            (file, written)
        })
        .await;
        file = returned;
        if let Some(admitted) = written? {
            reserved_until = received.saturating_add(admitted);
        }
        received += length;
        state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .received = received;
    }
    if received == 0 || total.is_some_and(|n| received != n) {
        return Err(Failure::Incomplete);
    }
    // PMS database sizes can differ from the actual served representation.
    // Framing/validators determine completion; retain that actual size locally.
    if let Some(part) = item.media.first_mut().and_then(|m| m.parts.first_mut()) {
        part.size = Some(received);
    }
    item.stations = Value::Null;
    let (root, state, epoch) = (root.to_path_buf(), state.clone(), epoch.clone());
    let (batch_epoch, quality_kbps, namespace) = (batch.epoch, quality, batch.namespace.clone());
    crate::rt::unblock(move || -> std::result::Result<(), Failure> {
        file.sync_all()?;
        drop(file);
        let mut s = state.lock().unwrap_or_else(|poison| poison.into_inner());
        if epoch.load(Ordering::SeqCst) != batch_epoch {
            return Err(Error::Input("Cache request cancelled").into());
        }
        let filename = format!("{id}.{extension}");
        fs::rename(&partial_path, root.join(&filename))?;
        let _ = fs::remove_file(meta_path);
        s.manifest.entries.insert(
            id,
            Entry {
                quality_kbps,
                namespace,
                item,
                file: filename,
                bytes: received,
                used: now(),
            },
        );
        s.catalogue_revision = s.catalogue_revision.wrapping_add(1);
        s.error.clear();
        s.downloading = false;
        save(&root, &s)?;
        fs::File::open(&root)?.sync_all()?;
        Ok(())
    })
    .await
}

/// Bitrate of an original-quality download whose format the engine cannot
/// decode: the server transcodes it to MP3 so offline playback works.
pub(crate) const UNDECODABLE_DOWNLOAD_KBPS: u32 = 320;

/// The transcode bitrate a download uses (0 = original file).
pub(crate) fn download_quality(requested_kbps: u32, item: &Item) -> u32 {
    if requested_kbps > 0 {
        requested_kbps
    } else if crate::audio::formats::item_supported(item) {
        0
    } else {
        UNDECODABLE_DOWNLOAD_KBPS
    }
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
                        kind: crate::model::ItemKind::Track,
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

#[cfg(test)]
mod review_tests {
    use super::*;

    fn entry(dir: &Path, state: &mut State, id: &str, used: u64) {
        let filename = format!("{}.wav", hash(id));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(dir.join(&filename))
            .unwrap();
        file.set_len(40 * 1024 * 1024).unwrap();
        state.manifest.entries.insert(
            hash(id),
            Entry {
                quality_kbps: 0,
                namespace: "scope".into(),
                item: Item {
                    rating_key: id.into(),
                    kind: crate::model::ItemKind::Track,
                    ..Default::default()
                },
                file: filename,
                bytes: 40 * 1024 * 1024,
                used,
            },
        );
    }

    // configure() runs room() eagerly and only restores the old config when
    // room() fails. A limit that pinned/permanent downloads cannot fit into
    // first evicts every unprotected track, then errors and reverts the limit:
    // the user sees "Cache is full" yet has lost all auto-cached audio.
    #[test]
    fn rejected_cache_limit_does_not_evict_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(
            dir.path().into(),
            Config {
                enabled: true,
                limit_mb: 256,
                ahead: 5,
            },
            "fixture",
        )
        .unwrap();
        {
            let mut state = crate::mutex_lock(&cache.state);
            entry(dir.path(), &mut state, "1", 1);
            entry(dir.path(), &mut state, "2", 2);
            entry(dir.path(), &mut state, "3", 3);
        }
        cache.protect(
            "scope",
            &[
                Item {
                    rating_key: "2".into(),
                    ..Default::default()
                },
                Item {
                    rating_key: "3".into(),
                    ..Default::default()
                },
            ],
        );
        let rejected = cache.configure(
            Config {
                enabled: true,
                limit_mb: 64,
                ahead: 5,
            },
            None,
        );
        assert!(rejected.is_err());
        assert_eq!(cache.config().limit_mb, 256);
        assert!(
            crate::mutex_lock(&cache.state)
                .manifest
                .entries
                .contains_key(&hash("1")),
            "failed configure() still evicted an unprotected track"
        );
    }
}

#[cfg(test)]
mod maintenance_tests {
    use super::*;

    fn track(id: &str) -> Item {
        Item {
            rating_key: id.into(),
            kind: crate::model::ItemKind::Track,
            media: vec![crate::model::Media {
                parts: vec![crate::model::Part {
                    key: format!("/library/parts/{id}/file.wav"),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }
    }
    fn seed_partial(dir: &Path, id: &str, quality_kbps: u32, item: &Item, bytes: usize) {
        fs::write(dir.join(format!("{id}.part")), vec![0; bytes]).unwrap();
        atomic_json(
            &dir.join(format!("{id}.resume.json")),
            &Partial {
                quality_kbps,
                total: None,
                validator: Some("\"v\"".into()),
                namespace: "scope".into(),
                item: Some(item.clone()),
            },
        )
        .unwrap();
    }

    #[test]
    fn retryability_is_structural_not_textual() {
        for failure in [Failure::Interrupted, Failure::Incomplete, Failure::Restart] {
            assert!(failure.retryable());
            assert!(matches!(failure.into_error(), Error::Input(_)));
        }
        assert!(Failure::Other(Error::Network).retryable());
        assert!(Failure::Other(Error::Http(503)).retryable());
        assert!(!Failure::Other(Error::Http(404)).retryable());
        // The same text through a different path must not become retryable.
        assert!(!Failure::Other(Error::Input(
            "Audio download is incomplete; waiting to resume"
        ))
        .retryable());
    }

    #[test]
    fn stale_corrupt_backup_no_longer_blocks_orphan_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let orphan = dir.path().join(format!("{}.wav", "a".repeat(64)));
        fs::write(&orphan, b"orphan").unwrap();
        let backup = dir.path().join("index.json.corrupt-old");
        fs::write(&backup, b"{broken").unwrap();
        fs::File::options()
            .write(true)
            .open(&backup)
            .unwrap()
            .set_modified(SystemTime::now() - CORRUPT_PRESERVE_WINDOW - Duration::from_secs(60))
            .unwrap();
        drop(Cache::new(dir.path().into(), Config::default(), "fixture").unwrap());
        assert!(
            !orphan.exists(),
            "old quarantine must not preserve orphans forever"
        );
        assert!(backup.exists(), "the corrupt evidence itself is kept");
    }

    #[test]
    fn pending_items_are_deduplicated_and_skip_completed_or_dead_partials() {
        let dir = tempfile::tempdir().unwrap();
        let song = track("1");
        seed_partial(dir.path(), &hash("p0"), 0, &song, 4);
        seed_partial(dir.path(), &hash("p320"), 320, &song, 4);
        // Track 2 is already cached at quality 0: its partial is stale.
        let done = track("2");
        seed_partial(dir.path(), &hash("p2"), 0, &done, 4);
        let file = format!("{}.wav", hash("e2"));
        fs::write(dir.path().join(&file), b"cached").unwrap();
        let mut manifest = Manifest::default();
        manifest.entries.insert(
            hash("e2"),
            Entry {
                quality_kbps: 0,
                namespace: "scope".into(),
                item: done,
                file,
                bytes: 6,
                used: 1,
            },
        );
        atomic_json(&dir.path().join("index.json"), &manifest).unwrap();
        let cache = Cache::new(dir.path().into(), Config::default(), "fixture").unwrap();
        let pending = cache.pending_items("scope");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].rating_key, "1");
        // Superseded partial removed entirely; non-resumable transcode bytes
        // released while its record keeps the track pending.
        assert!(!dir.path().join(format!("{}.part", hash("p2"))).exists());
        assert!(!dir
            .path()
            .join(format!("{}.resume.json", hash("p2")))
            .exists());
        assert!(!dir.path().join(format!("{}.part", hash("p320"))).exists());
        assert!(dir
            .path()
            .join(format!("{}.resume.json", hash("p320")))
            .exists());
        assert!(dir.path().join(format!("{}.part", hash("p0"))).exists());
    }

    #[test]
    fn legacy_resume_records_with_extension_still_load() {
        let info: Partial = serde_json::from_value(json!({
            "quality_kbps": 0, "total": 10, "validator": "\"v\"", "extension": "wav",
            "namespace": "scope", "item": track("1"),
        }))
        .unwrap();
        assert_eq!(info.total, Some(10));
    }

    #[test]
    fn lru_touches_are_persisted_periodically() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().into(), Config::default(), "fixture").unwrap();
        let song = track("1");
        let file = format!("{}.wav", hash("e1"));
        fs::write(dir.path().join(&file), b"cached").unwrap();
        {
            let mut state = crate::mutex_lock(&cache.state);
            state.manifest.entries.insert(
                hash("e1"),
                Entry {
                    quality_kbps: 0,
                    namespace: "scope".into(),
                    item: song.clone(),
                    file,
                    bytes: 6,
                    used: 1,
                },
            );
            save(dir.path(), &state).unwrap();
            state.lru_saved = Instant::now()
                .checked_sub(LRU_PERSIST_INTERVAL)
                .unwrap_or(state.lru_saved);
        }
        assert!(cache.lookup("scope", &song, true).is_some());
        // Read the index without dropping the cache (Drop would also save).
        let saved: Manifest =
            serde_json::from_slice(&fs::read(dir.path().join("index.json")).unwrap()).unwrap();
        assert!(saved.entries[&hash("e1")].used > 1);
    }
}
