//! Engine-level tests of the production PulseAudio output against a real,
//! private, headless daemon (`tools/pulse-test-server.sh`, two null sinks).
//! The Fake/Capture sinks never underrun, move between sinks or drift, which
//! hid the 2026-10-16 car stutter (estimate drift) and the prebuffer deadlock
//! after a sink move; these tests exercise exactly those server behaviours.
//!
//! They only run when `PLEXFREQ_PULSE_ISOLATED=1` and
//! `PLEXFREQ_PULSE_TEST_SERVER` (the script's copy of `PULSE_SERVER`, which other
//! unit tests repoint in-process) name a server with the `plexfreq_a`/`plexfreq_b` null sinks: they move streams and
//! load/unload sinks, which must never happen on a developer's real server.
//! Otherwise they skip, unless `PLEXFREQ_REQUIRE_PULSE=1` (CI) makes that a
//! failure.
use super::super::{
    output::{TEST_FLUSH, TEST_UNDERFLOWS},
    Engine, Event, Sink, Snapshot, Source, RATE,
};
use std::{
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard,
    },
    thread,
    time::{Duration, Instant},
};

const SINK_A: &str = "plexfreq_a";
const SINK_B: &str = "plexfreq_b";
/// Longest acceptable time without position progress while playback is
/// wanted (the car deadlock never progressed; stutter stalled ~100 ms).
const MAX_STALL: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(20);

/// Serializes every test that creates a PulseAudio stream, so a sink-input
/// lookup or a monitor recording sees only its own test's stream.
static SERVER: Mutex<()> = Mutex::new(());

pub(super) fn require_pulse() -> bool {
    std::env::var("PLEXFREQ_REQUIRE_PULSE").is_ok_and(|value| value == "1")
}

/// Holds the server for one test (poisoning by a failed test is ignored).
pub(super) fn lock_server() -> MutexGuard<'static, ()> {
    SERVER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The private daemon's address; pulse::Connection pins it under cfg(test).
fn test_server() -> Option<String> {
    std::env::var("PLEXFREQ_PULSE_TEST_SERVER").ok()
}

fn isolated_server(test: &str) -> Option<MutexGuard<'static, ()>> {
    let guard = lock_server();
    let isolated =
        std::env::var("PLEXFREQ_PULSE_ISOLATED").is_ok_and(|v| v == "1") && test_server().is_some();
    let reason = if !isolated {
        "PLEXFREQ_PULSE_ISOLATED=1/PLEXFREQ_PULSE_TEST_SERVER not set \
         (run under tools/pulse-test-server.sh)"
            .to_string()
    } else {
        match try_pactl(&["list", "short", "sinks"]) {
            Ok(sinks) if sinks.contains(SINK_A) && sinks.contains(SINK_B) => return Some(guard),
            Ok(sinks) => format!("isolated server lacks the {SINK_A}/{SINK_B} null sinks: {sinks}"),
            Err(error) => format!("pactl failed: {error}"),
        }
    };
    assert!(
        !require_pulse(),
        "{test}: PLEXFREQ_REQUIRE_PULSE=1 but no isolated PulseAudio server: {reason}"
    );
    eprintln!("{test}: skipping, {reason}");
    None
}

fn try_pactl(args: &[&str]) -> Result<String, String> {
    let output = Command::new("pactl")
        .args(args)
        .env("LC_ALL", "C")
        .env("PULSE_SERVER", test_server().unwrap_or_default())
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "pactl {args:?}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn pactl(args: &[&str]) -> String {
    try_pactl(args).unwrap_or_else(|error| panic!("{error}"))
}

fn sink_name(index: &str) -> String {
    pactl(&["list", "short", "sinks"])
        .lines()
        .find_map(|line| {
            let mut fields = line.split('\t');
            (fields.next() == Some(index)).then(|| fields.next().unwrap_or_default().to_string())
        })
        .unwrap_or_default()
}

/// PlexFreq's sink inputs as (index, sink name), from the long listing:
/// other clients (keepalive streams, a recorder) may share the server.
fn plexfreq_inputs() -> Vec<(String, String)> {
    let mut inputs = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in pactl(&["list", "sink-inputs"]).lines() {
        let line = line.trim();
        if let Some(index) = line.strip_prefix("Sink Input #") {
            current = Some((index.to_string(), String::new()));
        } else if let (Some(sink), Some(input)) = (line.strip_prefix("Sink: "), current.as_mut()) {
            input.1 = sink.to_string();
        } else if line == "application.id = \"harbour-plexfreq\"" {
            inputs.extend(current.take());
        }
    }
    inputs
        .into_iter()
        .map(|(index, sink)| (index, sink_name(&sink)))
        .collect()
}

