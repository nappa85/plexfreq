use crate::{
    model::{Envelope, Item},
    plex::{server_path, server_url},
    Error, Result,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
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
use tokio::sync::watch;
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
    /// Eviction journal: files whose entries were committed as removed but
    /// whose unlink may not have happened (failure or crash). Finished before
    /// further eviction and at startup. Absent when empty, so the index stays
    /// byte-compatible; older versions ignore it and reclaim the files as
    /// unindexed orphans.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    evicting: Vec<String>,
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
/// Unindexed completed media (`<id>.<ext>`) preserved after an index
/// corruption. Keyed by file name in [`State::orphans`].
#[derive(Clone)]
struct Orphan {
    bytes: u64,
    /// Modification time (seconds); the recovered entry's LRU stamp.
    modified: u64,
}
/// In-memory cache state. Lock discipline: the state mutex is held only for
/// in-memory reads/updates, never across filesystem calls. Filesystem
/// mutations of the cache directory (eviction, index commits, completion
/// renames) are serialized by the separate writer mutex, always taken
/// before the state mutex.
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
    /// Rating keys whose download the server refused as missing (404 for an
    /// original file, 400 for a transcode); confirmed by the runtime.
    suspects: BTreeSet<String>,
    active_key: String,
    lru_saved: Instant,
    /// The manifest changed without a commit (orphan adoption on lookup).
    dirty: bool,
    orphans: BTreeMap<String, Orphan>,
    /// Orphans re-indexed / reclaimed since this cache was opened.
    recovered: u64,
    reclaimed: u64,
    /// While set (and in the future), unindexed media is preserved.
    preserve_until: Option<u64>,
    /// Downloads (by download id) the server answered with a lasting refusal,
    /// and that error: skipped by later batches until [`COOLDOWN`] passes or
    /// the user asks again, instead of being re-requested on every schedule.
    cooldown: HashMap<String, (Instant, String)>,
    /// Batches up to this epoch ran before the user asked to retry: their
    /// refusals are not cooled down any more.
    forgotten: u64,
}

/// Playback lookups update LRU timestamps in memory; persist them at most this
/// often so a killed process keeps recent usage without rewriting the index
/// on every hit.
const LRU_PERSIST_INTERVAL: Duration = Duration::from_secs(60);
/// Keep unindexed media after an index corruption for this long so it can be
/// recovered (re-indexed when a requested track's id matches its file-name
/// hash), then resume normal orphan reclamation. Quota pressure reclaims
/// preserved orphans earlier, before any playable track.
const CORRUPT_PRESERVE_WINDOW: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// How long a refused download is not requested again automatically.
const COOLDOWN: Duration = Duration::from_secs(30 * 60);
const UNENCODED_REQUIRED: &str = "Cache requires an unencoded audio response";
const NON_AUDIO: &str = "Server returned non-audio content for this track";
/// Transcode bitrates a download id may encode.
const TRANSCODE_KBPS: &[u32] = crate::quality::BITRATES;

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
    /// The live stream is starving: the connection was dropped to yield the
    /// link; the item resumes by range once the hold is released.
    Held,
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
            Self::Cancelled | Self::Held => false,
            Self::Other(error) => matches!(error, Error::Network | Error::Http(500..=599)),
        }
    }
    /// A refusal that a retry within [`COOLDOWN`] would only repeat: client
    /// errors except authorization (401: the sign-in, not the item), missing
    /// (404, checked by the runtime), timeout and rate limit, and responses that are not cacheable audio. Local errors
    /// (disk, capacity) never qualify: the user can fix them at any time.
    fn lasting(&self) -> bool {
        match self {
            Self::Other(Error::Http(status)) => {
                (400..500).contains(status) && !matches!(status, 401 | 404 | 408 | 429)
            }
            Self::Other(Error::Input(message)) => {
                *message == UNENCODED_REQUIRED || *message == NON_AUDIO
            }
            _ => false,
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
            Self::Cancelled | Self::Held => Error::Input("Cache request cancelled"),
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
            suspects: BTreeSet::new(),
            active_key: String::new(),
            lru_saved: Instant::now(),
            dirty: false,
            orphans: BTreeMap::new(),
            recovered: 0,
            reclaimed: 0,
            preserve_until: None,
            cooldown: HashMap::new(),
            forgotten: 0,
        }
    }
    fn touch_catalogue(&mut self) {
        self.catalogue_revision = self.catalogue_revision.wrapping_add(1);
    }
}

pub struct Gate {
    wifi_only: AtomicBool,
    paused: AtomicBool,
    wifi: AtomicBool,
    /// Downloads yield to a starving live stream. Separate from `allowed`:
    /// a hold never cancels, resets progress or drops a partial.
    held: watch::Sender<bool>,
}
impl Default for Gate {
    fn default() -> Self {
        Self {
            wifi_only: AtomicBool::default(),
            paused: AtomicBool::default(),
            wifi: AtomicBool::default(),
            held: watch::Sender::new(false),
        }
    }
}
impl Gate {
    pub fn allowed(&self) -> bool {
        !self.paused.load(Ordering::SeqCst)
            && (!self.wifi_only.load(Ordering::SeqCst) || self.wifi.load(Ordering::SeqCst))
    }
    pub(crate) fn held(&self) -> bool {
        *self.held.borrow()
    }
}

