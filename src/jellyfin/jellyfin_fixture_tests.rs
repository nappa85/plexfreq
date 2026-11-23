//! A local fixture HTTP server in Jellyfin's vocabulary, shared by the
//! backend-level and Core-level Jellyfin tests. It serves synthetic data only;
//! every token and password is a dummy.
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    time::Duration,
};

pub const TOKEN: &str = "fixture-jellyfin-token";
pub const PASSWORD: &str = "fixture-password";
pub const SERVER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const USER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// A 32-digit id made of `c`.
pub fn id(c: char) -> String {
    c.to_string().repeat(32)
}
pub fn library() -> String {
    id('c')
}
pub fn artist() -> String {
    id('d')
}
pub fn album() -> String {
    id('e')
}
pub fn playlist() -> String {
    id('f')
}
/// Track `n` (1-9).
pub fn track_id(n: u32) -> String {
    id(char::from_digit(n, 10).unwrap())
}

pub struct Req {
    pub method: String,
    pub target: String,
    /// The path below the fixture's prefix.
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}
impl Req {
    pub fn line(&self) -> String {
        format!("{} {}", self.method, self.target)
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    /// The decoded value of query parameter `name` (case-insensitive).
    pub fn param(&self, name: &str) -> Option<String> {
        let query = self.target.split_once('?')?.1;
        url::form_urlencoded::parse(query.as_bytes())
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.into_owned())
    }
    pub fn has(&self, name: &str) -> bool {
        self.param(name).is_some()
    }
}

pub struct Reply {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
    pub content_type: &'static str,
}
impl Reply {
    pub fn json(value: Value) -> Self {
        Self {
            status: 200,
            headers: vec![],
            body: value.to_string().into_bytes(),
            content_type: "application/json",
        }
    }
    pub fn status(status: u16) -> Self {
        Self {
            status,
            headers: vec![],
            body: vec![],
            content_type: "text/plain",
        }
    }
    pub fn empty() -> Self {
        Self::status(204)
    }
    pub fn header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.push((name, value.into()));
        self
    }
}

pub struct Server {
    /// `http://127.0.0.1:port` and the prefix, no trailing slash.
    pub url: String,
    pub seen: Arc<Mutex<Vec<Req>>>,
}

impl Server {
    /// Serve `handler` for requests below `prefix` ("" or "/jellyfin"); the
    /// handler sees the path without the prefix, other paths are 404.
    pub fn start(prefix: &str, handler: impl Fn(&Req) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}{prefix}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (log, prefix) = (seen.clone(), prefix.to_owned());
        let handler = Arc::new(handler);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let (log, prefix, handler) = (log.clone(), prefix.clone(), handler.clone());
                std::thread::spawn(move || serve(stream, &log, &prefix, &*handler));
            }
        });
        Self { url, seen }
    }
    /// Requests seen since the previous call.
    pub fn take(&self) -> Vec<Req> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }
    pub fn lines(&self) -> Vec<String> {
        self.seen.lock().unwrap().iter().map(Req::line).collect()
    }
    pub fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

