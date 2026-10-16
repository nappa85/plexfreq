//! Play a local audio file through the production PulseAudio output for a
//! few seconds, printing transport snapshots, then pause/resume and seek.
//! Usage: cargo run --example pulse-play -- <file> [seconds]
use plexfreq_core::audio::{Engine, Event, Sink, Source};
use std::time::{Duration, Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: pulse-play <file> [seconds]");
        std::process::exit(2);
    };
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(4);
    let path = std::fs::canonicalize(path).expect("readable file");
    let engine = Engine::new(Sink::Pulse).expect("audio engine");
    engine.handle.volume(0.5).unwrap();
    engine
        .handle
        .load(
            Source {
                id: 1,
                url: url::Url::from_file_path(&path).unwrap().to_string(),
                ..Default::default()
            },
            false,
        )
        .unwrap();
    let started = Instant::now();
    let report = |label: &str| {
        let state = engine.handle.snapshot();
        println!(
            "{label:>7} t={:5}ms position={:6}ms playing={} buffering={} paused={} duration={}ms error={:?}",
            started.elapsed().as_millis(),
            state.position,
            state.playing,
            state.buffering,
            state.paused,
            state.duration,
            state.error
        );
        for event in engine.events() {
            println!("  event {event:?}");
            if matches!(event, Event::Error { .. }) {
                std::process::exit(1);
            }
        }
    };
    let step = |until: Duration, label: &str| {
        while started.elapsed() < until {
            std::thread::sleep(Duration::from_millis(500));
            report(label);
        }
    };
    step(Duration::from_secs(seconds), "play");
    engine.handle.pause().unwrap();
    step(Duration::from_secs(seconds + 1), "pause");
    engine.handle.play().unwrap();
    step(Duration::from_secs(seconds + 2), "resume");
    engine.handle.seek(1000).unwrap();
    step(Duration::from_secs(seconds + 4), "seek");
}
