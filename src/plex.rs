use crate::{
    model::{Container, Envelope, Pin, Resource},
    Error, Result,
};
use reqwest::{
    header::{HeaderMap, HeaderValue},
    Client, Method, RequestBuilder, Response,
};
use serde::de::DeserializeOwned;
use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
    time::Duration,
};
use tokio::sync::watch;
use url::Url;

pub(crate) const JSON_LIMIT: usize = 8 * 1024 * 1024;
/// Outcome of a view-bound request aborted because the user navigated away.
/// Callers never present it: the runtime turns it into a discarded reply.
pub(crate) const CANCELLED: Error = Error::Input(CANCELLED_MESSAGE);
const CANCELLED_MESSAGE: &str = "Request cancelled by navigation";

pub(crate) fn audio_mime(mime: &str) -> bool {
    audio_mime_type(mime).is_some()
}

/// Normalized essence (lowercase, parameters stripped) of an acceptable
/// audio Content-Type, or None for non-audio content.
fn audio_mime_type(mime: &str) -> Option<String> {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    (mime.is_empty()
        || mime.starts_with("audio/")
        || matches!(
            mime.as_str(),
            "application/octet-stream" | "application/binary" | "video/mp4"
        ))
    .then_some(mime)
}

fn status(code: u16) -> Result<()> {
    match code {
        200..=299 => Ok(()),
        401 | 403 => Err(Error::Unauthorized),
        code => Err(Error::Http(code)),
    }
}

/// Read at most `limit` body bytes; `Err(None)` when the body exceeds it.
pub(crate) async fn bounded_body(
    response: &mut Response,
    limit: usize,
) -> std::result::Result<Vec<u8>, Option<Error>> {
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(None);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Some(Error::Network))? {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(None);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Server-side health of one queued item, from a status-only metadata probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Probe {
    /// The server itself reports the item missing.
    Gone,
    /// The credential was refused; further probes would fail the same way.
    Auth,
    /// Anything else, including success: nothing is concluded.
    Unknown,
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Classify a metadata probe. PMS answers a missing item with 404 plus its
/// `X-Plex-Protocol` header (HTML body, no MediaContainer: measured
/// 2026-10-18); a 404 without that header may come from a proxy or another
/// host and proves nothing about the item.
pub(crate) fn classify_probe(status: u16, plex_header: bool) -> Probe {
    match status {
        404 if plex_header => Probe::Gone,
        401 | 403 => Probe::Auth,
        _ => Probe::Unknown,
    }
}

pub(crate) async fn bounded_json<T: DeserializeOwned>(
    mut response: reqwest::Response,
    context: &'static str,
) -> Result<T> {
    let bytes = bounded_body(&mut response, JSON_LIMIT)
        .await
        .map_err(|error| error.unwrap_or(Error::ProtocolAt(context)))?;
    serde_json::from_slice(&bytes).map_err(|_| Error::ProtocolAt(context))
}

/// One pooled async client for every Plex API caller in the process (Core,
/// plan refresh, artwork), so keep-alive connections to the server are reused.
pub(crate) fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        // `timeout` bounds the whole small-JSON request (headers + body),
        // so a stalled timeline/library call cannot block the worker beyond
        // 15s. `connect_timeout` fails fast on unreachable hosts. Redirects
        // stay disabled: credentials travel in headers.
        Client::builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default()
    })
}

/// Navigation generation a view-bound request belongs to.
type Scope = (watch::Receiver<u64>, u64);

pub struct Plex {
    client: Client,
    headers: HeaderMap,
    pub account_url: Url,
    scope: Mutex<Option<Scope>>,
    cancelled: AtomicBool,
}

