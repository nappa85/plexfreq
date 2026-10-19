//! Successor timing and the resume gate (Plan E2.2/E2.3) against a scripted
//! local HTTP server and the real-time fake sink.
use super::*;
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
};

/// A 48 kHz stereo 16-bit WAV of `milliseconds` (192 000 bytes per second).
fn wav(milliseconds: u32) -> Vec<u8> {
    let frames = 48 * milliseconds;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + frames * 4).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&48000u32.to_le_bytes());
    bytes.extend_from_slice(&(48000u32 * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(frames * 4).to_le_bytes());
    bytes.resize(bytes.len() + frames as usize * 4, 0x10);
    bytes
}
const BYTES_PER_SECOND: usize = 192_000;

/// How a route sends its body: `first` bytes at once, then a pause (`None`
/// holds the connection open forever), then the rest at `rate` bytes/s
/// (`None`: at once).
#[derive(Clone, Copy)]
struct Pace {
    first: usize,
    pause: Option<Duration>,
    rate: Option<usize>,
}
impl Pace {
    fn fast() -> Self {
        Self {
            first: usize::MAX,
            pause: Some(Duration::ZERO),
            rate: None,
        }
    }
    fn throttled(rate: usize) -> Self {
        Self {
            first: 0,
            pause: Some(Duration::ZERO),
            rate: Some(rate),
        }
    }
}

