use crate::{
    model::{Container, Envelope, Pin, Resource},
    Error, Result,
};
use reqwest::{
    blocking::Client,
    header::{HeaderMap, HeaderValue},
    Method,
};
use serde::de::DeserializeOwned;
use std::{io::Read, time::Duration};
use url::Url;

pub(crate) const JSON_LIMIT: usize = 8 * 1024 * 1024;

pub(crate) fn audio_mime(mime: &str) -> bool {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    mime.is_empty()
        || mime.starts_with("audio/")
        || matches!(
            mime.as_str(),
            "application/octet-stream" | "application/binary" | "video/mp4"
        )
}

pub(crate) async fn bounded_json<T: DeserializeOwned>(
    mut response: reqwest::Response,
    context: &'static str,
) -> Result<T> {
    if response
        .content_length()
        .is_some_and(|n| n > JSON_LIMIT as u64)
    {
        return Err(Error::ProtocolAt(context));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Network)? {
        if bytes.len().saturating_add(chunk.len()) > JSON_LIMIT {
            return Err(Error::ProtocolAt(context));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::ProtocolAt(context))
}

pub struct Plex {
    client: Client,
    pub account_url: Url,
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
        // `timeout` bounds the whole small-JSON request (headers + body),
        // so a stalled timeline/library call cannot block the worker beyond
        // 15s. `connect_timeout` fails fast on unreachable hosts.
        let client = Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Error::Network)?;
        Ok(Self {
            client,
            account_url,
        })
    }

    fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        url: Url,
        token: &str,
        form: &[(&str, &str)],
        context: &'static str,
    ) -> Result<T> {
        let mut request = self.client.request(method, url);
        if !token.is_empty() {
            request = request.header("X-Plex-Token", token);
        }
        if !form.is_empty() {
            request = request.form(form);
        }
        let response = request.send().map_err(|_| Error::Network)?;
        match response.status().as_u16() {
            200..=299 => {
                if response
                    .content_length()
                    .is_some_and(|n| n > JSON_LIMIT as u64)
                {
                    return Err(Error::ProtocolAt(context));
                }
                let mut bytes = Vec::new();
                response
                    .take(JSON_LIMIT as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| Error::Network)?;
                if bytes.len() > JSON_LIMIT {
                    return Err(Error::ProtocolAt(context));
                }
                serde_json::from_slice(&bytes).map_err(|_| Error::ProtocolAt(context))
            }
            401 | 403 => Err(Error::Unauthorized),
            code => Err(Error::Http(code)),
        }
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
        let mut valid = Vec::with_capacity(servers.len());
        for r in servers.iter() {
            match serde_json::from_value::<Resource>(r.clone()) {
                Ok(resource) if !resource.access_token.is_empty() => valid.push(resource),
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
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
    fn status(code: u16) -> Result<()> {
        match code {
            200..=299 => Ok(()),
            401 | 403 => Err(Error::Unauthorized),
            code => Err(Error::Http(code)),
        }
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
        let mut builder = self.client.request(method, url);
        if !token.is_empty() {
            builder = builder.header("X-Plex-Token", token);
        }
        let response = builder.send().map_err(|_| Error::Network)?;
        Self::status(response.status().as_u16())
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
        let mut url = server_path(base, "/:/timeline")?;
        url.query_pairs_mut()
            .extend_pairs(params.iter().map(|(k, v)| (*k, v.as_str())));
        let mut builder = self.client.post(url);
        if !token.is_empty() {
            builder = builder.header("X-Plex-Token", token);
        }
        let response = builder.send().map_err(|_| Error::Network)?;
        Self::status(response.status().as_u16())
    }
    pub fn rate(&self, base: &Url, token: &str, key: &str, rating: u8) -> Result<()> {
        let mut url = server_path(base, "/:/rate")?;
        url.query_pairs_mut().extend_pairs([
            ("key", key),
            ("identifier", "com.plexapp.plugins.library"),
            ("rating", &rating.to_string()),
        ]);
        let mut builder = self.client.put(url);
        if !token.is_empty() {
            builder = builder.header("X-Plex-Token", token);
        }
        let response = builder.send().map_err(|_| Error::Network)?;
        Self::status(response.status().as_u16())
    }
    pub fn text(&self, base: &Url, token: &str, path: &str) -> Result<String> {
        let mut builder = self.client.get(server_path(base, path)?);
        if !token.is_empty() {
            builder = builder.header("X-Plex-Token", token);
        }
        let response = builder.send().map_err(|_| Error::Network)?;
        let code = response.status().as_u16();
        if !(200..300).contains(&code) {
            return Err(if code == 401 || code == 403 {
                Error::Unauthorized
            } else {
                Error::Http(code)
            });
        }
        let mut bytes = Vec::new();
        std::io::Read::take(response, 512 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 512 * 1024 {
            return Err(Error::Input("Lyrics exceed the size limit"));
        }
        String::from_utf8(bytes).map_err(|_| Error::ProtocolAt("lyrics text"))
    }
    pub(crate) fn probe_audio(&self, url: Url, token: &str) -> Result<(usize, String)> {
        let mut builder = self.client.get(url);
        if !token.is_empty() {
            builder = builder.header("X-Plex-Token", token);
        }
        let response = builder
            .header("Accept", "*/*")
            .header("Accept-Encoding", "identity")
            .send()
            .map_err(|_| Error::Network)?;
        if !response.status().is_success() {
            return Err(Error::Http(response.status().as_u16()));
        }
        let mime = response
            .headers()
            .get("Content-Type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if !audio_mime(&mime) {
            return Err(Error::Input(
                "Server returned non-audio content for this track",
            ));
        }
        let mut bytes = Vec::new();
        response.take(256 * 1024).read_to_end(&mut bytes)?;
        if bytes.is_empty() {
            return Err(Error::Input(
                "Audio download is incomplete; waiting to resume",
            ));
        }
        Ok((bytes.len(), mime))
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
    use std::{io::Write, net::TcpListener, thread};

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
