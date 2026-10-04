use plexfreq_core::audio::{Engine, Event, Sink, Source, RATE};
use std::{
    io::Write,
    time::{Duration, Instant},
};

fn wav(path: &std::path::Path, rate: u32, frames: u32, sample: i16) {
    let mut file = std::fs::File::create(path).unwrap();
    file.write_all(b"RIFF").unwrap();
    file.write_all(&(36 + frames * 2).to_le_bytes()).unwrap();
    file.write_all(b"WAVEfmt ").unwrap();
    file.write_all(&16u32.to_le_bytes()).unwrap();
    file.write_all(&1u16.to_le_bytes()).unwrap();
    file.write_all(&1u16.to_le_bytes()).unwrap();
    file.write_all(&rate.to_le_bytes()).unwrap();
    file.write_all(&(rate * 2).to_le_bytes()).unwrap();
    file.write_all(&2u16.to_le_bytes()).unwrap();
    file.write_all(&16u16.to_le_bytes()).unwrap();
    file.write_all(b"data").unwrap();
    file.write_all(&(frames * 2).to_le_bytes()).unwrap();
    for _ in 0..frames {
        file.write_all(&sample.to_le_bytes()).unwrap();
    }
}
fn source(id: u64, path: &std::path::Path) -> Source {
    Source {
        id,
        url: url::Url::from_file_path(path).unwrap().to_string(),
        duration: 100,
        resume: 0,
        listened: 0,
        gain: 0.,
        album: "fixture album".into(),
    }
}

#[test]
fn continuous_pcm_output_joins_two_decoded_tracks_without_added_or_lost_frames() {
    let dir = tempfile::tempdir().unwrap();
    let one = dir.path().join("one.wav");
    let two = dir.path().join("two.wav");
    wav(&one, RATE, 4800, 4096);
    wav(&two, RATE, 4800, -8192);
    let engine = Engine::new(Sink::Capture).unwrap();
    engine.handle.volume(1.).unwrap();
    engine.handle.load(source(1, &one), false).unwrap();
    engine.handle.prepare(Some(source(2, &two))).unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    let mut transition = false;
    loop {
        let mut ended = false;
        for event in engine.events() {
            match event {
                Event::Transition { old: 1, new: 2, .. } => transition = true,
                Event::End { id: 2, .. } => ended = true,
                Event::Error { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
        if ended {
            break;
        }
        assert!(Instant::now() < until, "Audio join did not complete");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(transition);
    let pcm = engine.handle.captured();
    assert_eq!(pcm.len(), 4800 * 2 * 2);
    assert!(pcm[..9600].iter().all(|v| (*v - 0.125).abs() < 1e-5));
    assert!(pcm[9600..].iter().all(|v| (*v + 0.25).abs() < 1e-5));
}

#[test]
fn decoded_tracks_crossfade_with_exact_overlap_and_equal_power_pcm() {
    let dir = tempfile::tempdir().unwrap();
    let one = dir.path().join("one.wav");
    let two = dir.path().join("two.wav");
    wav(&one, RATE, 4800, 4096);
    wav(&two, RATE, 4800, 8192);
    let engine = Engine::new(Sink::Capture).unwrap();
    engine.handle.volume(1.).unwrap();
    let config = plexfreq_core::audio::dsp::Config {
        crossfade_ms: 50,
        ..Default::default()
    };
    engine.handle.configure(config).unwrap();
    engine.handle.load(source(1, &one), false).unwrap();
    let mut next = source(2, &two);
    next.album = "other album".into();
    engine.handle.prepare(Some(next)).unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let events = engine.events();
        if events.iter().any(|e| matches!(e, Event::End { id: 2, .. })) {
            break;
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(10));
    }
    let pcm = engine.handle.captured();
    assert_eq!(pcm.len(), 7200 * 2);
    assert!((pcm[3600 * 2] - 0.375 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5);
    assert!(pcm[4800 * 2..].iter().all(|v| (*v - 0.25).abs() < 1e-5));
}
