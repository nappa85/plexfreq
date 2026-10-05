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
        ..Default::default()
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

fn wait_end(engine: &Engine, id: u64) {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let events = engine.events();
        assert!(
            !events.iter().any(|e| matches!(e, Event::Error { .. })),
            "{events:?}"
        );
        if events
            .iter()
            .any(|e| matches!(e,Event::End{id:ended,..} if *ended==id))
        {
            return;
        }
        assert!(Instant::now() < until, "Audio sequence did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn mixed_rate_gapless_join_keeps_duration_within_one_output_frame() {
    let dir = tempfile::tempdir().unwrap();
    let one = dir.path().join("44k.wav");
    let two = dir.path().join("48k.wav");
    wav(&one, 44100, 4410, 4096);
    wav(&two, 48000, 4800, -8192);
    let engine = Engine::new(Sink::Capture).unwrap();
    engine.handle.volume(1.).unwrap();
    engine.handle.load(source(1, &one), false).unwrap();
    engine.handle.prepare(Some(source(2, &two))).unwrap();
    wait_end(&engine, 2);
    let pcm = engine.handle.captured();
    // The platform resampler rounds the 44.1kHz endpoint by one output frame.
    // Preserve its decoded samples rather than padding from a metadata hint.
    assert!(pcm.len().abs_diff(19200) <= 2);
    let seam = pcm.len() - 9600;
    assert!(pcm[100..seam - 100]
        .iter()
        .all(|x| (*x - 0.125).abs() < 1e-4));
    assert!(pcm[seam..].iter().all(|x| (*x + 0.25).abs() < 1e-5));
}

#[test]
fn flac_gapless_join_preserves_exact_decoded_samples() {
    use gstreamer::{self as gst, prelude::*};
    gst::init().unwrap();
    assert!(
        gst::ElementFactory::find("flacenc").is_some(),
        "FLAC fixture encoder is required"
    );
    let dir = tempfile::tempdir().unwrap();
    let wave = dir.path().join("one.wav");
    let flac = dir.path().join("one.flac");
    let two = dir.path().join("two.wav");
    wav(&wave, RATE, 4800, 4096);
    wav(&two, RATE, 4800, -8192);
    let pipeline = gst::parse::launch(&format!(
        "filesrc location={:?} ! wavparse ! flacenc ! filesink location={:?}",
        wave.to_str().unwrap(),
        flac.to_str().unwrap()
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    pipeline.set_state(gst::State::Playing).unwrap();
    let message = pipeline
        .bus()
        .unwrap()
        .timed_pop_filtered(
            gst::ClockTime::from_seconds(5),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
        .unwrap();
    assert_eq!(message.type_(), gst::MessageType::Eos);
    pipeline.set_state(gst::State::Null).unwrap();
    let engine = Engine::new(Sink::Capture).unwrap();
    engine.handle.volume(1.).unwrap();
    engine.handle.load(source(1, &flac), false).unwrap();
    engine.handle.prepare(Some(source(2, &two))).unwrap();
    wait_end(&engine, 2);
    let pcm = engine.handle.captured();
    assert_eq!(pcm.len(), 19200);
    assert!(pcm[..9600].iter().all(|x| (*x - 0.125).abs() < 1e-5));
    assert!(pcm[9600..].iter().all(|x| (*x + 0.25).abs() < 1e-5));
}

#[test]
fn same_album_suppresses_crossfade_and_pause_keeps_prepared_successor() {
    let dir = tempfile::tempdir().unwrap();
    let one = dir.path().join("one.wav");
    let two = dir.path().join("two.wav");
    wav(&one, RATE, 24000, 4096);
    wav(&two, RATE, 24000, -8192);
    let engine = Engine::new(Sink::Capture).unwrap();
    engine.handle.volume(1.).unwrap();
    engine
        .handle
        .configure(plexfreq_core::audio::dsp::Config {
            crossfade_ms: 100,
            ..Default::default()
        })
        .unwrap();
    let mut a = source(1, &one);
    a.duration = 500;
    let mut b = source(2, &two);
    b.duration = 500;
    engine.handle.load(a, false).unwrap();
    engine.handle.prepare(Some(b)).unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while engine.handle.snapshot().position < 30 {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    }
    engine.handle.pause().unwrap();
    while !engine.handle.snapshot().paused {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    }
    let position = engine.handle.snapshot().position;
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(engine.handle.snapshot().position, position);
    engine.handle.play().unwrap();
    wait_end(&engine, 2);
    assert_eq!(engine.handle.captured().len(), 96000);
}

#[test]
fn normalization_modes_produce_the_expected_decoded_pcm() {
    use plexfreq_core::audio::dsp::{Config, NormalizationMode};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gain.wav");
    wav(&path, RATE, 4800, 4096);
    for (mode, expected) in [
        (NormalizationMode::Track, 0.25),
        (NormalizationMode::Album, 0.0625),
        (NormalizationMode::Auto, 0.0625),
    ] {
        let engine = Engine::new(Sink::Capture).unwrap();
        engine.handle.volume(1.).unwrap();
        engine
            .handle
            .configure(Config {
                normalization: true,
                normalization_mode: mode,
                ..Default::default()
            })
            .unwrap();
        let mut input = source(1, &path);
        input.gain = 20. * 2f32.log10();
        input.album_gain = Some(20. * 0.5f32.log10());
        input.album_normalization = true;
        engine.handle.load(input, false).unwrap();
        wait_end(&engine, 1);
        let pcm = engine.handle.captured();
        assert_eq!(pcm.len(), 9600);
        assert!(
            pcm.iter().all(|sample| (*sample - expected).abs() < 1e-5),
            "{mode:?}"
        );
    }
}

fn encode_audio(wave: &std::path::Path, output: &std::path::Path, codec: &str) {
    if codec == "libmp3lame" {
        let status = std::process::Command::new("lame")
            .args(["--silent", "-b", "192"])
            .arg(wave)
            .arg(output)
            .status()
            .expect("LAME fixture encoder is required");
        assert!(status.success());
        return;
    }
    let status = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
        .arg(wave)
        .args(["-c:a", codec, "-b:a", "192k"])
        .arg(output)
        .status()
        .expect("FFmpeg fixture encoder is required");
    assert!(status.success());
}

#[test]
fn mp3_and_aac_container_joins_trim_encoder_padding() {
    let dir = tempfile::tempdir().unwrap();
    let one = dir.path().join("one.wav");
    let two = dir.path().join("two.wav");
    wav(&one, RATE, 48000, 4096);
    wav(&two, RATE, 48000, -8192);
    for (ext, codec) in [("mp3", "libmp3lame"), ("m4a", "aac")] {
        let a = dir.path().join(format!("one.{ext}"));
        let b = dir.path().join(format!("two.{ext}"));
        encode_audio(&one, &a, codec);
        encode_audio(&two, &b, codec);
        let engine = Engine::new(Sink::Capture).unwrap();
        engine.handle.volume(1.).unwrap();
        let mut sa = source(1, &a);
        sa.duration = 1000;
        let mut sb = source(2, &b);
        sb.duration = 1000;
        engine.handle.load(sa, false).unwrap();
        engine.handle.prepare(Some(sb)).unwrap();
        wait_end(&engine, 2);
        let pcm = engine.handle.captured();
        assert_eq!(
            pcm.len(),
            192000,
            "{ext}: decoded PCM must exclude encoder priming/padding"
        );
        assert!(
            pcm[4000..90000].iter().all(|x| (*x - 0.125).abs() < 0.03),
            "{ext}"
        );
        assert!(
            pcm[100000..188000].iter().all(|x| (*x + 0.25).abs() < 0.04),
            "{ext}"
        );
    }
}

#[test]
fn mp3_and_aac_resume_seek_keep_heard_time_separate_from_position() {
    let dir = tempfile::tempdir().unwrap();
    let wave = dir.path().join("seek.wav");
    wav(&wave, RATE, 96000, 4096);
    for (ext, codec) in [("mp3", "libmp3lame"), ("m4a", "aac")] {
        let file = dir.path().join(format!("seek.{ext}"));
        encode_audio(&wave, &file, codec);
        let engine = Engine::new(Sink::Fake).unwrap();
        let mut input = source(1, &file);
        input.duration = 2000;
        input.resume = 1000;
        engine.handle.load(input, true).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while !engine.handle.snapshot().loaded {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        engine.handle.play().unwrap();
        while engine.handle.snapshot().position < 1100 {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        engine.handle.pause().unwrap();
        while !engine.handle.snapshot().paused {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        let state = engine.handle.snapshot();
        assert!(state.position >= 1100);
        assert!(
            state.listened < 500,
            "{ext}: seek offsets are not listened time"
        );
        engine.handle.seek(1800).unwrap();
        engine.handle.play().unwrap();
        wait_end(&engine, 1);
        assert!(engine.handle.snapshot().listened < 800, "{ext}");
    }
}
