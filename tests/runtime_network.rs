//! Network observer behaviour against a private dbus-daemon (used as both the
//! system and session bus) with the fake audio sink; no real bus is touched.
use plexfreq_core::runtime::Runtime;
use serde_json::Value;
use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

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

type Services = Vec<(OwnedObjectPath, HashMap<String, OwnedValue>)>;
struct Connman {
    state: Arc<Mutex<&'static str>>,
}
#[zbus::interface(name = "net.connman.Manager")]
impl Connman {
    fn get_services(&self) -> Services {
        let properties = HashMap::from([
            (
                "State".to_string(),
                OwnedValue::try_from(zbus::zvariant::Value::from(*self.state.lock().unwrap()))
                    .unwrap(),
            ),
            (
                "Type".to_string(),
                OwnedValue::try_from(zbus::zvariant::Value::from("wifi")).unwrap(),
            ),
        ]);
        vec![(
            OwnedObjectPath::try_from("/net/connman/service/wifi_fixture").unwrap(),
            properties,
        )]
    }
}

fn network_state(runtime: &Runtime, online: bool, within: Duration) -> Option<Value> {
    let end = Instant::now() + within;
    while Instant::now() < end {
        if let Some(event) = runtime.poll()["events"].as_array().and_then(|events| {
            events
                .iter()
                .find(|e| e["request"]["op"] == "network_state" && e["request"]["online"] == online)
                .cloned()
        }) {
            return Some(event);
        }
        thread::sleep(Duration::from_millis(50));
    }
    None
}

// The observer reacts to ConnMan's change signals instead of waiting for its
// periodic fallback refresh (15 s), and reports the primary route's facts.
#[test]
fn connman_change_signal_updates_network_state_promptly() {
    let bus = private_bus();
    let state = Arc::new(Mutex::new("online"));
    drive_blocking_zbus();
    let service = zbus::blocking::ConnectionBuilder::address(bus.address.as_str())
        .unwrap()
        .name("net.connman")
        .unwrap()
        .serve_at(
            "/",
            Connman {
                state: state.clone(),
            },
        )
        .unwrap()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let runtime = Runtime::new(dir.path().into()).unwrap();
    let online = network_state(&runtime, true, Duration::from_secs(10))
        .expect("initial ConnMan facts were not reported");
    assert_eq!(online["request"]["wifi"], true);
    // Let the observer settle into waiting for signals before the change.
    thread::sleep(Duration::from_millis(500));
    *state.lock().unwrap() = "idle";
    service
        .emit_signal(
            None::<&str>,
            "/",
            "net.connman.Manager",
            "ServicesChanged",
            &(Services::new(), Vec::<OwnedObjectPath>::new()),
        )
        .unwrap();
    let offline = network_state(&runtime, false, Duration::from_secs(4))
        .expect("ConnMan ServicesChanged did not refresh network facts");
    assert_eq!(offline["request"]["wifi"], false);
    let shutdown = Instant::now();
    drop(runtime);
    assert!(shutdown.elapsed() < Duration::from_secs(5));
    drop(service);
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