/// Wait while downloads are held; a cancel still ends the wait.
async fn unheld(
    gate: &Gate,
    changed: &mut watch::Receiver<u64>,
) -> std::result::Result<(), Failure> {
    let mut held = gate.held.subscribe();
    if !*held.borrow_and_update() {
        return Ok(());
    }
    // Biased: a cancel that lands with the release must win.
    tokio::select! {
        biased;
        _ = changed.changed() => Err(Failure::Cancelled),
        _ = async { let _ = held.wait_for(|held| !*held).await; } => Ok(()),
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
            let mut state = crate::mutex_lock(&self.state);
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
    /// Yield the link to a starving live stream (or stop yielding). A held
    /// original download drops its connection and later resumes by range;
    /// other downloads finish their item and wait before the next.
    pub fn hold(&self, held: bool) {
        if self.gate.held.send_if_modified(|current| {
            let changed = *current != held;
            *current = held;
            changed
        }) {
            crate::diagnostics::event(format_args!("cache hold active={held}"));
        }
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

/// Single-slot download queue. Every schedule carries the complete wanted
/// list and supersedes earlier ones, so a batch the worker has not started
/// is replaced (coalesced) rather than queued: bursts hold at most one
/// waiting batch no matter how long the worker is busy.
#[derive(Default)]
struct Jobs {
    slot: Mutex<Option<Batch>>,
    ready: tokio::sync::Notify,
    closed: AtomicBool,
    superseded: AtomicU64,
}
impl Jobs {
    fn submit(&self, batch: Batch) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        if crate::mutex_lock(&self.slot).replace(batch).is_some() {
            self.superseded.fetch_add(1, Ordering::SeqCst);
        }
        self.ready.notify_one();
        true
    }
    async fn next(&self) -> Option<Batch> {
        loop {
            let ready = self.ready.notified();
            if let Some(batch) = crate::mutex_lock(&self.slot).take() {
                return Some(batch);
            }
            if self.closed.load(Ordering::SeqCst) {
                return None;
            }
            ready.await;
        }
    }
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        crate::mutex_lock(&self.slot).take();
        self.ready.notify_one();
    }
    #[cfg(test)]
    fn queued(&self) -> usize {
        usize::from(crate::mutex_lock(&self.slot).is_some())
    }
}

pub struct Cache {
    root: PathBuf,
    state: Arc<Mutex<State>>,
    writer: Arc<Mutex<()>>,
    epoch: Arc<AtomicU64>,
    cancel: watch::Sender<u64>,
    jobs: Arc<Jobs>,
    worker: Option<crate::rt::Task>,
    _lock: Option<fs::File>,
    read_only: bool,
}

pub fn namespace(server: &str, token: &str) -> String {
    crate::sha256_hex(&format!("{server}\0{token}"))
}
fn seconds(time: std::io::Result<SystemTime>) -> Option<u64> {
    Some(time.ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs())
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
    crate::sha256_hex(&format!(
        "{namespace}\0{}\0{}",
        item.rating_key,
        part_key(item).unwrap_or("")
    ))
}
/// Cache id (file stem) of a track's download at `quality` (0 = original).
fn download_id(namespace: &str, item: &Item, quality: u32) -> String {
    let original = key(namespace, item);
    if quality > 0 {
        crate::sha256_hex(&format!("{original}\0mp3:{quality}"))
    } else {
        original
    }
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
/// Leave `item` as a completed download under `root` for `namespace`, as a
/// crash after the completion rename does; `Cache::new` indexes it.
#[cfg(test)]
pub(crate) fn seed_completed(
    root: &Path,
    namespace: &str,
    item: &Item,
    extension: &str,
    bytes: &[u8],
) {
    let id = key(namespace, item);
    fs::create_dir_all(root).unwrap();
    fs::write(root.join(format!("{id}.{extension}")), bytes).unwrap();
    crate::atomic_write_json(
        &root.join(format!("{id}.resume.json")),
        &Partial {
            quality_kbps: 0,
            total: Some(bytes.len() as u64),
            validator: None,
            namespace: namespace.into(),
            item: Some(item.clone()),
        },
    )
    .unwrap();
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
/// Write the manifest. The state lock is held only to serialize it; the
/// caller holds the writer lock, so the newest snapshot is written last.
fn commit(root: &Path, state: &Mutex<State>) -> Result<()> {
    let bytes = {
        let mut s = crate::mutex_lock(state);
        s.dirty = false;
        serde_json::to_vec(&s.manifest)?
    };
    let result = write_index(root, &bytes);
    if result.is_err() {
        crate::mutex_lock(state).dirty = true;
    }
    result
}
fn write_index(root: &Path, bytes: &[u8]) -> Result<()> {
    let path = root.join("index.json");
    #[cfg(test)]
    faults::check(faults::Op::Commit, &path)?;
    crate::atomic_write_bytes(&path, bytes)
}
/// Every cache-owned unlink goes through here (test fault injection point).
fn unlink(path: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    faults::check(faults::Op::Unlink, path)?;
    fs::remove_file(path)
}
fn unlinked(path: &Path) -> bool {
    match unlink(path) {
        Ok(()) => true,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Test-only filesystem fault injection, keyed by exact path so parallel
/// tests in other temporary directories are unaffected.
#[cfg(test)]
pub(crate) mod faults {
    use std::{
        path::{Path, PathBuf},
        sync::{mpsc, Arc, Mutex},
    };
    #[derive(Clone, Copy, PartialEq)]
    pub(crate) enum Op {
        Unlink,
        Commit,
    }
    #[derive(Clone)]
    pub(crate) enum Action {
        Fail,
        /// Signal `entered`, then wait until `release` receives (a slow op).
        Block {
            entered: mpsc::Sender<()>,
            release: Arc<Mutex<mpsc::Receiver<()>>>,
        },
    }
    struct Fault {
        op: Op,
        path: PathBuf,
        action: Action,
        once: bool,
    }
    static FAULTS: Mutex<Vec<Fault>> = Mutex::new(Vec::new());
    pub(crate) fn inject(op: Op, path: &Path, action: Action, once: bool) {
        crate::mutex_lock(&FAULTS).push(Fault {
            op,
            path: path.to_path_buf(),
            action,
            once,
        });
    }
    pub(crate) fn clear(path: &Path) {
        crate::mutex_lock(&FAULTS).retain(|f| f.path != path);
    }
    pub(super) fn check(op: Op, path: &Path) -> std::io::Result<()> {
        let action = {
            let mut faults = crate::mutex_lock(&FAULTS);
            let Some(index) = faults.iter().position(|f| f.op == op && f.path == path) else {
                return Ok(());
            };
            if faults[index].once {
                faults.remove(index).action
            } else {
                faults[index].action.clone()
            }
        };
        match action {
            Action::Fail => Err(std::io::Error::other("injected fault")),
            Action::Block { entered, release } => {
                let _ = entered.send(());
                let _ = crate::mutex_lock(&release).recv();
                Ok(())
            }
        }
    }
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
    let _ = unlink(&part_path(resume));
    let _ = unlink(resume);
}
type Protected = BTreeSet<(String, String)>;
fn protected(state: &State, namespace: &str, rating_key: &str) -> bool {
    let key = (namespace.to_owned(), rating_key.to_owned());
    state.pinned.contains(&key) || state.permanent.contains(&key)
}
fn partial_protected(protected: &Protected, info: &Partial) -> bool {
    info.item
        .as_ref()
        .is_some_and(|i| protected.contains(&(info.namespace.clone(), i.rating_key.clone())))
}
/// True when a completed, usable entry already satisfies this partial.
fn superseded<'a>(
    root: &Path,
    mut entries: impl Iterator<Item = &'a Entry>,
    info: &Partial,
) -> bool {
    info.item.as_ref().is_some_and(|item| {
        entries.any(|e| {
            e.namespace == info.namespace
                && e.quality_kbps == info.quality_kbps
                && e.item.rating_key == item.rating_key
                && part_key(&e.item) == part_key(item)
                && usable(root, e)
        })
    })
}
/// The preserved orphan whose file stem is `id`, if any.
fn orphan_file<'a>(
    orphans: &'a BTreeMap<String, Orphan>,
    id: &str,
) -> Option<(&'a String, &'a Orphan)> {
    let prefix = format!("{id}.");
    orphans
        .range(prefix.clone()..)
        .next()
        .filter(|(name, _)| name.starts_with(&prefix))
}
/// Metadata of a recovered file: its actual size, no station payload.
fn recovered_entry(namespace: &str, item: &Item, quality: u32, file: String, o: &Orphan) -> Entry {
    let mut item = item.clone();
    if let Some(part) = item.media.first_mut().and_then(|m| m.parts.first_mut()) {
        part.size = Some(o.bytes);
    }
    item.stations = Value::Null;
    Entry {
        quality_kbps: quality,
        namespace: namespace.to_owned(),
        item,
        file,
        bytes: o.bytes,
        used: o.modified,
    }
}
/// Re-index a preserved orphan whose file-name hash is this track's id at
/// one of `qualities`. Holds the state lock only briefly; the file check
/// runs outside it. The caller commits (the manifest is marked dirty).
fn adopt(
    root: &Path,
    state: &Mutex<State>,
    namespace: &str,
    item: &Item,
    qualities: &[u32],
) -> bool {
    if part_key(item).is_none() {
        return false;
    }
    let found = {
        let s = crate::mutex_lock(state);
        if s.orphans.is_empty() {
            return false;
        }
        qualities.iter().find_map(|&quality| {
            let id = download_id(namespace, item, quality);
            orphan_file(&s.orphans, &id)
                .map(|(file, orphan)| (id, quality, file.clone(), orphan.clone()))
        })
    };
    let Some((id, quality, file, orphan)) = found else {
        return false;
    };
    if orphan.bytes == 0
        || !fs::symlink_metadata(root.join(&file))
            .is_ok_and(|m| m.is_file() && m.len() == orphan.bytes)
    {
        return false;
    }
    let mut s = crate::mutex_lock(state);
    // Claiming removes it from the orphan set; eviction claims the same way.
    if s.manifest.entries.contains_key(&id) || s.orphans.remove(&file).is_none() {
        return false;
    }
    let entry = recovered_entry(namespace, item, quality, file, &orphan);
    s.manifest.entries.insert(id, entry);
    s.recovered += 1;
    s.dirty = true;
    s.touch_catalogue();
    drop(s);
    crate::diagnostics::event(format_args!(
        "audio cache: re-indexed preserved media for track {}",
        item.rating_key
    ));
    true
}
/// Latest time until which unindexed media is preserved: a corruption found
/// now, or the newest quarantined index (unknown age counts as recent).
fn preserve_deadline(root: &Path, corrupt_now: bool) -> Option<u64> {
    let window = CORRUPT_PRESERVE_WINDOW.as_secs();
    let now = crate::unix_seconds();
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|file| {
            file.file_name()
                .to_string_lossy()
                .starts_with("index.json.corrupt-")
        })
        .map(|file| {
            seconds(file.metadata().and_then(|m| m.modified()))
                .unwrap_or(now)
                .saturating_add(window)
        })
        .chain(corrupt_now.then_some(now.saturating_add(window)))
        .max()
}
/// Startup: finish journaled evictions (a crash after the journal commit).
/// Returns whether the journal changed.
fn finish_journal(root: &Path, manifest: &mut Manifest) -> bool {
    if manifest.evicting.is_empty() {
        return false;
    }
    let referenced: BTreeSet<&str> = manifest.entries.values().map(|e| e.file.as_str()).collect();
    let before = manifest.evicting.len();
    manifest.evicting.retain(|name| {
        safe_file(name) && !referenced.contains(name.as_str()) && !unlinked(&root.join(name))
    });
    manifest.evicting.sort();
    manifest.evicting.dedup();
    manifest.evicting.len() != before
}
/// Runtime: retry journaled unlinks; commit when the journal shrank.
/// Caller holds the writer lock. Returns whether any file was deleted.
fn finish_evictions(root: &Path, state: &Mutex<State>) -> bool {
    let (stale, todo): (Vec<String>, Vec<String>) = {
        let s = crate::mutex_lock(state);
        if s.manifest.evicting.is_empty() {
            return false;
        }
        s.manifest.evicting.iter().cloned().partition(|name| {
            !safe_file(name) || s.manifest.entries.values().any(|e| &e.file == name)
        })
    };
    let done: Vec<String> = todo
        .into_iter()
        .filter(|name| unlinked(&root.join(name)))
        .collect();
    if stale.is_empty() && done.is_empty() {
        return false;
    }
    crate::mutex_lock(state)
        .manifest
        .evicting
        .retain(|name| !stale.contains(name) && !done.contains(name));
    let _ = commit(root, state);
    !done.is_empty()
}
/// Remove index entries transactionally: commit the manifest without them
/// (journaling their files) before unlinking, so a failure or crash at any
/// point leaves no playable entry without its file and no untracked file.
/// A failed commit restores the entries and deletes nothing. Caller holds
/// the writer lock. Returns the bytes actually freed.
fn evict_entries(root: &Path, state: &Mutex<State>, victims: &[(String, u64)]) -> Result<u64> {
    let removed: Vec<(String, Entry)> = {
        let mut s = crate::mutex_lock(state);
        let removed: Vec<_> = victims
            .iter()
            .filter_map(|(key, _)| s.manifest.entries.remove_entry(key))
            .collect();
        for (_, entry) in &removed {
            if safe_file(&entry.file) && !s.manifest.evicting.contains(&entry.file) {
                s.manifest.evicting.push(entry.file.clone());
            }
        }
        if !removed.is_empty() {
            s.touch_catalogue();
        }
        removed
    };
    if removed.is_empty() {
        return Ok(0);
    }
    if let Err(error) = commit(root, state) {
        let mut s = crate::mutex_lock(state);
        for (key, entry) in removed {
            s.manifest.evicting.retain(|name| name != &entry.file);
            s.manifest.entries.entry(key).or_insert(entry);
        }
        s.touch_catalogue();
        return Err(error);
    }
    let mut freed = 0;
    let mut done = Vec::new();
    for (key, entry) in &removed {
        if !safe_file(&entry.file) {
            continue;
        }
        let path = root.join(&entry.file);
        let bytes = fs::symlink_metadata(&path)
            .ok()
            .filter(|m| m.is_file())
            .map_or(0, |m| m.len());
        match unlink(&path) {
            Ok(()) => {
                freed += bytes;
                done.push(entry.file.clone());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                done.push(entry.file.clone());
            }
            Err(error) => crate::diagnostics::event(format_args!(
                "audio cache: eviction of {key} journaled; unlink failed: {error}"
            )),
        }
    }
    crate::mutex_lock(state)
        .manifest
        .evicting
        .retain(|name| !done.contains(name));
    // Best effort: a stale journal only lists already-deleted files, which
    // finishing treats as done.
    let _ = commit(root, state);
    Ok(freed)
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
        let mut corrupt = false;
        let mut manifest: Manifest = match fs::read(root.join("index.json")) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(manifest) => manifest,
                Err(_) => {
                    if !read_only {
                        crate::quarantine_corrupt(&root.join("index.json"), &bytes)?;
                    }
                    corrupt = true;
                    crate::diagnostics::event(format_args!(
                        "corrupt audio cache index preserved; using empty catalogue"
                    ));
                    Manifest::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Manifest::default(),
            Err(error) => return Err(error.into()),
        };
        let mut changed = !read_only && finish_journal(&root, &mut manifest);
        manifest.entries.retain(|_, e| usable(&root, e));
        // Retain unindexed media evidence across reopen while a recent
        // quarantine exists. Bounded in time: a permanent flag would disable
        // orphan reclamation forever and leave the quota stuck at "full".
        let preserve_until =
            preserve_deadline(&root, corrupt).filter(|until| *until > crate::unix_seconds());
        // A crash between temp write and rename can leave orphan `*.tmp` files
        // (old `index.<uuid>.tmp` naming included) plus unindexed hash files.
        // Be resilient to transient entries disappearing mid-scan instead of
        // aborting cache open.
        let indexed: BTreeSet<String> = manifest
            .entries
            .values()
            .map(|e| e.file.clone())
            .chain(manifest.evicting.iter().cloned())
            .collect();
        let mut orphans = BTreeMap::new();
        for file in fs::read_dir(&root).into_iter().flatten().flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            if !read_only && name.ends_with(".tmp") {
                let _ = unlink(&file.path());
                continue;
            }
            let Ok(file_type) = file.file_type() else {
                continue;
            };
            let stem = name.split('.').next().unwrap_or("");
            if stem.len() == 64
                && stem.bytes().all(|b| b.is_ascii_hexdigit())
                && !name.ends_with(".part")
                && !name.ends_with(".resume.json")
                && !indexed.contains(&name)
                && file_type.is_file()
            {
                if let Ok(meta) = file.metadata() {
                    let modified = seconds(meta.modified()).unwrap_or(0);
                    orphans.insert(
                        name,
                        Orphan {
                            bytes: meta.len(),
                            modified,
                        },
                    );
                }
            }
        }
        // A crash between the completion rename and the index commit leaves
        // the completed file plus its resume record: re-index it from the
        // record when the record's own id names that file.
        let mut recovered = 0;
        for (resume, info) in resume_records(&root) {
            let name = resume.file_name().unwrap_or_default().to_string_lossy();
            let id = name.strip_suffix(".resume.json").unwrap_or("").to_owned();
            let owner = info.item.as_ref().filter(|item| {
                !info.namespace.is_empty()
                    && download_id(&info.namespace, item, info.quality_kbps) == id
            });
            let file = orphan_file(&orphans, &id)
                .filter(|(_, o)| o.bytes > 0)
                .map(|(file, _)| file.clone());
            if let (Some(item), Some(file), std::collections::btree_map::Entry::Vacant(slot)) =
                (owner, file, manifest.entries.entry(id))
            {
                if let Some(orphan) = orphans.remove(&file) {
                    slot.insert(recovered_entry(
                        &info.namespace,
                        item,
                        info.quality_kbps,
                        file,
                        &orphan,
                    ));
                    recovered += 1;
                    changed = true;
                    if !read_only {
                        discard_partial(&resume);
                    }
                    continue;
                }
            }
            if read_only {
                continue;
            }
            if superseded(&root, manifest.entries.values(), &info) {
                // Completed elsewhere; the partial only consumes quota.
                discard_partial(&resume);
            } else if info.quality_kbps > 0 {
                // Transcodes always restart from zero; keep the record so
                // the track stays pending, but release the dead bytes.
                let _ = unlink(&part_path(&resume));
            }
        }
        if recovered > 0 {
            crate::diagnostics::event(format_args!(
                "audio cache: re-indexed {recovered} completed downloads from resume records"
            ));
        }
        // Unindexed files are not playable without metadata; outside the
        // preservation window reclaim our hash names.
        let mut reclaimed = 0;
        if !read_only && preserve_until.is_none() {
            for name in std::mem::take(&mut orphans).into_keys() {
                if unlinked(&root.join(name)) {
                    reclaimed += 1;
                }
            }
        }
        if reclaimed > 0 {
            crate::diagnostics::event(format_args!(
                "audio cache: reclaimed {reclaimed} unindexed media files"
            ));
        }
        if !orphans.is_empty() {
            crate::diagnostics::event(format_args!(
                "audio cache: preserving {} unindexed media files after index corruption",
                orphans.len()
            ));
        }
        if changed && !read_only {
            let _ = write_index(&root, &serde_json::to_vec(&manifest)?);
        }
        let state = Arc::new(Mutex::new(State {
            manifest,
            orphans,
            recovered,
            reclaimed,
            preserve_until,
            ..State::new(config)
        }));
        let writer = Arc::new(Mutex::new(()));
        let epoch = Arc::new(AtomicU64::new(0));
        let (cancel, changed) = watch::channel(0);
        let jobs = Arc::new(Jobs::default());
        let client_id = client_id.to_owned();
        // A task on the shared runtime; filesystem work inside it is handed
        // to the blocking pool so it cannot stall audio/D-Bus I/O.
        let worker = (!read_only).then(|| {
            crate::rt::spawn(worker(
                Shared {
                    root: root.clone(),
                    state: state.clone(),
                    writer: writer.clone(),
                    epoch: epoch.clone(),
                },
                jobs.clone(),
                changed,
                client_id,
            ))
        });
        Ok(Self {
            root,
            state,
            writer,
            epoch,
            cancel,
            jobs,
            worker,
            _lock: lock,
            read_only,
        })
    }

    pub fn config(&self) -> Config {
        crate::mutex_lock(&self.state).config.clone()
    }
    pub(crate) fn catalogue_revision(&self) -> u64 {
        crate::mutex_lock(&self.state).catalogue_revision
    }
    pub fn gate(&self) -> Arc<Gate> {
        crate::mutex_lock(&self.state).gate.clone()
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
        crate::mutex_lock(&self.state).permanent = items
            .iter()
            .map(|i| (namespace.to_owned(), i.rating_key.clone()))
            .collect();
    }
    pub fn configure(&self, config: Config, keep: Option<(&str, &str)>) -> Result<()> {
        config.validate()?;
        self.cancel();
        let _writer = crate::mutex_lock(&self.writer);
        let previous = {
            let mut state = crate::mutex_lock(&self.state);
            if let Some((namespace, key)) = keep {
                state.pinned.insert((namespace.to_owned(), key.to_owned()));
            }
            state.cooldown.clear();
            std::mem::replace(&mut state.config, config)
        };
        let result = room(&self.root, &self.state, 0, "");
        if result.is_err() {
            crate::mutex_lock(&self.state).config = previous;
        }
        result
    }
    /// Let the next batch request refused downloads again (an explicit user
    /// download request): failures of batches scheduled up to now are no
    /// longer cooled down, including those still in flight.
    pub fn forget_failures(&self) {
        let mut state = crate::mutex_lock(&self.state);
        state.cooldown.clear();
        state.forgotten = self.epoch.load(Ordering::SeqCst);
    }
    /// Drain the keys whose downloads looked like a missing server item.
    pub(crate) fn take_suspects(&self) -> Vec<String> {
        std::mem::take(&mut crate::mutex_lock(&self.state).suspects)
            .into_iter()
            .collect()
    }
    pub fn cancel(&self) {
        let epoch = self.epoch.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.cancel.send(epoch);
        let mut state = crate::mutex_lock(&self.state);
        state.pending = 0;
        state.downloading = false;
        state.active_key.clear();
        state.pinned.clear();
    }
    pub fn clear(&self) -> Result<()> {
        self.cancel();
        let _writer = crate::mutex_lock(&self.writer);
        // Commit the empty catalogue (journaling its files) before deleting.
        let removed = {
            let mut s = crate::mutex_lock(&self.state);
            s.cooldown.clear();
            let removed = std::mem::take(&mut s.manifest.entries);
            for entry in removed.values() {
                if safe_file(&entry.file) && !s.manifest.evicting.contains(&entry.file) {
                    s.manifest.evicting.push(entry.file.clone());
                }
            }
            s.touch_catalogue();
            removed
        };
        if let Err(error) = commit(&self.root, &self.state) {
            let mut s = crate::mutex_lock(&self.state);
            s.manifest
                .evicting
                .retain(|name| !removed.values().any(|e| &e.file == name));
            s.manifest.entries = removed;
            return Err(error);
        }
        for file in fs::read_dir(&self.root)?.flatten() {
            let Ok(file_type) = file.file_type() else {
                continue;
            };
            if file.file_name() != "cache.lock"
                && file.file_name() != "index.json"
                && file_type.is_file()
            {
                let _ = unlink(&file.path());
            }
        }
        let journal = crate::mutex_lock(&self.state).manifest.evicting.clone();
        let left: Vec<String> = journal
            .into_iter()
            .filter(|name| fs::symlink_metadata(self.root.join(name)).is_ok())
            .collect();
        {
            let mut s = crate::mutex_lock(&self.state);
            s.manifest.evicting = left;
            s.orphans.clear();
            s.preserve_until = None;
            s.permanent.clear();
            s.error.clear();
        }
        commit(&self.root, &self.state)
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
            let mut state = crate::mutex_lock(&self.state);
            state.pinned = items
                .iter()
                .map(|i| (ns.clone(), i.rating_key.clone()))
                .collect();
            state.pending = items.len();
            state.error.clear();
            state.errors.clear();
        }
        let batch = Batch {
            quality_kbps,
            epoch,
            base,
            token: token.into(),
            namespace: ns,
            items,
        };
        if self.worker.is_none() || !self.jobs.submit(batch) {
            return Err(Error::Input("Cache worker unavailable"));
        }
        Ok(())
    }
    /// The usable indexed entry for this track, most recently used first.
    /// File checks run outside the state lock.
    fn usable_key(&self, namespace: &str, item: &Item) -> Option<String> {
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
        candidates
            .into_iter()
            .filter(|(_, e)| usable(&self.root, e))
            .max_by_key(|(_, e)| e.used)
            .map(|(k, _)| k)
    }
    pub fn lookup(&self, namespace: &str, item: &Item, touch: bool) -> Option<(PathBuf, Item)> {
        let found = self.usable_key(namespace, item).or_else(|| {
            let qualities: Vec<u32> = std::iter::once(0)
                .chain(TRANSCODE_KBPS.iter().copied())
                .collect();
            adopt(&self.root, &self.state, namespace, item, &qualities)
                .then(|| self.usable_key(namespace, item))
                .flatten()
        })?;
        let mut state = crate::mutex_lock(&self.state);
        let entry = state.manifest.entries.get_mut(&found)?;
        if touch && !self.read_only {
            entry.used = crate::unix_seconds();
        }
        let result = (self.root.join(&entry.file), entry.item.clone());
        // Persist LRU with manifest mutations, orderly shutdown and at most
        // once per interval here, so playback hits don't rewrite the index
        // each time yet a killed process keeps recent usage. Recovered
        // entries are persisted promptly. Never waits for the writer.
        let persist = !self.read_only
            && (state.dirty || touch && state.lru_saved.elapsed() >= LRU_PERSIST_INTERVAL);
        drop(state);
        if persist {
            self.try_commit();
        }
        Some(result)
    }
    /// Commit unless another writer is busy (it commits newer state anyway
    /// or the next interval/shutdown does).
    fn try_commit(&self) {
        let _writer = match self.writer.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::Poisoned(poison)) => poison.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        crate::mutex_lock(&self.state).lru_saved = Instant::now();
        let _ = commit(&self.root, &self.state);
    }
    pub fn items(&self, namespace: &str) -> Vec<Item> {
        let mut entries: Vec<Entry> = crate::mutex_lock(&self.state)
            .manifest
            .entries
            .values()
            .filter(|e| e.namespace == namespace)
            .cloned()
            .collect();
        entries.retain(|e| usable(&self.root, e));
        entries.sort_by_key(|e| std::cmp::Reverse(e.used));
        let mut seen = BTreeSet::new();
        entries
            .into_iter()
            .filter(|e| seen.insert(e.item.rating_key.clone()))
            .map(|e| e.item)
            .collect()
    }
    pub fn remove(&self, namespace: &str, rating_key: &str) -> Result<()> {
        let _writer = crate::mutex_lock(&self.writer);
        let victims: Vec<_> = crate::mutex_lock(&self.state)
            .manifest
            .entries
            .iter()
            .filter(|(_, e)| e.namespace == namespace && e.item.rating_key == rating_key)
            .map(|(k, e)| (k.clone(), e.bytes))
            .collect();
        evict_entries(&self.root, &self.state, &victims)?;
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
        Ok(())
    }
    pub fn pending_items(&self, namespace: &str) -> Vec<Item> {
        let records = resume_records(&self.root);
        let entries: Vec<Entry> = crate::mutex_lock(&self.state)
            .manifest
            .entries
            .values()
            .filter(|e| e.namespace == namespace)
            .cloned()
            .collect();
        let mut seen = BTreeSet::new();
        records
            .into_iter()
            .map(|(_, info)| info)
            .filter(|p| p.namespace == namespace && !superseded(&self.root, entries.iter(), p))
            .filter_map(|p| p.item)
            .filter(|i| seen.insert(i.rating_key.clone()))
            .collect()
    }
    pub fn status(&self, namespace: &str) -> Value {
        // Snapshot under the lock; file checks and the directory scan run
        // after releasing it so a slow filesystem cannot stall other users.
        let (snapshot, entries, orphans) = {
            let state = crate::mutex_lock(&self.state);
            let entries: Vec<(String, String, u64)> = state
                .manifest
                .entries
                .values()
                .filter(|e| e.namespace == namespace)
                .map(|e| (e.item.rating_key.clone(), e.file.clone(), e.bytes))
                .collect();
            let orphans = json!({
                "count": state.orphans.len(),
                "bytes": state.orphans.values().map(|o| o.bytes).sum::<u64>(),
                "recovered": state.recovered,
                "reclaimed": state.reclaimed,
                "preserveUntil": state.preserve_until,
                "evictionPending": state.manifest.evicting.len(),
            });
            let snapshot = json!({"enabled":state.config.enabled,"limitMb":state.config.limit_mb,"ahead":state.config.ahead,
                "wifiOnly":state.gate.wifi_only.load(Ordering::SeqCst),"paused":state.gate.paused.load(Ordering::SeqCst),"waitingForWifi":state.gate.wifi_only.load(Ordering::SeqCst) && !state.gate.wifi.load(Ordering::SeqCst),
                "downloading":state.downloading,"held":state.gate.held(),"pending":state.pending,"received":state.received,"total":state.total,"error":state.error,"errors":state.errors,"activeKey":state.active_key});
            (snapshot, entries, orphans)
        };
        let ready_bytes: BTreeMap<_, _> = entries
            .into_iter()
            .filter(|(_, file, bytes)| {
                safe_file(file)
                    && *bytes > 0
                    && fs::symlink_metadata(self.root.join(file)).is_ok_and(|m| {
                        m.is_file() && !m.file_type().is_symlink() && m.len() == *bytes
                    })
            })
            .map(|(key, _, bytes)| (key, bytes))
            .collect();
        let ready: Vec<_> = ready_bytes.keys().collect();
        let mut value = snapshot;
        value["bytes"] = json!(disk_bytes(&self.root));
        value["tracks"] = json!(ready.len());
        value["readyKeys"] = json!(ready);
        value["readyBytes"] = json!(ready_bytes);
        value["orphans"] = orphans;
        value
    }
}
impl Drop for Cache {
    fn drop(&mut self) {
        self.cancel();
        self.jobs.close();
        if let Some(worker) = self.worker.take() {
            // Cancellation is cooperative (network awaits observe `cancel`);
            // in-flight filesystem steps complete before the task ends.
            worker.join(None);
        }
        if !self.read_only {
            let _writer = crate::mutex_lock(&self.writer);
            let _ = commit(&self.root, &self.state);
        }
    }
}

