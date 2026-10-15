//! Durable private session state.
//!
//! The state directory holds four documents so that frequent playback
//! checkpoints no longer rewrite megabytes:
//!
//! - `session.json`: every [`Settings`] field except the queue, Radio,
//!   listens and history; small, rewritten atomically and only when its bytes
//!   change.
//! - `queue.json`: the queue and Radio, rewritten atomically only when their
//!   content fingerprint changes (not on position-only checkpoints). Its
//!   `revision` is mirrored as `queueRevision` in `session.json`; a mismatch
//!   (crash between the two renames) keeps the newer queue but drops the
//!   position/occurrence that belonged to another queue.
//! - `listens.jsonl` / `history.jsonl`: append-only JSON Lines. A mutation
//!   appends the whole new record; replay keeps the newest record per
//!   identity (namespace + occurrence) at its first position and applies the
//!   same eviction policy as the in-memory callers, so replay reproduces
//!   memory. Damaged lines are skipped; the file is compacted (atomic rewrite)
//!   when it is damaged, exceeds twice its cap, or memory diverged from what
//!   replaying the appends would produce.
//!
//! Old sessions kept everything in `session.json`; loading merges those
//! records, and the owner's first save writes the new files before rewriting
//! `session.json` without them, so a crash in between only duplicates
//! records that replay deduplicates.
use crate::Result;
use serde::{de::IgnoredAny, Deserialize, Serialize};
use std::{
    collections::{hash_map::DefaultHasher, HashMap, VecDeque},
    fs::{self, OpenOptions},
    hash::{Hash, Hasher},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    sync::{Arc, Mutex},
};

const SESSION: &str = "session.json";
const QUEUE: &str = "queue.json";
const LISTENS: &str = "listens.jsonl";
const HISTORY: &str = "history.jsonl";
pub const MAX_LISTENS: usize = 10_000;
pub const MAX_HISTORY: usize = 2_000;
/// More changed records than this are written by compaction instead of a
/// long series of appends (migration, bulk delivery acknowledgements).
const MAX_APPEND: usize = 64;

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
    #[serde(deserialize_with = "crate::daily::lenient_groups")]
    pub download_groups: std::collections::BTreeMap<crate::GroupId, Vec<String>>,
    #[serde(deserialize_with = "crate::daily::lenient_groups")]
    pub download_titles: std::collections::BTreeMap<crate::GroupId, String>,
    #[serde(deserialize_with = "crate::daily::lenient_groups")]
    pub download_plans: std::collections::BTreeMap<crate::GroupId, crate::daily::DownloadPlan>,
    pub refresh_policy: crate::refresh::Policy,
    pub wifi_only: bool,
    pub downloads_paused: bool,
    pub autoplay: bool,
    pub audio: crate::audio::dsp::Config,
    pub quality: crate::quality::Config,
    /// Stored in `history.jsonl`; read from old `session.json` for migration.
    #[serde(skip_serializing)]
    pub history: Vec<crate::history::Play>,
    /// Stored in `listens.jsonl`; read from old `session.json` for migration.
    #[serde(skip_serializing)]
    pub listens: VecDeque<crate::discovery::Listen>,
    pub discovery: std::collections::BTreeMap<String, crate::discovery::Layout>,
    /// What this settings lineage last read/wrote on disk. Clones share it;
    /// a fresh value (e.g. logout's default) rewrites every file.
    #[serde(skip)]
    pub journal: Journal,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Playback {
    /// Stored in `queue.json`.
    #[serde(skip_serializing)]
    pub queue: crate::queue::Queue,
    /// Stored in `queue.json`: Radio advances together with the queue.
    #[serde(skip_serializing)]
    pub radio: Option<crate::radio::Radio>,
    pub position: u64,
    pub generation: u64,
    pub occurrence: String,
    pub listened: u64,
}

/// Opaque record of the persisted state used to skip unchanged writes.
#[derive(Clone, Default)]
pub struct Journal(Arc<Mutex<Disk>>);