/// The single PlexFreq sink input: its index and current sink name.
fn sink_input() -> (String, String) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let inputs = plexfreq_inputs();
        if let [input] = &inputs[..] {
            return input.clone();
        }
        assert!(
            Instant::now() < deadline,
            "expected exactly one PlexFreq sink input, got {inputs:?}"
        );
        thread::sleep(POLL);
    }
}

fn move_stream(sink: &str) {
    let (index, _) = sink_input();
    pactl(&["move-sink-input", &index, sink]);
    let deadline = Instant::now() + Duration::from_secs(2);
    while sink_input().1 != sink {
        assert!(Instant::now() < deadline, "stream did not move to {sink}");
        thread::sleep(POLL);
    }
}

/// 48 kHz stereo 16-bit sine (no resampling): non-silent everywhere, so a
/// sink-monitor recording shows any underrun as a run of silence.
fn tone(dir: &Path, name: &str, seconds: f64, hz: f64) -> PathBuf {
    let path = dir.join(name);
    let frames = (seconds * f64::from(RATE)) as u32;
    let mut file = BufWriter::new(std::fs::File::create(&path).unwrap());
    let data = frames * 4;
    file.write_all(b"RIFF").unwrap();
    file.write_all(&(36 + data).to_le_bytes()).unwrap();
    file.write_all(b"WAVEfmt ").unwrap();
    file.write_all(&16u32.to_le_bytes()).unwrap();
    file.write_all(&1u16.to_le_bytes()).unwrap(); // PCM
    file.write_all(&2u16.to_le_bytes()).unwrap(); // stereo
    file.write_all(&RATE.to_le_bytes()).unwrap();
    file.write_all(&(RATE * 4).to_le_bytes()).unwrap();
    file.write_all(&4u16.to_le_bytes()).unwrap();
    file.write_all(&16u16.to_le_bytes()).unwrap();
    file.write_all(b"data").unwrap();
    file.write_all(&data.to_le_bytes()).unwrap();
    for frame in 0..frames {
        let phase = f64::from(frame) * hz * std::f64::consts::TAU / f64::from(RATE);
        let sample = (phase.sin() * 0.3 * f64::from(i16::MAX)) as i16;
        file.write_all(&sample.to_le_bytes()).unwrap();
        file.write_all(&sample.to_le_bytes()).unwrap();
    }
    file.flush().unwrap();
    path
}

fn source(id: u64, path: &Path, seconds: f64, album: &str) -> Source {
    Source {
        id,
        url: url::Url::from_file_path(path).unwrap().to_string(),
        duration: (seconds * 1000.) as u64,
        album: album.into(),
        ..Default::default()
    }
}

fn underflows() -> u64 {
    TEST_UNDERFLOWS.load(Ordering::Relaxed)
}

/// Within the final drain: the last ~300 ms queued (plus sink latency) play
/// out and the position, capped at the duration, legitimately stops.
fn near_end(state: &Snapshot) -> bool {
    state.duration > 0 && state.position + 300 >= state.duration
}