/// What the download worker shares with the Cache handle.
#[derive(Clone)]
struct Shared {
    root: PathBuf,
    state: Arc<Mutex<State>>,
    writer: Arc<Mutex<()>>,
    epoch: Arc<AtomicU64>,
}

async fn worker(
    shared: Shared,
    jobs: Arc<Jobs>,
    mut changed: watch::Receiver<u64>,
    client_id: String,
) {
    let Shared { state, epoch, .. } = &shared;
    let gate = crate::mutex_lock(state).gate.clone();
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
    while let Some(batch) = jobs.next().await {
        if epoch.load(Ordering::SeqCst) != batch.epoch || !gate.allowed() {
            continue;
        }
        changed.borrow_and_update();
        let mut terminal = BTreeSet::new();
        // Items whose server ignored a resume range: never interrupted
        // again in this batch, across its retry passes.
        let mut unresumable: BTreeSet<String> = BTreeSet::new();
        for attempt in 0..4 {
            let mut failed = false;
            for (position, item) in batch.items.iter().enumerate() {
                if epoch.load(Ordering::SeqCst) != batch.epoch || !gate.allowed() {
                    break;
                }
                if terminal.contains(&item.rating_key) {
                    continue;
                }
                let id = download_id(&batch.namespace, item, batch.quality_kbps);
                {
                    let mut s = crate::mutex_lock(state);
                    s.cooldown
                        .retain(|_, (since, _)| since.elapsed() < COOLDOWN);
                    if let Some((_, error)) = s.cooldown.get(&id) {
                        let error = error.clone();
                        s.errors.insert(item.rating_key.clone(), error);
                        drop(s);
                        terminal.insert(item.rating_key.clone());
                        continue;
                    }
                }
                {
                    let mut s = crate::mutex_lock(state);
                    s.pending = batch.items.len() - position;
                    s.active_key = item.rating_key.clone();
                }
                let session = uuid::Uuid::new_v4().to_string();
                let mut transcoded = 0;
                // Cancellation interrupts network awaits only. Filesystem steps
                // run on the blocking pool and are never abandoned mid-way, so
                // a superseding batch cannot race a detached partial write.
                // A hold (live stream starving) waits before the item and
                // re-runs an interrupted original by range, spending no
                // attempt; `resumable` turns false if the server then
                // ignores the range, so that item is not interrupted again.
                let mut resumable = !unresumable.contains(&item.rating_key);
                let result = loop {
                    if unheld(&gate, &mut changed).await.is_err()
                        || epoch.load(Ordering::SeqCst) != batch.epoch
                    {
                        break None;
                    }
                    match download(
                        &client,
                        &shared,
                        &mut changed,
                        &batch,
                        item.clone(),
                        (&client_id, &session),
                        ItemRun {
                            transcoded: &mut transcoded,
                            gate: &gate,
                            resumable: &mut resumable,
                        },
                    )
                    .await
                    {
                        Err(Failure::Cancelled) => break None,
                        Err(Failure::Held) => continue,
                        result => break Some(result),
                    }
                };
                if !resumable {
                    unresumable.insert(item.rating_key.clone());
                }
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
                        let lasting = error.lasting();
                        let suspect = match &error {
                            Failure::Other(Error::Http(404)) => true,
                            Failure::Other(Error::Http(400)) => transcoded > 0,
                            _ => false,
                        };
                        let error = error.into_error();
                        if suspect {
                            crate::mutex_lock(state)
                                .suspects
                                .insert(item.rating_key.clone());
                        }
                        failed |= retryable;
                        if !retryable {
                            terminal.insert(item.rating_key.clone());
                        }
                        let mut s = crate::mutex_lock(state);
                        s.error = error.to_string();
                        s.errors.insert(item.rating_key.clone(), error.to_string());
                        if lasting && batch.epoch > s.forgotten {
                            s.cooldown
                                .insert(id.clone(), (Instant::now(), error.to_string()));
                        }
                    }
                } else {
                    crate::mutex_lock(state).errors.remove(&item.rating_key);
                }
            }
            if epoch.load(Ordering::SeqCst) != batch.epoch {
                break;
            }
            {
                let mut s = crate::mutex_lock(state);
                s.downloading = false;
                s.pending = 0;
                s.active_key.clear();
            }
            if !failed || attempt == 3 || !gate.allowed() {
                break;
            }
            // Retry stalled/interrupted downloads while the same queue is active.
            tokio::select! { _=changed.changed()=>break, _=tokio::time::sleep(RETRY_PASS)=>{} }
        }
    }
}

