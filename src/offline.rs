//! Private, credential-scoped raw metadata snapshots and bounded artwork storage.
use crate::{
    model::{Container, Item},
    plex::{advertised_path, server_url},
    Error, Result,
};
use std::{
    collections::BTreeMap,
    fs::{self},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
};

pub(crate) fn prune_excluding(
    root: &Path,
    extra: u64,
    limit: u64,
    exclude: Option<&Path>,
) -> Result<()> {
    if extra > limit {
        return Err(Error::Input("Artwork exceeds size limit"));
    }
    let mut files: Vec<_> = fs::read_dir(root)?
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if Some(path.as_path()) == exclude {
                return None;
            }
            e.metadata()
                .ok()
                .filter(|m| m.is_file())
                .map(|m| (m.modified().ok(), m.len(), path))
        })
        .collect();
    // Total excludes the file about to be overwritten: overwriting the same
    // key with an equal-or-smaller payload must not evict unrelated snapshots.
    // `extra` is the full new payload size.
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    // Account for the new payload.
    files.sort_by_key(|f| f.0);
    for (_, size, path) in files {
        if total.saturating_add(extra) <= limit {
            break;
        }
        fs::remove_file(path)?;
        total = total.saturating_sub(size);
    }
    Ok(())
}

/// Reads refresh a file's mtime (the prune age signal) at most this often, so
/// frequently read snapshots are not evicted first without a write per read.
const READ_TOUCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Outcome of offline lyric/snapshot writes, readable by the UI through
/// [`Library::store_status`]. Failures (quota prune, oversize, I/O) are
/// counted and reported instead of being dropped silently.
#[derive(Default)]
struct Health {
    failures: u64,
    /// The latest save failed (cleared by the next successful one).
    failing: bool,
    last_error: String,
    last_kind: &'static str,
    last_at: u64,
}

/// Snapshot/lyrics budget shared by the library directory.
const LIBRARY_LIMIT: u64 = 64 * 1024 * 1024;

