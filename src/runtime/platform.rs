use super::Shared;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::mpsc, thread, time::Duration};
use zbus::{
    blocking::{Connection, ConnectionBuilder, Proxy},
    zvariant::{OwnedObjectPath, OwnedValue},
};

pub struct Platform {
    threads: Vec<thread::JoinHandle<()>>,
}
impl Drop for Platform {
    fn drop(&mut self) {
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}
pub fn start(shared: Shared, events: mpsc::Sender<Value>) -> Platform {
    let mut threads = Vec::new();
    let network = shared.clone();
    if let Ok(net) = thread::Builder::new()
        .name("plexfreq-network".into())
        .spawn(move || {
            let connection = Connection::system().ok();
            let mut previous = None;
            while !network.stop.load(std::sync::atomic::Ordering::SeqCst) {
                let facts = connection
                    .as_ref()
                    .and_then(|c| facts(c).ok())
                    .unwrap_or((false, false));
                if previous != Some(facts) {
                    network.hint_network(facts.0);
                    let _ = network.send(json!({"op":"network_state","wifi":facts.0,"online":facts.1,"live_hint":true}));
                    previous = Some(facts);
                }
                for _ in 0..50 {
                    if network.stop.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        })
    {
        threads.push(net);
    }
    let bus = shared.clone();
    if let Ok(player) = thread::Builder::new()
        .name("plexfreq-mpris".into())
        .spawn(move || {
            let path = "/org/mpris/MediaPlayer2";
            let Ok(builder) = ConnectionBuilder::session() else {
                return;
            };
            let connection = builder
                .name("org.mpris.MediaPlayer2.plexfreq")
                .and_then(|b| b.serve_at(path, Root { events: events.clone() }))
                .and_then(|b| b.serve_at(path, Player { shared: bus.clone() }))
                .and_then(|b| b.build());
            let Ok(connection) = connection else {
                return;
            };
            let mut previous = Value::Null;
            while !bus.stop.load(std::sync::atomic::Ordering::SeqCst) {
                for position in bus
                    .seeks
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .drain(..)
                {
                    let _ = connection.emit_signal(
                        None::<&str>,
                        path,
                        "org.mpris.MediaPlayer2.Player",
                        "Seeked",
                        &(position,),
                    );
                }
                let state = bus.playback();
                let changed = {
                    let model=crate::mutex_lock(&bus.model);
                    json!({"playing":state.playing,"paused":state.paused,"volume":state.volume,"seekable":state.seekable,"id":state.id,"track":model["track"],"loop":model["queue"]["repeat"],"shuffle":model["queue"]["shuffled"],"queueLength":model["queue"]["items"].as_array().map_or(0,Vec::len),"current":model["queue"]["current"]})
                };
                if changed != previous {
                    let player = Player { shared: bus.clone() };
                    let mut properties = HashMap::new();
                    if let Ok(v) =
                        OwnedValue::try_from(zbus::zvariant::Value::from(player.playback_status()))
                    {
                        properties.insert("PlaybackStatus", v);
                    }
                    if let Ok(v) =
                        OwnedValue::try_from(zbus::zvariant::Value::from(player.metadata()))
                    {
                        properties.insert("Metadata", v);
                    }
                    if let Ok(v) =
                        OwnedValue::try_from(zbus::zvariant::Value::from(player.loop_status()))
                    {
                        properties.insert("LoopStatus", v);
                    }
                    properties.insert("Shuffle", OwnedValue::from(player.shuffle()));
                    properties.insert("Volume", OwnedValue::from(state.volume));
                    properties.insert("CanPlay", OwnedValue::from(player.can_play()));
                    properties.insert("CanPause", OwnedValue::from(player.can_pause()));
                    properties.insert("CanGoNext",OwnedValue::from(player.can_go_next()));
                    properties.insert("CanGoPrevious",OwnedValue::from(player.can_go_previous()));
                    properties.insert("CanSeek", OwnedValue::from(state.seekable));
                    let _ = connection.emit_signal(
                        None::<&str>,
                        path,
                        "org.freedesktop.DBus.Properties",
                        "PropertiesChanged",
                        &(
                            "org.mpris.MediaPlayer2.Player",
                            properties,
                            Vec::<String>::new(),
                        ),
                    );
                    previous = changed;
                }
                thread::sleep(Duration::from_millis(100));
            }
        })
    {
        threads.push(player);
    }
    Platform { threads }
}
fn facts(connection: &Connection) -> zbus::Result<(bool, bool)> {
    if let Ok(proxy) = Proxy::new(connection, "net.connman", "/", "net.connman.Manager") {
        let services: Result<Vec<(OwnedObjectPath, HashMap<String, OwnedValue>)>, _> =
            proxy.call("GetServices", &());
        if let Ok(services) = services {
            // ConnMan returns favourite-first: the first ready/online service
            // is the default route. wifi_only must reflect that primary route,
            // not any secondary interface, otherwise downloads could run over
            // cellular while a dormant wifi service is also listed.
            for (_, properties) in &services {
                let state = properties
                    .get("State")
                    .and_then(|v| <&str>::try_from(v).ok())
                    .unwrap_or("");
                if matches!(state, "ready" | "online") {
                    let kind = properties
                        .get("Type")
                        .and_then(|v| <&str>::try_from(v).ok())
                        .unwrap_or("");
                    return Ok((kind == "wifi", true));
                }
            }
            return Ok((false, false));
        }
    }
    let proxy = Proxy::new(
        connection,
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager",
        "org.freedesktop.NetworkManager",
    )?;
    let kind: String = proxy.get_property("PrimaryConnectionType")?;
    let state: u32 = proxy.get_property("State")?;
    Ok((kind == "802-11-wireless", state >= 50))
}
struct Root {
    events: mpsc::Sender<Value>,
}
#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {
        let _ = self.events.send(json!({"kind":"raise"}));
    }
    fn quit(&self) {}
    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn identity(&self) -> &str {
        "PlexFreq"
    }
    #[zbus(property)]
    fn desktop_entry(&self) -> &str {
        "harbour-plexfreq"
    }
    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        vec![]
    }
    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        vec![]
    }
}
struct Player {
    shared: Shared,
}
impl Player {
    fn command(&self, value: Value) {
        let _ = self.shared.send(value);
    }
    fn track_id(&self) -> OwnedObjectPath {
        // Playback ids are numeric, so this path is always valid; fall back to
        // the zero track instead of panicking the DBus thread on corruption.
        OwnedObjectPath::try_from(format!("/org/plexfreq/track/{}", self.shared.playback().id))
            .or_else(|_| OwnedObjectPath::try_from("/org/plexfreq/track/0"))
            .expect("static MPRIS track path is valid")
    }
    fn owned(value: zbus::zvariant::Value<'_>) -> Option<OwnedValue> {
        OwnedValue::try_from(value).ok()
    }
}
#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn next(&self) {
        if self.can_go_next() {
            self.command(
                json!({"op":"next","automatic":false,"_paused":!self.shared.playback().playing}),
            );
        }
    }
    fn previous(&self) {
        if self.can_go_previous() {
            self.command(json!({"op":"previous","_paused":!self.shared.playback().playing}));
        }
    }
    fn play(&self) {
        self.command(json!({"op":"audio_play"}));
    }
    fn pause(&self) {
        self.command(json!({"op":"audio_pause"}));
    }
    fn play_pause(&self) {
        self.command(json!({"op":"audio_toggle"}));
    }
    fn stop(&self) {
        self.command(json!({"op":"audio_stop"}));
    }
    fn seek(&self, offset: i64) {
        let state = self.shared.playback();
        let position = (state.position as i64 * 1000).saturating_add(offset);
        if position > state.duration as i64 * 1000 {
            self.next();
        } else {
            self.set_position(self.track_id(), position.max(0));
        }
    }
    fn set_position(&self, id: OwnedObjectPath, position: i64) {
        let state = self.shared.playback();
        if state.seekable
            && id == self.track_id()
            && position >= 0
            && position <= state.duration as i64 * 1000
        {
            self.command(json!({"op":"audio_seek","position":position/1000}));
        }
    }
    fn open_uri(&self, _uri: &str) {}
    #[zbus(signal)]
    async fn seeked(context: &zbus::SignalContext<'_>, position: i64) -> zbus::Result<()>;
    #[zbus(property)]
    fn playback_status(&self) -> String {
        let state = self.shared.playback();
        if state.playing {
            "Playing"
        } else if state.paused {
            "Paused"
        } else {
            "Stopped"
        }
        .into()
    }
    #[zbus(property)]
    fn loop_status(&self) -> String {
        match self
            .shared
            .model
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())["queue"]["repeat"]
            .as_str()
        {
            Some("one") => "Track",
            Some("all") => "Playlist",
            _ => "None",
        }
        .into()
    }
    #[zbus(property)]
    fn set_loop_status(&self, value: &str) {
        let mode = match value {
            "Track" => "one",
            "Playlist" => "all",
            "None" => "off",
            _ => return,
        };
        self.command(json!({"op":"repeat","mode":mode}));
    }
    #[zbus(property)]
    fn shuffle(&self) -> bool {
        self.shared
            .model
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())["queue"]["shuffled"]
            .as_bool()
            .unwrap_or(false)
    }
    #[zbus(property)]
    fn set_shuffle(&self, value: bool) {
        self.command(json!({"op":"shuffle","enabled":value}));
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let model = self
            .shared
            .model
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let track = &model["track"];
        let mut data = HashMap::new();
        if !track.is_object() {
            return data;
        }
        if let Some(id) = Self::owned(zbus::zvariant::Value::from(self.track_id())) {
            data.insert("mpris:trackid".into(), id);
        }
        data.insert(
            "mpris:length".into(),
            OwnedValue::from(track["duration"].as_i64().unwrap_or(0) * 1000),
        );
        for (property, key) in [("xesam:title", "title"), ("xesam:album", "parentTitle")] {
            if let Some(value) = Self::owned(zbus::zvariant::Value::from(
                track[key].as_str().unwrap_or("").to_string(),
            )) {
                data.insert(property.into(), value);
            }
        }
        let artist = track["originalTitle"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| track["grandparentTitle"].as_str())
            .unwrap_or("");
        if let Some(artists) = Self::owned(zbus::zvariant::Value::from(vec![artist.to_string()])) {
            data.insert("xesam:artist".into(), artists);
        }
        if let Some(art) = track["artwork"].as_str().filter(|s| s.starts_with("file:")) {
            if let Some(url) = Self::owned(zbus::zvariant::Value::from(art.to_string())) {
                data.insert("mpris:artUrl".into(), url);
            }
        }
        data
    }
    #[zbus(property)]
    fn position(&self) -> i64 {
        self.shared.playback().position as i64 * 1000
    }
    #[zbus(property)]
    fn volume(&self) -> f64 {
        self.shared.playback().volume
    }
    #[zbus(property)]
    fn set_volume(&self, value: f64) {
        self.command(json!({"op":"audio_volume","volume":value}));
    }
    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.
    }
    #[zbus(property)]
    fn set_rate(&self, value: f64) {
        if value == 0. {
            self.pause();
        }
    }
    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.
    }
    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.shared
            .model
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())["queue"]["items"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.can_play()
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.shared.playback().seekable
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        // Transport must stay available while library browsing loads (busy);
        // busy tracks foreground list work, not audio capability.
        self.can_play()
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.can_go_next()
    }
    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
}