/// Wait before a batch retries its interrupted items (short in unit tests).
const RETRY_PASS: Duration = if cfg!(test) {
    Duration::from_millis(300)
} else {
    Duration::from_secs(30)
};

const CACHE_FULL: Error = Error::Input("Cache is full; increase its limit or clear cached audio");

fn evictable_entry(state: &State, key: &str, entry: &Entry, current_key: &str) -> bool {
    key != current_key && !protected(state, &entry.namespace, &entry.item.rating_key)
}
fn evictable_partial(
    protected: &Protected,
    part: &Path,
    info: &Partial,
    current_key: &str,
) -> bool {
    part.file_name()
        .is_some_and(|n| n.to_string_lossy() != format!("{current_key}.part"))
        && !partial_protected(protected, info)
}

/// Reclaimable data, cheapest loss first: preserved orphans (unplayable),
/// then unprotected entries by LRU, then unprotected partial downloads.
struct Plan {
    orphans: Vec<(String, u64)>,
    entries: Vec<(String, u64)>,
    partials: Vec<(PathBuf, u64)>,
}
impl Plan {
    /// Snapshot candidates under the state lock, then size them without it.
    fn collect(root: &Path, state: &Mutex<State>, current_key: &str) -> Self {
        let (mut orphans, mut entries, protected) = {
            let s = crate::mutex_lock(state);
            let orphans: Vec<_> = s
                .orphans
                .iter()
                .map(|(name, o)| (o.modified, name.clone(), o.bytes))
                .collect();
            let entries: Vec<_> = s
                .manifest
                .entries
                .iter()
                .filter(|(k, e)| evictable_entry(&s, k, e, current_key) && safe_file(&e.file))
                .map(|(k, e)| (e.used, k.clone(), e.file.clone()))
                .collect();
            let protected: Protected = s.pinned.union(&s.permanent).cloned().collect();
            (orphans, entries, protected)
        };
        orphans.sort();
        entries.sort();
        Self {
            orphans: orphans
                .into_iter()
                .map(|(_, name, bytes)| (name, bytes))
                .collect(),
            entries: entries
                .into_iter()
                .filter_map(|(_, key, file)| {
                    let meta = fs::symlink_metadata(root.join(file)).ok()?;
                    meta.is_file().then_some((key, meta.len()))
                })
                .collect(),
            partials: partials(root)
                .into_iter()
                .filter(|(path, _, info)| evictable_partial(&protected, path, info, current_key))
                .map(|(path, bytes, _)| (path, bytes))
                .collect(),
        }
    }
    fn total(&self) -> u64 {
        let orphans: u64 = self.orphans.iter().map(|o| o.1).sum();
        let entries: u64 = self.entries.iter().map(|e| e.1).sum();
        let partials: u64 = self.partials.iter().map(|p| p.1).sum();
        orphans.saturating_add(entries).saturating_add(partials)
    }
    /// Reclaim at least `need` bytes when possible. Index entries go first
    /// through the journaled transaction; a failed commit deletes nothing.
    fn execute(mut self, root: &Path, state: &Mutex<State>, need: u64) -> Result<u64> {
        let mut chosen = 0u64;
        let mut take = |bytes: u64| {
            let more = chosen < need;
            if more {
                chosen = chosen.saturating_add(bytes);
            }
            more
        };
        self.orphans.retain(|o| take(o.1));
        self.entries.retain(|e| take(e.1));
        self.partials.retain(|p| take(p.1));
        let mut freed = evict_entries(root, state, &self.entries)?;
        for (name, bytes) in self.orphans {
            // Claim first: a concurrent lookup may have adopted it.
            let Some(orphan) = crate::mutex_lock(state).orphans.remove(&name) else {
                continue;
            };
            if unlinked(&root.join(&name)) {
                freed += bytes;
                crate::mutex_lock(state).reclaimed += 1;
                crate::diagnostics::event(format_args!(
                    "audio cache: reclaimed preserved unindexed media under quota pressure"
                ));
            } else {
                crate::mutex_lock(state).orphans.insert(name, orphan);
            }
        }
        for (part, bytes) in self.partials {
            // The bytes first: a record without bytes merely stays pending.
            if unlink(&part).is_ok() {
                freed += bytes;
                let _ = unlink(&resume_path(&part));
            }
        }
        Ok(freed)
    }
}

