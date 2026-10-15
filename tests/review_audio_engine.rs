//! Review of the Rust audio engine (src/audio). Each test documents one
//! unfixed defect and fails until it is fixed.
use plexfreq_core::audio::{dsp, Engine, Event, Sink, Source, RATE};
use std::{
    io::Write,
    path::Path,
    time::{Duration, Instant},
};

fn wav(path: &Path, frames: u32, sample: i16) {
    let mut data = Vec::with_capacity(44 + frames as usize * 2);
    data.extend_from_slice(b"RIFF");
    data.extend_from_slice(&(36 + frames * 2).to_le_bytes());
    data.extend_from_slice(b"WAVEfmt ");
    data.extend_from_slice(&16u32.to_le_bytes());
    data.extend_from_slice(&1u16.to_le_bytes());
    data.extend_from_slice(&1u16.to_le_bytes());
    data.extend_from_slice(&RATE.to_le_bytes());
    data.extend_from_slice(&(RATE * 2).to_le_bytes());
    data.extend_from_slice(&2u16.to_le_bytes());
    data.extend_from_slice(&16u16.to_le_bytes());
    data.extend_from_slice(b"data");
    data.extend_from_slice(&(frames * 2).to_le_bytes());
    for _ in 0..frames {
        data.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::File::create(path)
        .unwrap()
        .write_all(&data)
        .unwrap();
}

fn source(id: u64, path: &Path, duration: u64, album: &str) -> Source {
    Source {
        id,
        url: url::Url::from_file_path(path).unwrap().to_string(),
        duration,
        album: album.into(),
        ..Default::default()
    }
}

/// Collects events until `done` matches one or the deadline passes.
fn collect_until(engine: &Engine, timeout: Duration, done: impl Fn(&Event) -> bool) -> Vec<Event> {
    let until = Instant::now() + timeout;
    let mut seen = Vec::new();
    while Instant::now() < until {
        let batch = engine.events();
        let finished = batch.iter().any(&done);
        seen.extend(batch);
        if finished {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    seen
}

// BUG (src/audio/mod.rs:1010-1025 + Actor::load:908): Control::Seek reloads
// the current track through Actor::load, which sets `self.next = None`. The
// runtime (src/runtime.rs Event::Seeked) never re-prepares, so any seek
// silently discards the prepared successor: the track ends with End instead
// of a gapless/crossfaded Transition, and a gap follows while Core loads the
// next item from scratch.
#[test]
fn seek_keeps_prepared_successor_for_gapless_transition() {
    let dir = tempfile::tempdir().unwrap();
    let one = dir.path().join("one.wav");
    let two = dir.path().join("two.wav");
    wav(&one, RATE, 4096);
    wav(&two, RATE / 2, -8192);
    let engine = Engine::new(Sink::Capture).unwrap();
    engine.handle.volume(1.).unwrap();
    engine
        .handle
        .load(source(1, &one, 1000, "album"), true)
        .unwrap();
    engine
        .handle
        .prepare(Some(source(2, &two, 500, "album")))
        .unwrap();
    engine.handle.seek(500).unwrap();
    engine.handle.play().unwrap();
    let events = collect_until(&engine, Duration::from_secs(5), |e| {
        matches!(e, Event::End { .. } | Event::Error { .. })
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Transition { old: 1, new: 2, .. })),
        "seek discarded the prepared successor: {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(e, Event::End { id: 2, .. })),
        "{events:?}"
    );
}

// BUG (src/audio/mod.rs:936-952 vs 1136/1173): Prepare does not reset
// `self.fade`. The runtime re-prepares (Prepare(None) + Prepare(Some(new)))
// after every enqueue/shuffle/repeat/queue edit/audio_config, so an edit
// during a crossfade swaps the successor while the fade keeps its stale
// `(total, done)`. The new track enters at sin(done/total) gain instead of
// from silence, and at handoff the segment start is backdated by the full
// `total` although the new track overlapped only `total - done` frames. Its
// reported position/listened time (and End position) are inflated by `done`.
#[test]
fn successor_replaced_mid_crossfade_reports_true_position() {
    let dir = tempfile::tempdir().unwrap();
    let frames = RATE * 13;
    let (a, b, c) = (
        dir.path().join("a.wav"),
        dir.path().join("b.wav"),
        dir.path().join("c.wav"),
    );
    wav(&a, frames, 4096);
    wav(&b, frames, 8192);
    wav(&c, frames, -8192);
    let engine = Engine::new(Sink::Capture).unwrap();
    engine.handle.volume(1.).unwrap();
    engine
        .handle
        .configure(dsp::Config {
            crossfade_ms: 12000,
            ..Default::default()
        })
        .unwrap();
    engine
        .handle
        .load(source(1, &a, 13000, "a"), false)
        .unwrap();
    engine
        .handle
        .prepare(Some(source(2, &b, 13000, "b")))
        .unwrap();
    // The fade starts after 1s of A; replace the successor 2s into the fade,
    // well before its 12s end (the capture sink runs faster than real time).
    let fade_start = RATE as usize;
    let until = Instant::now() + Duration::from_secs(10);
    while engine.handle.captured().len() / 2 < fade_start + 2 * RATE as usize {
        assert!(Instant::now() < until, "crossfade never started");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        engine.handle.captured().len() / 2 < fade_start + 10 * RATE as usize,
        "fixture missed the crossfade window"
    );
    engine.handle.prepare(None).unwrap();
    engine
        .handle
        .prepare(Some(source(3, &c, 13000, "c")))
        .unwrap();
    let events = collect_until(&engine, Duration::from_secs(20), |e| {
        matches!(e, Event::End { id: 3, .. } | Event::Error { .. })
    });
    let position = events
        .iter()
        .find_map(|e| match e {
            Event::End {
                id: 3, position, ..
            } => Some(*position),
            _ => None,
        })
        .unwrap_or_else(|| panic!("successor never ended: {events:?}"));
    // C is 13s long; any accounting of the actual overlap ends it at ~13000ms.
    assert!(
        position.abs_diff(13000) <= 100,
        "replaced successor's position is inflated by the stale fade: {position}ms"
    );
}

// BUG (src/audio/mod.rs:1217-1224 vs 1126-1186): when the last queued track
// finishes decoding, pump pushes appsrc EOS and sets `ended`, while ~0.5s of
// PCM is still queued for the sink. A Prepare arriving in that window (user
// enqueues a track, radio refill) passes the join check (`current.eof &&
// next.is_some() && available == 0`), hands off to the successor and pushes
// its PCM into an appsrc that already received EOS. push_buffer returns
// FlowError::Eos and the actor reports "Audio output failed", stopping
// playback with an error. (`ended` also stays true, so even a successful
// push would never send EOS for the successor.)
#[test]
fn prepare_after_final_eos_is_queued_does_not_fail_output() {
    let dir = tempfile::tempdir().unwrap();
    let one = dir.path().join("one.wav");
    let two = dir.path().join("two.wav");
    wav(&one, RATE * 2, 4096);
    wav(&two, RATE / 2, -8192);
    // Fake sink is clock-synchronized, so queued PCM drains in real time.
    let engine = Engine::new(Sink::Fake).unwrap();
    engine
        .handle
        .load(source(1, &one, 2000, "album"), false)
        .unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    // EOS is pushed once the decoder drains (~0.5s before audible end).
    while engine.handle.snapshot().position < 1700 {
        assert!(Instant::now() < until, "playback did not progress");
        assert!(engine.events().is_empty());
        std::thread::sleep(Duration::from_millis(2));
    }
    engine
        .handle
        .prepare(Some(source(2, &two, 500, "album")))
        .unwrap();
    let events = collect_until(&engine, Duration::from_secs(5), |e| {
        matches!(e, Event::Error { .. } | Event::End { .. })
    });
    assert!(
        !events.iter().any(|e| matches!(e, Event::Error { .. })),
        "late successor broke the output: {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(e, Event::End { .. })),
        "{events:?}"
    );
}