#[derive(Clone, Default)]
struct Disk {
    session: Option<u64>,
    /// Queue revision and Radio fingerprint last written to queue.json.
    queue: Option<(u64, u64)>,
    listens: Model<()>,
    history: Model<()>,
}

#[derive(Clone, Copy)]
struct Entry {
    id: u64,
    hash: u64,
    evictable: bool,
    seq: u64,
}

/// Ordered newest-record-per-identity replay state. `clean == false` means
/// the file must be rewritten before anything is appended (missing/torn
/// newline, damaged lines, unknown contents).
#[derive(Clone)]
struct Model<P> {
    entries: VecDeque<(Entry, P)>,
    ids: HashMap<u64, u64>,
    next: u64,
    lines: usize,
    clean: bool,
}

impl<P> Default for Model<P> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            ids: HashMap::new(),
            next: 0,
            lines: 0,
            clean: false,
        }
    }
}

#[derive(Clone, Copy)]
enum Evict {
    /// Listens: drop the oldest record.
    Oldest,
    /// History: drop delivered plays; refuse when none are delivered.
    Delivered,
}

trait Record: Serialize + serde::de::DeserializeOwned + Hash {
    const FILE: &'static str;
    const CAP: usize;
    const EVICT: Evict;
    fn identity(&self) -> (&str, &str);
    fn evictable(&self) -> bool;
}

impl Record for crate::discovery::Listen {
    const FILE: &'static str = LISTENS;
    const CAP: usize = MAX_LISTENS;
    const EVICT: Evict = Evict::Oldest;
    fn identity(&self) -> (&str, &str) {
        (&self.namespace, &self.occurrence)
    }
    fn evictable(&self) -> bool {
        true
    }
}

impl Record for crate::history::Play {
    const FILE: &'static str = HISTORY;
    const CAP: usize = MAX_HISTORY;
    const EVICT: Evict = Evict::Delivered;
    fn identity(&self) -> (&str, &str) {
        (&self.namespace, &self.occurrence)
    }
    fn evictable(&self) -> bool {
        self.delivered
    }
}