/// Make room for `extra` bytes. The caller holds the writer lock; the state
/// lock is only taken for snapshots and in-memory updates.
fn room(root: &Path, state: &Mutex<State>, extra: u64, current_key: &str) -> Result<()> {
    let limit = crate::mutex_lock(state).config.limit_mb * 1024 * 1024;
    if extra > limit {
        return Err(Error::Input("Audio file exceeds the cache limit"));
    }
    let mut used = disk_bytes(root);
    let mut checked = false;
    loop {
        if used.saturating_add(extra) <= limit {
            return Ok(());
        }
        // Earlier evictions whose unlink failed come back before anything
        // else is evicted.
        if finish_evictions(root, state) {
            used = disk_bytes(root);
            continue;
        }
        let plan = Plan::collect(root, state, current_key);
        // Reject before deleting anything when protected/active data alone
        // cannot fit; otherwise a doomed request would evict every
        // unprotected track.
        if !checked {
            checked = true;
            if used.saturating_sub(plan.total()).saturating_add(extra) > limit {
                return Err(CACHE_FULL);
            }
        }
        let need = used.saturating_add(extra).saturating_sub(limit);
        if plan.execute(root, state, need)? == 0 {
            return Err(CACHE_FULL);
        }
        used = disk_bytes(root);
    }
}

/// Await a network step unless a newer schedule/cancel supersedes the batch.
async fn cancellable<T>(
    changed: &mut watch::Receiver<u64>,
    step: impl std::future::Future<Output = T>,
) -> std::result::Result<T, Failure> {
    tokio::select! {
        biased;
        _ = changed.changed() => Err(Failure::Cancelled),
        value = step => Ok(value),
    }
}

/// Delete a partial download and its resume record off the I/O thread.
async fn remove_partial(partial: &Path, meta: &Path) {
    let (partial, meta) = (partial.to_path_buf(), meta.to_path_buf());
    crate::rt::unblock(move || {
        let _ = unlink(&partial);
        let _ = unlink(&meta);
    })
    .await;
}

/// Whether a usable completed copy of `item` at `quality` is indexed. The
/// file checks run on the blocking pool, never on the runtime thread.
async fn completed(shared: &Shared, namespace: &str, item: &Item, quality: u32) -> bool {
    let entries: Vec<Entry> = crate::mutex_lock(&shared.state)
        .manifest
        .entries
        .values()
        .filter(|e| {
            e.namespace == namespace
                && e.quality_kbps == quality
                && e.item.rating_key == item.rating_key
                && part_key(item).is_none_or(|p| part_key(&e.item) == Some(p))
        })
        .cloned()
        .collect();
    if entries.is_empty() {
        return false;
    }
    let root = shared.root.clone();
    crate::rt::unblock(move || entries.iter().any(|e| usable(&root, e))).await
}