fn serve(
    mut stream: TcpStream,
    seen: &Mutex<Vec<Req>>,
    prefix: &str,
    handler: &(dyn Fn(&Req) -> Reply + Send + Sync),
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut raw = Vec::new();
    let mut byte = [0];
    while !raw.ends_with(b"\r\n\r\n") {
        if !matches!(stream.read(&mut byte), Ok(1)) {
            return;
        }
        raw.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let method = first.next().unwrap_or_default().to_owned();
    let target = first.next().unwrap_or_default().to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    let _ = stream.read_exact(&mut body);
    let full_path = target.split('?').next().unwrap_or_default().to_owned();
    let path = full_path.strip_prefix(prefix).map(str::to_owned);
    let request = Req {
        method,
        target,
        path: path.clone().unwrap_or_else(|| full_path.clone()),
        headers,
        body,
    };
    let reply = if path.is_some() {
        handler(&request)
    } else {
        Reply::status(404)
    };
    seen.lock().unwrap().push(request);
    let extra: String = reply
        .headers
        .iter()
        .map(|(k, v)| format!("{k}: {v}\r\n"))
        .collect();
    let _ = write!(
        stream,
        "HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
        reply.status,
        reply.content_type,
        reply.body.len()
    );
    let _ = stream.write_all(&reply.body);
}

/// What the standard routes answer; tests change it between calls.
pub struct Script {
    pub product: &'static str,
    pub version: &'static str,
    pub quick_connect_enabled: bool,
    pub approved: bool,
    pub expired: bool,
    /// The server's playlist: (entry id, track number).
    pub playlist: Vec<(String, u32)>,
    pub lyrics: Option<Value>,
    pub mix: Vec<u32>,
    /// Seeds whose mix request fails with this status.
    pub mix_status: std::collections::HashMap<String, u16>,
    /// Mixes of particular seeds (item id), instead of `mix`.
    pub mix_by_seed: std::collections::HashMap<String, Vec<u32>>,
    pub token: &'static str,
    /// The server's identity in its public info and its sign-ins.
    pub server: &'static str,
}
impl Default for Script {
    fn default() -> Self {
        Self {
            product: "Jellyfin Server",
            version: "10.11.4",
            quick_connect_enabled: true,
            approved: false,
            expired: false,
            playlist: vec![(id('1'), 1), (id('2'), 2), (id('3'), 3)],
            lyrics: None,
            mix: vec![1, 2, 3, 4, 5],
            mix_status: Default::default(),
            mix_by_seed: Default::default(),
            token: TOKEN,
            server: SERVER,
        }
    }
}

pub fn authentication(token: &str, server: &str) -> Value {
    json!({"AccessToken": token, "ServerId": server, "User": {"Id": USER, "Name": "tester"}})
}

/// A music track DTO with one MediaSource.
pub fn track(n: u32) -> Value {
    json!({
        "Id": track_id(n), "Name": format!("Track {n}"), "Type": "Audio",
        "RunTimeTicks": 2_400_000_000u64, "IndexNumber": n, "ParentIndexNumber": 1,
        "ProductionYear": 1999,
        "Album": "Album", "AlbumId": album(), "AlbumPrimaryImageTag": "albumtag",
        "AlbumArtist": "Artist", "AlbumArtists": [{"Id": artist(), "Name": "Artist"}],
        "Artists": ["Artist"],
        "ImageTags": {},
        "UserData": {"IsFavorite": n == 2, "PlayCount": n},
        "NormalizationGain": -3.5,
        "HasLyrics": n == 1,
        "MediaSources": [{
            "Container": "flac", "Bitrate": 900_000, "Size": 27_000_000u64,
            "MediaStreams": [{"Type": "Audio", "Codec": "flac"}]
        }]
    })
}

pub fn items(list: Vec<Value>) -> Value {
    let total = list.len();
    json!({"Items": list, "TotalRecordCount": total, "StartIndex": 0})
}

/// The routes every test starts from.
pub fn standard(script: Arc<Mutex<Script>>) -> impl Fn(&Req) -> Reply + Send + Sync + 'static {
    move |req: &Req| {
        let mut script = script.lock().unwrap();
        let path = req.path.as_str();
        let types = req.param("IncludeItemTypes").unwrap_or_default();
        match (req.method.as_str(), path) {
            ("GET", "/System/Info/Public") => Reply::json(json!({
                "ProductName": script.product, "Version": script.version,
                "Id": script.server, "ServerName": "Fixture"
            })),
            ("POST", "/Users/AuthenticateByName") => {
                if req.json()["Pw"] == PASSWORD {
                    Reply::json(authentication(script.token, script.server))
                } else {
                    Reply::status(401)
                }
            }
            ("GET", "/QuickConnect/Enabled") => Reply::json(json!(script.quick_connect_enabled)),
            ("POST", "/QuickConnect/Initiate") => Reply::json(
                json!({"Secret": "fixture-secret", "Code": "123456", "Authenticated": false}),
            ),
            ("GET", "/QuickConnect/Connect") => {
                if script.expired || req.param("secret").as_deref() != Some("fixture-secret") {
                    Reply::status(404)
                } else {
                    Reply::json(json!({"Authenticated": script.approved}))
                }
            }
            ("POST", "/Users/AuthenticateWithQuickConnect") => {
                if req.json()["Secret"] == "fixture-secret" {
                    Reply::json(authentication(script.token, script.server))
                } else {
                    Reply::status(401)
                }
            }
            ("POST", "/Sessions/Logout") => Reply::empty(),
            ("GET", "/UserViews") => Reply::json(items(vec![
                json!({"Id": library(), "Name": "Music", "CollectionType": "music"}),
                json!({"Id": id('9'), "Name": "Films", "CollectionType": "movies"}),
            ])),
            ("GET", "/Artists/AlbumArtists") => Reply::json(items(vec![
                json!({"Id": artist(), "Name": "Artist", "Type": "MusicArtist",
                       "ImageTags": {"Primary": "arttag"}}),
            ])),
            ("GET", "/Artists") => Reply::json(items(vec![
                json!({"Id": artist(), "Name": "Artist", "Type": "MusicArtist"}),
            ])),
            ("GET", p) if p.ends_with("/Similar") => Reply::json(items(vec![
                json!({"Id": id('6'), "Name": "Other", "Type": "MusicArtist"}),
                json!({"Id": album(), "Name": "Album", "Type": "MusicAlbum"}),
            ])),
            ("GET", "/Items/Latest") => Reply::json(json!([
                {"Id": album(), "Name": "Album", "Type": "MusicAlbum"}
            ])),
            ("GET", "/Items") if req.has("Ids") => {
                let want = req.param("Ids").unwrap();
                if want == album() {
                    Reply::json(items(vec![json!({
                        "Id": album(), "Name": "Album", "Type": "MusicAlbum", "ProductionYear": 1999,
                        "AlbumArtist": "Artist", "AlbumArtists": [{"Id": artist(), "Name": "Artist"}],
                        "ChildCount": 3, "ImageTags": {"Primary": "albumtag"}
                    })]))
                } else if want == artist() {
                    Reply::json(items(vec![
                        json!({"Id": artist(), "Name": "Artist", "Type": "MusicArtist"}),
                    ]))
                } else if want == playlist() {
                    Reply::json(items(vec![json!({
                        "Id": playlist(), "Name": "Mine", "Type": "Playlist", "MediaType": "Audio",
                        "ChildCount": script.playlist.len()
                    })]))
                } else if let Some(n) = (1..=9).find(|n| track_id(*n) == want) {
                    Reply::json(items(vec![track(n)]))
                } else {
                    Reply::json(items(vec![]))
                }
            }
            ("GET", "/Items") if types == "Playlist" => Reply::json(items(vec![
                json!({"Id": playlist(), "Name": "Mine", "Type": "Playlist", "MediaType": "Audio"}),
                json!({"Id": id('8'), "Name": "Films", "Type": "Playlist", "MediaType": "Video"}),
            ])),
            ("GET", "/Items") if types == "MusicAlbum" => Reply::json(items(vec![json!({
                "Id": album(), "Name": "Album", "Type": "MusicAlbum", "ProductionYear": 1999
            })])),
            ("GET", "/Items") if types == "Audio" => {
                Reply::json(items((1..=3).map(track).collect()))
            }
            ("GET", "/Items") => Reply::json(items(vec![])),
            ("GET", "/Genres") => Reply::json(items(vec![
                json!({"Id": id('7'), "Name": "Rock"}),
                json!({"Id": "not-an-id", "Name": "Ignored"}),
            ])),
            ("GET", p) if p.ends_with("/InstantMix") => {
                let seed = p.split('/').nth(2).unwrap_or_default();
                if let Some(status) = script.mix_status.get(seed) {
                    return Reply::status(*status);
                }
                let mix = script.mix_by_seed.get(seed).unwrap_or(&script.mix);
                Reply::json(items(mix.iter().map(|n| track(*n)).collect()))
            }
            ("GET", p) if p == format!("/Playlists/{}/Items", playlist()) => {
                let start: usize = req
                    .param("StartIndex")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let limit: usize = req
                    .param("Limit")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(usize::MAX);
                let rows: Vec<Value> = script
                    .playlist
                    .iter()
                    .skip(start)
                    .take(limit)
                    .map(|(entry, n)| {
                        let mut t = track(*n);
                        t["PlaylistItemId"] = json!(entry);
                        t
                    })
                    .collect();
                let mut reply = items(rows);
                reply["TotalRecordCount"] = json!(script.playlist.len());
                reply["StartIndex"] = json!(start);
                Reply::json(reply)
            }
            ("POST", p) if p.starts_with(&format!("/Playlists/{}/Items/", playlist())) => {
                // `.../Items/{entry}/Move/{index}`: the entry takes that
                // position in the resulting list; an unknown entry is a
                // silent success, as on the real server.
                let rest = p.trim_start_matches(&format!("/Playlists/{}/Items/", playlist()));
                if let Some((entry, index)) = rest.split_once("/Move/") {
                    if let Some(at) = script.playlist.iter().position(|(e, _)| e == entry) {
                        let moved = script.playlist.remove(at);
                        let index = index
                            .parse::<usize>()
                            .unwrap_or(0)
                            .min(script.playlist.len());
                        script.playlist.insert(index, moved);
                    }
                }
                Reply::empty()
            }
            ("POST", "/Playlists") => Reply::json(json!({"Id": playlist()})),
            ("GET", p) if p == format!("/Audio/{}/Lyrics", track_id(1)) => match &script.lyrics {
                Some(lyrics) => Reply::json(lyrics.clone()),
                None => Reply::status(404),
            },
            ("GET", p) if p.starts_with("/Audio/") && p.contains("/stream") => {
                let mut reply = Reply::status(200);
                reply.content_type = "audio/mpeg";
                reply.body = vec![0x55; 4096];
                reply
            }
            ("GET", p) if p.starts_with("/Items/") => {
                let want = p.trim_start_matches("/Items/");
                if want == track_id(1) {
                    Reply::json(track(1))
                } else {
                    Reply::status(404).header("X-Response-Time-ms", "1.2")
                }
            }
            ("POST" | "DELETE", _) => Reply::empty(),
            _ => Reply::status(404),
        }
    }
}