/// Polls the engine, keeping events and the longest time without position
/// progress while `tracking` (playback wanted and not deliberately paused).
struct Probe<'a> {
    engine: &'a Engine,
    events: Vec<Event>,
    position: u64,
    advanced: Instant,
    tracking: bool,
    max_stall: Duration,
    /// What the test was waiting for, attached to stall/underflow reports.
    phase: String,
    stalled_in: String,
    started: Instant,
    underflows: u64,
    /// Underflows as (ms since the probe started, track id, position ms, phase).
    underflow_log: Vec<(u128, u64, u64, String)>,
    /// Recent samples (ms since start, position, playing, buffering) for
    /// failure messages.
    trace: std::collections::VecDeque<(u128, u64, bool, bool)>,
}
impl<'a> Probe<'a> {
    fn new(engine: &'a Engine) -> Self {
        Self {
            engine,
            events: Vec::new(),
            position: 0,
            advanced: Instant::now(),
            tracking: false,
            max_stall: Duration::ZERO,
            phase: "start".into(),
            stalled_in: String::new(),
            started: Instant::now(),
            underflows: underflows(),
            underflow_log: Vec::new(),
            trace: std::collections::VecDeque::new(),
        }
    }
    fn poll(&mut self) -> Snapshot {
        let state = self.engine.handle.snapshot();
        self.events.extend(self.engine.events());
        let now = Instant::now();
        if state.position != self.position {
            self.position = state.position;
            self.advanced = now;
        } else if self.tracking && !near_end(&state) {
            let stall = now.duration_since(self.advanced);
            if stall > self.max_stall {
                self.max_stall = stall;
                self.stalled_in = format!("{} at position {}", self.phase, state.position);
            }
        }
        let count = underflows();
        if count != self.underflows {
            self.underflows = count;
            self.underflow_log.push((
                now.duration_since(self.started).as_millis(),
                state.id,
                state.position,
                self.phase.clone(),
            ));
        }
        if self.trace.len() == 120 {
            self.trace.pop_front();
        }
        self.trace.push_back((
            now.duration_since(self.started).as_millis(),
            state.position,
            state.playing,
            state.buffering,
        ));
        assert!(
            !self.events.iter().any(|e| matches!(e, Event::Error { .. })),
            "engine error: {:?}",
            self.events
        );
        state
    }
    fn track(&mut self, on: bool) {
        self.poll();
        self.tracking = on;
        self.advanced = Instant::now();
    }
    fn run_for(&mut self, duration: Duration) -> Snapshot {
        let end = Instant::now() + duration;
        loop {
            let state = self.poll();
            if Instant::now() >= end {
                return state;
            }
            thread::sleep(POLL);
        }
    }
    fn until(
        &mut self,
        timeout: Duration,
        what: &str,
        mut done: impl FnMut(&Snapshot, &[Event]) -> bool,
    ) -> Snapshot {
        self.phase = what.to_string();
        let deadline = Instant::now() + timeout;
        loop {
            let state = self.poll();
            if done(&state, &self.events) {
                return state;
            }
            assert!(
                Instant::now() < deadline,
                "timed out after {timeout:?} waiting for {what}; state {state:?}, events {:?}, \
                 {}, recent (ms, position, playing, buffering) {:?}",
                self.events,
                self.report(),
                self.trace
            );
            thread::sleep(POLL);
        }
    }
    fn playing(&mut self) -> Snapshot {
        self.until(Duration::from_secs(3), "playing", |s, _| s.playing)
    }
    /// Position must advance by `ms` within a generous wall-clock budget.
    fn advances(&mut self, ms: u64, what: &str) -> Snapshot {
        let from = self.poll().position;
        self.until(
            Duration::from_millis(ms + 1000),
            &format!("{what}: {ms} ms of progress from {from}"),
            |s, _| s.position >= from + ms,
        )
    }
    /// Paused: position stays put (within interpolation noise).
    fn frozen(&mut self, what: &str) {
        self.until(Duration::from_secs(1), "paused snapshot", |s, _| {
            s.paused && !s.playing
        });
        let from = self.run_for(Duration::from_millis(150)).position;
        let to = self.run_for(Duration::from_millis(500)).position;
        assert!(
            to.abs_diff(from) <= 30,
            "{what}: position moved while paused ({from} -> {to})"
        );
    }
    /// Underflows other than the one PulseAudio reports when a drained
    /// stream's queue runs out within the last `tail_ms` of track `last`
    /// (protocol-native sends UNDERFLOW while a drain is still pending).
    fn underflows_except_final_drain(&self, last: u64, duration: u64, tail_ms: u64) -> usize {
        self.underflow_log
            .iter()
            .filter(|(_, id, position, _)| !(*id == last && position + tail_ms >= duration))
            .count()
    }
    fn report(&self) -> String {
        format!(
            "max stall {:?} ({}), underflows (ms, id, position, phase) {:?}",
            self.max_stall, self.stalled_in, self.underflow_log
        )
    }
    fn assert_no_stall(&self, what: &str) {
        assert!(
            self.max_stall < MAX_STALL,
            "{what}: playback stalled for {:?} ({}); underflows {:?}",
            self.max_stall,
            self.stalled_in,
            self.underflow_log
        );
    }
}

