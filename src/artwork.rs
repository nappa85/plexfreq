//! Artwork for the UI without the Plex credential, and the one pipeline that
//! downloads and caches it.
//!
//! QML shows images that are not cached locally through
//! `image://plexfreq/<namespace>/<hex path>` ids. The namespace part (a prefix
//! of the server/credential hash) keeps the same path on another server or
//! account a distinct image in Qt's cache; the path is server-relative. The
//! image provider (C++) resolves an id with [`Source::request`]: the token is
//! sent in a header and never leaves Rust.
//!
//! [`Store`] serves both the offline prefetch queue (`offline::Artwork`) and
//! these requests: a cached file is read from disk, and concurrent loads of
//! one image share a single download. Prefetched artwork lives in the store
//! root; images fetched for the UI while browsing live in `browse/` with
//! their own budget, so browsing never evicts artwork saved for offline use.
use crate::{
    plex::{advertised_path, server_url},
    Error, Result,
};
use std::{
    collections::HashMap,
    fs,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
};
use tokio::sync::{watch, Semaphore};

/// The image provider scheme and name, as registered in the Qt engine.
pub(crate) const SCHEME: &str = "image://plexfreq/";
/// Hex characters of the namespace hash carried in an id.
const NAMESPACE_CHARS: usize = 16;
/// Longest id accepted from the provider.
const ID_LIMIT: usize = 2048;
/// Largest artwork image accepted, downloaded or read from the cache.
pub(crate) const ARTWORK_LIMIT: u64 = 4 * 1024 * 1024;
/// Budget of artwork prefetched for offline use.
const OFFLINE_BUDGET: u64 = 128 * 1024 * 1024;
/// Budget of artwork fetched for the UI while browsing.
const BROWSE_BUDGET: u64 = 64 * 1024 * 1024;
/// Downloads running at once for the UI and the prefetch queue together.
const DOWNLOADS: usize = 4;
/// Reads refresh a file's mtime (the prune age signal) at most this often.
const TOUCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Download the image at server-relative `path`, the credential in a header:
/// image content type only, at most [`ARTWORK_LIMIT`].
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

/// The cache file name of `path` for the account `namespace`.
fn file_name(namespace: &str, path: &str) -> String {
    crate::sha256_hex(&format!("{namespace}\0{path}"))
}

/// A regular, non-empty file (never a symlink).
fn regular(file: &Path) -> bool {
    fs::symlink_metadata(file)
        .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink() && m.len() > 0)
}

/// Whether a UI request is still waiting for its image.
pub(crate) type Interest = Arc<dyn Fn() -> bool + Send + Sync>;

/// Who asks for an image.
#[derive(Clone)]
pub(crate) enum Party {
    /// A UI request, until it is cancelled. Its image is saved for browsing.
    Browsing(Interest),
    /// The offline prefetch queue, for the generation it was queued in. Its
    /// image is saved for offline use while that generation is current.
    Prefetch(u64),
}

/// One download shared by every concurrent load of the same image.
struct Flight {
    /// The answer, once known: `Some(None)` is a failed download. A leader
    /// that ends without answering drops the sender, and followers retry.
    answer: watch::Receiver<Option<Option<Arc<Vec<u8>>>>>,
    parties: Mutex<Vec<Party>>,
}

/// Removes a leader's flight however it ends.
struct Leading<'a> {
    store: &'a Store,
    name: String,
}
impl Drop for Leading<'_> {
    fn drop(&mut self) {
        crate::mutex_lock(&self.store.inflight).remove(&self.name);
    }
}

/// What every store of one cache directory shares: a rebuilt Core opens the
/// same directory while the old one's downloads finish.
#[derive(Default)]
struct Directory {
    /// Serialises pruning, writing, moving and clearing.
    lock: Mutex<()>,
    /// Advanced by clearing: every save that started before is skipped.
    cleared: AtomicU64,
}
fn directory(root: &Path) -> Arc<Directory> {
    static DIRECTORIES: OnceLock<Mutex<HashMap<PathBuf, Arc<Directory>>>> = OnceLock::new();
    crate::mutex_lock(DIRECTORIES.get_or_init(Default::default))
        .entry(root.to_path_buf())
        .or_default()
        .clone()
}

/// The artwork cache directory and its downloads.
pub(crate) struct Store {
    root: PathBuf,
    browse: PathBuf,
    writable: bool,
    directory: Arc<Directory>,
    /// The owner closed the cache: nothing more is written.
    closed: AtomicBool,
    /// Prefetch queue generation: a newer one cancels queued prefetch and its
    /// writes (download policy, network, server changes).
    generation: Arc<AtomicU64>,
    inflight: Mutex<HashMap<String, Arc<Flight>>>,
    downloads: Semaphore,
    /// Offline and browsing budgets.
    budgets: (u64, u64),
}