fn hash_of(value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn entry<T: Record>(record: &T) -> Entry {
    Entry {
        id: hash_of(&record.identity()),
        hash: hash_of(record),
        evictable: record.evictable(),
        seq: 0,
    }
}

impl<P> Model<P> {
    fn position(&self, id: u64) -> Option<usize> {
        let seq = *self.ids.get(&id)?;
        self.entries.binary_search_by_key(&seq, |(e, _)| e.seq).ok()
    }
    /// Apply one record exactly like the in-memory callers do: replace an
    /// existing identity in place, otherwise evict per policy and push.
    fn apply(&mut self, mut entry: Entry, payload: P, cap: usize, evict: Evict) -> bool {
        if let Some(index) = self.position(entry.id) {
            entry.seq = self.entries[index].0.seq;
            self.entries[index] = (entry, payload);
            return true;
        }
        if self.entries.len() >= cap {
            match evict {
                Evict::Oldest => {
                    while self.entries.len() >= cap {
                        if let Some((old, _)) = self.entries.pop_front() {
                            self.ids.remove(&old.id);
                        }
                    }
                }
                Evict::Delivered => {
                    let ids = &mut self.ids;
                    self.entries.retain(|(e, _)| {
                        if e.evictable {
                            ids.remove(&e.id);
                        }
                        !e.evictable
                    });
                }
            }
            if self.entries.len() >= cap {
                return false;
            }
        }
        entry.seq = self.next;
        self.next += 1;
        self.ids.insert(entry.id, entry.seq);
        self.entries.push_back((entry, payload));
        true
    }
    fn shape(&self) -> Model<()> {
        Model {
            entries: self.entries.iter().map(|(e, _)| (*e, ())).collect(),
            ids: self.ids.clone(),
            next: self.next,
            lines: self.lines,
            clean: self.clean,
        }
    }
    fn matches(&self, memory: &[Entry]) -> bool {
        self.entries.len() == memory.len()
            && self
                .entries
                .iter()
                .zip(memory)
                .all(|((a, _), b)| a.id == b.id && a.hash == b.hash)
    }
}

fn line<T: Serialize>(out: &mut Vec<u8>, record: &T) -> Result<()> {
    serde_json::to_writer(&mut *out, record)?;
    out.push(b'\n');
    Ok(())
}

/// Replay `legacy` records then the log file. Never writes.
fn load_log<T: Record>(dir: &Path, legacy: Option<Vec<T>>) -> Result<(Vec<T>, Model<()>)> {
    let mut model = Model::<T> {
        clean: legacy.is_none(),
        ..Default::default()
    };
    let mut refused = 0usize;
    for record in legacy.into_iter().flatten() {
        let entry = entry(&record);
        refused += usize::from(!model.apply(entry, record, T::CAP, T::EVICT));
    }
    let bytes = match fs::read(dir.join(T::FILE)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    let mut damaged = 0usize;
    let mut rest = &bytes[..];
    while !rest.is_empty() {
        let (text, terminated) = match rest.iter().position(|&b| b == b'\n') {
            Some(end) => {
                let text = &rest[..end];
                rest = &rest[end + 1..];
                (text, true)
            }
            None => (std::mem::take(&mut rest), false),
        };
        model.lines += 1;
        // A missing final newline (torn append) would glue the next append
        // onto this line: force a rewrite before appending again.
        if !terminated {
            model.clean = false;
        }
        match serde_json::from_slice::<T>(text) {
            Ok(record) => {
                let entry = entry(&record);
                refused += usize::from(!model.apply(entry, record, T::CAP, T::EVICT));
            }
            Err(_) => {
                damaged += 1;
                model.clean = false;
            }
        }
    }
    if damaged > 0 || refused > 0 {
        model.clean = false;
        crate::diagnostics::event(format_args!(
            "{}: ignored {damaged} damaged and {refused} excess records",
            T::FILE
        ));
    }
    if model.lines > 2 * T::CAP {
        model.clean = false;
    }
    let shape = model.shape();
    Ok((model.entries.into_iter().map(|(_, r)| r).collect(), shape))
}

/// Persist `records` (memory order) into the log: append changed records
/// when replaying them reproduces memory, otherwise compact.
fn sync_log<'a, T: Record + 'a>(
    dir: &Path,
    records: impl Iterator<Item = &'a T> + Clone,
    model: &mut Model<()>,
) -> Result<()> {
    let memory: Vec<Entry> = records.clone().map(entry).collect();
    let changed: Vec<usize> = memory
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            model
                .position(e.id)
                .is_none_or(|i| model.entries[i].0.hash != e.hash)
        })
        .map(|(i, _)| i)
        .collect();
    if changed.is_empty() && model.clean && model.matches(&memory) {
        return Ok(());
    }
    let mut replay = model.clone();
    let consistent = changed
        .iter()
        .all(|&i| replay.apply(memory[i], (), T::CAP, T::EVICT))
        && replay.matches(&memory);
    let path = dir.join(T::FILE);
    if consistent
        && model.clean
        && changed.len() <= MAX_APPEND
        && model.lines + changed.len() <= 2 * T::CAP
    {
        let mut wanted = changed.iter().copied().peekable();
        let mut bytes = Vec::new();
        for (i, record) in records.enumerate() {
            if wanted.peek() == Some(&i) {
                wanted.next();
                line(&mut bytes, record)?;
            }
        }
        let created = !path.exists();
        // From here on a failure may leave a partial line: rewrite next time.
        model.clean = false;
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(&bytes)?;
        file.sync_data()?;
        if created {
            fs::File::open(dir)?.sync_all()?;
        }
        replay.lines += changed.len();
        *model = replay;
        return Ok(());
    }
    let mut bytes = Vec::new();
    for record in records {
        line(&mut bytes, record)?;
    }
    replace(&path, &bytes)?;
    let mut fresh = Model::<()> {
        clean: true,
        lines: memory.len(),
        ..Default::default()
    };
    for entry in memory {
        fresh.apply(entry, (), usize::MAX, T::EVICT);
    }
    *model = fresh;
    Ok(())
}

