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

pub(crate) fn status(code: u16) -> Result<()> {
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
pub enum Probe {
    /// The server itself reports the item missing.
    Gone,
    /// The credential was refused; further probes would fail the same way.
    Auth,
    /// Anything else, including success: nothing is concluded.
    Unknown,
}

pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

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

/// Plex Media Server passes tag bytes through unchanged, so a file tagged in
/// Latin-1 or Windows-1252 yields invalid UTF-8 inside an otherwise valid body.
/// Keep valid UTF-8 and read each invalid byte as Windows-1252 (the WHATWG
/// "latin1" decoder) rather than rejecting the page.
fn plex_text(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    // WHATWG windows-1252 for 0x80..=0x9F; unassigned bytes keep their C1 code point.
    const C1: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
        '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}',
        'ž', 'Ÿ',
    ];
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.into();
    }
    let mut text = String::with_capacity(bytes.len() + bytes.len() / 8);
    for chunk in bytes.utf8_chunks() {
        text.push_str(chunk.valid());
        text.extend(chunk.invalid().iter().map(|&byte| match byte {
            0x80..=0x9f => C1[usize::from(byte - 0x80)],
            _ => char::from(byte),
        }));
    }
    text.into()
}

pub(crate) fn plex_json<T: DeserializeOwned>(bytes: &[u8], context: &'static str) -> Result<T> {
    serde_json::from_str(&plex_text(bytes)).map_err(|_| Error::ProtocolAt(context))
}

/// Builder for every HTTP client in the process. TLS is rustls with the
/// `ring` provider and the Mozilla roots compiled in (`webpki-roots`): no
/// system trust store and no C crypto library to cross-compile for Sailfish.
/// The config is handed to reqwest prebuilt, so it must come from the same
/// `rustls` version reqwest uses (otherwise `build()` fails).
pub(crate) fn client_builder() -> reqwest::ClientBuilder {
    Client::builder().tls_backend_preconfigured(tls_config().clone())
}

/// The Mozilla root certificates bundled by `webpki-roots`.
fn compiled_in_roots() -> rustls::RootCertStore {
    rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    }
}

fn tls_config() -> &'static rustls::ClientConfig {
    static CONFIG: OnceLock<rustls::ClientConfig> = OnceLock::new();
    CONFIG.get_or_init(|| {
        let mut config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("ring supports the default TLS versions")
        .with_root_certificates(compiled_in_roots())
        .with_no_client_auth();
        // reqwest leaves a preconfigured ALPN untouched; offer HTTP/1.1 as
        // reqwest's own rustls setup does without the `http2` feature.
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        config
    })
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
        client_builder()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            // Static configuration, covered by a unit test. A default client
            // would lose the redirect policy and the compiled-in roots.
            .expect("HTTP client configuration")
    })
}

/// Navigation generation a view-bound request belongs to.
type Scope = (watch::Receiver<u64>, u64);

/// The view-cancellation scope of one client (see [`Plex::bind_view`]),
/// shared by every backend that drives requests from the synchronous Core.
#[derive(Default)]
pub(crate) struct ViewScope {
    scope: Mutex<Option<Scope>>,
    cancelled: AtomicBool,
}

impl ViewScope {
    /// Bind following requests to a navigation generation: once `navigation`
    /// moves past `view`, in-flight and later requests end with [`CANCELLED`].
    /// `None` unbinds.
    pub(crate) fn bind(&self, scope: Option<Scope>) {
        *crate::mutex_lock(&self.scope) = scope;
        self.cancelled.store(false, Ordering::SeqCst);
    }
    /// Whether a request was cancelled by navigation since [`bind`](Self::bind).
    pub(crate) fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
    /// Drive one request on the shared runtime from the synchronous Core.
    pub(crate) fn run<T: Send>(&self, step: impl Future<Output = Result<T>> + Send) -> Result<T> {
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
}

pub struct Plex {
    client: Client,
    headers: HeaderMap,
    pub account_url: Url,
    view: ViewScope,
}

/// A clone is an independent client for another thread (the radio refill
/// worker): same pooled connections, headers and account URL, but a fresh,
/// unbound view scope and a cleared cancellation flag. Sharing the scope would
/// let a navigation abort the refill, or `bind_view(None)` on Core unbind it.
impl Clone for Plex {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            headers: self.headers.clone(),
            account_url: self.account_url.clone(),
            view: ViewScope::default(),
        }
    }
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
            view: ViewScope::default(),
        })
    }

    /// Bind following requests to a navigation generation (see
    /// [`ViewScope::bind`]). Used by the runtime around view-bound commands.
    pub(crate) fn bind_view(&self, scope: Option<Scope>) {
        self.view.bind(scope);
    }
    /// Whether a request was cancelled by navigation since `bind_view`.
    pub(crate) fn cancelled(&self) -> bool {
        self.view.cancelled()
    }

    /// Drive one request on the shared runtime from the synchronous Core.
    fn run<T: Send>(&self, step: impl Future<Output = Result<T>> + Send) -> Result<T> {
        self.view.run(step)
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
        plex_json(&bytes, context)
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
        Ok(plex_text(&bytes).into_owned())
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
        valid_id(key)?;
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
        self.run(read_audio_probe(request))
    }
}