/// Where a finished download may be saved, decided by its parties when it
/// is saved.
struct Eligible {
    /// Generations of the prefetch jobs that wanted it.
    prefetch: Vec<u64>,
    browsing: bool,
    /// The clear count when the download started.
    cleared: u64,
}

impl Store {
    pub(crate) fn new(root: PathBuf, writable: bool) -> Result<Self> {
        let browse = root.join("browse");
        if writable {
            use std::os::unix::fs::PermissionsExt;
            for dir in [&root, &browse] {
                fs::create_dir_all(dir)?;
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
            }
        }
        Ok(Self {
            directory: directory(&root),
            root,
            browse,
            writable,
            closed: AtomicBool::new(false),
            generation: Arc::default(),
            inflight: Mutex::default(),
            downloads: Semaphore::new(DOWNLOADS),
            budgets: (OFFLINE_BUDGET, BROWSE_BUDGET),
        })
    }
    fn wanted(&self, party: &Party) -> bool {
        match party {
            Party::Browsing(interest) => interest(),
            Party::Prefetch(epoch) => *epoch == self.generation.load(Ordering::SeqCst),
        }
    }
    pub(crate) fn generation(&self) -> &Arc<AtomicU64> {
        &self.generation
    }
    /// The cached file of `path`, prefetched or browsed; a hit counts as a
    /// use for pruning.
    pub(crate) fn local(&self, namespace: &str, path: &str) -> Option<PathBuf> {
        let name = file_name(namespace, path);
        let file = [self.root.join(&name), self.browse.join(&name)]
            .into_iter()
            .find(|file| regular(file))?;
        self.touch(&file);
        Some(file)
    }
    /// Whether `path` is saved for offline use (browsed images are not).
    pub(crate) fn offline(&self, namespace: &str, path: &str) -> bool {
        regular(&self.root.join(file_name(namespace, path)))
    }
    fn touch(&self, file: &Path) {
        if !self.writable {
            return;
        }
        let now = std::time::SystemTime::now();
        let stale = fs::metadata(file)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age >= TOUCH_INTERVAL);
        if stale {
            if let Ok(handle) = fs::File::options()
                .write(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(file)
            {
                let _ = handle.set_modified(now);
            }
        }
    }
    /// Bounded read of a cached image, off the runtime thread.
    async fn read(self: &Arc<Self>, namespace: &str, path: &str) -> Option<Arc<Vec<u8>>> {
        let store = self.clone();
        let (namespace, path) = (namespace.to_owned(), path.to_owned());
        crate::rt::unblock(move || {
            let file = store.local(&namespace, &path)?;
            let mut bytes = Vec::new();
            fs::File::options()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(file)
                .ok()?
                .take(ARTWORK_LIMIT + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            (!bytes.is_empty() && bytes.len() as u64 <= ARTWORK_LIMIT).then(|| Arc::new(bytes))
        })
        .await
    }
    /// The image at `path`: cached, or downloaded once however many loads ask
    /// for it at the same time, then cached for the parties that wanted it.
    pub(crate) async fn load(
        self: &Arc<Self>,
        server: &str,
        token: &str,
        namespace: &str,
        path: &str,
        party: Party,
    ) -> Option<Arc<Vec<u8>>> {
        let name = file_name(namespace, path);
        let cleared = self.directory.cleared.load(Ordering::SeqCst);
        let bytes = self
            .fetch(server, token, namespace, path, &name, &party)
            .await;
        // Wanted for offline use: a browsed copy moves into the offline budget.
        if let (Some(_), Party::Prefetch(epoch)) = (&bytes, &party) {
            let (store, name, epoch) = (self.clone(), name.clone(), *epoch);
            let _ = crate::rt::unblock(move || store.promote(&name, epoch, cleared)).await;
        }
        bytes
    }
    async fn fetch(
        self: &Arc<Self>,
        server: &str,
        token: &str,
        namespace: &str,
        path: &str,
        name: &str,
        party: &Party,
    ) -> Option<Arc<Vec<u8>>> {
        loop {
            if let Some(bytes) = self.read(namespace, path).await {
                return Some(bytes);
            }
            if !self.wanted(party) {
                return None;
            }
            let (flight, answer) = {
                let mut inflight = crate::mutex_lock(&self.inflight);
                match inflight.get(name) {
                    Some(flight) => {
                        crate::mutex_lock(&flight.parties).push(party.clone());
                        (flight.clone(), None)
                    }
                    None => {
                        let (answer, receiver) = watch::channel(None);
                        let flight = Arc::new(Flight {
                            answer: receiver,
                            parties: Mutex::new(vec![party.clone()]),
                        });
                        inflight.insert(name.to_owned(), flight.clone());
                        (flight, Some(answer))
                    }
                }
            };
            let Some(answer) = answer else {
                // Follow the leader; if it ends without an answer, start over.
                let mut receiver = flight.answer.clone();
                loop {
                    if let Some(outcome) = receiver.borrow_and_update().clone() {
                        return outcome;
                    }
                    if receiver.changed().await.is_err() {
                        break;
                    }
                }
                continue;
            };
            let leading = Leading {
                store: self,
                name: name.to_owned(),
            };
            // Another leader may have saved it since the first read.
            if let Some(bytes) = self.read(namespace, path).await {
                let _ = answer.send(Some(Some(bytes.clone())));
                return Some(bytes);
            }
            let _permit = self.downloads.acquire().await.ok()?;
            let wanted = crate::mutex_lock(&flight.parties)
                .iter()
                .any(|party| self.wanted(party));
            if !wanted {
                // Nobody left: no download. Interested late joiners retry.
                return None;
            }
            let cleared = self.directory.cleared.load(Ordering::SeqCst);
            let bytes = fetch_artwork(crate::plex::client(), server, token, path)
                .await
                .ok()
                .map(Arc::new);
            if let Some(bytes) = &bytes {
                let eligible = {
                    let parties = crate::mutex_lock(&flight.parties);
                    Eligible {
                        prefetch: parties
                            .iter()
                            .filter_map(|party| match party {
                                Party::Prefetch(epoch) => Some(*epoch),
                                Party::Browsing(_) => None,
                            })
                            .collect(),
                        browsing: parties
                            .iter()
                            .any(|party| matches!(party, Party::Browsing(_))),
                        cleared,
                    }
                };
                let (store, name, bytes) = (self.clone(), name.to_owned(), bytes.clone());
                let _ = crate::rt::unblock(move || store.save(&name, &bytes, &eligible)).await;
            }
            let _ = answer.send(Some(bytes.clone()));
            drop(leading);
            return bytes;
        }
    }
    /// Whether nothing may be written now (under the directory lock).
    fn frozen(&self, cleared: u64) -> bool {
        !self.writable
            || self.closed.load(Ordering::SeqCst)
            || self.directory.cleared.load(Ordering::SeqCst) != cleared
    }
    /// Save a download: for offline use while a prefetch job that wanted it
    /// is of the current generation, else for browsing when a UI request
    /// wanted it. Pruning and writing are one transaction per directory.
    fn save(&self, name: &str, bytes: &[u8], eligible: &Eligible) -> Result<()> {
        let _lock = crate::mutex_lock(&self.directory.lock);
        if self.frozen(eligible.cleared) {
            return Ok(());
        }
        let current = self.generation.load(Ordering::SeqCst);
        let (dir, budget) = if eligible.prefetch.contains(&current) {
            (&self.root, self.budgets.0)
        } else if eligible.browsing {
            (&self.browse, self.budgets.1)
        } else {
            return Ok(());
        };
        let file = dir.join(name);
        crate::offline::prune_excluding(dir, bytes.len() as u64, budget, Some(&file))?;
        crate::atomic_write_bytes(&file, bytes)
    }
    /// Move a browsed image into the offline budget for a prefetch job of the
    /// current generation that started after the last clear (`cleared`).
    fn promote(&self, name: &str, epoch: u64, cleared: u64) -> Result<()> {
        let _lock = crate::mutex_lock(&self.directory.lock);
        if self.frozen(cleared) || self.generation.load(Ordering::SeqCst) != epoch {
            return Ok(());
        }
        let (from, to) = (self.browse.join(name), self.root.join(name));
        if regular(&to) || !regular(&from) {
            return Ok(());
        }
        let size = fs::metadata(&from)?.len();
        crate::offline::prune_excluding(&self.root, size, self.budgets.0, Some(&to))?;
        fs::rename(from, to)?;
        Ok(())
    }
    /// Delete every cached image; downloads already running are not saved.
    pub(crate) fn clear(&self) -> Result<()> {
        let _lock = crate::mutex_lock(&self.directory.lock);
        self.directory.cleared.fetch_add(1, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
        for dir in [&self.root, &self.browse] {
            let entries = match fs::read_dir(dir) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            for entry in entries {
                let entry = entry?;
                if entry.file_type()?.is_file() {
                    fs::remove_file(entry.path())?;
                }
            }
        }
        Ok(())
    }
    /// The owner closed the cache: nothing more is written.
    pub(crate) fn close(&self) {
        let _lock = crate::mutex_lock(&self.directory.lock);
        self.closed.store(true, Ordering::SeqCst);
    }
}

/// What a request needs: the server, its credential, the cache namespace and
/// the store. Held in Rust only.
#[derive(Clone)]
pub(crate) struct Snapshot {
    pub server: String,
    pub token: String,
    pub namespace: String,
    pub store: Arc<Store>,
}

/// The current artwork source, shared with the image provider. Empty while
/// signed out, offline or once the runtime stopped.
#[derive(Default)]
pub struct Source {
    current: Mutex<Option<Snapshot>>,
}

/// The UI id of server-relative `path` for the account `namespace`.
pub(crate) fn id(namespace: &str, path: &str) -> String {
    let prefix = &namespace[..namespace.len().min(NAMESPACE_CHARS)];
    let hex: String = path.bytes().map(|b| format!("{b:02x}")).collect();
    format!("{SCHEME}{prefix}/{hex}")
}

/// Split an id (with or without the scheme) into namespace prefix and path.
fn parse(id: &str) -> Option<(&str, String)> {
    let id = id.strip_prefix(SCHEME).unwrap_or(id);
    if id.len() > ID_LIMIT {
        return None;
    }
    let (namespace, hex) = id.split_once('/')?;
    if hex.len() % 2 != 0 {
        return None;
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    Some((namespace, String::from_utf8(bytes).ok()?))
}

/// Called once with the image bytes (valid only during the call), or None.
pub type Done = Box<dyn FnOnce(Option<&[u8]>) + Send>;

/// The callback slot of a request: emptied by the answer or by cancelling.
struct Pending(Mutex<Option<Done>>);
impl Pending {
    /// Answer unless cancelled. The callback runs under the slot lock, so a
    /// concurrent cancel waits for it to return.
    fn answer(&self, bytes: Option<&[u8]>) {
        let mut slot = crate::mutex_lock(&self.0);
        if let Some(done) = slot.take() {
            done(bytes);
        }
    }
    fn wanted(&self) -> bool {
        crate::mutex_lock(&self.0).is_some()
    }
}
/// Answers None if the task ends, or is dropped, without an answer.
struct Answer(Arc<Pending>);
impl Drop for Answer {
    fn drop(&mut self) {
        self.0.answer(None);
    }
}

/// An image request of the UI.
pub struct Request {
    pending: Arc<Pending>,
}
impl Request {
    /// After this returns the callback never runs; a callback already
    /// running is waited for.
    pub fn cancel(&self) {
        crate::mutex_lock(&self.pending.0).take();
    }
    /// A request that only answers None (asynchronously, like any other).
    pub(crate) fn failed(done: Done) -> Self {
        Self::spawn(done, |_| async { None })
    }
    fn spawn<F>(done: Done, load: impl FnOnce(Interest) -> F) -> Self
    where
        F: std::future::Future<Output = Option<Arc<Vec<u8>>>> + Send + 'static,
    {
        // First: should the runtime fail to start, nothing has been answered.
        let runtime = crate::rt::handle();
        let pending = Arc::new(Pending(Mutex::new(Some(done))));
        let weak: Weak<Pending> = Arc::downgrade(&pending);
        let interest: Interest = Arc::new(move || weak.upgrade().is_some_and(|p| p.wanted()));
        let answer = Answer(pending.clone());
        let load = load(interest);
        // Detached: the request owns no task handle; the answer guard
        // completes it however the task ends.
        drop(runtime.spawn(async move {
            let bytes = load.await;
            answer.0.answer(bytes.as_deref().map(Vec::as_slice));
        }));
        Self { pending }
    }
}

impl Source {
    pub(crate) fn set(&self, snapshot: Option<Snapshot>) {
        *crate::mutex_lock(&self.current) = snapshot;
    }
    fn matching(&self, namespace: &str) -> Option<Snapshot> {
        crate::mutex_lock(&self.current)
            .as_ref()
            .filter(|s| s.namespace.get(..NAMESPACE_CHARS) == Some(namespace))
            .cloned()
    }
    /// Load the image of `id` and call `done` with it, or with None for an
    /// invalid id, another account or server, no source, or a failed fetch.
    /// `done` is called at most once, possibly before this returns (so its
    /// context must be ready first), from the async runtime thread, or from
    /// whichever thread drops the request's task at process teardown.
    pub fn request(self: &Arc<Self>, id: &str, done: Done) -> Request {
        let source = self.clone();
        let id = id.to_owned();
        Request::spawn(done, move |interest| async move {
            let (namespace, path) = parse(&id)?;
            let snapshot = source.matching(namespace)?;
            let bytes = snapshot
                .store
                .load(
                    &snapshot.server,
                    &snapshot.token,
                    &snapshot.namespace,
                    &path,
                    Party::Browsing(interest),
                )
                .await?;
            // Signed out or switched while loading: discard.
            source.matching(namespace)?;
            Some(bytes)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        net::TcpListener,
        sync::{mpsc, Condvar},
        thread,
        time::Duration,
    };

    /// Serves `body` as an image to every request, recording request heads.
    /// While `held` is set, answers wait until it is cleared.
    struct Server {
        origin: String,
        log: Arc<Mutex<Vec<String>>>,
        held: Arc<(Mutex<bool>, Condvar)>,
    }
    impl Server {
        fn hold(&self, held: bool) {
            *crate::mutex_lock(&self.held.0) = held;
            self.held.1.notify_all();
        }
        fn requests(&self) -> Vec<String> {
            crate::mutex_lock(&self.log).clone()
        }
        fn wait_requests(&self, count: usize) {
            let end = std::time::Instant::now() + Duration::from_secs(10);
            while self.requests().len() < count {
                assert!(std::time::Instant::now() < end, "{:?}", self.requests());
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
    fn server(body: &'static [u8]) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let log = Arc::new(Mutex::new(Vec::new()));
        let held = Arc::new((Mutex::new(false), Condvar::new()));
        let (seen, gate) = (log.clone(), held.clone());
        thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let (seen, gate) = (seen.clone(), gate.clone());
                thread::spawn(move || {
                    let mut head = Vec::new();
                    let mut byte = [0];
                    while !head.ends_with(b"\r\n\r\n") {
                        if !matches!(stream.read(&mut byte), Ok(1)) {
                            return;
                        }
                        head.push(byte[0]);
                    }
                    crate::mutex_lock(&seen).push(String::from_utf8_lossy(&head).to_lowercase());
                    let mut held = crate::mutex_lock(&gate.0);
                    while *held {
                        held = gate.1.wait(held).unwrap_or_else(|p| p.into_inner());
                    }
                    drop(held);
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(body);
                });
            }
        });
        Server { origin, log, held }
    }
    fn snapshot(server: &str, token: &str, store: &Arc<Store>) -> Snapshot {
        Snapshot {
            server: server.into(),
            token: token.into(),
            namespace: crate::cache::namespace(server, token),
            store: store.clone(),
        }
    }
    fn store(root: &Path) -> Arc<Store> {
        Arc::new(Store::new(root.into(), true).unwrap())
    }
    /// Request `id` and wait for its answer.
    fn fetch(source: &Arc<Source>, id: &str) -> Option<Vec<u8>> {
        let (sender, receiver) = mpsc::channel();
        let _request = source.request(
            id,
            Box::new(move |bytes| {
                let _ = sender.send(bytes.map(<[u8]>::to_vec));
            }),
        );
        receiver.recv_timeout(Duration::from_secs(10)).unwrap()
    }
    fn files(dir: &Path) -> usize {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_type().unwrap().is_file())
            .count()
    }

    #[test]
    fn ids_round_trip_and_reject_malformed_input() {
        let id = id(&"ab".repeat(32), "/library/metadata/7/thumb/1?x=1");
        assert!(id.starts_with("image://plexfreq/abababababababab/"));
        assert_eq!(
            parse(&id),
            Some((
                "abababababababab",
                "/library/metadata/7/thumb/1?x=1".to_owned()
            ))
        );
        let bare = id.strip_prefix(SCHEME).unwrap();
        assert!(parse(bare).is_some());
        assert!(parse("abababababababab/2f6").is_none(), "odd hex");
        assert!(parse("abababababababab/zz").is_none(), "not hex");
        assert!(parse("abababababababab/ff").is_none(), "not UTF-8");
        assert!(parse("noslash").is_none());
        assert!(
            parse(&format!("ab/{}", "2f".repeat(ID_LIMIT))).is_none(),
            "oversized"
        );
    }

    /// The credential goes in a header only; the image is saved for
    /// browsing and served from disk next time; another account's or
    /// server's id, or a cleared source, gets nothing.
    #[test]
    fn requests_use_the_current_account_and_cache_what_they_fetch() {
        let server = server(b"PNGDATA");
        let root = tempfile::tempdir().unwrap();
        let store = store(root.path());
        let source = Arc::new(Source::default());
        let path = "/library/metadata/7/thumb/1";
        let current = snapshot(&server.origin, "fixture-secret", &store);
        let image = id(&current.namespace, path);
        assert!(fetch(&source, &image).is_none(), "no source yet");
        source.set(Some(current.clone()));
        assert_eq!(fetch(&source, &image).as_deref(), Some(&b"PNGDATA"[..]));
        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        let line = requests[0].lines().next().unwrap().to_owned();
        assert!(line.starts_with(&format!("get {path} ")), "{line}");
        assert!(!line.contains("fixture-secret"));
        assert!(requests[0].contains("x-plex-token: fixture-secret"));
        assert_eq!(files(&root.path().join("browse")), 1);
        assert_eq!(
            files(root.path()),
            0,
            "browsing never fills the offline budget"
        );
        assert_eq!(fetch(&source, &image).as_deref(), Some(&b"PNGDATA"[..]));
        assert_eq!(server.requests().len(), 1, "served from the cache");
        // Same path, another account: a different id, refused by this source.
        let other = snapshot(&server.origin, "other-token", &store);
        assert_ne!(id(&other.namespace, path), image);
        assert!(fetch(&source, &id(&other.namespace, path)).is_none());
        // A foreign URL is not a server path.
        assert!(fetch(&source, &id(&current.namespace, "http://evil.invalid/x")).is_none());
        source.set(None);
        assert!(fetch(&source, &image).is_none(), "signed out");
    }

    /// Answers come from the runtime, never synchronously, also for an
    /// invalid id or a missing source.
    #[test]
    fn answers_are_asynchronous_and_come_once() {
        let source = Arc::new(Source::default());
        for id in ["not an id", "abababababababab/2f78"] {
            let (sender, receiver) = mpsc::channel();
            let caller = thread::current().id();
            let _request = source.request(
                id,
                Box::new(move |bytes| {
                    let _ = sender.send((bytes.is_none(), thread::current().id() != caller));
                }),
            );
            assert_eq!(
                receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
                (true, true)
            );
            assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
        }
    }

    /// Two requests for one image make one download; a cancelled request
    /// does not stop it for the other.
    #[test]
    fn concurrent_requests_share_one_download() {
        let server = server(b"PNGDATA");
        server.hold(true);
        let root = tempfile::tempdir().unwrap();
        let store = store(root.path());
        let source = Arc::new(Source::default());
        let current = snapshot(&server.origin, "fixture-token", &store);
        let image = id(&current.namespace, "/library/metadata/7/thumb/1");
        source.set(Some(current));
        let (sender, receiver) = mpsc::channel();
        let first = {
            let sender = sender.clone();
            source.request(&image, Box::new(move |_| sender.send("first").unwrap()))
        };
        server.wait_requests(1);
        let second = source.request(
            &image,
            Box::new(move |bytes| {
                assert_eq!(bytes, Some(&b"PNGDATA"[..]));
                sender.send("second").unwrap();
            }),
        );
        first.cancel();
        server.hold(false);
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(10)).unwrap(),
            "second"
        );
        assert!(receiver.recv_timeout(Duration::from_millis(200)).is_err());
        assert_eq!(server.requests().len(), 1);
        drop(second);
    }

    /// A follower whose leader is aborted downloads on its own.
    #[test]
    fn a_follower_takes_over_from_an_aborted_leader() {
        let server = server(b"PNGDATA");
        server.hold(true);
        let root = tempfile::tempdir().unwrap();
        let store = store(root.path());
        let source = Arc::new(Source::default());
        let current = snapshot(&server.origin, "fixture-token", &store);
        let path = "/library/metadata/7/thumb/1";
        let image = id(&current.namespace, path);
        source.set(Some(current.clone()));
        let leader = {
            let store = store.clone();
            crate::rt::spawn(async move {
                store
                    .load(
                        &current.server,
                        &current.token,
                        &current.namespace,
                        path,
                        Party::Prefetch(0),
                    )
                    .await;
            })
        };
        server.wait_requests(1);
        let (sender, receiver) = mpsc::channel();
        let _follower = source.request(
            &image,
            Box::new(move |bytes| sender.send(bytes.map(<[u8]>::to_vec)).unwrap()),
        );
        thread::sleep(Duration::from_millis(100));
        leader.abort();
        leader.join(Some(Duration::from_secs(5)));
        server.wait_requests(2);
        server.hold(false);
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .as_deref(),
            Some(&b"PNGDATA"[..])
        );
    }

    /// A request cancelled while waiting for a download slot downloads
    /// nothing, and its callback never runs.
    #[test]
    fn a_request_cancelled_while_queued_downloads_nothing() {
        let server = server(b"PNGDATA");
        server.hold(true);
        let root = tempfile::tempdir().unwrap();
        let store = store(root.path());
        let source = Arc::new(Source::default());
        let current = snapshot(&server.origin, "fixture-token", &store);
        source.set(Some(current.clone()));
        let busy: Vec<_> = (0..DOWNLOADS)
            .map(|n| {
                source.request(
                    &id(&current.namespace, &format!("/library/metadata/{n}/thumb")),
                    Box::new(|_| {}),
                )
            })
            .collect();
        server.wait_requests(DOWNLOADS);
        let called = Arc::new(AtomicBool::new(false));
        let flag = called.clone();
        let queued = source.request(
            &id(&current.namespace, "/library/metadata/99/thumb"),
            Box::new(move |_| flag.store(true, Ordering::SeqCst)),
        );
        thread::sleep(Duration::from_millis(100));
        queued.cancel();
        server.hold(false);
        thread::sleep(Duration::from_millis(500));
        assert_eq!(
            server.requests().len(),
            DOWNLOADS,
            "{:?}",
            server.requests()
        );
        assert!(!called.load(Ordering::SeqCst));
        drop(busy);
    }

    /// Callbacks never run after `cancel` returns, and run exactly once for
    /// requests never cancelled.
    #[test]
    fn cancel_and_completion_never_overlap() {
        let server = server(b"PNGDATA");
        let root = tempfile::tempdir().unwrap();
        let store = store(root.path());
        let source = Arc::new(Source::default());
        let current = snapshot(&server.origin, "fixture-token", &store);
        let image = id(&current.namespace, "/library/metadata/7/thumb/1");
        source.set(Some(current));
        assert!(fetch(&source, &image).is_some());
        let mut kept = Vec::new();
        for n in 0..300 {
            let cancelled = Arc::new(AtomicBool::new(false));
            let calls = Arc::new(AtomicU64::new(0));
            let (seen, count) = (cancelled.clone(), calls.clone());
            let request = source.request(
                &image,
                Box::new(move |_| {
                    assert!(!seen.load(Ordering::SeqCst), "answered after cancel");
                    count.fetch_add(1, Ordering::SeqCst);
                }),
            );
            if n % 2 == 0 {
                if n % 4 == 0 {
                    thread::yield_now();
                }
                request.cancel();
                cancelled.store(true, Ordering::SeqCst);
            } else {
                kept.push((request, calls));
            }
        }
        let end = std::time::Instant::now() + Duration::from_secs(10);
        for (_, calls) in &kept {
            while calls.load(Ordering::SeqCst) == 0 {
                assert!(std::time::Instant::now() < end);
                thread::sleep(Duration::from_millis(1));
            }
        }
        thread::sleep(Duration::from_millis(100));
        assert!(kept
            .iter()
            .all(|(_, calls)| calls.load(Ordering::SeqCst) == 1));
    }

    /// Saves for browsing stay within their budget however many run at once;
    /// clearing the cache (through any store of the directory) stops a running
    /// download from being saved, a new prefetch generation does not stop a
    /// browsing save, and a closed store saves nothing more.
    #[test]
    fn saves_respect_the_budget_clearing_and_generations() {
        let root = tempfile::tempdir().unwrap();
        let mut bare = Store::new(root.path().into(), true).unwrap();
        bare.budgets = (4 * 1024, 4 * 1024);
        let store = Arc::new(bare);
        let image = vec![7u8; 1024];
        let browsing = |cleared| Eligible {
            prefetch: Vec::new(),
            browsing: true,
            cleared,
        };
        let writers: Vec<_> = (0..16)
            .map(|n| {
                let (store, image) = (store.clone(), image.clone());
                thread::spawn(move || {
                    store
                        .save(&format!("{n:02}"), &image, &browsing(0))
                        .unwrap()
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        let browse = root.path().join("browse");
        let used: u64 = fs::read_dir(&browse)
            .unwrap()
            .flatten()
            .map(|e| e.metadata().unwrap().len())
            .sum();
        assert!(used <= 4 * 1024, "{used}");
        // A new prefetch generation (download policy) leaves browsing saves.
        store.generation.fetch_add(1, Ordering::SeqCst);
        store.save("kept", &image, &browsing(0)).unwrap();
        assert!(regular(&browse.join("kept")));
        // Only a prefetch job of the current generation saves for offline use.
        let prefetch = |epochs: Vec<u64>| Eligible {
            prefetch: epochs,
            browsing: false,
            cleared: 0,
        };
        store.save("stale", &image, &prefetch(vec![0])).unwrap();
        assert!(!regular(&root.path().join("stale")));
        store
            .save("current", &image, &prefetch(vec![0, 1]))
            .unwrap();
        assert!(regular(&root.path().join("current")));
        // Promotion is for the current generation only.
        fs::write(browse.join("browsed"), b"PNG").unwrap();
        store.promote("browsed", 0, 0).unwrap();
        assert!(!regular(&root.path().join("browsed")));
        store.promote("browsed", 1, 0).unwrap();
        assert!(regular(&root.path().join("browsed")));
        // Clearing through another store of the directory (a rebuilt Core)
        // drops what this one started before it.
        Store::new(root.path().into(), true)
            .unwrap()
            .clear()
            .unwrap();
        store.save("late", &image, &browsing(0)).unwrap();
        assert_eq!(files(&browse) + files(root.path()), 0);
        // A prefetch that started before that clear promotes nothing.
        fs::write(browse.join("browsed"), b"PNG").unwrap();
        store.promote("browsed", 1, 0).unwrap();
        assert_eq!(files(root.path()), 0);
        fs::remove_file(browse.join("browsed")).unwrap();
        store.close();
        store.save("closed", &image, &browsing(1)).unwrap();
        fs::write(browse.join("browsed"), b"PNG").unwrap();
        store.promote("browsed", 1, 0).unwrap();
        assert_eq!(files(root.path()), 0);
        assert_eq!(files(&browse), 1);
    }

    /// A prefetch job that joins a download a UI request started gets the
    /// image saved for offline use.
    #[test]
    fn a_prefetch_joining_a_browsing_download_keeps_it_offline() {
        let server = server(b"PNGDATA");
        server.hold(true);
        let root = tempfile::tempdir().unwrap();
        let store = store(root.path());
        let source = Arc::new(Source::default());
        let current = snapshot(&server.origin, "fixture-token", &store);
        let path = "/library/metadata/7/thumb/1";
        source.set(Some(current.clone()));
        let (sender, receiver) = mpsc::channel();
        let _request = source.request(
            &id(&current.namespace, path),
            Box::new(move |bytes| sender.send(bytes.is_some()).unwrap()),
        );
        server.wait_requests(1);
        let prefetch = {
            let store = store.clone();
            thread::spawn(move || {
                crate::rt::block_on(store.load(
                    &current.server,
                    &current.token,
                    &current.namespace,
                    path,
                    Party::Prefetch(0),
                ))
            })
        };
        thread::sleep(Duration::from_millis(100));
        server.hold(false);
        assert!(receiver.recv_timeout(Duration::from_secs(10)).unwrap());
        assert!(prefetch.join().unwrap().is_some());
        assert_eq!(server.requests().len(), 1);
        assert!(store.offline(
            &crate::cache::namespace(&server.origin, "fixture-token"),
            path
        ));
    }

    /// A cached image that is shown again counts as recently used, and a
    /// prefetch of a browsed image moves it into the offline budget.
    #[test]
    fn shown_images_stay_recent_and_prefetch_promotes_browsed_ones() {
        let root = tempfile::tempdir().unwrap();
        let store = store(root.path());
        let (namespace, path) = ("ns", "/library/metadata/7/thumb");
        let name = file_name(namespace, path);
        let browsed = root.path().join("browse").join(&name);
        fs::write(&browsed, b"PNGDATA").unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(3 * 3600);
        fs::File::options()
            .write(true)
            .open(&browsed)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert_eq!(store.local(namespace, path), Some(browsed.clone()));
        assert!(fs::metadata(&browsed).unwrap().modified().unwrap() > old);
        assert!(!store.offline(namespace, path));
        let loaded = crate::rt::block_on(store.load(
            "http://127.0.0.1:9/",
            "fixture-token",
            namespace,
            path,
            Party::Prefetch(0),
        ));
        assert_eq!(loaded.as_deref().map(Vec::as_slice), Some(&b"PNGDATA"[..]));
        assert!(store.offline(namespace, path));
        assert!(!browsed.exists());
    }

    /// No status, queue or item JSON carries the token: images are ids.
    #[test]
    fn the_token_never_reaches_ui_json() {
        let dir = tempfile::tempdir().unwrap();
        let item = crate::model::Item {
            rating_key: "7".into(),
            kind: crate::model::ItemKind::Track,
            title: "Track".into(),
            thumb: "/library/metadata/7/thumb/1".into(),
            parent_thumb: "/library/metadata/6/thumb/1".into(),
            art: "/library/metadata/5/art/1".into(),
            ..Default::default()
        };
        let mut settings = crate::store::Settings {
            client_identifier: "fixture-client".into(),
            server_url: "http://127.0.0.1:9/".into(),
            server_token: "fixture-secret-token".into(),
            ..Default::default()
        };
        settings
            .playback
            .queue
            .replace(vec![item.clone()], 0)
            .unwrap();
        crate::store::save(dir.path(), &settings).unwrap();
        let mut core = crate::Core::new(dir.path().into()).unwrap();
        let status = core.execute(crate::Command::Status).unwrap();
        let shown = core.display_item_for_tests(&item);
        for value in [&status, &shown] {
            assert!(
                !value.to_string().contains("fixture-secret-token"),
                "{value}"
            );
        }
        assert!(shown["artwork"].as_str().unwrap().starts_with(SCHEME));
        assert!(shown["albumArtwork"].as_str().unwrap().starts_with(SCHEME));
        assert!(shown["background"].as_str().unwrap().starts_with(SCHEME));
        assert_eq!(shown["photos"].as_array().unwrap().len(), 2);
    }

    /// A provider that outlives the runtime cannot fetch with the credential.
    #[test]
    fn a_retained_source_stops_when_the_runtime_stops() {
        let origin = server(b"PNGDATA").origin;
        let dir = tempfile::tempdir().unwrap();
        crate::store::save(
            dir.path(),
            &crate::store::Settings {
                client_identifier: "fixture-client".into(),
                server_url: origin.clone(),
                server_token: "fixture-token".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let runtime =
            crate::runtime::Runtime::spawn(dir.path().into(), crate::audio::Sink::Fake, false)
                .unwrap();
        let source = runtime.artwork();
        let image = id(
            &crate::cache::namespace(&origin, "fixture-token"),
            "/library/metadata/7/thumb/1",
        );
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while fetch(&source, &image).is_none() {
            assert!(std::time::Instant::now() < end, "the source was never set");
            thread::sleep(std::time::Duration::from_millis(20));
        }
        drop(runtime);
        assert!(fetch(&source, &image).is_none());
    }
}