/// Serve `routes` by path; every request path is logged.
fn server(routes: Vec<(&'static str, Vec<u8>, Pace)>) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let requests = log.clone();
    let routes = Arc::new(routes);
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let (routes, requests) = (routes.clone(), requests.clone());
            thread::spawn(move || serve(stream, &routes, &requests));
        }
    });
    (address, log)
}
fn serve(
    mut stream: TcpStream,
    routes: &[(&'static str, Vec<u8>, Pace)],
    requests: &Mutex<Vec<String>>,
) {
    let mut request = Vec::new();
    let mut byte = [0];
    while !request.ends_with(b"\r\n\r\n") && stream.read_exact(&mut byte).is_ok() {
        request.push(byte[0]);
    }
    let request = String::from_utf8_lossy(&request).to_string();
    let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
    requests.lock().unwrap().push(path.clone());
    let Some((_, body, pace)) = routes.iter().find(|(route, ..)| path == *route) else {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        return;
    };
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let first = pace.first.min(body.len());
    if stream.write_all(&body[..first]).is_err() {
        return;
    }
    match pace.pause {
        Some(pause) => thread::sleep(pause),
        None => loop {
            thread::sleep(Duration::from_secs(60));
        },
    }
    let rest = &body[first..];
    match pace.rate {
        None => {
            let _ = stream.write_all(rest);
        }
        Some(rate) => {
            for chunk in rest.chunks(rate / 20) {
                if stream.write_all(chunk).is_err() {
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn source(id: u64, address: SocketAddr, path: &str, duration: u64) -> Source {
    Source {
        id,
        url: format!("http://{address}{path}"),
        duration,
        ..Default::default()
    }
}
fn actor() -> (Actor, mpsc::Receiver<Event>, Arc<Mutex<Snapshot>>) {
    let (sender, events) = mpsc::channel();
    let state = Arc::new(Mutex::new(Snapshot::default()));
    let actor = Actor::new(
        Sink::Fake,
        state.clone(),
        Arc::new(Mutex::new(Vec::new())),
        sender,
        http::SessionCloser::new().unwrap(),
    );
    (actor, events, state)
}
/// Run actor iterations (as `run` does after controls) until `done`.
fn run_until(actor: &mut Actor, limit: Duration, done: impl Fn(&mut Actor) -> bool) {
    let end = Instant::now() + limit;
    while !done(actor) {
        assert!(Instant::now() < end, "timed out");
        actor.tick();
        thread::sleep(Duration::from_millis(5));
    }
}
fn run_for(actor: &mut Actor, duration: Duration) {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        actor.tick();
        thread::sleep(Duration::from_millis(5));
    }
}
fn requested(log: &Mutex<Vec<String>>, path: &str) -> bool {
    log.lock().unwrap().iter().any(|p| p == path)
}

#[test]
fn a_remote_successor_waits_for_a_slow_stream_far_from_its_end() {
    // Served at about 1.3x real time: the window never fills.
    let (address, log) = server(vec![
        (
            "/a.wav",
            wav(120_000),
            Pace::throttled(BYTES_PER_SECOND * 4 / 3),
        ),
        ("/b.wav", wav(2_000), Pace::fast()),
    ]);
    let (mut actor, _events, _state) = actor();
    actor
        .control(Control::Load(source(1, address, "/a.wav", 120_000), false))
        .unwrap();
    actor
        .control(Control::Prepare(Some(source(2, address, "/b.wav", 2_000))))
        .unwrap();
    run_for(&mut actor, Duration::from_millis(1500));
    assert!(actor.pending.is_some() && actor.next.is_none());
    assert!(!requested(&log, "/b.wav"));
    assert!(
        actor.current.as_ref().is_some_and(|c| c.frames > 0),
        "playing"
    );
    // A far seek keeps it pending; Stop drops it unrequested.
    actor.control(Control::Seek(30_000)).unwrap();
    assert!(actor.pending.as_ref().is_some_and(|s| s.id == 2));
    run_for(&mut actor, Duration::from_millis(300));
    actor.control(Control::Stop).unwrap();
    assert!(actor.pending.is_none());
    run_for(&mut actor, Duration::from_millis(100));
    assert!(!requested(&log, "/b.wav"));
}

/// Load `current` with a deferred remote successor `/b.wav` and run until
/// the successor starts; returns why it started.
fn start_reason(current: Source, address: SocketAddr, lead_ms: u64) -> &'static str {
    let (mut actor, _events, _state) = actor();
    actor.preload_lead_ms = lead_ms;
    actor.control(Control::Load(current, false)).unwrap();
    actor
        .control(Control::Prepare(Some(source(2, address, "/b.wav", 2_000))))
        .unwrap();
    assert!(actor.pending.is_some());
    run_until(&mut actor, Duration::from_secs(5), |a| a.next.is_some());
    actor.preload_reason.unwrap()
}

#[test]
fn each_healthy_condition_starts_the_successor() {
    let (address, log) = server(vec![
        // 10 s (1.9 MB): fully downloaded at once, below 3/4 of the window.
        ("/short.wav", wav(10_000), Pace::fast()),
        // 60 s (11.5 MB): the window fills to its capacity at once.
        ("/long.wav", wav(60_000), Pace::fast()),
        // 10 s at 1.3x real time: never complete or full, but near its end.
        (
            "/slow.wav",
            wav(10_000),
            Pace::throttled(BYTES_PER_SECOND * 4 / 3),
        ),
        ("/b.wav", wav(2_000), Pace::fast()),
    ]);
    assert_eq!(
        start_reason(source(1, address, "/short.wav", 10_000), address, 0),
        "complete"
    );
    assert!(requested(&log, "/b.wav"));
    let reason = start_reason(source(1, address, "/long.wav", 60_000), address, 0);
    assert!(matches!(reason, "window" | "ahead"), "{reason}");
    assert_eq!(
        start_reason(
            source(1, address, "/slow.wav", 10_000),
            address,
            PRELOAD_LEAD_MS
        ),
        "lead"
    );
}

#[test]
fn a_local_successor_starts_at_once() {
    let (address, _log) = server(vec![("/a.wav", wav(10_000), Pace::fast())]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("d.wav");
    std::fs::write(&path, wav(1_000)).unwrap();
    let local = Source {
        id: 4,
        url: url::Url::from_file_path(path).unwrap().to_string(),
        duration: 1_000,
        ..Default::default()
    };
    let (mut actor, _events, _state) = actor();
    actor
        .control(Control::Load(source(1, address, "/a.wav", 10_000), false))
        .unwrap();
    actor.control(Control::Prepare(Some(local))).unwrap();
    assert!(actor.next.is_some() && actor.pending.is_none());
}

#[test]
fn a_replaced_pending_successor_is_never_requested() {
    let (address, log) = server(vec![
        (
            "/a.wav",
            wav(120_000),
            Pace::throttled(BYTES_PER_SECOND * 4 / 3),
        ),
        ("/b.wav", wav(2_000), Pace::fast()),
    ]);
    let (mut actor, _events, _state) = actor();
    actor
        .control(Control::Load(source(1, address, "/a.wav", 120_000), false))
        .unwrap();
    actor
        .control(Control::Prepare(Some(source(2, address, "/b.wav", 2_000))))
        .unwrap();
    actor
        .control(Control::Prepare(Some(source(
            3,
            address,
            "/missing.wav",
            2_000,
        ))))
        .unwrap();
    actor.control(Control::Prepare(None)).unwrap();
    assert!(actor.pending.is_none());
    run_for(&mut actor, Duration::from_millis(500));
    let log = log.lock().unwrap();
    assert!(!log.iter().any(|p| p != "/a.wav"), "{log:?}");
}

#[test]
fn a_deferred_successor_that_fails_reports_once() {
    let (address, _log) = server(vec![("/a.wav", wav(4_000), Pace::fast())]);
    let (mut actor, events, _state) = actor();
    actor
        .control(Control::Load(source(1, address, "/a.wav", 4_000), false))
        .unwrap();
    actor
        .control(Control::Prepare(Some(source(
            2,
            address,
            "/gone.wav",
            2_000,
        ))))
        .unwrap();
    run_until(&mut actor, Duration::from_secs(10), |a| {
        a.next.is_none() && a.pending.is_none()
    });
    run_for(&mut actor, Duration::from_millis(300));
    let failures = events
        .try_iter()
        .filter(|event| {
            matches!(event, Event::Error { id: 2, message, .. } if *message == "Next track decoding failed")
        })
        .count();
    assert_eq!(failures, 1);
}

#[test]
fn a_remote_load_starts_with_a_short_buffer_and_a_local_one_at_once() {
    let (address, _log) = server(vec![("/a.wav", wav(3_000), Pace::fast())]);
    let (mut actor, _events, _state) = actor();
    actor
        .control(Control::Load(source(1, address, "/a.wav", 3_000), false))
        .unwrap();
    assert!(actor.gate.is_some_and(|gate| gate.kind == GateKind::Start));
    assert!(actor.output.as_ref().is_some_and(Output::corked));
    run_until(&mut actor, Duration::from_secs(3), |a| a.gate.is_none());
    // Released with at least the start buffer decoded (or the whole track);
    // the same tick may already have submitted some of it.
    let current = actor.current.as_ref().unwrap();
    let decoded = current.frames as usize + current.remaining();
    assert!(decoded >= START_BUFFER_FRAMES || current.eof, "{decoded}");
    run_until(&mut actor, Duration::from_secs(1), |a| {
        a.output.as_ref().is_some_and(|out| !out.corked())
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.wav");
    std::fs::write(&path, wav(1_000)).unwrap();
    let local = Source {
        id: 2,
        url: url::Url::from_file_path(path).unwrap().to_string(),
        duration: 1_000,
        ..Default::default()
    };
    actor.control(Control::Load(local, false)).unwrap();
    assert!(actor.gate.is_none());
}

/// A stream that sends `first_ms` of audio, stalls for `stall`, then sends
/// the rest at once.
fn stalling(first_ms: usize, stall: Duration, total_ms: u32) -> (Actor, Arc<Mutex<Snapshot>>) {
    let (address, _log) = server(vec![(
        "/a.wav",
        wav(total_ms),
        Pace {
            first: 44 + BYTES_PER_SECOND * first_ms / 1000,
            pause: Some(stall),
            rate: None,
        },
    )]);
    let (mut actor, _events, state) = actor();
    actor
        .control(Control::Load(
            source(1, address, "/a.wav", total_ms as u64),
            false,
        ))
        .unwrap();
    (actor, state)
}
fn stalled(actor: &mut Actor) -> bool {
    actor.gate.is_some_and(|gate| gate.kind == GateKind::Stall)
}

#[test]
fn a_stalled_stream_resumes_only_with_a_full_buffer() {
    let (mut actor, state) = stalling(1_000, Duration::from_secs(2), 20_000);
    run_until(&mut actor, Duration::from_secs(4), stalled);
    assert!(actor.output.as_ref().is_some_and(Output::corked));
    actor.publish();
    let snapshot = state.lock().unwrap().clone();
    assert!(
        snapshot.buffering && snapshot.starved && snapshot.intent,
        "{snapshot:?}"
    );
    // Play while stalled keeps the stream corked.
    actor.control(Control::Play).unwrap();
    assert!(actor.output.as_ref().is_some_and(Output::corked));
    let before = actor.output.as_ref().unwrap().frames;
    run_until(&mut actor, Duration::from_secs(5), |a| a.gate.is_none());
    let current = actor.current.as_ref().unwrap();
    let submitted = (actor.output.as_ref().unwrap().frames - before) as usize;
    assert!(
        current.remaining() + submitted >= RESUME_BUFFER_FRAMES || current.eof,
        "{} + {submitted}",
        current.remaining()
    );
    actor.tick();
    assert!(actor.output.as_ref().is_some_and(|out| !out.corked()));
    actor.publish();
    assert!(!state.lock().unwrap().starved);
}

#[test]
fn a_stall_with_little_left_resumes_at_the_end_of_the_track() {
    // The stall outlasts the 1 s played plus the 0.5 s output queue.
    let (mut actor, _state) = stalling(1_000, Duration::from_secs(3), 3_000);
    run_until(&mut actor, Duration::from_secs(5), stalled);
    run_until(&mut actor, Duration::from_secs(5), |a| a.gate.is_none());
    // Under 5 s was left: released at the decoded end.
    assert!(actor.current.as_ref().unwrap().eof);
}

#[test]
fn a_stall_with_crossfade_counts_only_playable_audio() {
    // 5 s arrive (3 s of them reserved for the fade), then a 4 s stall.
    let (mut actor, _state) = stalling(5_000, Duration::from_secs(4), 30_000);
    actor
        .control(Control::Configure(dsp::Config {
            crossfade_ms: 3_000,
            ..Default::default()
        }))
        .unwrap();
    // The decoder keeps the 3 s tail in reserve, so only the rest plays.
    run_until(&mut actor, Duration::from_secs(6), stalled);
    let before = actor.output.as_ref().unwrap().frames;
    run_until(&mut actor, Duration::from_secs(6), |a| a.gate.is_none());
    let current = actor.current.as_ref().unwrap();
    let submitted = (actor.output.as_ref().unwrap().frames - before) as usize;
    assert!(
        current.remaining() + submitted >= RESUME_BUFFER_FRAMES + 3 * RATE as usize,
        "{} + {submitted}",
        current.remaining()
    );
}

#[test]
fn patience_settles_for_less_and_ignores_paused_time() {
    // 1 s arrives, a 2.5 s stall, then the rest at a fifth of real time:
    // 5 s would take 25 s, the start buffer 2.5 s.
    let (address, _log) = server(vec![(
        "/a.wav",
        wav(60_000),
        Pace {
            first: 44 + BYTES_PER_SECOND,
            pause: Some(Duration::from_millis(2500)),
            rate: Some(BYTES_PER_SECOND / 5),
        },
    )]);
    let (mut actor, _events, _state) = actor();
    actor.gate_patience = Duration::from_millis(800);
    actor
        .control(Control::Load(source(1, address, "/a.wav", 60_000), false))
        .unwrap();
    run_until(&mut actor, Duration::from_secs(4), |a| {
        a.gate.is_some_and(|gate| gate.kind == GateKind::Stall)
    });
    actor.control(Control::Pause).unwrap();
    run_for(&mut actor, Duration::from_millis(1200));
    assert!(
        actor
            .gate
            .is_some_and(|gate| gate.waited < Duration::from_millis(800)),
        "paused time is not counted"
    );
    actor.control(Control::Play).unwrap();
    run_until(&mut actor, Duration::from_secs(6), |a| a.gate.is_none());
    let current = actor.current.as_ref().unwrap();
    assert!(current.remaining() < RESUME_BUFFER_FRAMES && !current.eof);
    assert!(actor.output.as_ref().is_some_and(|out| !out.corked()));
}

#[test]
fn a_stalled_successor_drops_the_crossfade_and_plays_the_tail() {
    let (address, _log) = server(vec![
        ("/a.wav", wav(3_000), Pace::fast()),
        (
            "/b.wav",
            wav(10_000),
            Pace {
                first: 44 + BYTES_PER_SECOND / 4,
                pause: None,
                rate: None,
            },
        ),
    ]);
    let (mut actor, _events, _state) = actor();
    actor
        .control(Control::Configure(dsp::Config {
            crossfade_ms: 2_000,
            ..Default::default()
        }))
        .unwrap();
    actor
        .control(Control::Load(source(1, address, "/a.wav", 3_000), false))
        .unwrap();
    actor
        .control(Control::Prepare(Some(source(2, address, "/b.wav", 10_000))))
        .unwrap();
    run_until(&mut actor, Duration::from_secs(8), |a| a.fade_dropped);
    // The unfaded tail is submitted and the successor takes over.
    run_until(&mut actor, Duration::from_secs(5), |a| {
        a.segments.len() == 2
    });
    assert_eq!(actor.current.as_ref().unwrap().source.id, 2);
    assert!(
        actor
            .current
            .as_ref()
            .unwrap()
            .http
            .as_ref()
            .unwrap()
            .facts()
            .capacity
            == http::CAPACITY
    );
}