/// Records a sink's monitor as mono 16-bit PCM through `parec`.
struct Monitor {
    child: Child,
    path: PathBuf,
}
impl Monitor {
    fn start(sink: &str, path: PathBuf) -> Self {
        let child = Command::new("parec")
            .args([
                &format!("--device={sink}.monitor"),
                "--format=s16le",
                "--rate=48000",
                "--channels=1",
                "--raw",
                "--latency-msec=20",
            ])
            .env("PULSE_SERVER", test_server().unwrap_or_default())
            .stdout(std::fs::File::create(&path).unwrap())
            .stderr(Stdio::null())
            .spawn()
            .expect("parec (pulseaudio-utils) is required for monitor checks");
        // Let the record stream connect before the first audio arrives.
        let deadline = Instant::now() + Duration::from_secs(3);
        while !pactl(&["list", "short", "source-outputs"]).contains('\t') {
            assert!(Instant::now() < deadline, "parec did not connect");
            thread::sleep(POLL);
        }
        Self { child, path }
    }
    /// (audible milliseconds from first to last audible sample, longest
    /// silent run inside it in milliseconds).
    fn stop(mut self) -> (u64, u64) {
        // Let the tail reach the recorder before stopping it.
        thread::sleep(Duration::from_millis(300));
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Debugging aid: keep the raw recording (s16le mono 48 kHz).
        if let Some(dir) = std::env::var_os("PLEXFREQ_PULSE_KEEP_PCM") {
            let name = self.path.file_name().unwrap_or_default();
            let _ = std::fs::copy(&self.path, Path::new(&dir).join(name));
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&self.path)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        let samples: Vec<i16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        silence_profile(&samples)
    }
}

/// Audible span and longest quiet run within it, in ms at 48 kHz. Quiet is
/// below ~1% of full scale; the 0.24-amplitude fixture tone spends at most a
/// sample or two near each zero crossing, while an underrun inserts tens of
/// milliseconds of digital silence.
fn silence_profile(samples: &[i16]) -> (u64, u64) {
    let loud = |s: &i16| s.unsigned_abs() > 300;
    let (Some(first), Some(last)) = (
        samples.iter().position(loud),
        samples.iter().rposition(loud),
    ) else {
        return (0, 0);
    };
    let (mut longest, mut run) = (0usize, 0usize);
    for sample in &samples[first..=last] {
        if loud(sample) {
            run = 0;
        } else {
            run += 1;
            longest = longest.max(run);
        }
    }
    let ms = |n: usize| n as u64 * 1000 / u64::from(RATE);
    (ms(last + 1 - first), ms(longest))
}

/// Busy threads competing with the actor and the daemon for every core.
struct Load(Arc<AtomicBool>, Vec<thread::JoinHandle<()>>);
impl Load {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let cores = thread::available_parallelism().map_or(2, |n| n.get());
        let threads = (0..cores)
            .map(|_| {
                let stop = stop.clone();
                thread::spawn(move || {
                    let mut x = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        for _ in 0..10_000 {
                            x = std::hint::black_box(x.wrapping_mul(6364136223846793005) + 1);
                        }
                    }
                })
            })
            .collect();
        Self(stop, threads)
    }
}
impl Drop for Load {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
        for thread in self.1.drain(..) {
            let _ = thread.join();
        }
    }
}

#[test]
fn silence_profile_finds_inserted_gaps() {
    let tone: Vec<i16> = (0..4800)
        .map(|n| ((f64::from(n) * 440. * std::f64::consts::TAU / 48000.).sin() * 7800.) as i16)
        .collect();
    let mut samples = vec![0i16; 480];
    samples.extend(&tone);
    // The sine starts at a zero crossing: its first sample is quiet.
    assert_eq!(silence_profile(&samples), (99, 0));
    samples.extend(vec![0i16; 960]);
    samples.extend(&tone);
    samples.extend(vec![0i16; 4800]);
    assert_eq!(silence_profile(&samples), (219, 20));
}

