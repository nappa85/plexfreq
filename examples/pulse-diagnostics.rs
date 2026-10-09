//! Read-only probe of the exact production observer; no audio or Plex requests.
#[path = "../src/runtime/pulse.rs"]
mod pulse;

fn main() {
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
