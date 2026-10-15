//! Seeking must not discard the prepared successor, even mid-crossfade when
//! the successor decoder was already partly consumed and must be rebuilt.
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

#[test]
fn seek_during_crossfade_rebuilds_the_consumed_successor() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a.wav"), dir.path().join("b.wav"));
    wav(&a, RATE * 4, 4096);
    wav(&b, RATE * 3, -8192);
    let engine = Engine::new(Sink::Capture).unwrap();
    engine
        .handle
        .configure(dsp::Config {
            crossfade_ms: 2000,
            ..Default::default()
        })
        .unwrap();
    engine.handle.load(source(1, &a, 4000, "a"), false).unwrap();
    engine
        .handle
        .prepare(Some(source(2, &b, 3000, "b")))
        .unwrap();
    // The fade covers A's last 2s; seek once it is underway.
    let until = Instant::now() + Duration::from_secs(10);
    while engine.handle.captured().len() / 2 < RATE as usize * 5 / 2 {
        assert!(Instant::now() < until, "crossfade never started");
        std::thread::sleep(Duration::from_millis(1));
    }
    engine.handle.seek(1000).unwrap();
    let until = Instant::now() + Duration::from_secs(20);
    let mut events = Vec::new();
    while !events
        .iter()
        .any(|e| matches!(e, Event::End { .. } | Event::Error { .. }))
    {
        assert!(Instant::now() < until, "{events:?}");
        events.extend(engine.events());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Transition { old: 1, new: 2, .. })),
        "{events:?}"
    );
    let position = events
        .iter()
        .find_map(|e| match e {
            Event::End {
                id: 2, position, ..
            } => Some(*position),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{events:?}"));
    // The rebuilt successor starts from its beginning: full 3s are accounted.
    assert!(position.abs_diff(3000) <= 100, "{position}");
}