/// A minute through the real server: zero underflows, no silent gap on the
/// sink, position tracks the wall clock, End arrives on time.
#[test]
fn pulse_soak_plays_a_minute_in_real_time_without_underflow() {
    let Some(_server) = isolated_server("pulse_soak") else {
        return;
    };
    const SECONDS: f64 = 62.;
    let dir = tempfile::tempdir().unwrap();
    let path = tone(dir.path(), "soak.wav", SECONDS, 440.);
    move_default_sink();
    let monitor = Monitor::start(SINK_A, dir.path().join("soak.pcm"));
    let engine = Engine::new(Sink::Pulse).unwrap();
    let mut probe = Probe::new(&engine);
    engine
        .handle
        .load(source(1, &path, SECONDS, ""), false)
        .unwrap();
    let start = probe.playing();
    let (t0, p0) = (Instant::now(), start.position);
    probe.track(true);
    let mut drift_max = 0i64;
    let mut buffering_samples = 0;
    let mut samples = 0;
    let state = probe.until(
        Duration::from_secs_f64(SECONDS + 15.),
        "natural End",
        |state, events| {
            if events.iter().any(|e| matches!(e, Event::End { id: 1, .. })) {
                return true;
            }
            samples += 1;
            buffering_samples += usize::from(state.buffering && !near_end(state));
            let elapsed = t0.elapsed().as_millis() as i64;
            let advanced = state.position as i64 - p0 as i64;
            if elapsed > 2000 {
                drift_max = drift_max.max((advanced - elapsed).abs());
            }
            false
        },
    );
    let elapsed = t0.elapsed();
    let expected = Duration::from_millis((SECONDS * 1000.) as u64 - p0);
    let (audible, gap) = monitor.stop();
    probe.poll();
    let underflowed = probe.underflows_except_final_drain(1, (SECONDS * 1000.) as u64, 300);
    eprintln!(
        "pulse_soak: wall {elapsed:?} for {expected:?} of media, max drift {drift_max} ms, \
         buffering {buffering_samples}/{samples} samples, underflows {underflowed}, \
         monitor audible {audible} ms, longest gap {gap} ms, end {state:?}, {}",
        probe.report()
    );
    assert_eq!(underflowed, 0, "PulseAudio reported underflows");
    assert!(
        drift_max <= 500,
        "position drifted {drift_max} ms from wall clock"
    );
    assert!(
        elapsed.abs_diff(expected) <= Duration::from_millis(1500),
        "End after {elapsed:?}, expected {expected:?}"
    );
    assert_eq!(buffering_samples, 0, "buffering flicker while playing");
    probe.assert_no_stall("soak");
    assert!(gap <= 5, "{gap} ms of silence inside the played audio");
    assert!(
        audible.abs_diff((SECONDS * 1000.) as u64) <= 600,
        "{audible} ms audible on the sink, expected {SECONDS} s"
    );
}

/// The stream is rescued to plexfreq_a when a test leaves the default sink
/// elsewhere; keep monitor-based tests independent of earlier moves.
fn move_default_sink() {
    pactl(&["set-default-sink", SINK_A]);
}

/// Repeated `pactl move-sink-input` between the null sinks mid-track, while
/// playing and while paused: playback continues without a stall and
/// pause/play keep working (the car deadlock: Play after a move did nothing).
#[test]
fn pulse_stream_survives_moves_between_sinks() {
    let Some(_server) = isolated_server("pulse_sink_move") else {
        return;
    };
    const SECONDS: f64 = 40.;
    let dir = tempfile::tempdir().unwrap();
    let path = tone(dir.path(), "move.wav", SECONDS, 330.);
    move_default_sink();
    let engine = Engine::new(Sink::Pulse).unwrap();
    let mut probe = Probe::new(&engine);
    engine
        .handle
        .load(source(1, &path, SECONDS, ""), false)
        .unwrap();
    probe.playing();
    probe.track(true);
    probe.advances(1000, "before the first move");
    for round in 0..6 {
        let target = if round % 2 == 0 { SINK_B } else { SINK_A };
        move_stream(target);
        probe.advances(1200, &format!("after move {round} to {target}"));
    }
    probe.assert_no_stall("playing through moves");
    // Pause, move while paused (the 15:47 Bluetooth disconnect), then Play.
    for round in 0..3 {
        probe.track(false);
        engine.handle.pause().unwrap();
        probe.frozen(&format!("pause {round}"));
        let target = if round % 2 == 0 { SINK_B } else { SINK_A };
        move_stream(target);
        probe.run_for(Duration::from_millis(300));
        engine.handle.play().unwrap();
        probe.track(true);
        probe.until(
            Duration::from_secs(1),
            "Play after a paused move",
            |s, _| s.playing,
        );
        probe.advances(1000, &format!("Play after paused move {round}"));
    }
    probe.assert_no_stall("Play after paused moves");
    let state = probe.poll();
    eprintln!("pulse_sink_move: final {state:?}, {}", probe.report());
}

