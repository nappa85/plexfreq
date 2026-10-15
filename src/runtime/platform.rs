use super::{Outgoing, Shared};
use crate::rt::{self, Halt, Halter, Task};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread,
    time::Duration,
};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

/// Deadline for any single D-Bus connect, subscribe or method call.
const DEADLINE: Duration = Duration::from_secs(2);
/// Wait before reconnecting an observer whose bus or service failed.
const RECONNECT: Duration = Duration::from_secs(5);
/// MPRIS re-derives its properties at least this often, as a fallback for a
/// change that bypassed the model/transport notifications.
const MPRIS_REFRESH: Duration = Duration::from_secs(1);
/// How long `Platform::drop` lets halted tasks finish their async cleanup
/// (bounded by `DEADLINE`) before aborting them.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// Platform observers. D-Bus/network/diagnostic work runs as tasks on the
/// shared runtime; only the libpulse recording subscription keeps a thread.
pub struct Platform {
    stop: Arc<AtomicBool>,
    halter: Halter,
    tasks: Vec<(&'static str, Task)>,
    threads: Vec<thread::JoinHandle<()>>,
    /// MPRIS loop iterations, for wakeup accounting.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) mpris_wakes: Arc<AtomicU64>,
}
impl Drop for Platform {
    fn drop(&mut self) {
        // Joining requires the observers to see stop, even when the owner is
        // unwinding before it could raise the flag itself. All tasks are
        // halted together, so their bounded cleanups overlap.
        self.stop.store(true, Ordering::SeqCst);
        self.halter.halt();
        for (name, task) in self.tasks.drain(..) {
            crate::diagnostics::event(format_args!("shutdown joining {name}"));
            task.finish(SHUTDOWN_GRACE);
            crate::diagnostics::event(format_args!("shutdown joined {name}"));
        }
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

/// A D-Bus connection. zbus uses its `tokio` feature, so its internal tasks
/// run on the shared runtime (the caller's context) instead of private
/// executor/reactor threads; dropping the last clone closes the socket.
struct Bus {
    connection: zbus::Connection,
}
impl Bus {
    async fn connect(builder: zbus::Result<zbus::connection::Builder<'_>>) -> zbus::Result<Self> {
        Ok(Self {
            connection: builder?.build().await?,
        })
    }
    async fn system() -> zbus::Result<Self> {
        Self::connect(zbus::connection::Builder::system()).await
    }
}

/// Sleep unless halted first; returns whether the owner halted.
async fn pause(halt: &Halt, duration: Duration) -> bool {
    tokio::select! {
        _ = halt.wait() => true,
        _ = tokio::time::sleep(duration) => halt.halted(),
    }
}

pub fn start(shared: Shared, events: mpsc::Sender<Outgoing>) -> Platform {
    let halter = rt::halter();
    let mut tasks = Vec::new();
    let mut threads = Vec::new();
    let recording = shared.clone();
    // libpulse's mainloop is blocking and stays on its own thread.
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
    tasks.push((
        "bluetooth observer",
        rt::spawn(bluetooth_observer(shared.clone(), halter.subscribe())),
    ));
    tasks.push((
        "network observer",
        rt::spawn(network_observer(shared.clone(), halter.subscribe())),
    ));
    if crate::diagnostics::enabled() {
        tasks.push((
            "diagnostic heartbeat",
            rt::spawn(heartbeat(shared.clone(), halter.subscribe())),
        ));
    }
    let mpris_wakes = Arc::new(AtomicU64::new(0));
    tasks.push((
        "MPRIS",
        rt::spawn(mpris(
            shared.clone(),
            events,
            halter.subscribe(),
            mpris_wakes.clone(),
        )),
    ));
    Platform {
        stop: shared.stop.clone(),
        halter,
        tasks,
        threads,
        mpris_wakes,
    }
}

async fn bluetooth_observer(bluetooth: Shared, halt: Halt) {
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
    while !halt.halted() {
        let watched = watch(
            &halt,
            bluetooth_rules(),
            DEADLINE,
            Duration::from_secs(2),
            |connection| async move { bluetooth_query(&connection).await },
            &mut observe,
        );
        if let Err(reason) = watched.await {
            crate::diagnostics::event(format_args!(
                "Bluetooth observer unavailable reason={reason}"
            ));
            observe(None);
        }
        if pause(&halt, RECONNECT).await {
            break;
        }
    }
}

async fn network_observer(network: Shared, halt: Halt) {
    let mut previous = None;
    let mut unavailable = false;
    let mut observe = |facts: Option<(bool, bool)>| {
        // Unanswered facts are unknown, not offline: reporting them as
        // disconnected would force cache-only playback and skip
        // timelines. Keep the last known state (initially unknown).
        let Some(facts) = facts else {
            if !unavailable {
                crate::diagnostics::event(format_args!(
                    "network facts unavailable (not treated as offline)"
                ));
                unavailable = true;
            }
            return;
        };
        unavailable = false;
        if previous != Some(facts) {
            crate::diagnostics::event(format_args!(
                "network change wifi={} online={}",
                facts.0, facts.1
            ));
            network.hint_network(facts.0);
            let _ = network.send(
                json!({"op":"network_state","wifi":facts.0,"online":facts.1,"live_hint":true}),
            );
            previous = Some(facts);
        }
    };
    while !halt.halted() {
        // ConnMan and NetworkManager signal changes; the refresh is
        // only a fallback for a missed or unsubscribed signal.
        let watched = watch(
            &halt,
            network_rules(),
            NETWORK_DEADLINE,
            Duration::from_secs(15),
            network_query,
            &mut observe,
        );
        if let Err(reason) = watched.await {
            crate::diagnostics::event(format_args!("network observer unavailable reason={reason}"));
            observe(None);
        }
        if pause(&halt, RECONNECT).await {
            break;
        }
    }
}

/// Persistent-log heartbeat: transport state, load, BlueZ and PulseAudio
/// facts every 10 s while playing (60 s idle), and immediately on a transport
/// state change, which arrives as an audio change notification.
async fn heartbeat(diagnostic: Shared, halt: Halt) {
    let connection = match tokio::time::timeout(DEADLINE, Bus::system()).await {
        Ok(Ok(bus)) => Some(bus),
        _ => None,
    };
    let mut changes = diagnostic.audio_changes();
    let key = |s: &crate::audio::Snapshot| (s.id, s.playing, s.buffering, s.paused, s.loaded);
    while !halt.halted() {
        let state = diagnostic.playback();
        let load: Vec<f64> = std::fs::read_to_string("/proc/loadavg")
            .unwrap_or_default()
            .split_whitespace()
            .take(3)
            .filter_map(|s| s.parse().ok())
            .collect();
        crate::diagnostics::event(format_args!("platform heartbeat occurrence={} playing={} buffering={} paused={} loaded={} position_ms={} load={load:?}",
            state.id, state.playing, state.buffering, state.paused, state.loaded, state.position));
        let facts = match &connection {
            // The diagnostic task must not reintroduce an unbounded BlueZ wait
            // during the same outage that the live observer handles safely.
            Some(bus) => tokio::time::timeout(DEADLINE, bluetooth_query(&bus.connection))
                .await
                .ok()
                .and_then(Result::ok),
            None => None,
        };
        log_bluetooth(&facts, "heartbeat");
        // libpulse is blocking; its sample is bounded and checks `stop`.
        let stop = diagnostic.stop.clone();
        match rt::unblock(move || super::pulse::snapshot(&stop)).await {
            Ok(snapshot) => {
                crate::diagnostics::event(format_args!("PulseAudio {}", snapshot.streams()));
                crate::diagnostics::event(format_args!("PulseAudio routes {}", snapshot.routes()));
            }
            Err(reason) => crate::diagnostics::event(format_args!(
                "PulseAudio snapshot unavailable backend=libpulse reason={reason}"
            )),
        }
        let period = Duration::from_secs(if state.playing || state.buffering {
            10
        } else {
            60
        });
        let next = tokio::time::sleep(period);
        tokio::pin!(next);
        loop {
            if changes.is_none() {
                changes = diagnostic.audio_changes();
            }
            tokio::select! {
                _ = halt.wait() => return,
                _ = &mut next => break,
                _ = rt::changed(&mut changes) => {
                    if key(&diagnostic.playback()) != key(&state) {
                        break;
                    }
                }
            }
        }
    }
}

/// MPRIS service. Properties are re-derived when the audio transport, the
/// presented model or a seek notifies, with a 1 s fallback refresh; position
/// alone never wakes it (clients read Position on demand).
async fn mpris(bus: Shared, events: mpsc::Sender<Outgoing>, halt: Halt, wakes: Arc<AtomicU64>) {
    let path = "/org/mpris/MediaPlayer2";
    let builder = zbus::connection::Builder::session()
        .and_then(|b| b.name("org.mpris.MediaPlayer2.plexfreq"))
        .and_then(|b| b.serve_at(path, Root { events }))
        .and_then(|b| {
            b.serve_at(
                path,
                Player {
                    shared: bus.clone(),
                },
            )
        });
    let connection = tokio::select! {
        _ = halt.wait() => return,
        connection = Bus::connect(builder) => connection,
    };
    let Ok(connection) = connection else {
        return;
    };
    let mut model = bus.revisions();
    let mut audio = bus.audio_changes();
    let mut previous = Value::Null;
    loop {
        wakes.fetch_add(1, Ordering::Relaxed);
        if audio.is_none() {
            audio = bus.audio_changes();
        }
        model.borrow_and_update();
        if let Some(audio) = audio.as_mut() {
            audio.borrow_and_update();
        }
        let seeks: Vec<i64> = crate::mutex_lock(&bus.seeks).drain(..).collect();
        for position in seeks {
            let _ = connection
                .connection
                .emit_signal(
                    None::<&str>,
                    path,
                    "org.mpris.MediaPlayer2.Player",
                    "Seeked",
                    &(position,),
                )
                .await;
        }
        let state = bus.playback();
        let changed = {
            let model = crate::mutex_lock(&bus.model);
            json!({"playing":state.playing,"paused":state.paused,"volume":state.volume,"seekable":state.seekable,"id":state.id,"track":model["track"],"loop":model["queue"]["repeat"],"shuffle":model["queue"]["shuffled"],"queueLength":model["queue"]["items"].as_array().map_or(0,Vec::len),"current":model["queue"]["current"]})
        };
        if changed != previous {
            let properties = Player {
                shared: bus.clone(),
            }
            .changed_properties();
            let _ = connection
                .connection
                .emit_signal(
                    None::<&str>,
                    path,
                    "org.freedesktop.DBus.Properties",
                    "PropertiesChanged",
                    &(
                        "org.mpris.MediaPlayer2.Player",
                        properties,
                        Vec::<String>::new(),
                    ),
                )
                .await;
            previous = changed;
        }
        tokio::select! {
            _ = halt.wait() => break,
            _ = model.changed() => {},
            _ = rt::changed(&mut audio) => {},
            _ = tokio::time::sleep(MPRIS_REFRESH) => {},
        }
    }
}
type Objects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;
/// Bounded D-Bus observer shared by the BlueZ and network tasks. Every
/// connect/subscribe/query has a deadline and every wait also selects on
/// `halt`, so neither a silent service nor a signal flood can delay shutdown.
/// `query` is bounded by `deadline` and repeated after any matching signal, or
/// after `refresh`. A failed or late query is observed as `None` (unknown).
async fn watch<T, F>(
    halt: &Halt,
    rules: Vec<zbus::MatchRule<'static>>,
    deadline: Duration,
    refresh: Duration,
    query: impl Fn(zbus::Connection) -> F,
    mut observe: impl FnMut(Option<T>),
) -> Result<(), &'static str>
where
    F: Future<Output = zbus::Result<T>>,
{
    let bus = tokio::select! {
        _ = halt.wait() => return Ok(()),
        bus = tokio::time::timeout(DEADLINE, Bus::system()) => bus,
    }
    .map_err(|_| "connect timeout")?
    .map_err(|_| "connect failed")?;
    let connection = bus.connection.clone();
    let mut streams = Vec::new();
    let mut result = Ok(());
    for rule in rules {
        match tokio::time::timeout(
            DEADLINE,
            zbus::MessageStream::for_match_rule(rule, &connection, Some(64)),
        )
        .await
        {
            Ok(Ok(stream)) => streams.push(stream),
            Ok(Err(_)) => result = Err("subscribe failed"),
            Err(_) => result = Err("subscribe timeout"),
        }
        if result.is_err() {
            break;
        }
    }
    let mut signals = futures_util::stream::select_all(streams.iter().cloned());
    if result.is_ok() {
        result = async {
            // Always drain signals concurrently with RPCs. zbus 4's socket reader
            // otherwise blocks on a full signal queue before delivering the reply.
            while !halt.halted() {
                let reply = tokio::time::timeout(deadline, query(connection.clone()));
                tokio::pin!(reply);
                let mut dirty = false;
                loop {
                    tokio::select! {
                        _ = halt.wait() => return Ok(()),
                        facts = &mut reply => {
                            observe(facts.ok().and_then(Result::ok));
                            break;
                        }
                        signal = signals.next() => {
                            if !matches!(signal, Some(Ok(_))) { return Err("signal connection lost"); }
                            dirty = true;
                        }
                    }
                }
                if dirty {
                    continue;
                }
                tokio::select! {
                    _ = halt.wait() => return Ok(()),
                    signal = signals.next() => {
                        if !matches!(signal, Some(Ok(_))) { return Err("signal connection lost"); }
                    }
                    _ = tokio::time::sleep(refresh) => {},
                }
            }
            Ok(())
        }
        .await;
    }
    // Deregister before disconnect while draining clones, so RemoveMatch's reply
    // cannot sit behind a full signal queue. Cleanup itself has a fixed deadline.
    let cleanup = tokio::time::timeout(
        DEADLINE,
        futures_util::future::join_all(streams.into_iter().map(zbus::AsyncDrop::async_drop)),
    );
    tokio::pin!(cleanup);
    loop {
        tokio::select! {
            done = &mut cleanup => {
                crate::diagnostics::event(format_args!("D-Bus observer cleanup completed={}", done.is_ok()));
                break;
            }
            _ = signals.next() => {},
        }
    }
    drop(signals);
    drop(connection);
    drop(bus);
    result
}
fn signal_rule<'a>(
    sender: &'a str,
    narrow: impl FnOnce(zbus::MatchRuleBuilder<'a>) -> zbus::Result<zbus::MatchRuleBuilder<'a>>,
) -> Option<zbus::MatchRule<'a>> {
    let builder = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(sender)
        .ok()?;
    narrow(builder).ok().map(zbus::MatchRuleBuilder::build)
}
fn bluetooth_rules() -> Vec<zbus::MatchRule<'static>> {
    signal_rule("org.bluez", Ok).into_iter().collect()
}
fn network_rules() -> Vec<zbus::MatchRule<'static>> {
    // ConnMan reports default-route/state changes on its Manager (ServicesChanged,
    // PropertyChanged); NetworkManager on its root object's PropertiesChanged.
    [
        signal_rule("net.connman", |b| b.interface("net.connman.Manager")),
        signal_rule("org.freedesktop.NetworkManager", |b| {
            b.path("/org/freedesktop/NetworkManager")?
                .interface("org.freedesktop.DBus.Properties")
        }),
    ]
    .into_iter()
    .flatten()
    .collect()
}
async fn proxy<'a>(
    connection: &zbus::Connection,
    destination: &'a str,
    path: &'a str,
    interface: &'a str,
) -> zbus::Result<zbus::Proxy<'a>> {
    zbus::proxy::Builder::new(connection)
        .destination(destination)?
        .path(path)?
        .interface(interface)?
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
}
async fn bluetooth_query(connection: &zbus::Connection) -> zbus::Result<crate::bluetooth::Facts> {
    let proxy = proxy(
        connection,
        "org.bluez",
        "/",
        "org.freedesktop.DBus.ObjectManager",
    )
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
/// D-Bus address variables are process-global: serialize fixtures using them.
#[cfg(test)]
static BUS_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[cfg(test)]
/// zbus (tokio feature) runs its blocking API on a private current-thread
/// runtime that only progresses while some thread blocks on it, so blocking
/// fixture services would never answer between calls. Keep it driven.
pub(crate) fn drive_blocking_zbus() {
    static DRIVER: std::sync::Once = std::sync::Once::new();
    DRIVER.call_once(|| {
        std::thread::spawn(|| zbus::block_on(std::future::pending::<()>()));
    });
}

#[cfg(test)]
mod bluetooth_tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        sync::atomic::AtomicUsize,
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
        let _serial = BUS_ENV.lock().unwrap_or_else(|p| p.into_inner());
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
        crate::runtime::platform::drive_blocking_zbus();
        let service = zbus::blocking::ConnectionBuilder::address(address)
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
        requests
            .try_send(crate::runtime::WorkerMsg::Shutdown)
            .unwrap();
        let shared = Shared::new(requests, json!({"track":null,"queue":{"items":[]}}));
        *crate::mutex_lock(&shared.audio) = Some(engine.handle.clone());
        shared.foreground.store(1, Ordering::SeqCst);
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
/// Room for a silent ConnMan's deadline plus the NetworkManager fallback.
const NETWORK_DEADLINE: Duration = Duration::from_secs(5);
type Services = Vec<(OwnedObjectPath, HashMap<String, OwnedValue>)>;
async fn network_query(connection: zbus::Connection) -> zbus::Result<(bool, bool)> {
    let connman = async {
        proxy(&connection, "net.connman", "/", "net.connman.Manager")
            .await?
            .call::<_, _, Services>("GetServices", &())
            .await
    };
    // Bounded separately so a hung ConnMan cannot hide NetworkManager facts.
    if let Ok(Ok(services)) = tokio::time::timeout(DEADLINE, connman).await {
        return Ok(connman_facts(&services));
    }
    let manager = async {
        let proxy = proxy(
            &connection,
            "org.freedesktop.NetworkManager",
            "/org/freedesktop/NetworkManager",
            "org.freedesktop.NetworkManager",
        )
        .await?;
        let kind: String = proxy.get_property("PrimaryConnectionType").await?;
        let state: u32 = proxy.get_property("State").await?;
        Ok((kind == "802-11-wireless", state >= 50))
    };
    tokio::time::timeout(DEADLINE, manager)
        .await
        .map_err(|_| zbus::Error::Failure("NetworkManager query timed out".into()))?
}
fn connman_facts(services: &Services) -> (bool, bool) {
    // ConnMan returns favourite-first: the first ready/online service is the
    // default route. wifi_only must reflect that primary route, not any
    // secondary interface, otherwise downloads could run over cellular while a
    // dormant wifi service is also listed.
    for (_, properties) in services {
        let state = properties
            .get("State")
            .and_then(|v| <&str>::try_from(v).ok())
            .unwrap_or("");
        if matches!(state, "ready" | "online") {
            let kind = properties
                .get("Type")
                .and_then(|v| <&str>::try_from(v).ok())
                .unwrap_or("");
            return (kind == "wifi", true);
        }
    }
    (false, false)
}
struct Root {
    events: mpsc::Sender<Outgoing>,
}
#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {
        let _ = self.events.send(json!({"kind":"raise"}).into());
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
    /// The PropertiesChanged payload (Position is never emitted, per MPRIS).
    fn changed_properties(&self) -> HashMap<&'static str, OwnedValue> {
        let state = self.shared.playback();
        let mut properties = HashMap::new();
        for (name, value) in [
            ("PlaybackStatus", Self::owned(self.playback_status().into())),
            ("Metadata", Self::owned(self.metadata().into())),
            ("LoopStatus", Self::owned(self.loop_status().into())),
        ] {
            if let Some(value) = value {
                properties.insert(name, value);
            }
        }
        properties.insert("Shuffle", OwnedValue::from(self.shuffle()));
        properties.insert("Volume", OwnedValue::from(state.volume));
        properties.insert("CanPlay", OwnedValue::from(self.can_play()));
        properties.insert("CanPause", OwnedValue::from(self.can_pause()));
        properties.insert("CanGoNext", OwnedValue::from(self.can_go_next()));
        properties.insert("CanGoPrevious", OwnedValue::from(self.can_go_previous()));
        properties.insert("CanSeek", OwnedValue::from(state.seekable));
        properties
    }
}
#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn next(&self) {
        if self.can_go_next() {
            let _ = self.shared.send_paused(
                json!({"op":"next","automatic":false}),
                !self.shared.playback().playing,
            );
        }
    }
    fn previous(&self) {
        if self.can_go_previous() {
            let _ = self
                .shared
                .send_paused(json!({"op":"previous"}), !self.shared.playback().playing);
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
        // MPRIS: Seek has no effect when CanSeek is false. Past the end means
        // next only when the length is known; zero is an unknown length.
        let state = self.shared.playback();
        if !state.seekable {
            return;
        }
        let position = (state.position as i64 * 1000).saturating_add(offset);
        if state.duration > 0 && position > state.duration as i64 * 1000 {
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
        use crate::queue::Repeat;
        let repeat = crate::mutex_lock(&self.shared.model)["queue"]["repeat"]
            .as_str()
            .and_then(|mode| mode.parse().ok());
        match repeat {
            Some(Repeat::One) => "Track",
            Some(Repeat::All) => "Playlist",
            Some(Repeat::Off) | None => "None",
        }
        .into()
    }
    #[zbus(property)]
    fn set_loop_status(&self, value: &str) {
        use crate::queue::Repeat;
        let mode = match value {
            "Track" => Repeat::One,
            "Playlist" => Repeat::All,
            "None" => Repeat::Off,
            _ => return,
        };
        self.command(json!({"op":"repeat","mode":mode}));
    }
    #[zbus(property)]
    fn shuffle(&self) -> bool {
        crate::mutex_lock(&self.shared.model)["queue"]["shuffled"]
            .as_bool()
            .unwrap_or(false)
    }
    #[zbus(property)]
    fn set_shuffle(&self, value: bool) {
        self.command(json!({"op":"shuffle","enabled":value}));
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let model = crate::mutex_lock(&self.shared.model);
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
        crate::mutex_lock(&self.shared.model)["queue"]["items"]
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

#[cfg(test)]
mod review_tests {
    use super::*;

    // Review: MPRIS `Seek` says "If the CanSeek property is false, this has no
    // effect", but Player::seek compares against `duration * 1000` before any
    // seekability check. With no decoder/unknown length (duration 0) every
    // positive offset is "past the end" and calls next(), so a lock-screen or
    // headset "skip forward 10 s" on a restored/unseekable track changes track.
    #[test]
    fn mpris_seek_on_unseekable_or_unknown_length_track_does_not_skip() {
        let (requests, receiver) = mpsc::sync_channel(8);
        let shared = Shared::new(
            requests,
            json!({
                "track": {"ratingKey": "1", "duration": 0},
                "queue": {"items": [{"ratingKey": "1"}, {"ratingKey": "2"}]}
            }),
        );
        let player = Player { shared };
        assert!(!player.can_seek());
        player.seek(10_000_000);
        let sent: Vec<&str> = receiver.try_iter().map(|m| m.describe()).collect();
        assert!(
            sent.is_empty(),
            "Seek with CanSeek=false must have no effect, but sent {sent:?}"
        );
    }
}

#[cfg(test)]
mod mpris_tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        time::Instant,
    };
    struct Daemon(std::process::Child);
    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    type Changed = (String, HashMap<String, OwnedValue>, Vec<String>);

    fn wav(path: &std::path::Path, seconds: u32) {
        let frames = 48000 * seconds;
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(36 + frames * 2).to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        for value in [16u32.to_le_bytes().to_vec(), vec![1, 0, 1, 0]] {
            file.write_all(&value).unwrap();
        }
        file.write_all(&48000u32.to_le_bytes()).unwrap();
        file.write_all(&96000u32.to_le_bytes()).unwrap();
        file.write_all(&2u16.to_le_bytes()).unwrap();
        file.write_all(&16u16.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&(frames * 2).to_le_bytes()).unwrap();
        file.set_len(44 + u64::from(frames) * 2).unwrap();
    }

    /// State changes reach MPRIS clients as PropertiesChanged without the
    /// former 100 ms polling: neither an idle nor a playing (position-only
    /// changing) player wakes the service at 10 Hz.
    #[test]
    fn state_changes_emit_promptly_and_idle_player_does_not_poll() {
        let _serial = BUS_ENV.lock().unwrap_or_else(|p| p.into_inner());
        let mut process = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("dbus-daemon fixture is required");
        let mut address = String::new();
        BufReader::new(process.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let _daemon = Daemon(process);
        let address = address.trim().to_string();
        let saved: Vec<_> = ["DBUS_SYSTEM_BUS_ADDRESS", "DBUS_SESSION_BUS_ADDRESS"]
            .into_iter()
            .map(|key| (key, std::env::var_os(key)))
            .collect();
        for (key, _) in &saved {
            std::env::set_var(key, &address);
        }
        // Client side: collect PropertiesChanged from the player object.
        crate::runtime::platform::drive_blocking_zbus();
        let client = zbus::blocking::ConnectionBuilder::address(address.as_str())
            .unwrap()
            .build()
            .unwrap();
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface("org.freedesktop.DBus.Properties")
            .unwrap()
            .member("PropertiesChanged")
            .unwrap()
            .path("/org/mpris/MediaPlayer2")
            .unwrap()
            .build();
        let iterator =
            zbus::blocking::MessageIterator::for_match_rule(rule, &client, None).unwrap();
        let (signals, received) = mpsc::channel();
        thread::spawn(move || {
            for message in iterator.flatten() {
                if let Ok(body) = message.body().deserialize::<Changed>() {
                    if signals.send((Instant::now(), body.1)).is_err() {
                        break;
                    }
                }
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.wav");
        wav(&path, 20);
        let engine = crate::audio::Engine::new(crate::audio::Sink::Fake).unwrap();
        let (requests, _requests) = mpsc::sync_channel(8);
        let shared = Shared::new(requests, json!({"track":null,"queue":{"items":[]}}));
        *crate::mutex_lock(&shared.audio) = Some(engine.handle.clone());
        let (events, _events) = mpsc::channel();
        let platform = start(shared.clone(), events);
        let dbus = zbus::blocking::fdo::DBusProxy::new(&client).unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while !dbus
            .name_has_owner("org.mpris.MediaPlayer2.plexfreq".try_into().unwrap())
            .unwrap()
        {
            assert!(Instant::now() < end, "MPRIS name was not acquired");
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(300));
        // Idle: only the 1 s fallback may wake the service.
        let wakes = platform.mpris_wakes.load(Ordering::SeqCst);
        thread::sleep(Duration::from_secs(2));
        let idle = platform.mpris_wakes.load(Ordering::SeqCst) - wakes;
        assert!(idle <= 4, "idle MPRIS woke {idle} times in 2 s");
        let wait_for = |what: &str, check: &dyn Fn(&HashMap<String, OwnedValue>) -> bool| {
            let end = Instant::now() + Duration::from_secs(3);
            loop {
                let left = end.saturating_duration_since(Instant::now());
                let (at, properties) = received
                    .recv_timeout(left)
                    .unwrap_or_else(|_| panic!("no PropertiesChanged for {what}"));
                if check(&properties) {
                    return at;
                }
            }
        };
        // Model changes (track metadata) notify immediately. Repeat at
        // staggered offsets so a 1 s fallback alone cannot pass by luck.
        for (index, offset) in [0u64, 330, 660].into_iter().enumerate() {
            thread::sleep(Duration::from_millis(offset));
            while received.try_recv().is_ok() {}
            let title = format!("Fixture {index}");
            let changed = Instant::now();
            shared.merge(&json!({"track":{"ratingKey":"1","title":title,"duration":20000}}));
            let at = wait_for("metadata", &|properties| {
                properties.get("Metadata").is_some_and(|metadata| {
                    format!("{metadata:?}").contains(&format!("Fixture {index}"))
                })
            });
            let latency = at.duration_since(changed);
            assert!(
                latency < Duration::from_millis(400),
                "metadata change took {latency:?}"
            );
        }
        // Transport changes come from the audio actor's change notification.
        engine
            .handle
            .load(
                crate::audio::Source {
                    id: 1,
                    url: url::Url::from_file_path(&path).unwrap().into(),
                    duration: 20000,
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while !engine.handle.snapshot().playing {
            assert!(Instant::now() < end, "fixture did not start playing");
            thread::sleep(Duration::from_millis(2));
        }
        let playing = Instant::now();
        let at = wait_for("Playing", &|properties| {
            properties
                .get("PlaybackStatus")
                .is_some_and(|status| format!("{status:?}").contains("Playing"))
        });
        assert!(
            at.saturating_duration_since(playing) < Duration::from_millis(400),
            "PlaybackStatus took {:?}",
            at.saturating_duration_since(playing)
        );
        // Playing: position advances continuously but does not wake MPRIS.
        let wakes = platform.mpris_wakes.load(Ordering::SeqCst);
        let position = engine.handle.snapshot().position;
        thread::sleep(Duration::from_secs(2));
        let active = platform.mpris_wakes.load(Ordering::SeqCst) - wakes;
        assert!(engine.handle.snapshot().position > position);
        assert!(active <= 4, "playing MPRIS woke {active} times in 2 s");
        engine.handle.pause().unwrap();
        wait_for("Paused", &|properties| {
            properties
                .get("PlaybackStatus")
                .is_some_and(|status| format!("{status:?}").contains("Paused"))
        });
        let shutdown = Instant::now();
        drop(platform);
        assert!(shutdown.elapsed() < Duration::from_secs(1));
        drop(engine);
        for (key, value) in saved {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    /// The diagnostic heartbeat is a halt-aware task: a 60 s idle period or
    /// an in-progress sample does not delay shutdown.
    #[test]
    fn heartbeat_task_halts_promptly() {
        let _serial = BUS_ENV.lock().unwrap_or_else(|p| p.into_inner());
        let mut process = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("dbus-daemon fixture is required");
        let mut address = String::new();
        BufReader::new(process.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let _daemon = Daemon(process);
        let saved = std::env::var_os("DBUS_SYSTEM_BUS_ADDRESS");
        std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", address.trim());
        let (requests, _requests) = mpsc::sync_channel(8);
        let shared = Shared::new(requests, json!({}));
        let halter = rt::halter();
        let task = rt::spawn(heartbeat(shared.clone(), halter.subscribe()));
        thread::sleep(Duration::from_millis(500));
        assert!(!task.join(Some(Duration::ZERO)), "heartbeat ended early");
        let stopped = Instant::now();
        shared.stop.store(true, Ordering::SeqCst);
        halter.halt();
        assert!(task.join(Some(Duration::from_secs(3))));
        assert!(stopped.elapsed() < Duration::from_secs(3));
        match saved {
            Some(value) => std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", value),
            None => std::env::remove_var("DBUS_SYSTEM_BUS_ADDRESS"),
        }
    }
}
