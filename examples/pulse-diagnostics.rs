//! Read-only probe of the exact production observer; no audio or Plex requests.
#[path = "../src/runtime/pulse.rs"]
mod pulse;

fn main() {
    if std::env::args().any(|arg| arg == "--recording") {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let deadline = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(30));
            deadline.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let mut previous = None;
        if let Err(reason) = pulse::watch_recording(&stop, |active| {
            if previous != Some(active) {
                println!("microphone recording active={active}");
                previous = Some(active);
            }
        }) {
            if !stop.load(std::sync::atomic::Ordering::SeqCst) {
                eprintln!("recording observer unavailable reason={reason}");
                std::process::exit(1);
            }
        }
        return;
    }
    match pulse::snapshot(&std::sync::atomic::AtomicBool::new(false)) {
        Ok(snapshot) => {
            println!("PulseAudio {}", snapshot.streams());
            println!("PulseAudio routes {}", snapshot.routes());
        }
        Err(reason) => {
            eprintln!("PulseAudio snapshot unavailable backend=libpulse reason={reason}");
            std::process::exit(1);
        }
    }
}