/// One item's download state across its attempts.
struct ItemRun<'a> {
    /// Transcode bitrate used (for the session stop request), 0 if none.
    transcoded: &'a mut u32,
    gate: &'a Gate,
    /// Cleared when the server ignored a resume range after a hold.
    resumable: &'a mut bool,
}

async fn download(
    client: &reqwest::Client,
    shared: &Shared,
    changed: &mut watch::Receiver<u64>,
    batch: &Batch,
    mut item: Item,
    identity: (&str, &str),
    run: ItemRun<'_>,
) -> std::result::Result<(), Failure> {
    let ItemRun {
        transcoded,
        gate,
        resumable,
    } = run;
    let (root, state, epoch) = (&shared.root, &shared.state, &shared.epoch);
    let (client_id, session) = identity;
    let quality_of = |item: &Item| download_quality(batch.quality_kbps, item);
    if completed(shared, &batch.namespace, &item, quality_of(&item)).await {
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
    let quality = quality_of(&item);
    if completed(shared, &batch.namespace, &item, quality).await {
        return Ok(());
    }
    *transcoded = quality;
    let url = if quality > 0 {
        crate::quality::transcode_url(&batch.base, &item.rating_key, quality, session)?
    } else {
        server_path(&batch.base, part)?
    };
    let id = download_id(&batch.namespace, &item, quality);
    let partial_path = root.join(format!("{id}.part"));
    let meta_path = root.join(format!("{id}.resume.json"));
    // Partials of this track at another quality/part can never complete this
    // request and would otherwise count toward the quota indefinitely. The
    // directory scan and record reads stay off the shared I/O thread: on a
    // large cache on phone flash they could otherwise stall audio streaming.
    let resumed: Option<(Partial, u64)> = {
        let shared = shared.clone();
        let (namespace, wanted) = (batch.namespace.clone(), item.clone());
        let (partial_path, meta_path) = (partial_path.clone(), meta_path.clone());
        crate::rt::unblock(move || {
            // Preserved media of exactly this download is re-indexed instead
            // of downloaded again.
            if adopt(&shared.root, &shared.state, &namespace, &wanted, &[quality]) {
                let _writer = crate::mutex_lock(&shared.writer);
                let _ = commit(&shared.root, &shared.state);
                return None;
            }
            for (resume, info) in resume_records(&shared.root) {
                if resume != meta_path
                    && info.namespace == namespace
                    && info
                        .item
                        .as_ref()
                        .is_some_and(|i| i.rating_key == wanted.rating_key)
                {
                    discard_partial(&resume);
                }
            }
            let old: Partial = fs::read(&meta_path)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            Some((old, fs::metadata(&partial_path).map_or(0, |m| m.len())))
        })
        .await
    };
    let Some((old, mut offset)) = resumed else {
        return Ok(());
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
        return Err(Error::Input(UNENCODED_REQUIRED).into());
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
        return Err(Error::Input(NON_AUDIO).into());
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
        if offset > 0 {
            // The range was ignored: interrupting again would restart it.
            *resumable = false;
        }
        offset = 0;
        response.content_length()
    };
    // Only an original that can resume by range drops its connection for
    // a hold (a paused read would not stop the server sending until the
    // socket buffers fill); anything else finishes this item first.
    let interruptible = *resumable && quality == 0 && validator.is_some() && total.is_some();
    let mut held = gate.held.subscribe();
    let extension = if quality > 0 {
        "mp3".into()
    } else {
        extension(part, mime.as_str())
    };
    let mut file = {
        let (shared, id) = (shared.clone(), id.clone());
        let (batch_epoch, quality_kbps, namespace) =
            (batch.epoch, quality, batch.namespace.clone());
        let (partial_path, meta_path) = (partial_path.clone(), meta_path.clone());
        let mut saved = item.clone();
        saved.stations = Value::Null;
        crate::rt::unblock(move || -> std::result::Result<fs::File, Failure> {
            let _writer = crate::mutex_lock(&shared.writer);
            if shared.epoch.load(Ordering::SeqCst) != batch_epoch {
                return Err(Error::Input("Cache request cancelled").into());
            }
            if offset == 0 {
                let _ = unlink(&partial_path);
            }
            if let Some(total) = total {
                room(
                    &shared.root,
                    &shared.state,
                    total.saturating_sub(offset),
                    &id,
                )?;
            }
            crate::atomic_write_json(
                &meta_path,
                &Partial {
                    quality_kbps,
                    total,
                    validator,
                    namespace,
                    item: Some(saved),
                },
            )?;
            {
                // Re-checked under the state lock: cancel() bumps the epoch
                // before resetting these flags under the same lock.
                let mut s = crate::mutex_lock(&shared.state);
                if shared.epoch.load(Ordering::SeqCst) != batch_epoch {
                    return Err(Error::Input("Cache request cancelled").into());
                }
                s.downloading = true;
                s.received = offset;
                s.total = total;
            }
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
    loop {
        let chunk = if interruptible {
            tokio::select! {
                biased;
                _ = changed.changed() => return Err(Failure::Cancelled),
                _ = async { let _ = held.wait_for(|held| *held).await; } => {
                    crate::diagnostics::event(format_args!(
                        "cache download held key={} received={received}",
                        item.rating_key
                    ));
                    return Err(Failure::Held);
                }
                chunk = response.chunk() => chunk,
            }
        } else {
            cancellable(changed, response.chunk()).await?
        };
        let Some(bytes) = chunk.map_err(|_| Failure::Interrupted)? else {
            break;
        };
        if epoch.load(Ordering::SeqCst) != batch.epoch {
            return Err(Error::Input("Cache request cancelled").into());
        }
        let length = bytes.len() as u64;
        if total.is_some_and(|n| received.saturating_add(length) > n) {
            return Err(Error::Input("Audio response exceeds its declared size").into());
        }
        let reserve = total.is_none() && received.saturating_add(length) > reserved_until;
        let (job, job_id) = (shared.clone(), id.clone());
        // Each chunk is written before the next is read, as before; only the
        // thread changes. A write is never abandoned half-way.
        let (returned, written) = crate::rt::unblock(move || {
            let written = (|| -> std::result::Result<Option<u64>, Failure> {
                let mut admitted = None;
                if reserve {
                    let _writer = crate::mutex_lock(&job.writer);
                    // Amortize filesystem accounting across 4 MiB windows. If a
                    // protected cache cannot reserve headroom, admit just this chunk.
                    let window = (4 * 1024 * 1024).max(length);
                    admitted = Some(if room(&job.root, &job.state, window, &job_id).is_ok() {
                        window
                    } else {
                        room(&job.root, &job.state, length, &job_id)?;
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
        crate::mutex_lock(state).received = received;
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
    let shared = shared.clone();
    let (batch_epoch, quality_kbps, namespace) = (batch.epoch, quality, batch.namespace.clone());
    crate::rt::unblock(move || -> std::result::Result<(), Failure> {
        file.sync_all()?;
        drop(file);
        let _writer = crate::mutex_lock(&shared.writer);
        if shared.epoch.load(Ordering::SeqCst) != batch_epoch {
            return Err(Error::Input("Cache request cancelled").into());
        }
        let filename = format!("{id}.{extension}");
        fs::rename(&partial_path, shared.root.join(&filename))?;
        let _ = unlink(&meta_path);
        {
            let mut s = crate::mutex_lock(&shared.state);
            // The new file reuses this name: it is no longer journaled for
            // deletion nor an orphan.
            s.manifest.evicting.retain(|name| name != &filename);
            s.orphans.remove(&filename);
            s.manifest.entries.insert(
                id,
                Entry {
                    quality_kbps,
                    namespace,
                    item,
                    file: filename,
                    bytes: received,
                    used: crate::unix_seconds(),
                },
            );
            s.touch_catalogue();
            s.error.clear();
            s.downloading = false;
        }
        commit(&shared.root, &shared.state)?;
        fs::File::open(&shared.root)?.sync_all()?;
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
        let mut state = crate::mutex_lock(&cache.state);
        for (id, used) in [("1", 1), ("2", 2)] {
            let filename = format!("{}.wav", crate::sha256_hex(id));
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(dir.path().join(&filename))
                .unwrap();
            file.set_len(40 * 1024 * 1024).unwrap();
            state.manifest.entries.insert(
                crate::sha256_hex(id),
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
        // room() takes the state lock itself, only around in-memory steps.
        drop(state);
        room(dir.path(), &cache.state, 1024 * 1024, "new").unwrap();
        let state = crate::mutex_lock(&cache.state);
        assert!(!state.manifest.entries.contains_key(&crate::sha256_hex("1")));
        assert!(state.manifest.entries.contains_key(&crate::sha256_hex("2")));
        assert!(disk_bytes(dir.path()) + 1024 * 1024 <= 64 * 1024 * 1024);
        drop(state);
        cache.cancel();
        assert!(room(dir.path(), &cache.state, 40 * 1024 * 1024, "new").is_err());
        let state = crate::mutex_lock(&cache.state);
        assert!(state.manifest.entries.contains_key(&crate::sha256_hex("2")));
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;

    fn entry(dir: &Path, state: &mut State, id: &str, used: u64) {
        let filename = format!("{}.wav", crate::sha256_hex(id));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(dir.join(&filename))
            .unwrap();
        file.set_len(40 * 1024 * 1024).unwrap();
        state.manifest.entries.insert(
            crate::sha256_hex(id),
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
                .contains_key(&crate::sha256_hex("1")),
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
        crate::atomic_write_json(
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
        seed_partial(dir.path(), &crate::sha256_hex("p0"), 0, &song, 4);
        seed_partial(dir.path(), &crate::sha256_hex("p320"), 320, &song, 4);
        // Track 2 is already cached at quality 0: its partial is stale.
        let done = track("2");
        seed_partial(dir.path(), &crate::sha256_hex("p2"), 0, &done, 4);
        let file = format!("{}.wav", crate::sha256_hex("e2"));
        fs::write(dir.path().join(&file), b"cached").unwrap();
        let mut manifest = Manifest::default();
        manifest.entries.insert(
            crate::sha256_hex("e2"),
            Entry {
                quality_kbps: 0,
                namespace: "scope".into(),
                item: done,
                file,
                bytes: 6,
                used: 1,
            },
        );
        crate::atomic_write_json(&dir.path().join("index.json"), &manifest).unwrap();
        let cache = Cache::new(dir.path().into(), Config::default(), "fixture").unwrap();
        let pending = cache.pending_items("scope");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].rating_key, "1");
        // Superseded partial removed entirely; non-resumable transcode bytes
        // released while its record keeps the track pending.
        assert!(!dir
            .path()
            .join(format!("{}.part", crate::sha256_hex("p2")))
            .exists());
        assert!(!dir
            .path()
            .join(format!("{}.resume.json", crate::sha256_hex("p2")))
            .exists());
        assert!(!dir
            .path()
            .join(format!("{}.part", crate::sha256_hex("p320")))
            .exists());
        assert!(dir
            .path()
            .join(format!("{}.resume.json", crate::sha256_hex("p320")))
            .exists());
        assert!(dir
            .path()
            .join(format!("{}.part", crate::sha256_hex("p0")))
            .exists());
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
        let file = format!("{}.wav", crate::sha256_hex("e1"));
        fs::write(dir.path().join(&file), b"cached").unwrap();
        {
            let mut state = crate::mutex_lock(&cache.state);
            state.manifest.entries.insert(
                crate::sha256_hex("e1"),
                Entry {
                    quality_kbps: 0,
                    namespace: "scope".into(),
                    item: song.clone(),
                    file,
                    bytes: 6,
                    used: 1,
                },
            );
            state.lru_saved = Instant::now()
                .checked_sub(LRU_PERSIST_INTERVAL)
                .unwrap_or(state.lru_saved);
        }
        commit(dir.path(), &cache.state).unwrap();
        assert!(cache.lookup("scope", &song, true).is_some());
        // Read the index without dropping the cache (Drop would also save).
        let saved: Manifest =
            serde_json::from_slice(&fs::read(dir.path().join("index.json")).unwrap()).unwrap();
        assert!(saved.entries[&crate::sha256_hex("e1")].used > 1);
    }
}

/// Plan E3 hold state that spans a batch's retry passes.
#[cfg(test)]
mod hold_tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
    };

    fn accept(listener: &TcpListener) -> TcpStream {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut byte = [0];
        while !bytes.ends_with(b"\r\n\r\n") && socket.read_exact(&mut byte).is_ok() {
            bytes.push(byte[0]);
        }
        let request = String::from_utf8_lossy(&bytes).to_lowercase();
        assert!(request.contains("/library/parts/1/"), "{request}");
        socket
    }
    fn wait(mut done: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(8);
        while !done() {
            assert!(Instant::now() < until, "timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    // A server that ignored a resume range after a hold is not interrupted
    // by a later hold, also on the batch's next retry pass.
    #[test]
    fn an_ignored_range_stays_uninterruptible_across_retry_passes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().into(), Config::default(), "fixture").unwrap();
        let control = cache.control(Default::default());
        let item = Item {
            rating_key: "1".into(),
            kind: crate::model::ItemKind::Track,
            media: vec![crate::model::Media {
                audio_codec: "pcm".into(),
                container: "wav".into(),
                parts: vec![crate::model::Part {
                    key: "/library/parts/1/file.wav".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let received = || crate::mutex_lock(&cache.state).received;
        cache.schedule(&url, "token", vec![item.clone()]).unwrap();
        let head = "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: 20\r\nETag: \"one\"\r\nConnection: close\r\n\r\n";
        let mut first = accept(&listener);
        first
            .write_all(format!("{head}abcdefghij").as_bytes())
            .unwrap();
        wait(|| received() == 10);
        control.hold(true);
        // Dropped for the hold.
        assert!(matches!(first.read(&mut [0; 16]), Ok(0) | Err(_)));
        control.hold(false);
        // The resume ignores the range (200), then breaks: the next pass
        // resumes from the 5 bytes it wrote.
        let mut second = accept(&listener);
        second.write_all(format!("{head}abcde").as_bytes()).unwrap();
        wait(|| received() == 5);
        drop(second);
        // This time the server honours the range; a hold must still not
        // interrupt the item, which is marked for the whole batch.
        let mut third = accept(&listener);
        third
            .write_all(
                b"HTTP/1.1 206 Partial Content\r\nContent-Type: audio/wav\r\nContent-Length: 15\r\nContent-Range: bytes 5-19/20\r\nETag: \"one\"\r\nConnection: close\r\n\r\nfghij",
            )
            .unwrap();
        wait(|| received() == 10);
        control.hold(true);
        std::thread::sleep(Duration::from_millis(200));
        third.write_all(b"klmnopqrst").unwrap();
        drop(third);
        let namespace = namespace(&url, "token");
        wait(|| cache.lookup(&namespace, &item, false).is_some());
        control.hold(false);
        let (path, _) = cache.lookup(&namespace, &item, false).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"abcdefghijklmnopqrst");
    }
}

/// Eviction journal, orphan recovery and lock-scope regressions.
#[cfg(test)]
mod recovery_tests {
    use super::*;
    use std::sync::mpsc as std_mpsc;

    const MIB: u64 = 1024 * 1024;

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
    fn config(limit_mb: u64) -> Config {
        Config {
            enabled: true,
            limit_mb,
            ahead: 5,
        }
    }
    fn sparse(path: &Path, bytes: u64) {
        fs::File::create(path).unwrap().set_len(bytes).unwrap();
    }
    /// Index a sparse completed file for `id` and return its file name.
    fn put(cache: &Cache, id: &str, bytes: u64, used: u64) -> String {
        let item = track(id);
        let key = key("scope", &item);
        let file = format!("{key}.wav");
        sparse(&cache.root.join(&file), bytes);
        crate::mutex_lock(&cache.state).manifest.entries.insert(
            key,
            Entry {
                quality_kbps: 0,
                namespace: "scope".into(),
                item,
                file: file.clone(),
                bytes,
                used,
            },
        );
        file
    }
    fn indexed(cache: &Cache, id: &str) -> bool {
        crate::mutex_lock(&cache.state)
            .manifest
            .entries
            .contains_key(&key("scope", &track(id)))
    }
    fn on_disk(dir: &Path) -> Value {
        serde_json::from_slice(&fs::read(dir.join("index.json")).unwrap()).unwrap()
    }
    fn fresh_quarantine(dir: &Path) {
        fs::write(dir.join("index.json.corrupt-fixture"), b"{broken").unwrap();
    }

    // An unlink that fails after the index committed the eviction must not
    // leave an untracked file eating quota: the journal retries it before
    // anything else (here: the newest playable track) is evicted.
    #[test]
    fn failed_unlink_is_retried_from_the_journal_before_evicting_more() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().into(), config(128), "fixture").unwrap();
        let one = put(&cache, "1", 40 * MIB, 1);
        let two = put(&cache, "2", 40 * MIB, 2);
        put(&cache, "3", 40 * MIB, 3);
        faults::inject(
            faults::Op::Unlink,
            &dir.path().join(&one),
            faults::Action::Fail,
            true,
        );
        cache.configure(config(64), None).unwrap();
        assert!(
            !dir.path().join(&one).exists(),
            "journal retried the unlink"
        );
        assert!(!dir.path().join(&two).exists());
        assert!(
            indexed(&cache, "3"),
            "newest track evicted for a failed unlink"
        );
        let saved = on_disk(dir.path());
        assert_eq!(saved["entries"].as_object().unwrap().len(), 1);
        assert!(saved.get("evicting").is_none());
    }

    // A persistently failing unlink stays journaled (index and disk agree:
    // the file is listed for deletion, not as playable) and is finished on
    // the next start, even while a fresh quarantine preserves orphans.
    #[test]
    fn journaled_eviction_survives_failure_and_is_finished_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().into(), config(128), "fixture").unwrap();
        let one = put(&cache, "1", 40 * MIB, 1);
        put(&cache, "2", 40 * MIB, 2);
        put(&cache, "3", 40 * MIB, 3);
        crate::mutex_lock(&cache.state)
            .permanent
            .insert(("scope".into(), "3".into()));
        let path = dir.path().join(&one);
        faults::inject(faults::Op::Unlink, &path, faults::Action::Fail, false);
        assert!(cache.configure(config(64), None).is_err());
        assert!(path.exists());
        assert!(!indexed(&cache, "1"));
        let saved = on_disk(dir.path());
        assert!(saved["entries"].get(key("scope", &track("1"))).is_none());
        assert_eq!(saved["evicting"], json!([one]));
        faults::clear(&path);
        drop(cache);
        fresh_quarantine(dir.path());
        let cache = Cache::new(dir.path().into(), config(128), "fixture").unwrap();
        assert!(!path.exists(), "journaled eviction was not finished");
        assert!(indexed(&cache, "3"));
        drop(cache);
        assert!(on_disk(dir.path()).get("evicting").is_none());
    }

    // Commit-first: when the index cannot be written nothing is deleted
    // and the in-memory catalogue still matches the disk.
    #[test]
    fn failed_index_commit_deletes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().into(), config(128), "fixture").unwrap();
        let files: Vec<_> = (1..=3)
            .map(|id| put(&cache, &id.to_string(), 40 * MIB, id))
            .collect();
        let index = dir.path().join("index.json");
        faults::inject(faults::Op::Commit, &index, faults::Action::Fail, true);
        assert!(cache.configure(config(64), None).is_err());
        for (id, file) in files.iter().enumerate() {
            assert!(dir.path().join(file).exists(), "{file} deleted uncommitted");
            assert!(indexed(&cache, &(id + 1).to_string()));
        }
        assert!(cache.lookup("scope", &track("1"), false).is_some());
        assert_eq!(cache.config().limit_mb, 128);
    }

    // A crash after the journal commit but before the unlinks.
    #[test]
    fn startup_finishes_a_crashed_eviction_even_while_preserving_orphans() {
        let dir = tempfile::tempdir().unwrap();
        let victim = format!("{}.wav", "b".repeat(64));
        fs::write(dir.path().join(&victim), b"evicted").unwrap();
        fs::write(
            dir.path().join("index.json"),
            serde_json::to_vec(&json!({"entries":{},"evicting":[victim]})).unwrap(),
        )
        .unwrap();
        fresh_quarantine(dir.path());
        drop(Cache::new(dir.path().into(), Config::default(), "fixture").unwrap());
        assert!(!dir.path().join(&victim).exists());
    }

    // Lookup/status only take the state lock briefly; a slow unlink in
    // remove() (holding the writer side) must not stall playback lookups.
    #[test]
    fn slow_filesystem_work_does_not_block_lookup_or_status() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().into(), config(128), "fixture").unwrap();
        let one = put(&cache, "1", 40 * MIB, 1);
        put(&cache, "2", 40 * MIB, 2);
        crate::mutex_lock(&cache.state).config.limit_mb = 64;
        let (entered, inside) = std_mpsc::channel();
        let (release, wait) = std_mpsc::channel();
        faults::inject(
            faults::Op::Unlink,
            &dir.path().join(&one),
            faults::Action::Block {
                entered,
                release: Arc::new(Mutex::new(wait)),
            },
            true,
        );
        // The download worker's admission path: writer lock, then room().
        let (root, state, writer) = (
            cache.root.clone(),
            cache.state.clone(),
            cache.writer.clone(),
        );
        let evictor = std::thread::spawn(move || {
            let _writer = crate::mutex_lock(&writer);
            room(&root, &state, 0, "")
        });
        inside.recv_timeout(Duration::from_secs(10)).unwrap();
        // Watchdog: never hang the suite if the lookup does block.
        let (stop, stopped) = std_mpsc::channel::<()>();
        let watchdog = std::thread::spawn(move || {
            let blocked = stopped.recv_timeout(Duration::from_secs(5)).is_err();
            let _ = release.send(());
            blocked
        });
        let found = cache.lookup("scope", &track("2"), true).is_some();
        let status = cache.status("scope");
        let items = cache.items("scope");
        let _ = stop.send(());
        assert!(
            !watchdog.join().unwrap(),
            "lookup blocked behind a slow unlink"
        );
        assert!(found);
        assert_eq!(status["tracks"], 1, "evicted entry already uncommitted");
        assert_eq!(items.len(), 1);
        evictor.join().unwrap().unwrap();
        assert!(!dir.path().join(&one).exists());
        assert!(indexed(&cache, "2"));
    }

    #[test]
    fn schedule_bursts_coalesce_into_one_waiting_batch() {
        let jobs = Jobs::default();
        let base = Url::parse("http://127.0.0.1:9/").unwrap();
        for epoch in 1..=1000 {
            assert!(jobs.submit(Batch {
                quality_kbps: 0,
                epoch,
                base: base.clone(),
                token: String::new(),
                namespace: "scope".into(),
                items: vec![track(&epoch.to_string())],
            }));
            assert_eq!(jobs.queued(), 1);
        }
        assert_eq!(jobs.superseded.load(Ordering::SeqCst), 999);
        let batch = crate::rt::block_on(jobs.next()).unwrap();
        assert_eq!(batch.epoch, 1000);
        assert_eq!(jobs.queued(), 0);
        jobs.close();
        assert!(crate::rt::block_on(jobs.next()).is_none());
        assert!(!jobs.submit(batch));
    }

    // Through the public API while the worker is busy on a stalled server:
    // a burst never queues more than one batch.
    #[test]
    fn schedule_burst_on_a_busy_worker_stays_bounded() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().into(), Config::default(), "fixture").unwrap();
        for id in 1..=500 {
            cache
                .schedule(&url, "fixture-token", vec![track(&id.to_string())])
                .unwrap();
            assert!(cache.jobs.queued() <= 1);
        }
        let pinned = crate::mutex_lock(&cache.state).pinned.clone();
        assert_eq!(pinned.len(), 1);
        assert!(pinned.iter().all(|(_, key)| key == "500"));
        drop(cache);
        drop(listener);
    }

    // A crash between the completion rename and the index commit leaves an
    // unindexed media file plus its resume record: re-index it.
    #[test]
    fn completed_file_with_resume_record_is_reindexed_at_startup() {
        let dir = tempfile::tempdir().unwrap();
        let song = track("7");
        let id = key("scope", &song);
        fs::write(dir.path().join(format!("{id}.wav")), b"complete").unwrap();
        crate::atomic_write_json(
            &dir.path().join(format!("{id}.resume.json")),
            &Partial {
                quality_kbps: 0,
                total: Some(8),
                validator: Some("\"v\"".into()),
                namespace: "scope".into(),
                item: Some(song.clone()),
            },
        )
        .unwrap();
        let cache = Cache::new(dir.path().into(), Config::default(), "fixture").unwrap();
        let (path, saved) = cache.lookup("scope", &song, false).unwrap();
        assert_eq!(fs::read(path).unwrap(), b"complete");
        assert_eq!(saved.media[0].parts[0].size, Some(8));
        assert!(!dir.path().join(format!("{id}.resume.json")).exists());
        assert_eq!(cache.status("scope")["orphans"]["recovered"], 1);
        drop(cache);
        assert!(on_disk(dir.path())["entries"].get(&id).is_some());
    }

    // After an index corruption, preserved media whose file-name hash maps
    // to a requested track (original or transcode) is adopted, not lost.
    #[test]
    fn preserved_orphans_are_adopted_when_their_hash_matches_a_track() {
        let dir = tempfile::tempdir().unwrap();
        let (song, mp3) = (track("1"), track("2"));
        let original = format!("{}.flac", key("scope", &song));
        let transcode = format!(
            "{}.mp3",
            crate::sha256_hex(&format!("{}\0mp3:320", key("scope", &mp3)))
        );
        let stray = format!("{}.wav", "c".repeat(64));
        for name in [&original, &transcode, &stray] {
            fs::write(dir.path().join(name), b"audio").unwrap();
        }
        fs::write(dir.path().join("index.json"), b"{broken").unwrap();
        let cache = Cache::new(dir.path().into(), Config::default(), "fixture").unwrap();
        let status = cache.status("scope");
        assert_eq!(status["orphans"]["count"], 3);
        assert_eq!(status["orphans"]["bytes"], 15);
        assert!(status["orphans"]["preserveUntil"].as_u64().unwrap() > crate::unix_seconds());
        assert!(cache.lookup("scope", &song, true).is_some());
        assert!(cache.lookup("scope", &mp3, false).is_some());
        assert!(cache.lookup("other", &track("3"), false).is_none());
        let status = cache.status("scope");
        assert_eq!(status["orphans"]["count"], 1);
        assert_eq!(status["orphans"]["recovered"], 2);
        assert_eq!(status["tracks"], 2);
        drop(cache);
        let cache = Cache::new(dir.path().into(), Config::default(), "fixture").unwrap();
        assert!(cache.lookup("scope", &song, false).is_some());
        let entry = crate::mutex_lock(&cache.state).manifest.entries
            [&crate::sha256_hex(&format!("{}\0mp3:320", key("scope", &mp3)))]
            .quality_kbps;
        assert_eq!(entry, 320);
        assert!(dir.path().join(&stray).exists(), "window still preserves");
    }

    // Unplayable orphans give way under quota pressure before playable
    // tracks, instead of pinning the cache at "full" for the whole window.
    #[test]
    fn quota_pressure_reclaims_preserved_orphans_first() {
        let dir = tempfile::tempdir().unwrap();
        let stray = dir.path().join(format!("{}.wav", "d".repeat(64)));
        sparse(&stray, 70 * MIB);
        fresh_quarantine(dir.path());
        let cache = Cache::new(dir.path().into(), config(128), "fixture").unwrap();
        put(&cache, "1", 20 * MIB, 1);
        cache.configure(config(64), None).unwrap();
        assert!(!stray.exists());
        assert!(indexed(&cache, "1"));
        let status = cache.status("scope");
        assert_eq!(status["orphans"]["count"], 0);
        assert_eq!(status["orphans"]["reclaimed"], 1);
    }
}
