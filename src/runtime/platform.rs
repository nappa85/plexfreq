use super::Shared;
use futures_util::StreamExt;
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
            let name = thread
                .thread()
                .name()
                .unwrap_or("platform-worker")
                .to_owned();
            crate::diagnostics::event(format_args!("shutdown joining {name}"));
            let _ = thread.join();
            crate::diagnostics::event(format_args!("shutdown joined {name}"));
        }
    }
}
pub fn start(shared: Shared, events: mpsc::Sender<Value>) -> Platform {
    let mut threads = Vec::new();
    let recording = shared.clone();
    if let Ok(worker) = thread::Builder::new()
        .name("plexfreq-recording".into())
        .spawn(move || {
            let mut previous = None;
            while !recording.stop.load(std::sync::atomic::Ordering::SeqCst) {
                let result = super::pulse::watch_recording(&recording.stop, |active| {
                    if previous != Some(active) {
                        crate::diagnostics::event(format_args!(
                            "microphone recording active={active}"
                        ));
                        let result = crate::mutex_lock(&recording.audio)
                            .as_ref()
                            .map(|audio| audio.recording(active));
                        crate::diagnostics::event(format_args!(
                            "recording hold active={active} accepted={}",
                            matches!(result, Some(Ok(())))
                        ));
                        if matches!(result, Some(Ok(()))) {
                            previous = Some(active);
                        }
                    }
                });
                if let Err(reason) = result {
                    if recording.stop.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                    crate::diagnostics::event(format_args!(
                        "recording observer unavailable reason={reason}"
                    ));
                }
                for _ in 0..50 {
                    if recording.stop.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        })
    {
        threads.push(worker);
    }
    let bluetooth = shared.clone();
    if let Ok(worker) = thread::Builder::new()
        .name("plexfreq-bluetooth".into())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            let mut policy = crate::bluetooth::PausePolicy::default();
            let mut previous_facts = None;
            let mut observe = |facts| {
                let snapshot = bluetooth.playback();
                if facts != previous_facts {
                    log_bluetooth(&facts, "change");
                    previous_facts = facts.clone();
                }
                if policy.observe(facts, snapshot.loaded) {
                    let result = bluetooth.send(json!({"op":"audio_pause"}));
                    crate::diagnostics::event(format_args!(
                        "Bluetooth disconnect auto-pause occurrence={} accepted={}",
                        snapshot.id,
                        result.is_ok()
                    ));
                }
            };
            runtime.block_on(async {
                while !bluetooth.stop.load(std::sync::atomic::Ordering::SeqCst) {
                    if let Err(reason) = watch_bluetooth(&bluetooth.stop, &mut observe).await {
                        crate::diagnostics::event(format_args!(
                            "Bluetooth observer unavailable reason={reason}"
                        ));
                        observe(None);
                    }
                    for _ in 0..50 {
                        if bluetooth.stop.load(std::sync::atomic::Ordering::SeqCst) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            });
        })
    {
        threads.push(worker);
    }
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
                    crate::diagnostics::event(format_args!("network change wifi={} online={}", facts.0, facts.1));
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
    if crate::diagnostics::enabled() {
        let diagnostic = shared.clone();
        if let Ok(worker) = thread::Builder::new().name("plexfreq-diagnostics".into()).spawn(move || {
            let connection = Connection::system().ok();
            while !diagnostic.stop.load(std::sync::atomic::Ordering::SeqCst) {
                let state = diagnostic.playback();
                let load: Vec<f64> = std::fs::read_to_string("/proc/loadavg").unwrap_or_default()
                    .split_whitespace().take(3).filter_map(|s| s.parse().ok()).collect();
                crate::diagnostics::event(format_args!("platform heartbeat occurrence={} playing={} buffering={} paused={} loaded={} position_ms={} load={load:?}",
                    state.id, state.playing, state.buffering, state.paused, state.loaded, state.position));
                log_bluetooth(&connection.as_ref().and_then(|c| bluetooth_audio(c).ok()), "heartbeat");
                match super::pulse::snapshot(&diagnostic.stop) {
                    Ok(snapshot) => {
                        crate::diagnostics::event(format_args!("PulseAudio {}", snapshot.streams()));
                        crate::diagnostics::event(format_args!("PulseAudio routes {}", snapshot.routes()));
                    }
                    Err(reason) => crate::diagnostics::event(format_args!("PulseAudio snapshot unavailable backend=libpulse reason={reason}")),
                }
                let ticks = if state.playing || state.buffering { 100 } else { 600 };
                for _ in 0..ticks {
                    if diagnostic.stop.load(std::sync::atomic::Ordering::SeqCst) { break; }
                    thread::sleep(Duration::from_millis(100));
                    let next = diagnostic.playback();
                    if (state.id, state.playing, state.buffering, state.paused, state.loaded)
                        != (next.id, next.playing, next.buffering, next.paused, next.loaded) { break; }
                }
            }
        }) { threads.push(worker); }
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
type Objects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;
async fn watch_bluetooth(
    stop: &std::sync::atomic::AtomicBool,
    mut observe: impl FnMut(Option<crate::bluetooth::Facts>),
) -> Result<(), &'static str> {
    let connection = tokio::time::timeout(Duration::from_secs(2), zbus::Connection::system())
        .await
        .map_err(|_| "connect timeout")?
        .map_err(|_| "connect failed")?;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.bluez")
        .map_err(|_| "invalid match")?
        .build();
    let mut stream = tokio::time::timeout(
        Duration::from_secs(2),
        zbus::MessageStream::for_match_rule(rule, &connection, Some(64)),
    )
    .await
    .map_err(|_| "subscribe timeout")?
    .map_err(|_| "subscribe failed")?;
    let result = async {
        // Always drain signals concurrently with RPCs. zbus 4's socket reader
        // otherwise blocks on a full signal queue before delivering the reply.
        while !stop.load(std::sync::atomic::Ordering::SeqCst) {
            let query = tokio::time::timeout(Duration::from_secs(2), bluetooth_query(&connection));
            tokio::pin!(query);
            let mut dirty = false;
            loop {
                if stop.load(std::sync::atomic::Ordering::SeqCst) {
                    return Ok(());
                }
                tokio::select! {
                    facts = &mut query => {
                        observe(facts.ok().and_then(Result::ok));
                        break;
                    }
                    signal = stream.next() => {
                        if !matches!(signal, Some(Ok(_))) { return Err("signal connection lost"); }
                        dirty = true;
                    }
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {},
                }
            }
            if dirty {
                continue;
            }
            let refresh = tokio::time::sleep(Duration::from_secs(2));
            tokio::pin!(refresh);
            loop {
                if stop.load(std::sync::atomic::Ordering::SeqCst) {
                    return Ok(());
                }
                tokio::select! {
                    signal = stream.next() => {
                        if !matches!(signal, Some(Ok(_))) { return Err("signal connection lost"); }
                        break;
                    }
                    _ = &mut refresh => break,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {},
                }
            }
        }
        Ok(())
    }
    .await;
    // Deregister before disconnect, with a draining clone so RemoveMatch's reply
    // cannot sit behind a full signal queue. Cleanup itself has a fixed deadline.
    let mut drain = stream.clone();
    let cleanup = tokio::time::timeout(Duration::from_secs(2), zbus::AsyncDrop::async_drop(stream));
    tokio::pin!(cleanup);
    loop {
        tokio::select! {
            result = &mut cleanup => {
                crate::diagnostics::event(format_args!("Bluetooth observer cleanup completed={}", result.is_ok()));
                break;
            }
            _ = drain.next() => {},
        }
    }
    result
}
fn bluetooth_audio(connection: &Connection) -> zbus::Result<crate::bluetooth::Facts> {
    // The diagnostic worker must not reintroduce an unbounded BlueZ wait during
    // the same outage that the live disconnect observer now handles safely.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), bluetooth_query(connection.inner()))
            .await
            .map_err(|_| zbus::Error::Failure("Bluetooth query timed out".into()))?
    })
}
async fn bluetooth_query(connection: &zbus::Connection) -> zbus::Result<crate::bluetooth::Facts> {
    let proxy = zbus::proxy::Builder::<zbus::Proxy<'_>>::new(connection)
        .destination("org.bluez")?
        .path("/")?
        .interface("org.freedesktop.DBus.ObjectManager")?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await?;
    let objects: Objects = proxy.call("GetManagedObjects", &()).await?;
    Ok(bluetooth_objects(objects))
}
fn bluetooth_objects(objects: Objects) -> crate::bluetooth::Facts {
    let mut facts = crate::bluetooth::Facts::default();
    for (path, interfaces) in &objects {
        if let Some(device) = interfaces.get("org.bluez.Device1") {
            let connected = device
                .get("Connected")
                .and_then(|v| bool::try_from(v).ok())
                .unwrap_or(false);
            let uuids = device
                .get("UUIDs")
                .and_then(|v| v.try_clone().ok())
                .and_then(|v| Vec::<String>::try_from(v).ok())
                .unwrap_or_default();
            if connected && uuids.iter().any(|uuid| crate::bluetooth::audio_uuid(uuid)) {
                facts.connected.insert(path.to_string());
            }
        }
        if let Some(transport) = interfaces.get("org.bluez.MediaTransport1") {
            let state = transport
                .get("State")
                .and_then(|v| <&str>::try_from(v).ok());
            match state {
                Some("idle") => facts.idle_transports += 1,
                Some("pending") => facts.pending_transports += 1,
                Some("active") => facts.active_transports += 1,
                _ => {}
            }
            let active = state.is_some_and(|state| matches!(state, "active" | "pending"));
            if let Some(device) = transport
                .get("Device")
                .and_then(|v| v.try_clone().ok())
                .and_then(|v| OwnedObjectPath::try_from(v).ok())
            {
                facts.transports.insert(device.to_string());
                if active {
                    facts.active.insert(device.to_string());
                }
            }
        }
    }
    facts
}

fn log_bluetooth(facts: &Option<crate::bluetooth::Facts>, reason: &str) {
    match facts {
        Some(facts) => crate::diagnostics::event(format_args!("Bluetooth {reason} audio_connected={} tracked_active={} transports_idle={} pending={} active={}",
            facts.connected.len(), facts.active.len(), facts.idle_transports, facts.pending_transports, facts.active_transports)),
        None => crate::diagnostics::event(format_args!("Bluetooth {reason} facts unavailable (not treated as disconnect)")),
    }
}
#[cfg(test)]
mod bluetooth_tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        sync::{
            atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
            Arc, Mutex,
        },
        time::Instant,
    };
    struct Manager {
        connected: Arc<AtomicBool>,
        transport: Arc<AtomicBool>,
        stalled: Arc<AtomicBool>,
        reads: Arc<AtomicUsize>,
    }
    #[zbus::interface(name = "org.freedesktop.DBus.ObjectManager")]
    impl Manager {
        async fn get_managed_objects(
            &self,
        ) -> HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if self.stalled.load(Ordering::SeqCst) {
                futures_util::future::pending::<()>().await;
            }
            let properties = HashMap::from([
                (
                    "Connected".into(),
                    OwnedValue::from(self.connected.load(Ordering::SeqCst)),
                ),
                (
                    "UUIDs".into(),
                    OwnedValue::try_from(zbus::zvariant::Value::from(vec![
                        "0000110b-0000-1000-8000-00805f9b34fb".to_string(),
                    ]))
                    .unwrap(),
                ),
            ]);
            let mut objects = HashMap::from([(
                OwnedObjectPath::try_from("/org/bluez/hci0/dev_fixture").unwrap(),
                HashMap::from([("org.bluez.Device1".into(), properties)]),
            )]);
            if self.transport.load(Ordering::SeqCst) {
                objects.insert(
                    OwnedObjectPath::try_from("/org/bluez/hci0/dev_fixture/fd0").unwrap(),
                    HashMap::from([(
                        "org.bluez.MediaTransport1".into(),
                        HashMap::from([
                            (
                                "Device".into(),
                                OwnedValue::try_from(zbus::zvariant::Value::from(
                                    OwnedObjectPath::try_from("/org/bluez/hci0/dev_fixture")
                                        .unwrap(),
                                ))
                                .unwrap(),
                            ),
                            (
                                "State".into(),
                                OwnedValue::try_from(zbus::zvariant::Value::from("idle")).unwrap(),
                            ),
                        ]),
                    )]),
                );
            }
            objects
        }
    }
    struct Bus(std::process::Child);
    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    struct Environment(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for Environment {
        fn drop(&mut self) {
            for (key, value) in self.0.drain(..) {
                if let Some(value) = value {
                    std::env::set_var(key, value)
                } else {
                    std::env::remove_var(key)
                }
            }
        }
    }
    #[test]
    fn bluez_disconnect_signal_pauses_audio_even_with_a_full_metadata_queue() {
        let mut process = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("dbus-daemon fixture is required");
        let mut address = String::new();
        BufReader::new(process.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let _bus = Bus(process);
        let address = address.trim();
        let _environment = Environment(
            ["DBUS_SYSTEM_BUS_ADDRESS", "DBUS_SESSION_BUS_ADDRESS"]
                .into_iter()
                .map(|key| (key, std::env::var_os(key)))
                .collect(),
        );
        for key in ["DBUS_SYSTEM_BUS_ADDRESS", "DBUS_SESSION_BUS_ADDRESS"] {
            std::env::set_var(key, address);
        }
        let connected = Arc::new(AtomicBool::new(true));
        let transport = Arc::new(AtomicBool::new(true));
        let stalled = Arc::new(AtomicBool::new(false));
        let reads = Arc::new(AtomicUsize::new(0));
        let service = ConnectionBuilder::address(address)
            .unwrap()
            .name("org.bluez")
            .unwrap()
            .serve_at(
                "/",
                Manager {
                    connected: connected.clone(),
                    transport: transport.clone(),
                    stalled: stalled.clone(),
                    reads: reads.clone(),
                },
            )
            .unwrap()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.wav");
        let frames = 480000u32;
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(36 + frames * 2).to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16u32.to_le_bytes()).unwrap();
        file.write_all(&1u16.to_le_bytes()).unwrap();
        file.write_all(&1u16.to_le_bytes()).unwrap();
        file.write_all(&48000u32.to_le_bytes()).unwrap();
        file.write_all(&96000u32.to_le_bytes()).unwrap();
        file.write_all(&2u16.to_le_bytes()).unwrap();
        file.write_all(&16u16.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&(frames * 2).to_le_bytes()).unwrap();
        file.set_len(44 + u64::from(frames) * 2).unwrap();
        drop(file);
        let engine = crate::audio::Engine::new(crate::audio::Sink::Fake).unwrap();
        engine
            .handle
            .load(
                crate::audio::Source {
                    id: 1,
                    url: url::Url::from_file_path(path).unwrap().into(),
                    duration: 10000,
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        let (requests, _receiver) = mpsc::sync_channel(1);
        requests.try_send(json!({"op":"blocked"})).unwrap();
        let shared = Shared {
            requests,
            audio: Arc::new(Mutex::new(Some(engine.handle.clone()))),
            model: Arc::new(Mutex::new(json!({"track":null,"queue":{"items":[]}}))),
            control: Arc::new(Mutex::new(None)),
            stop: Arc::new(AtomicBool::new(false)),
            foreground: Arc::new(AtomicUsize::new(1)),
            pages: Arc::new(AtomicUsize::new(0)),
            view: Arc::new(AtomicU64::new(0)),
            plans: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            shown: Arc::new(AtomicU64::new(0)),
            seeks: Arc::new(Mutex::new(Vec::new())),
            checkpoint_overflow: Arc::new(Mutex::new(Vec::new())),
        };
        let (events, _output) = mpsc::channel();
        let platform = start(shared.clone(), events);
        let end = Instant::now() + Duration::from_secs(5);
        while !engine.handle.snapshot().playing || reads.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(30));
        // Profile removal must pause even while Device1 remains connected.
        transport.store(false, Ordering::SeqCst);
        service
            .emit_signal(
                None::<&str>,
                "/",
                "org.freedesktop.DBus.ObjectManager",
                "InterfacesRemoved",
                &(
                    OwnedObjectPath::try_from("/org/bluez/hci0/dev_fixture/fd0").unwrap(),
                    vec!["org.bluez.MediaTransport1"],
                ),
            )
            .unwrap();
        let end = Instant::now() + Duration::from_secs(3);
        while !engine.handle.snapshot().paused {
            assert!(
                Instant::now() < end,
                "Audio profile removal did not pause audio"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(connected.load(Ordering::SeqCst));
        // Restore the profile and explicitly resume for the separate full-device test.
        transport.store(true, Ordering::SeqCst);
        service
            .emit_signal(
                None::<&str>,
                "/org/bluez/hci0/dev_fixture",
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                &(
                    "org.bluez.Device1",
                    HashMap::from([("Connected", true)]),
                    Vec::<String>::new(),
                ),
            )
            .unwrap();
        thread::sleep(Duration::from_millis(100));
        assert!(engine.handle.snapshot().paused);
        engine.handle.play().unwrap();
        let end = Instant::now() + Duration::from_secs(3);
        while !engine.handle.snapshot().playing {
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(10));
        }
        connected.store(false, Ordering::SeqCst);
        service
            .emit_signal(
                None::<&str>,
                "/org/bluez/hci0/dev_fixture",
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                &(
                    "org.bluez.Device1",
                    HashMap::from([("Connected", false)]),
                    Vec::<String>::new(),
                ),
            )
            .unwrap();
        let end = Instant::now() + Duration::from_secs(3);
        while !engine.handle.snapshot().paused {
            assert!(
                Instant::now() < end,
                "Bluetooth disconnect did not pause audio"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(shared.busy());
        assert!(!crate::mutex_lock(&shared.checkpoint_overflow).is_empty());
        // A silent RPC plus more signals than the 64-message subscription cap
        // used to block the socket reader and then shutdown's RemoveMatch reply.
        stalled.store(true, Ordering::SeqCst);
        let before = reads.load(Ordering::SeqCst);
        service
            .emit_signal(
                None::<&str>,
                "/org/bluez/hci0/dev_fixture",
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                &(
                    "org.bluez.Device1",
                    HashMap::from([("Connected", false)]),
                    Vec::<String>::new(),
                ),
            )
            .unwrap();
        let end = Instant::now() + Duration::from_secs(3);
        while reads.load(Ordering::SeqCst) == before {
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(10));
        }
        for _ in 0..512 {
            service
                .emit_signal(
                    None::<&str>,
                    "/org/bluez/hci0/dev_fixture",
                    "org.freedesktop.DBus.Properties",
                    "PropertiesChanged",
                    &(
                        "org.bluez.Device1",
                        HashMap::from([("Connected", false)]),
                        Vec::<String>::new(),
                    ),
                )
                .unwrap();
        }
        let end = Instant::now() + Duration::from_secs(3);
        while reads.load(Ordering::SeqCst) <= before + 1 {
            assert!(
                Instant::now() < end,
                "Silent BlueZ query did not time out and reconcile queued signals"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let shutdown = Instant::now();
        shared.stop.store(true, Ordering::SeqCst);
        drop(platform);
        assert!(
            shutdown.elapsed() < Duration::from_secs(3),
            "Stalled BlueZ RPC/signal flood blocked shutdown"
        );
        drop(service);
        drop(engine);
    }
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