impl Plex {
    pub fn new(identifier: &str, account_url: Url) -> Result<Self> {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("accept", "application/json"),
            ("x-plex-product", "PlexFreq"),
            ("x-plex-version", env!("CARGO_PKG_VERSION")),
            ("x-plex-platform", "Linux"),
            ("x-plex-client-identifier", identifier),
        ] {
            headers.insert(
                name,
                HeaderValue::from_str(value)
                    .map_err(|_| Error::Input("Invalid client identifier"))?,
            );
        }
        Ok(Self {
            client: client().clone(),
            headers,
            account_url,
            scope: Mutex::new(None),
            cancelled: AtomicBool::new(false),
        })
    }

    /// Bind following requests to a navigation generation: once `navigation`
    /// moves past `view`, in-flight and later requests end with [`CANCELLED`].
    /// `None` unbinds. Used by the runtime around view-bound commands.
    pub(crate) fn bind_view(&self, scope: Option<Scope>) {
        *crate::mutex_lock(&self.scope) = scope;
        self.cancelled.store(false, Ordering::SeqCst);
    }
    /// Whether a request was cancelled by navigation since `bind_view`.
    pub(crate) fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Drive one request on the shared runtime from the synchronous Core.
    fn run<T: Send>(&self, step: impl Future<Output = Result<T>> + Send) -> Result<T> {
        let scope = crate::mutex_lock(&self.scope).clone();
        let Some((mut navigation, view)) = scope else {
            return crate::rt::block_on(step);
        };
        if *navigation.borrow() != view {
            self.cancelled.store(true, Ordering::SeqCst);
            return Err(CANCELLED);
        }
        let result = crate::rt::block_on(async move {
            tokio::select! {
                result = step => result,
                stale = async { navigation.wait_for(|current| *current != view).await.is_ok() } => {
                    if stale {
                        Err(CANCELLED)
                    } else {
                        // The navigation owner is gone: nothing can go stale.
                        std::future::pending().await
                    }
                }
            }
        });
        if matches!(result, Err(Error::Input(message)) if message == CANCELLED_MESSAGE) {
            self.cancelled.store(true, Ordering::SeqCst);
        }
        result
    }

    /// Every server/account request goes through here so the credential
    /// header is attached in one place (never in the URL or logs).
    fn builder(&self, method: Method, url: Url, token: &str) -> RequestBuilder {
        let builder = self
            .client
            .request(method, url)
            .headers(self.headers.clone());
        if token.is_empty() {
            builder
        } else {
            builder.header("X-Plex-Token", token)
        }
    }

    fn send(&self, method: Method, url: Url, token: &str) -> Result<()> {
        let request = self.builder(method, url, token);
        self.run(async move {
            let response = request.send().await.map_err(|_| Error::Network)?;
            status(response.status().as_u16())
        })
    }

    fn request<T: DeserializeOwned + Send>(
        &self,
        method: Method,
        url: Url,
        token: &str,
        form: &[(&str, &str)],
        context: &'static str,
    ) -> Result<T> {
        let mut request = self.builder(method, url, token);
        if !form.is_empty() {
            request = request.form(form);
        }
        // Only the transfer runs inside the future; decoding stays on the
        // calling thread after it.
        let bytes = self.run(async move {
            let mut response = request.send().await.map_err(|_| Error::Network)?;
            status(response.status().as_u16())?;
            bounded_body(&mut response, JSON_LIMIT)
                .await
                .map_err(|error| error.unwrap_or(Error::ProtocolAt(context)))
        })?;
        serde_json::from_slice(&bytes).map_err(|_| Error::ProtocolAt(context))
    }

    pub fn pin(&self) -> Result<Pin> {
        self.request(
            Method::POST,
            self.account_url.join("api/v2/pins")?,
            "",
            &[("strong", "true")],
            "browser sign-in creation",
        )
    }
    pub fn poll(&self, id: u64, code: &str) -> Result<Pin> {
        let mut url = self.account_url.join(&format!("api/v2/pins/{id}"))?;
        url.query_pairs_mut().append_pair("code", code);
        self.request(Method::GET, url, "", &[], "browser sign-in polling")
    }
    pub fn resources(&self, token: &str) -> Result<Vec<Resource>> {
        let mut url = self.account_url.join("api/v2/resources")?;
        url.query_pairs_mut()
            .append_pair("includeHttps", "1")
            .append_pair("includeRelay", "1");
        // The account endpoint mixes servers and players. Players legitimately
        // have absent/null accessToken, so validate server fields only after
        // selecting the resources which advertise the server capability.
        // A single malformed server entry must not poison discovery of the
        // remaining servers; it is skipped. If server entries existed but none
        // decoded, report a protocol error instead of silently claiming
        // "no servers".
        let resources: Vec<serde_json::Value> =
            self.request(Method::GET, url, token, &[], "server discovery")?;
        let servers: Vec<serde_json::Value> = resources
            .into_iter()
            .filter(|r| {
                r.get("provides")
                    .and_then(|p| p.as_str())
                    .is_some_and(|p| p.split(',').any(|capability| capability.trim() == "server"))
            })
            .collect();
        let valid: Vec<Resource> = servers
            .iter()
            .filter_map(|r| serde_json::from_value::<Resource>(r.clone()).ok())
            .filter(|resource| !resource.access_token.is_empty())
            .collect();
        if valid.is_empty() && !servers.is_empty() {
            return Err(Error::ProtocolAt("server discovery"));
        }
        Ok(valid)
    }
    pub fn container(
        &self,
        base: &Url,
        token: &str,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<Container> {
        let mut url = server_path(base, path)?;
        url.query_pairs_mut()
            .extend_pairs(params.iter().map(|(k, v)| (*k, v.as_str())));
        let envelope: Envelope =
            self.request(Method::GET, url, token, &[], "server library request")?;
        Ok(envelope.container)
    }

    pub fn create_radio_queue(&self, base: &Url, token: &str, uri: &str) -> Result<Container> {
        let mut url = server_path(base, "/playQueues")?;
        url.query_pairs_mut().extend_pairs([
            ("type", "audio"),
            ("uri", uri),
            ("continuous", "1"),
            ("includeRelated", "1"),
        ]);
        let envelope: Envelope =
            self.request(Method::POST, url, token, &[], "radio queue creation")?;
        Ok(envelope.container)
    }
    pub fn mutate(
        &self,
        base: &Url,
        token: &str,
        method: Method,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<()> {
        let mut url = server_path(base, path)?;
        url.query_pairs_mut()
            .extend_pairs(params.iter().map(|(k, v)| (*k, v.as_str())));
        self.send(method, url, token).map(drop)
    }
    pub fn create_playlist(
        &self,
        base: &Url,
        token: &str,
        params: &[(&str, String)],
    ) -> Result<Container> {
        let mut url = server_path(base, "/playlists")?;
        url.query_pairs_mut()
            .extend_pairs(params.iter().map(|(k, v)| (*k, v.as_str())));
        let envelope: Envelope =
            self.request(Method::POST, url, token, &[], "playlist creation")?;
        Ok(envelope.container)
    }

    pub fn timeline(&self, base: &Url, token: &str, params: &[(&str, String)]) -> Result<()> {
        self.mutate(base, token, Method::POST, "/:/timeline", params)
    }
    pub fn rate(&self, base: &Url, token: &str, key: &str, rating: u8) -> Result<()> {
        self.mutate(
            base,
            token,
            Method::PUT,
            "/:/rate",
            &[
                ("key", key.to_owned()),
                ("identifier", "com.plexapp.plugins.library".to_owned()),
                ("rating", rating.to_string()),
            ],
        )
    }
    pub fn text(&self, base: &Url, token: &str, path: &str) -> Result<String> {
        let request = self.builder(Method::GET, server_path(base, path)?, token);
        let bytes = self.run(async move {
            let mut response = request.send().await.map_err(|_| Error::Network)?;
            status(response.status().as_u16())?;
            bounded_body(&mut response, 512 * 1024)
                .await
                .map_err(|error| error.unwrap_or(Error::Input("Lyrics exceed the size limit")))
        })?;
        String::from_utf8(bytes).map_err(|_| Error::ProtocolAt("lyrics text"))
    }
    /// A status-only probe of `/library/metadata/{key}`, as an owned future
    /// for a background task: it never binds a view scope or blocks the
    /// caller, sends the credential as a header and discards the body.
    pub(crate) fn probe_metadata(
        &self,
        base: &Url,
        token: &str,
        key: &str,
    ) -> Result<impl Future<Output = Probe> + Send + 'static> {
        crate::numeric(key)?;
        let url = server_path(base, &format!("/library/metadata/{key}"))?;
        let request = self.builder(Method::GET, url, token);
        Ok(async move {
            match tokio::time::timeout(PROBE_TIMEOUT, request.send()).await {
                Ok(Ok(response)) => classify_probe(
                    response.status().as_u16(),
                    response.headers().contains_key("x-plex-protocol"),
                ),
                _ => Probe::Unknown,
            }
        })
    }

    pub(crate) fn probe_audio(&self, url: Url, token: &str) -> Result<(usize, String)> {
        // `headers` replaces the default JSON Accept instead of appending.
        let request = self
            .builder(Method::GET, url, token)
            .headers(HeaderMap::from_iter([
                (reqwest::header::ACCEPT, HeaderValue::from_static("*/*")),
                (
                    reqwest::header::ACCEPT_ENCODING,
                    HeaderValue::from_static("identity"),
                ),
            ]));
        self.run(async move {
            let mut response = request.send().await.map_err(|_| Error::Network)?;
            if !response.status().is_success() {
                return Err(Error::Http(response.status().as_u16()));
            }
            let mime = audio_mime_type(
                response
                    .headers()
                    .get("Content-Type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or(""),
            )
            .ok_or(Error::Input(
                "Server returned non-audio content for this track",
            ))?;
            // A bounded prefix proves the media is readable; the rest of the
            // body is abandoned with the response.
            let mut read = 0usize;
            while read < 256 * 1024 {
                match response.chunk().await {
                    Ok(Some(chunk)) => read += chunk.len(),
                    Ok(None) => break,
                    Err(error) => return Err(std::io::Error::other(error).into()),
                }
            }
            if read == 0 {
                return Err(Error::Input(
                    "Audio download is incomplete; waiting to resume",
                ));
            }
            Ok((read.min(256 * 1024), mime))
        })
    }
}

pub fn server_url(value: &str) -> Result<Url> {
    let url = Url::parse(value)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(Error::Input(
            "Server URL must be an HTTP(S) origin, e.g. http://192.168.1.10:32400",
        ));
    }
    Ok(url)
}