/// The sink disappears under the stream (Bluetooth disconnect): the server
/// rescues the stream to the remaining sink, dropping what it had queued.
/// Playback must continue, and Play after a paused rescue must play.
#[test]
fn pulse_stream_survives_removal_of_its_sink() {
    let Some(_server) = isolated_server("pulse_sink_removal") else {
        return;
    };
    const SECONDS: f64 = 30.;
    let dir = tempfile::tempdir().unwrap();
    let path = tone(dir.path(), "removal.wav", SECONDS, 550.);
    move_default_sink();
    let engine = Engine::new(Sink::Pulse).unwrap();
    let mut probe = Probe::new(&engine);
    engine
        .handle
        .load(source(1, &path, SECONDS, ""), false)
        .unwrap();
    probe.playing();
    probe.track(true);
    for paused in [false, true, false, true] {
        let module = pactl(&[
            "load-module",
            "module-null-sink",
            "sink_name=plexfreq_car",
            "rate=48000",
            "channels=2",
            "format=float32le",
        ]);
        let module = module.trim().to_string();
        move_stream("plexfreq_car");
        probe.advances(800, "on the temporary sink");
        if paused {
            probe.track(false);
            engine.handle.pause().unwrap();
            probe.frozen("before sink removal");
        }
        pactl(&["unload-module", &module]);
        let deadline = Instant::now() + Duration::from_secs(2);
        while sink_input().1 == "plexfreq_car" || sink_input().1.is_empty() {
            assert!(Instant::now() < deadline, "stream was not rescued");
            thread::sleep(POLL);
        }
        if paused {
            probe.run_for(Duration::from_millis(300));
            engine.handle.play().unwrap();
            probe.track(true);
        }
        probe.until(Duration::from_secs(1), "playing after rescue", |s, _| {
            s.playing
        });
        probe.advances(1000, &format!("after sink removal (paused={paused})"));
    }
    probe.assert_no_stall("sink removal");
    eprintln!("pulse_sink_removal: {}", probe.report());
}

/// The server discards our queued audio without advancing stream time (the
/// 2026-10-16 car log after the Bluetooth sink vanished; null sinks rewind
/// on a move, so this is injected with pa_stream_flush). The writer must
/// follow the server's request, not "submitted minus played", which still
/// claims a full buffer: that starved and finally deadlocked the stream.
#[test]
fn pulse_discarded_server_queue_neither_starves_nor_deadlocks() {
    let Some(_server) = isolated_server("pulse_discarded_queue") else {
        return;
    };
    const SECONDS: f64 = 30.;
    let dir = tempfile::tempdir().unwrap();
    let path = tone(dir.path(), "flush.wav", SECONDS, 770.);
    move_default_sink();
    let engine = Engine::new(Sink::Pulse).unwrap();
    let mut probe = Probe::new(&engine);
    engine
        .handle
        .load(source(1, &path, SECONDS, ""), false)
        .unwrap();
    probe.playing();
    probe.track(true);
    for round in 0..4 {
        probe.advances(700, &format!("before discard {round}"));
        let paused = round % 2 == 1;
        if paused {
            probe.track(false);
            engine.handle.pause().unwrap();
            probe.frozen(&format!("paused before discard {round}"));
        }
        probe.phase = format!("discard {round} (paused={paused})");
        TEST_FLUSH.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + Duration::from_secs(1);
        while TEST_FLUSH.load(Ordering::Relaxed) {
            assert!(Instant::now() < deadline, "output never serviced the flush");
            probe.run_for(POLL);
        }
        if paused {
            probe.run_for(Duration::from_millis(300));
            engine.handle.play().unwrap();
            probe.track(true);
        }
        probe.until(Duration::from_secs(1), "playing after discard", |s, _| {
            s.playing
        });
        probe.advances(1000, &format!("after discard {round} (paused={paused})"));
    }
    probe.assert_no_stall("discarded server queue");
    eprintln!("pulse_discarded_queue: {}", probe.report());
}