/// Send `request` and read a bounded prefix of its audio body: proves the
/// media is readable. Returns (bytes read, normalized MIME essence).
pub(crate) async fn read_audio_probe(request: RequestBuilder) -> Result<(usize, String)> {
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
}

/// Plex identifier rule: 1-20 ASCII digits (they fit a `u64`). Rejects
/// unbounded digit strings before they become huge URL paths or overflow
/// parsers. Core reaches this through `MediaServer::valid_id`.
pub(crate) fn valid_id(value: &str) -> Result<&str> {
    if value.is_empty() || value.len() > 20 || !value.bytes().all(|c| c.is_ascii_digit()) {
        Err(Error::Input("Expected a numeric Plex identifier"))
    } else {
        Ok(value)
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

/// Like [`server_url`] but the URL may carry a path prefix (a server behind a
/// reverse proxy at `/jellyfin`). The result is normalised to end in exactly
/// one `/`; the prefix has no empty, dot or encoded-separator segments, so
/// [`server_path`] can keep every request under it. Plex servers are always
/// origin-only and use [`server_url`].
pub fn server_url_with_base_path(value: &str) -> Result<Url> {
    let invalid = || {
        Error::Input(
            "Server URL must be an HTTP(S) address, optionally with a path such as /jellyfin",
        )
    };
    let mut url = Url::parse(value).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    let path = url.path().to_owned();
    let lower = path.to_ascii_lowercase();
    let trimmed = path.trim_matches('/');
    if path.contains('\\')
        || path.chars().any(char::is_control)
        || ["%2e", "%2f", "%5c", "%25", "%3f", "%23", "%00"]
            .iter()
            .any(|escape| lower.contains(escape))
        || trimmed
            .split('/')
            .any(|segment| !trimmed.is_empty() && matches!(segment, "" | "." | ".."))
    {
        return Err(invalid());
    }
    url.set_path(&if trimmed.is_empty() {
        "/".to_owned()
    } else {
        format!("/{trimmed}/")
    });
    Ok(url)
}

/// Join a server-relative `path` to `base`. An origin-only base (every Plex
/// server) maps `/x` to `/x`. A base with a path prefix maps it to
/// `<prefix>/x` and the result must stay below the prefix on the same origin.
pub fn server_path(base: &Url, path: &str) -> Result<Url> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.contains('\\')
        || path.contains(['?', '#'])
        || path.chars().any(char::is_control)
    {
        return Err(Error::Input("Expected a server-relative path"));
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
        return Err(Error::Input("Expected a server-relative path"));
    }
    let prefix = base.path().trim_end_matches('/');
    let url = if prefix.is_empty() {
        base.join(path)?
    } else {
        // `join` of an absolute path would replace the prefix.
        let mut url = base.clone();
        url.set_query(None);
        url.set_fragment(None);
        url.set_path(&format!("{prefix}{path}"));
        if !url.path().starts_with(&format!("{prefix}/")) {
            return Err(Error::Input("Cross-origin path rejected"));
        }
        url
    };
    if url.origin() != base.origin() {
        return Err(Error::Input("Cross-origin path rejected"));
    }
    Ok(url)
}

/// Station and artwork keys may advertise queries. Keep credential/header
/// parameters out of that input and validate the pathname before joining.
pub(crate) fn advertised_path(base: &Url, key: &str) -> Result<Url> {
    advertised_url(base, key, is_plex_credential)
}

/// A Plex credential or client-identity query parameter (`X-Plex-*`), which a
/// server-handed key must never smuggle into a request.
pub(crate) fn is_plex_credential(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("x-plex-")
}

/// [`advertised_path`] with the backend's own credential-parameter filter.
pub(crate) fn advertised_url(
    base: &Url,
    key: &str,
    credential: impl Fn(&str) -> bool,
) -> Result<Url> {
    let (path, query) = key
        .split_once('?')
        .map_or((key, None), |(p, q)| (p, Some(q)));
    let mut url = server_path(base, path)?;
    if let Some(query) = query {
        if query.contains('#') || query.chars().any(char::is_control) {
            return Err(Error::Input("Expected a server-relative path"));
        }
        for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
            if credential(&name) || name.chars().chain(value.chars()).any(char::is_control) {
                return Err(Error::Input("Expected a server-relative path"));
            }
        }
        url.set_query(Some(query));
    }
    Ok(url)
}

