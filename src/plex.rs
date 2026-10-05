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
            200..=299 => response.json().map_err(|_| Error::ProtocolAt(context)),
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
            match serde_json::from_value(r.clone()) {
                Ok(resource) => valid.push(resource),
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
        let response = self
            .client
            .request(method, url)
            .header("X-Plex-Token", token)
            .send()
            .map_err(|_| Error::Network)?;
        match response.status().as_u16() {
            200..=299 => Ok(()),
            401 | 403 => Err(Error::Unauthorized),
            code => Err(Error::Http(code)),
        }
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
        let response = self
            .client
            .post(url)
            .header("X-Plex-Token", token)
            .send()
            .map_err(|_| Error::Network)?;
        match response.status().as_u16() {
            200..=299 => Ok(()),
            401 | 403 => Err(Error::Unauthorized),
            code => Err(Error::Http(code)),
        }
    }
    pub fn rate(&self, base: &Url, token: &str, key: &str, rating: u8) -> Result<()> {
        let mut url = server_path(base, "/:/rate")?;
        url.query_pairs_mut().extend_pairs([
            ("key", key),
            ("identifier", "com.plexapp.plugins.library"),
            ("rating", &rating.to_string()),
        ]);
        let response = self
            .client
            .put(url)
            .header("X-Plex-Token", token)
            .send()
            .map_err(|_| Error::Network)?;
        match response.status().as_u16() {
            200..=299 => Ok(()),
            401 | 403 => Err(Error::Unauthorized),
            code => Err(Error::Http(code)),
        }
    }
    pub fn text(&self, base: &Url, token: &str, path: &str) -> Result<String> {
        let response = self
            .client
            .get(server_path(base, path)?)
            .header("X-Plex-Token", token)
            .send()
            .map_err(|_| Error::Network)?;
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
        let response = self
            .client
            .get(url)
            .header("X-Plex-Token", token)
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
            .to_string();
        if !mime.starts_with("audio/") {
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
    if !path.starts_with('/') || path.starts_with("//") || path.contains('\\') || path.contains('#')
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
    let mut url = server_path(base, path)?;
    url.query_pairs_mut().append_pair("X-Plex-Token", token);
    Ok(url.into())
}