pub struct Library {
    root: PathBuf,
    writable: bool,
    health: Mutex<Health>,
}
impl Library {
    /// Bounded, prune-first atomic write of one library file; the outcome is
    /// recorded for [`Library::store_status`] and diagnostics.
    fn store(&self, kind: &'static str, target: &Path, bytes: Result<Vec<u8>>) {
        let result = bytes.and_then(|bytes| {
            if bytes.len() as u64 > LIBRARY_LIMIT {
                return Err(Error::Input("Offline data exceeds its storage limit"));
            }
            prune_excluding(&self.root, bytes.len() as u64, LIBRARY_LIMIT, Some(target))?;
            crate::atomic_write_bytes(target, &bytes)
        });
        let mut health = crate::mutex_lock(&self.health);
        match result {
            Ok(()) => health.failing = false,
            Err(error) => {
                health.failures += 1;
                health.failing = true;
                health.last_error = error.to_string();
                health.last_kind = kind;
                health.last_at = crate::unix_seconds();
                drop(health);
                crate::diagnostics::event(format_args!("offline {kind} not saved: {error}"));
            }
        }
    }
    /// `{ok, failures, error, lastKind, lastAt}` of offline lyric and
    /// metadata snapshot saves since this library was opened.
    pub fn store_status(&self) -> serde_json::Value {
        let health = crate::mutex_lock(&self.health);
        serde_json::json!({
            "ok": !health.failing,
            "failures": health.failures,
            // Named `error`: the UI translates Core messages in such fields.
            "error": health.last_error,
            "lastKind": health.last_kind,
            "lastAt": health.last_at,
        })
    }
    /// Mark a file as recently used for oldest-first pruning.
    fn touch(&self, path: &Path) {
        if !self.writable {
            return;
        }
        let now = std::time::SystemTime::now();
        let stale = fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age >= READ_TOUCH_INTERVAL);
        if stale {
            if let Ok(file) = fs::File::options().write(true).open(path) {
                let _ = file.set_modified(now);
            }
        }
    }
    /// Metadata snapshots of a namespace, excluding its lyrics files.
    fn snapshots(&self, namespace: &str) -> impl Iterator<Item = PathBuf> {
        let prefix = format!("{namespace}-");
        let lyrics = format!("{namespace}-lyrics-");
        fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(move |entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with(&prefix) && !name.starts_with(&lyrics)
            })
            .map(|entry| entry.path())
    }
    pub(crate) fn catalogue_stamp(&self) -> Option<std::time::SystemTime> {
        fs::metadata(&self.root).ok()?.modified().ok()
    }
    pub fn new(root: PathBuf, writable: bool) -> Result<Self> {
        if writable {
            fs::create_dir_all(&root)?;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            root,
            writable,
            health: Mutex::default(),
        })
    }
    fn path(&self, namespace: &str, path: &str, params: &[(&str, String)]) -> PathBuf {
        // Params are simple string pairs; serialization cannot fail in practice.
        // Avoid panicking library lookups on allocator failure: fall back to a
        // debug representation which is still deterministic for this process.
        let encoded = serde_json::to_string(params).unwrap_or_else(|_| format!("{params:?}"));
        self.root.join(format!(
            "{namespace}-{}.json",
            crate::sha256_hex(&format!("{path}\0{encoded}"))
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
            let target = self.path(namespace, path, params);
            let bytes = serde_json::to_vec(container).map_err(Error::from);
            self.store("snapshot", &target, bytes);
        }
    }
    pub fn load(
        &self,
        namespace: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Option<Container> {
        let file = self.path(namespace, path, params);
        let container = serde_json::from_slice(&fs::read(&file).ok()?).ok()?;
        self.touch(&file);
        Some(container)
    }
    pub fn items(&self, namespace: &str) -> Vec<Item> {
        let mut items = std::collections::BTreeMap::new();
        for path in self.snapshots(namespace) {
            if let Some(c) = fs::read(path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Container>(&b).ok())
            {
                for i in c
                    .items
                    .into_iter()
                    .chain(c.hubs.into_iter().flat_map(|h| h.items))
                {
                    if matches!(
                        i.kind,
                        crate::model::ItemKind::Artist
                            | crate::model::ItemKind::Album
                            | crate::model::ItemKind::Track
                    ) {
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
        for path in self.snapshots(namespace) {
            if let Some(mut c) = fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Container>(&b).ok())
            {
                let mut changed = false;
                for i in c
                    .items
                    .iter_mut()
                    .chain(c.hubs.iter_mut().flat_map(|h| &mut h.items))
                {
                    if i.rating_key == key && i.user_rating != rating {
                        i.user_rating = rating;
                        changed = true;
                    }
                }
                // Rewriting untouched snapshots resets their prune age and
                // wears flash for nothing.
                if changed {
                    let _ = crate::atomic_write_json(&path, &c);
                }
            }
        }
    }
    pub fn lyrics(&self, namespace: &str, key: &str) -> Option<Vec<crate::lyrics::Line>> {
        let file = self.lyrics_file(namespace, key);
        let lines = serde_json::from_slice(&fs::read(&file).ok()?).ok()?;
        self.touch(&file);
        Some(lines)
    }
    fn lyrics_file(&self, namespace: &str, key: &str) -> PathBuf {
        // Keys are numeric ratingKeys in production, but the storage layer must
        // not trust callers: '/' or '..' would create nested/missing parents
        // and silently lose lyrics. Safe keys keep the legacy filename for
        // backward compatibility; anything else is hashed.
        let safe = !key.is_empty()
            && key.len() <= 64
            && key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if safe {
            self.root.join(format!("{namespace}-lyrics-{key}.json"))
        } else {
            self.root.join(format!(
                "{namespace}-lyrics-hash-{}.json",
                crate::sha256_hex(key)
            ))
        }
    }
    pub fn save_lyrics(&self, namespace: &str, key: &str, lines: &[crate::lyrics::Line]) {
        if self.writable {
            let target = self.lyrics_file(namespace, key);
            let bytes = serde_json::to_vec(lines).map_err(Error::from);
            self.store("lyrics", &target, bytes);
        }
    }
}

struct Job {
    epoch: u64,
    server: String,
    token: String,
    namespace: String,
    path: String,
}
/// The offline prefetch queue over the artwork store: one job at a time,
/// gated like audio downloads and cancelled by a new generation.
pub struct Artwork {
    gate: Option<Arc<crate::cache::Gate>>,
    store: Arc<crate::artwork::Store>,
    jobs: Option<tokio::sync::mpsc::Sender<Job>>,
    /// Queued images (cache file key -> generation), deduplicated.
    pending: Arc<Mutex<BTreeMap<String, u64>>>,
    worker: Option<crate::rt::Task>,
    stopping: Arc<AtomicBool>,
}
impl Artwork {
    pub fn new(root: PathBuf, writable: bool) -> Result<Self> {
        let store = Arc::new(crate::artwork::Store::new(root, writable)?);
        let pending: Arc<Mutex<BTreeMap<String, u64>>> = Arc::default();
        let stopping = Arc::new(AtomicBool::new(false));
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Job>(128);
        let (active, stop, queue) = (pending.clone(), stopping.clone(), store.clone());
        let worker = writable.then(|| {
            crate::rt::spawn(async move {
                let generation = queue.generation().clone();
                while let Some(job) = rx.recv().await {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let key = format!("{}\0{}", job.namespace, job.path);
                    if job.epoch == generation.load(Ordering::SeqCst) {
                        queue
                            .load(
                                &job.server,
                                &job.token,
                                &job.namespace,
                                &job.path,
                                crate::artwork::Party::Prefetch(job.epoch),
                            )
                            .await;
                    }
                    // Old generations only drop their own entry, never a
                    // newer reschedule of the same image.
                    let mut active = crate::mutex_lock(&active);
                    if active.get(&key).is_some_and(|e| *e == job.epoch) {
                        active.remove(&key);
                    }
                }
            })
        });
        Ok(Self {
            gate: None,
            store,
            jobs: if writable { Some(tx) } else { None },
            pending,
            worker,
            stopping,
        })
    }
    /// The `file://` URL of a cached image.
    pub fn local(&self, namespace: &str, path: &str) -> Option<String> {
        let file = self.store.local(namespace, path)?;
        url::Url::from_file_path(file).ok().map(String::from)
    }
    pub(crate) fn store(&self) -> &Arc<crate::artwork::Store> {
        &self.store
    }
    pub fn schedule(&self, server: &str, token: &str, path: &str) {
        if self.gate.as_ref().is_some_and(|gate| !gate.allowed()) {
            return;
        }
        if server_url(server)
            .and_then(|base| advertised_path(&base, path))
            .is_err()
        {
            return;
        }
        let namespace = crate::cache::namespace(server, token);
        // A browsed copy is still queued: the job moves it into the offline
        // budget without downloading it again.
        if self.store.offline(&namespace, path) {
            return;
        }
        let Some(tx) = &self.jobs else {
            return;
        };
        let key = format!("{namespace}\0{path}");
        let epoch = self.store.generation().load(Ordering::SeqCst);
        {
            let mut pending = crate::mutex_lock(&self.pending);
            if pending.contains_key(&key) {
                return;
            }
            pending.insert(key.clone(), epoch);
        }
        let job = Job {
            epoch,
            server: server.into(),
            token: token.into(),
            namespace,
            path: path.into(),
        };
        if tx.try_send(job).is_err() {
            let mut pending = crate::mutex_lock(&self.pending);
            if pending.get(&key).is_some_and(|e| *e == epoch) {
                pending.remove(&key);
            }
        }
    }
    pub fn cancel(&self) {
        self.store.generation().fetch_add(1, Ordering::SeqCst);
        // Clear dedup set so an immediate reschedule of the same path can
        // queue a new-epoch job. Stale worker jobs are epoch-guarded and only
        // remove their own epoch entry, never the rescheduled one.
        crate::mutex_lock(&self.pending).clear();
    }
    pub fn generation(&self) -> Arc<AtomicU64> {
        self.store.generation().clone()
    }
    pub fn set_gate(&mut self, gate: Arc<crate::cache::Gate>) {
        self.gate = Some(gate);
    }
    pub fn clear(&self) -> Result<()> {
        crate::mutex_lock(&self.pending).clear();
        self.store.clear()
    }
}
impl Drop for Artwork {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        // Requests still holding the store save nothing more.
        self.store.close();
        self.jobs.take();
        if let Some(worker) = self.worker.take() {
            // Shutdown does not wait out an artwork download.
            worker.abort();
            worker.join(Some(std::time::Duration::from_secs(1)));
        }
    }
}

#[cfg(test)]
mod artwork_pending_tests {
    use super::*;

    #[test]
    fn cancel_clears_pending_to_allow_immediate_reschedule() {
        // Pending dedups identical artwork paths. Cancel bumps the epoch but
        // currently leaves the old entry behind, so an immediate reschedule of
        // the same path finds `insert() == false`, sends no new job, and the
        // old worker job is then discarded for epoch mismatch: artwork lost.
        let dir = tempfile::tempdir().unwrap();
        let artwork = Artwork::new(dir.path().into(), true).unwrap();
        let file = "pending-fixture".to_owned();
        artwork
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(file.clone(), 0);
        artwork.cancel();
        let pending = artwork
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        assert!(
            !pending.contains_key(&file),
            "cancel must clear pending, otherwise reschedule after cancel is silently dropped"
        );
    }
}

#[cfg(test)]
mod store_health_tests {
    use super::*;

    #[test]
    fn failed_lyric_and_snapshot_saves_are_observable() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::new(dir.path().into(), true).unwrap();
        assert_eq!(library.store_status()["ok"], true);
        assert_eq!(library.store_status()["failures"], 0);
        // A read-only directory makes the atomic write fail.
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
        if fs::write(dir.path().join("probe"), b"x").is_ok() {
            return; // Privileged runner: permissions are not enforced.
        }
        let line = crate::lyrics::Line {
            time: None,
            text: "fixture".into(),
        };
        library.save_lyrics("ns", "5", std::slice::from_ref(&line));
        let status = library.store_status();
        assert_eq!(status["ok"], false);
        assert_eq!(status["failures"], 1);
        assert_eq!(status["lastKind"], "lyrics");
        assert!(!status["error"].as_str().unwrap().is_empty());
        library.save("ns", "/library/metadata/1", &[], &Container::default());
        let status = library.store_status();
        assert_eq!(status["failures"], 2);
        assert_eq!(status["lastKind"], "snapshot");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        library.save_lyrics("ns", "5", std::slice::from_ref(&line));
        let status = library.store_status();
        assert_eq!(
            status["ok"], true,
            "a later success clears the failing state"
        );
        assert_eq!(status["failures"], 2);
        assert_eq!(library.lyrics("ns", "5").unwrap().len(), 1);
    }

    #[test]
    fn oversized_saves_are_rejected_without_pruning_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::new(dir.path().into(), true).unwrap();
        let line = crate::lyrics::Line {
            time: None,
            text: "keep".into(),
        };
        library.save_lyrics("ns", "1", std::slice::from_ref(&line));
        let huge = crate::lyrics::Line {
            time: None,
            text: "x".repeat(LIBRARY_LIMIT as usize + 1),
        };
        library.save_lyrics("ns", "2", std::slice::from_ref(&huge));
        assert!(
            library.lyrics("ns", "1").is_some(),
            "oversize save pruned others"
        );
        assert!(library.lyrics("ns", "2").is_none());
        let status = library.store_status();
        assert_eq!(status["ok"], false);
        assert_eq!(status["lastKind"], "lyrics");
    }
}
