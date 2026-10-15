//! Review regressions for runtime worker lifetime and platform observers.
//! Every runtime here runs against a private dbus-daemon (both system and
//! session addresses) with the fake audio sink; nothing touches real buses.
use plexfreq_core::runtime::Runtime;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

// Environment variables are process-global: serialize every runtime test.
static SERIAL: Mutex<()> = Mutex::new(());

struct Bus {
    child: std::process::Child,
    address: String,
}
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn private_bus() -> Bus {
    let mut child = std::process::Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("dbus-daemon fixture is required");
    let mut address = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    let address = address.trim().to_string();
    std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", &address);
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address);
    std::env::set_var("PLEXFREQ_AUDIO_SINK", "fake");
    Bus { child, address }
}
fn poll_for(runtime: &Runtime, duration: Duration) -> Vec<Value> {
    let end = Instant::now() + duration;
    let mut events = Vec::new();
    while Instant::now() < end {
        if let Some(list) = runtime.poll()["events"].as_array() {
            events.extend(list.iter().cloned());
        }
        thread::sleep(Duration::from_millis(50));
    }
    events
}

struct StalledConnman {
    calls: Arc<AtomicUsize>,
}
#[zbus::interface(name = "net.connman.Manager")]
impl StalledConnman {
    async fn get_services(
        &self,
    ) -> Vec<(
        zbus::zvariant::OwnedObjectPath,
        std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
    )> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        futures_util::future::pending::<()>().await;
        Vec::new()
    }
}

// Review: the network observer (runtime/platform.rs `facts`) uses blocking
// zbus calls with no method timeout (zbus 4.4 has none by default), unlike the
// BlueZ observer which bounds every RPC to 2 s. A ConnMan that accepts the
// call but never replies (daemon restart/hang) blocks plexfreq-network
// forever, so Platform::drop -> Runtime::drop -> pf_runtime_free never returns
// and the GUI freezes on exit.
#[test]
fn stalled_connman_reply_does_not_block_runtime_shutdown() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let bus = private_bus();
    let calls = Arc::new(AtomicUsize::new(0));
    drive_blocking_zbus();
    let _service = zbus::blocking::ConnectionBuilder::address(bus.address.as_str())
        .unwrap()
        .name("net.connman")
        .unwrap()
        .serve_at(
            "/",
            StalledConnman {
                calls: calls.clone(),
            },
        )
        .unwrap()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let runtime = Runtime::new(dir.path().into()).unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while calls.load(Ordering::SeqCst) == 0 {
        assert!(
            Instant::now() < end,
            "network observer never queried ConnMan"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        drop(runtime);
        let _ = done.send(());
    });
    assert!(
        finished.recv_timeout(Duration::from_secs(8)).is_ok(),
        "Runtime shutdown is blocked by an unanswered ConnMan GetServices call"
    );
}

// Review: when neither ConnMan nor NetworkManager answers (desktop with
// systemd-networkd/iwd/netifrc, or no system bus), `facts()` fails and the
// observer substitutes (wifi=false, online=false) and submits it as a live
// network_state. Core then sets network_known && !network_online, which is the
// "known disconnected" state: cache-only playback, cancelled refresh and
// skipped timelines/history. architecture.md requires network facts to stay
// distinguished from the initial unknown state; unavailable facts must not be
// reported as offline.
#[test]
fn unavailable_network_facts_are_not_reported_as_offline() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let _bus = private_bus();
    let dir = tempfile::tempdir().unwrap();
    let runtime = Runtime::new(dir.path().into()).unwrap();
    let events = poll_for(&runtime, Duration::from_secs(3));
    let offline: Vec<&Value> = events
        .iter()
        .filter(|event| {
            event["request"]["op"] == "network_state" && event["request"]["online"] == false
        })
        .collect();
    assert!(
        offline.is_empty(),
        "no network manager answered, yet the runtime reported offline: {offline:?}"
    );
    drop(runtime);
}

// Review: worker() returns early when Engine::new or Core::new fails, dropping
// its receiver together with any request already accepted by Shared::send.
// Those requests incremented `foreground` but are never processed, so busy
// stays true forever (the Qt Backend submits "status" in its constructor, so
// the UI shows a permanent busy state next to the startup error). Here
// Core::new fails because session.json is unreadable (it is a directory).
#[test]
fn startup_failure_does_not_leave_runtime_busy_forever() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let _bus = private_bus();
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("session.json")).unwrap();
    let runtime = Runtime::new(dir.path().into()).unwrap();
    let accepted = runtime.submit(json!({"op":"status"}));
    let end = Instant::now() + Duration::from_secs(5);
    let mut failed = false;
    while Instant::now() < end && !failed {
        let polled = runtime.poll();
        failed = polled["events"].as_array().is_some_and(|events| {
            events
                .iter()
                .any(|e| e["request"]["op"] == "status" && e["response"]["ok"] == false)
        });
        thread::sleep(Duration::from_millis(20));
    }
    assert!(failed, "startup failure was not reported");
    // Give the worker time to finish exiting, then the accepted request must
    // not keep the runtime busy.
    thread::sleep(Duration::from_millis(200));
    assert!(
        accepted.is_err() || runtime.poll()["busy"] == false,
        "accepted request was dropped with the dead worker but busy stays true"
    );
    drop(runtime);
}

/// zbus (tokio feature) runs its blocking API on a private current-thread
/// runtime that only progresses while some thread blocks on it, so blocking
/// fixture services would never answer between calls. Keep it driven.
fn drive_blocking_zbus() {
    static DRIVER: std::sync::Once = std::sync::Once::new();
    DRIVER.call_once(|| {
        std::thread::spawn(|| zbus::block_on(std::future::pending::<()>()));
    });
}