/// Atomically replace `path`; empty content removes the file instead, so
/// a fresh or logged-out state leaves no empty documents behind.
fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    if !bytes.is_empty() {
        return crate::atomic_write_bytes(path, bytes);
    }
    match fs::remove_file(path) {
        Ok(()) => {
            if let Some(dir) = path.parent() {
                fs::File::open(dir)?.sync_all()?;
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

struct HashWriter(DefaultHasher);
impl Write for HashWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Serialize)]
struct QueueRef<'a> {
    revision: u64,
    queue: &'a crate::queue::Queue,
    radio: &'a Option<crate::radio::Radio>,
}

#[derive(Deserialize)]
struct QueueFile {
    revision: u64,
    #[serde(default)]
    queue: crate::queue::Queue,
    #[serde(default)]
    radio: Option<crate::radio::Radio>,
}

/// What queue.json holds: the queue's own revision (replaced by every queue
/// mutation, so no content scan) plus a fingerprint of the small Radio state,
/// which `Core` advances in place.
fn queue_state(playback: &Playback) -> Result<(u64, u64)> {
    let mut writer = HashWriter(DefaultHasher::new());
    serde_json::to_writer(&mut writer, &playback.radio)?;
    Ok((playback.queue.revision(), writer.0.finish()))
}

#[derive(Serialize)]
struct SessionRef<'a> {
    #[serde(flatten)]
    settings: &'a Settings,
    #[serde(rename = "queueRevision")]
    queue_revision: u64,
}