pub fn server_path(base: &Url, path: &str) -> Result<Url> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.contains('\\')
        || path.contains(['?', '#'])
        || path.chars().any(char::is_control)
    {
        return Err(Error::Input("Expected a server-relative Plex path"));
    }
    // Block dot segments before Url::join normalizes them away: /a/../b would
    // otherwise silently become /b under the same origin.
    // Encoded separators and dots can be normalized differently by proxies/PMS.
    // Reject these before URL parsing, including double-encoded variants.
    let lower = path.to_ascii_lowercase();
    if ["%2e", "%2f", "%5c", "%25", "%3f", "%23", "%00"]
        .iter()
        .any(|escape| lower.contains(escape))
        || path
            .split('/')
            .any(|segment| segment == ".." || segment == ".")
    {
        return Err(Error::Input("Expected a server-relative Plex path"));
    }
    let url = base.join(path)?;
    if url.origin() != base.origin() {
        return Err(Error::Input("Cross-origin Plex path rejected"));
    }
    Ok(url)
}

pub fn authenticated_url(base: &Url, path: &str, token: &str) -> Result<String> {
    let mut url = advertised_path(base, path)?;
    url.query_pairs_mut().append_pair("X-Plex-Token", token);
    Ok(url.into())
}

/// Station and artwork keys may advertise queries. Keep credential/header
/// parameters out of that input and validate the pathname before joining.
pub(crate) fn advertised_path(base: &Url, key: &str) -> Result<Url> {
    let (path, query) = key
        .split_once('?')
        .map_or((key, None), |(p, q)| (p, Some(q)));
    let mut url = server_path(base, path)?;
    if let Some(query) = query {
        if query.contains('#') || query.chars().any(char::is_control) {
            return Err(Error::Input("Expected a server-relative Plex path"));
        }
        for (name, _) in url::form_urlencoded::parse(query.as_bytes()) {
            if name.to_ascii_lowercase().starts_with("x-plex-") {
                return Err(Error::Input("Expected a server-relative Plex path"));
            }
        }
        url.set_query(Some(query));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    #[test]
    fn json_body_limit_checks_streamed_bytes_and_declared_length() {
        for declared in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut byte = [0];
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                if declared {
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        JSON_LIMIT + 1
                    )
                    .unwrap();
                } else {
                    stream
                        .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n")
                        .unwrap();
                    // An otherwise valid large JSON string with no length header.
                    let _ = stream.write_all(b"\"");
                    let _ = stream.write_all(&vec![b'a'; JSON_LIMIT]);
                    let _ = stream.write_all(b"\"");
                }
            });
            let plex = Plex::new("fixture", base.clone()).unwrap();
            assert!(matches!(
                plex.request::<serde_json::Value>(Method::GET, base, "", &[], "fixture"),
                Err(Error::ProtocolAt(context)) if context == "fixture"
            ));
            server.join().unwrap();
        }
    }

    #[test]
    fn metadata_probe_is_gone_only_for_a_plex_404() {
        assert_eq!(classify_probe(404, true), Probe::Gone);
        assert_eq!(classify_probe(404, false), Probe::Unknown);
        assert_eq!(classify_probe(401, true), Probe::Auth);
        assert_eq!(classify_probe(403, false), Probe::Auth);
        for status in [200, 206, 400, 500, 503] {
            assert_eq!(classify_probe(status, true), Probe::Unknown);
        }
    }

    #[test]
    fn metadata_probe_sends_the_token_as_a_header_and_reads_the_status() {
        for (reply, expected) in [
            (
                "HTTP/1.1 404 Not Found\r\nX-Plex-Protocol: 1.0\r\nContent-Type: text/html\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                Probe::Gone,
            ),
            (
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                Probe::Unknown,
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut byte = [0];
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                stream.write_all(reply.as_bytes()).unwrap();
                String::from_utf8(request).unwrap()
            });
            let plex = Plex::new("fixture", base.clone()).unwrap();
            let probe = plex.probe_metadata(&base, "synthetic-secret", "42").unwrap();
            assert_eq!(crate::rt::block_on(probe), expected);
            let request = server.join().unwrap();
            assert!(request.starts_with("GET /library/metadata/42 "), "{request}");
            assert!(request.to_ascii_lowercase().contains("x-plex-token: synthetic-secret"));
            assert!(!request.lines().next().unwrap().contains("synthetic-secret"));
        }
        let plex = Plex::new("fixture", Url::parse("http://127.0.0.1/").unwrap()).unwrap();
        assert!(plex
            .probe_metadata(&Url::parse("http://127.0.0.1/").unwrap(), "", "../1")
            .is_err());
    }

    #[test]
    fn advertised_queries_cannot_supply_plex_credentials() {
        let base = Url::parse("https://fixture.invalid/").unwrap();
        assert!(advertised_path(&base, "/photo/:/transcode?url=test").is_ok());
        for query in [
            "X-Plex-Token=evil",
            "x%2dplex%2dtoken=evil",
            "url=test#fragment",
        ] {
            assert!(advertised_path(&base, &format!("/photo/:/transcode?{query}")).is_err());
        }
    }
}