/// A server-advertised key (hub, filter-choice, station key) validated by
/// [`advertised_url`] and split into its server-relative path and decoded
/// query pairs, independent of the server's base path.
pub(crate) fn advertised_parts(
    key: &str,
    credential: impl Fn(&str) -> bool,
) -> Result<(String, Vec<(String, String)>)> {
    let origin = Url::parse("http://server.invalid/")?;
    let url = advertised_url(&origin, key, credential)?;
    let params = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    Ok((url.path().to_owned(), params))
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
    fn clients_get_ring_tls_with_compiled_in_roots() {
        let config = tls_config();
        let suites = |p: &rustls::crypto::CryptoProvider| {
            p.cipher_suites
                .iter()
                .map(|s| s.suite())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            suites(config.crypto_provider()),
            suites(&rustls::crypto::ring::default_provider())
        );
        assert!(!webpki_roots::TLS_SERVER_ROOTS.is_empty());
        assert_eq!(
            compiled_in_roots().len(),
            webpki_roots::TLS_SERVER_ROOTS.len()
        );
        assert_eq!(config.alpn_protocols, vec![b"http/1.1".to_vec()]);
        assert!(client_builder().build().is_ok());
        let _ = client();
    }

    #[test]
    fn latin1_tag_bytes_decode_instead_of_rejecting_the_page() {
        let body = b"{\"title\":\"Can\xe7\xe3o \xc3\xa9 \xfa\",\"n\":1}";
        let value: serde_json::Value = plex_json(body, "test").unwrap();
        assert_eq!(value["title"], "Can\u{e7}\u{e3}o \u{e9} \u{fa}");
        assert!(plex_json::<serde_json::Value>(b"{\"title\":", "test").is_err());
        assert!(matches!(plex_text(b"plain"), std::borrow::Cow::Borrowed(_)));
        assert_eq!(
            plex_text(b"\x93it\x92s\x94 \x96 \x81\"x"),
            "\u{201c}it\u{2019}s\u{201d} \u{2013} \u{81}\"x"
        );
    }

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
            "url=a%00b",
            "u%0Arl=test",
        ] {
            assert!(advertised_path(&base, &format!("/photo/:/transcode?{query}")).is_err());
        }
    }
}

#[cfg(test)]
mod clone_tests {
    use super::*;

    #[test]
    fn a_clone_does_not_share_the_view_scope_or_cancellation() {
        let plex = Plex::new("fixture", Url::parse("https://plex.tv/").unwrap()).unwrap();
        let (_tx, rx) = watch::channel(0u64);
        plex.bind_view(Some((rx, 0)));
        plex.view.cancelled.store(true, Ordering::SeqCst);
        let clone = plex.clone();
        assert!(clone.view.scope.lock().unwrap().is_none());
        assert!(!clone.cancelled());
        // Unbinding the original (as Core does after each command) leaves a
        // clone bound later on its own.
        let (_tx2, rx2) = watch::channel(0u64);
        clone.bind_view(Some((rx2, 0)));
        plex.bind_view(None);
        assert!(clone.view.scope.lock().unwrap().is_some());
    }
}

#[cfg(test)]
mod base_path_tests {
    use super::*;

    fn joined(base: &str, path: &str) -> Result<String> {
        server_path(&server_url_with_base_path(base)?, path).map(String::from)
    }

    #[test]
    fn a_base_path_is_normalised_and_prefixes_every_request() {
        for base in [
            "https://media.example/jellyfin",
            "https://media.example/jellyfin/",
        ] {
            assert_eq!(
                server_url_with_base_path(base).unwrap().as_str(),
                "https://media.example/jellyfin/"
            );
            assert_eq!(
                joined(base, "/Items/abc").unwrap(),
                "https://media.example/jellyfin/Items/abc"
            );
        }
        assert_eq!(
            joined("http://127.0.0.1:8096/a/b", "/Audio/1/stream").unwrap(),
            "http://127.0.0.1:8096/a/b/Audio/1/stream"
        );
        // An origin-only address behaves exactly like a Plex origin.
        assert_eq!(
            joined("http://127.0.0.1:8096", "/System/Info").unwrap(),
            "http://127.0.0.1:8096/System/Info"
        );
    }

    #[test]
    fn requests_cannot_leave_the_prefix_or_the_origin() {
        let base = "https://media.example/jellyfin/";
        for path in [
            "/../admin",
            "/Items/../../admin",
            "/%2e%2e/admin",
            "/Items/%2E%2e/x",
            "/a%2fb",
            "/a%5cb",
            "//evil.example/x",
            "/Items\\..\\x",
            "Items/relative",
            "/Items?api_key=1",
            "/Items#frag",
            "/Items/\u{7}x",
        ] {
            assert!(joined(base, path).is_err(), "{path}");
        }
    }

    #[test]
    fn a_malformed_base_path_is_refused() {
        for base in [
            "https://media.example/jelly//fin",
            "https://media.example/%252e%252e/",
            "https://media.example/a%2fb/",
            "https://media.example/a%5Cb/",
            "https://media.example/jellyfin?x=1",
            "https://media.example/jellyfin#x",
            "https://user:pw@media.example/jellyfin",
            "ftp://media.example/jellyfin",
            "not a url",
        ] {
            assert!(server_url_with_base_path(base).is_err(), "{base}");
        }
    }

    #[test]
    fn plex_stays_origin_only() {
        assert!(server_url("https://plex.example:32400/").is_ok());
        assert!(server_url("https://plex.example:32400/jellyfin/").is_err());
        assert!(server_url("https://plex.example:32400/jellyfin").is_err());
    }
}