/// Pause/Play and seeks in quick succession while every core is busy.
#[test]
fn pulse_cork_uncork_and_seek_under_load() {
    let Some(_server) = isolated_server("pulse_cork_seek") else {
        return;
    };
    const SECONDS: f64 = 40.;
    let dir = tempfile::tempdir().unwrap();
    let path = tone(dir.path(), "seek.wav", SECONDS, 660.);
    move_default_sink();
    let _load = Load::start();
    let engine = Engine::new(Sink::Pulse).unwrap();
    let mut probe = Probe::new(&engine);
    engine
        .handle
        .load(source(1, &path, SECONDS, ""), false)
        .unwrap();
    probe.playing();
    probe.track(true);
    for round in 0..10u64 {
        probe.advances(300, &format!("round {round}"));
        probe.track(false);
        engine.handle.pause().unwrap();
        probe.frozen(&format!("pause round {round}"));
        engine.handle.play().unwrap();
        probe.track(true);
        probe.until(Duration::from_secs(1), "playing after uncork", |s, _| {
            s.playing
        });
        probe.advances(300, &format!("uncork round {round}"));
        let target = 2000 + round * 7919 % 30000;
        probe.events.clear();
        engine.handle.seek(target).unwrap();
        probe.track(false);
        let state = probe.until(Duration::from_secs(2), "seek", |s, events| {
            events
                .iter()
                .any(|e| matches!(e, Event::Seeked(p) if *p == target))
                && s.playing
                && s.position >= target
        });
        assert!(
            state.position < target + 1000,
            "seek to {target} landed at {}",
            state.position
        );
        probe.track(true);
        probe.advances(300, &format!("after seek to {target}"));
    }
    probe.assert_no_stall("cork/seek under load");
    eprintln!("pulse_cork_seek: {}", probe.report());
    assert!(probe.underflow_log.is_empty(), "underflows under load");
}

/// A prepared same-album successor follows on the same server stream: one
/// Transition at the boundary, no underflow and no silence on the sink, and
/// the audible span equals both tracks.
#[test]
fn pulse_prepared_successor_is_gapless() {
    let Some(_server) = isolated_server("pulse_gapless") else {
        return;
    };
    const SECONDS: f64 = 4.;
    let dir = tempfile::tempdir().unwrap();
    let first = tone(dir.path(), "first.wav", SECONDS, 440.);
    let second = tone(dir.path(), "second.wav", SECONDS, 523.25);
    move_default_sink();
    let monitor = Monitor::start(SINK_A, dir.path().join("gapless.pcm"));
    let engine = Engine::new(Sink::Pulse).unwrap();
    let mut probe = Probe::new(&engine);
    engine
        .handle
        .load(source(1, &first, SECONDS, "album"), false)
        .unwrap();
    engine
        .handle
        .prepare(Some(source(2, &second, SECONDS, "album")))
        .unwrap();
    probe.playing();
    let t0 = Instant::now();
    probe.track(true);
    probe.until(Duration::from_secs(7), "Transition 1 -> 2", |_, events| {
        events
            .iter()
            .any(|e| matches!(e, Event::Transition { old: 1, new: 2, .. }))
    });
    let transition = t0.elapsed();
    probe.until(Duration::from_secs(7), "End of 2", |_, events| {
        events.iter().any(|e| matches!(e, Event::End { id: 2, .. }))
    });
    let end = t0.elapsed();
    let (audible, gap) = monitor.stop();
    probe.poll();
    let underflowed = probe.underflows_except_final_drain(2, (SECONDS * 1000.) as u64, 300);
    eprintln!(
        "pulse_gapless: transition at {transition:?}, end at {end:?}, \
         audible {audible} ms, longest gap {gap} ms, {}",
        probe.report()
    );
    assert!(
        transition.abs_diff(Duration::from_secs(4)) <= Duration::from_millis(600),
        "transition at {transition:?}"
    );
    assert!(
        end.abs_diff(Duration::from_secs(8)) <= Duration::from_millis(1000),
        "end at {end:?}"
    );
    assert_eq!(underflowed, 0, "underflow at the track boundary");
    assert!(gap <= 5, "{gap} ms of silence between gapless tracks");
    assert!(
        audible.abs_diff(8000) <= 100,
        "{audible} ms audible, expected 8000 (gap or overlap)"
    );
}
