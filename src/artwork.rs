//! Artwork for the UI without the Plex credential.
//!
//! QML shows images that are not cached locally through
//! `image://plexfreq/<namespace>/<hex path>` ids. The namespace part (a prefix
//! of the server/credential hash) keeps the same path on another server or
//! account a distinct image in Qt's cache; the path is server-relative. The
//! image provider (C++) resolves an id with [`Source::fetch`]: the token is
//! sent in a header and never leaves Rust.
use std::{path::PathBuf, sync::Mutex};

/// The image provider scheme and name, as registered in the Qt engine.
pub(crate) const SCHEME: &str = "image://plexfreq/";
/// Hex characters of the namespace hash carried in an id.
const NAMESPACE_CHARS: usize = 16;
/// Longest id accepted from the provider.
const ID_LIMIT: usize = 2048;

/// What a fetch needs: the server, its credential, the cache namespace and
/// the artwork cache directory. Held in Rust only.
#[derive(Clone)]
pub(crate) struct Snapshot {
    pub server: String,
    pub token: String,
    pub namespace: String,
    pub root: PathBuf,
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
    /// The image of `id`: the cached file, else downloaded. None for an
    /// invalid id, another account or server, no source, or a failed fetch.
    /// Blocks: call from a loader thread, never the GUI or I/O runtime.
    pub fn fetch(&self, id: &str) -> Option<Vec<u8>> {
        let (namespace, path) = parse(id)?;
        let source = self.matching(namespace)?;
        if let Some(file) = crate::offline::local_artwork(&source.root, &source.namespace, &path) {
            // Bounded read: an oversized (or growing) file is never loaded whole.
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(file)
                .ok()?
                .take(crate::offline::ARTWORK_LIMIT + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            return (bytes.len() as u64 <= crate::offline::ARTWORK_LIMIT).then_some(bytes);
        }
        let bytes = crate::rt::block_on(crate::offline::fetch_artwork(
            crate::plex::client(),
            &source.server,
            &source.token,
            &path,
        ))
        .ok()?;
        // Signed out or switched while downloading: discard.
        self.matching(namespace)?;
        Some(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
    };

    /// Serves `body` as an image to every request, recording request heads.
    fn server(body: &'static [u8]) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let log = Arc::new(Mutex::new(Vec::new()));
        let seen = log.clone();
        thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut head = Vec::new();
                let mut byte = [0];
                while !head.ends_with(b"\r\n\r\n") {
                    if !matches!(stream.read(&mut byte), Ok(1)) {
                        break;
                    }
                    head.push(byte[0]);
                }
                crate::mutex_lock(&seen).push(String::from_utf8_lossy(&head).to_lowercase());
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(body);
            }
        });
        (origin, log)
    }
    fn snapshot(server: &str, token: &str, root: &std::path::Path) -> Snapshot {
        Snapshot {
            server: server.into(),
            token: token.into(),
            namespace: crate::cache::namespace(server, token),
            root: root.into(),
        }
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

    /// The credential goes in a header only; another account's or server's
    /// id, or a cleared source, fetches nothing; a cached file wins.
    #[test]
    fn fetch_uses_the_current_account_and_never_puts_the_token_in_the_url() {
        let (origin, log) = server(b"PNGDATA");
        let root = tempfile::tempdir().unwrap();
        let source = Source::default();
        let path = "/library/metadata/7/thumb/1";
        let current = snapshot(&origin, "fixture-secret", root.path());
        let image = id(&current.namespace, path);
        assert!(source.fetch(&image).is_none(), "no source yet");
        source.set(Some(current.clone()));
        assert_eq!(source.fetch(&image).as_deref(), Some(&b"PNGDATA"[..]));
        let requests = crate::mutex_lock(&log).clone();
        assert_eq!(requests.len(), 1);
        let line = requests[0].lines().next().unwrap().to_owned();
        assert!(line.starts_with(&format!("get {path} ")), "{line}");
        assert!(!line.contains("fixture-secret"));
        assert!(requests[0].contains("x-plex-token: fixture-secret"));
        // Same path, another account: a different id, refused by this source.
        let other = snapshot(&origin, "other-token", root.path());
        assert_ne!(id(&other.namespace, path), image);
        assert!(source.fetch(&id(&other.namespace, path)).is_none());
        // A foreign URL is not a server path.
        assert!(source
            .fetch(&id(&current.namespace, "http://evil.invalid/x"))
            .is_none());
        // The cached file is served without the network.
        use sha2::Digest;
        let file = root.path().join(format!(
            "{:x}",
            sha2::Sha256::digest(format!("{}\0{path}", current.namespace).as_bytes())
        ));
        std::fs::write(file, b"CACHED").unwrap();
        assert_eq!(source.fetch(&image).as_deref(), Some(&b"CACHED"[..]));
        assert_eq!(crate::mutex_lock(&log).len(), 1);
        source.set(None);
        assert!(source.fetch(&image).is_none(), "signed out");
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
        let (origin, _log) = server(b"PNGDATA");
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
        while source.fetch(&image).is_none() {
            assert!(std::time::Instant::now() < end, "the source was never set");
            thread::sleep(std::time::Duration::from_millis(20));
        }
        drop(runtime);
        assert!(source.fetch(&image).is_none());
    }
}
