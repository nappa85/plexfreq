//! Thread inventory of a running Runtime (private dbus-daemon, fake audio sink).
//! Run with `--nocapture` to print the per-name inventory used in docs.
use plexfreq_core::runtime::Runtime;
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader},
    thread,
    time::Duration,
};

struct Bus(std::process::Child);
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn threads() -> BTreeMap<String, usize> {
    let mut names = BTreeMap::new();
    for task in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        let name = std::fs::read_to_string(task.path().join("comm")).unwrap_or_default();
        *names.entry(name.trim().to_string()).or_insert(0) += 1;
    }
    names
}

#[test]
fn running_runtime_thread_inventory() {
    let mut child = std::process::Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("dbus-daemon fixture is required");
    let mut address = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    let _bus = Bus(child);
    std::env::set_var("DBUS_SYSTEM_BUS_ADDRESS", address.trim());
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", address.trim());
    std::env::set_var("PLEXFREQ_AUDIO_SINK", "fake");
    let before = threads();
    let dir = tempfile::tempdir().unwrap();
    let runtime = Runtime::new(dir.path().into()).unwrap();
    // Let startup settle: observers connect, the first network query fails
    // (no ConnMan/NetworkManager on the private bus) and blocking-pool
    // threads used only for startup expire.
    thread::sleep(Duration::from_secs(3));
    let running = threads();
    let total = |m: &BTreeMap<String, usize>| m.values().sum::<usize>();
    println!(
        "threads before={} running={} added={}",
        total(&before),
        total(&running),
        total(&running) - total(&before)
    );
    for (name, count) in &running {
        let added = count - before.get(name).copied().unwrap_or(0);
        if added > 0 {
            println!("  {name}: {added}");
        }
    }
    drop(runtime);
    // Shutdown is deterministic: no PlexFreq-owned dedicated thread survives.
    thread::sleep(Duration::from_millis(200));
    let after = threads();
    // /proc comm names are truncated to 15 bytes.
    for name in [
        "plexfreq-core",
        "plexfreq-audio",
        "plexfreq-cache",
        "plexfreq-networ",
        "plexfreq-mpris",
        "plexfreq-blueto",
    ] {
        assert!(
            !after.contains_key(name),
            "{name} survived shutdown: {after:?}"
        );
    }
}
