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

fn prune_excluding(root: &Path, extra: u64, limit: u64, exclude: Option<&Path>) -> Result<()> {
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

/// Download one image: image content type only, at most 4 MiB.
async fn fetch(client: &reqwest::Client, job: &Job) -> Result<Vec<u8>> {
    fetch_artwork(client, &job.server, &job.token, &job.path).await
}
/// Largest artwork image accepted, downloaded or read from the cache.
pub(crate) const ARTWORK_LIMIT: u64 = 4 * 1024 * 1024;
/// Download the image at server-relative `path`, the credential in a header.
pub(crate) async fn fetch_artwork(
    client: &reqwest::Client,
    server: &str,
    token: &str,
    path: &str,
) -> Result<Vec<u8>> {
    let url = advertised_path(&server_url(server)?, path)?;
    let mut response = client
        .get(url)
        .header("X-Plex-Token", token)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .map_err(|_| Error::Network)?;
    if !response.status().is_success()
        || !response
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .map(|s| {
                s.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase()
            })
            .is_some_and(|s| s.starts_with("image/"))
    {
        return Err(Error::ProtocolAt("artwork"));
    }
    let bytes = crate::plex::bounded_body(&mut response, ARTWORK_LIMIT as usize)
        .await
        .map_err(|error| error.unwrap_or(Error::Input("Artwork exceeds size limit")))?;
    if bytes.is_empty() {
        return Err(Error::Input("Artwork exceeds size limit"));
    }
    Ok(bytes)
}
/// Persist a downloaded image unless its generation was cancelled meanwhile.
fn store(
    file: &Path,
    epoch: u64,
    bytes: &[u8],
    active: &Mutex<BTreeMap<PathBuf, u64>>,
    generation: &AtomicU64,
) -> Result<()> {
    // file is always root.join(hash); parent is root. Skip defensively.
    let Some(root) = file.parent() else {
        return Ok(());
    };
    // Reuse the bounded library prune: oldest-first until the incoming image
    // fits in the 128 MiB artwork budget.
    prune_excluding(root, bytes.len() as u64, 128 * 1024 * 1024, Some(file))?;
    let _guard = crate::mutex_lock(active);
    if epoch != generation.load(Ordering::SeqCst) {
        return Ok(());
    }
    crate::atomic_write_bytes(file, bytes)
}

struct Job {
    epoch: u64,
    server: String,
    token: String,
    path: String,
    file: PathBuf,
}
/// The cached image file of `path` under `root`, if present: a regular,
/// non-empty file (never a symlink).
pub(crate) fn local_artwork(root: &Path, namespace: &str, path: &str) -> Option<PathBuf> {
    let file = root.join(crate::sha256_hex(&format!("{namespace}\0{path}")));
    fs::symlink_metadata(&file)
        .ok()
        .filter(|m| m.is_file() && !m.file_type().is_symlink() && m.len() > 0)?;
    Some(file)
}
pub struct Artwork {
    gate: Option<Arc<crate::cache::Gate>>,
    root: PathBuf,
    jobs: Option<tokio::sync::mpsc::Sender<Job>>,
    pending: Arc<Mutex<BTreeMap<PathBuf, u64>>>,
    worker: Option<crate::rt::Task>,
    stopping: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
}
impl Artwork {
    pub fn new(root: PathBuf, writable: bool) -> Result<Self> {
        if writable {
            fs::create_dir_all(&root)?;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        }
        let pending = Arc::new(Mutex::new(BTreeMap::new()));
        let stopping = Arc::new(AtomicBool::new(false));
        let generation = Arc::new(AtomicU64::new(0));
        let gen = generation.clone();
        let stop = stopping.clone();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Job>(128);
        let active = pending.clone();
        // One task on the shared runtime with the pooled Plex client; the
        // prune/write step runs on the blocking pool.
        let worker = writable.then(|| {
            crate::rt::spawn(async move {
                let client = crate::plex::client().clone();
                while let Some(job) = rx.recv().await {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if job.epoch != gen.load(Ordering::SeqCst) {
                        // Old generation: drop without clobbering a newer
                        // reschedule for the same file (pending maps file->epoch).
                        let mut active = crate::mutex_lock(&active);
                        if active.get(&job.file).is_some_and(|e| *e == job.epoch) {
                            active.remove(&job.file);
                        }
                        continue;
                    }
                    if let Ok(bytes) = fetch(&client, &job).await {
                        let (active, gen, file, epoch) =
                            (active.clone(), gen.clone(), job.file.clone(), job.epoch);
                        // The result is ignored by design.
                        let _ =
                            crate::rt::unblock(move || store(&file, epoch, &bytes, &active, &gen))
                                .await;
                    }
                    let mut active = crate::mutex_lock(&active);
                    if active.get(&job.file).is_some_and(|e| *e == job.epoch) {
                        active.remove(&job.file);
                    }
                }
            })
        });
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
        let file = local_artwork(&self.root, namespace, path)?;
        url::Url::from_file_path(file).ok().map(String::from)
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn schedule(&self, server: &str, token: &str, path: &str) {
        if self.gate.as_ref().is_some_and(|gate| !gate.allowed()) {
            return;
        }
        if server_url(server)
            .and_then(|base| crate::plex::advertised_path(&base, path))
            .is_err()
        {
            return;
        }
        let ns = crate::cache::namespace(server, token);
        if self.local(&ns, path).is_some() {
            return;
        }
        let file = self.root.join(crate::sha256_hex(&format!("{ns}\0{path}")));
        if let Some(tx) = &self.jobs {
            let epoch = self.generation.load(Ordering::SeqCst);
            {
                let mut pending = crate::mutex_lock(&self.pending);
                if pending.contains_key(&file) {
                    return;
                }
                pending.insert(file.clone(), epoch);
            }
            if tx
                .try_send(Job {
                    epoch,
                    server: server.into(),
                    token: token.into(),
                    path: path.into(),
                    file: file.clone(),
                })
                .is_err()
            {
                let mut pending = crate::mutex_lock(&self.pending);
                if pending.get(&file).is_some_and(|e| *e == epoch) {
                    pending.remove(&file);
                }
            }
        }
    }
    pub fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        // Clear dedup set so an immediate reschedule of the same path can
        // queue a new-epoch job. Stale worker jobs are epoch-guarded and only
        // remove their own epoch entry, never the rescheduled one.
        crate::mutex_lock(&self.pending).clear();
    }
    pub fn generation(&self) -> Arc<AtomicU64> {
        self.generation.clone()
    }
    pub fn set_gate(&mut self, gate: Arc<crate::cache::Gate>) {
        self.gate = Some(gate);
    }
    pub fn clear(&self) -> Result<()> {
        self.cancel();
        // Pending already cleared by cancel(); don't hold its lock during
        // filesystem deletion so concurrent schedule() isn't blocked.
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
            // Shutdown does not wait out an artwork download; a store step
            // already handed to the blocking pool completes atomically.
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
        let file = dir.path().join("pending-fixture");
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