/// Which parts of `session.json` are in the old all-in-one format.
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Probe {
    queue_revision: Option<u64>,
    playback: Option<ProbePlayback>,
    listens: Option<IgnoredAny>,
    history: Option<IgnoredAny>,
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct ProbePlayback {
    queue: Option<IgnoredAny>,
}

/// Read the session. Never writes except to preserve a corrupt
/// `session.json`/`queue.json` (as before); migration and compaction happen
/// on the owner's next [`save`].
pub fn load(dir: &Path) -> Result<Settings> {
    let path = dir.join(SESSION);
    let mut disk = Disk::default();
    let (mut settings, probe, present) = match fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<Settings>(&bytes) {
            Ok(settings) => {
                disk.session = Some(hash_of(&bytes));
                let probe = serde_json::from_slice(&bytes).unwrap_or_default();
                (settings, probe, true)
            }
            Err(_) => {
                crate::quarantine_corrupt(&path, &bytes)?;
                crate::diagnostics::event(format_args!(
                    "corrupt session preserved; using defaults"
                ));
                (Settings::default(), Probe::default(), false)
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            (Settings::default(), Probe::default(), false)
        }
        Err(e) => return Err(e.into()),
    };
    let legacy_queue = probe.playback.as_ref().is_some_and(|p| p.queue.is_some());
    // Without a session (absent/corrupt) a queue is not restored, as before;
    // the next save replaces queue.json.
    if present && !legacy_queue {
        let queue_path = dir.join(QUEUE);
        let mut corrupt = false;
        let file = match fs::read(&queue_path) {
            Ok(bytes) => match serde_json::from_slice::<QueueFile>(&bytes) {
                Ok(file) => Some(file),
                Err(_) => {
                    corrupt = true;
                    crate::quarantine_corrupt(&queue_path, &bytes)?;
                    crate::diagnostics::event(format_args!(
                        "corrupt queue preserved; using an empty queue"
                    ));
                    None
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        // An empty queue is stored as an absent queue.json.
        let revision = match file {
            Some(file) => {
                settings.playback.queue = file.queue;
                settings.playback.radio = file.radio;
                file.revision
            }
            // An absent file is the empty queue session.json was saved with;
            // a corrupt one is a different queue whose checkpoint is void.
            None if !corrupt => probe
                .queue_revision
                .unwrap_or_else(|| settings.playback.queue.revision()),
            None => settings.playback.queue.revision(),
        };
        settings.playback.queue.restore_revision(revision);
        disk.queue = Some(queue_state(&settings.playback)?);
        if probe
            .queue_revision
            .is_some_and(|expected| expected != revision)
        {
            // The checkpoint belongs to another queue.
            let playback = &mut settings.playback;
            playback.position = 0;
            playback.listened = 0;
            playback.occurrence.clear();
        }
    }
    let legacy_listens = probe
        .listens
        .map(|_| std::mem::take(&mut settings.listens).into());
    let (listens, model) = load_log(dir, legacy_listens)?;
    settings.listens = listens.into();
    disk.listens = model;
    let legacy_history = probe.history.map(|_| std::mem::take(&mut settings.history));
    let (history, model) = load_log(dir, legacy_history)?;
    settings.history = history;
    disk.history = model;
    settings.journal = Journal(Arc::new(Mutex::new(disk)));
    Ok(settings)
}

/// Whether the owner should [`save`] at startup: old format to migrate,
/// damaged or oversized logs to compact.
pub fn needs_maintenance(settings: &Settings) -> bool {
    let disk = settings.journal.lock();
    disk.session.is_none() || !disk.listens.clean || !disk.history.clean || disk.queue.is_none()
}

impl Journal {
    fn lock(&self) -> std::sync::MutexGuard<'_, Disk> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Remove session temp files (they contain tokens) left by an interrupted
/// save. Only the state directory owner may call this: a concurrent save's
/// temp file would otherwise disappear before its rename.
pub fn remove_stale_temps(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // `.session-<uuid>.tmp`: legacy writer; `<file>.<uuid>.tmp`: atomic_write_bytes
        // (including `<file>.corrupt-<uuid>.<uuid>.tmp` backups).
        if name.ends_with(".tmp")
            && (name.starts_with(".session-")
                || [SESSION, QUEUE, LISTENS, HISTORY]
                    .iter()
                    .any(|file| name.starts_with(&format!("{file}."))))
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn prepare(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Persist everything that changed since this lineage last read/wrote:
/// queue.json (content changed), log appends/compaction, then session.json
/// (bytes changed). The order keeps a crash recoverable: session.json only
/// names a queue revision after that queue is durable, and old-format
/// fields leave session.json only after the logs hold them.
pub fn save(dir: &Path, settings: &Settings) -> Result<()> {
    prepare(dir)?;
    let mut disk = settings.journal.lock();
    let state = queue_state(&settings.playback)?;
    let revision = state.0;
    if disk.queue != Some(state) {
        disk.queue = None;
        let empty = settings.playback.queue.items().is_empty() && settings.playback.radio.is_none();
        let bytes = if empty {
            Vec::new()
        } else {
            serde_json::to_vec(&QueueRef {
                revision,
                queue: &settings.playback.queue,
                radio: &settings.playback.radio,
            })?
        };
        replace(&dir.join(QUEUE), &bytes)?;
        disk.queue = Some(state);
    }
    sync_logs(dir, settings, &mut disk)?;
    let bytes = serde_json::to_vec(&SessionRef {
        settings,
        queue_revision: revision,
    })?;
    let hash = hash_of(&bytes);
    if disk.session != Some(hash) {
        disk.session = None;
        crate::atomic_write_bytes(&dir.join(SESSION), &bytes)?;
        disk.session = Some(hash);
    }
    Ok(())
}

/// Persist only listen/history changes (normally one appended line). Used
/// by listening events, which are followed by a full playback checkpoint.
pub fn save_journals(dir: &Path, settings: &Settings) -> Result<()> {
    prepare(dir)?;
    let mut disk = settings.journal.lock();
    sync_logs(dir, settings, &mut disk)
}

fn sync_logs(dir: &Path, settings: &Settings, disk: &mut Disk) -> Result<()> {
    sync_log(dir, settings.listens.iter(), &mut disk.listens)?;
    sync_log(dir, settings.history.iter(), &mut disk.history)
}

#[cfg(test)]
mod tests;
