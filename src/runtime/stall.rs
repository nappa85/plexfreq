//! GUI stall diagnostics: the Qt GUI thread polls the runtime every 100 ms. A
//! watchdog thread owned by the `Runtime` (independent of the worker and of the
//! shared async runtime, so a busy or blocked worker cannot hide a stall) logs
//! when the gap between polls grows past [`THRESHOLD_MS`], naming the request
//! the worker was running. Only timing and fixed op names are logged.
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

/// A poll gap above this is a stall.
pub(super) const THRESHOLD_MS: u64 = 2000;
const INTERVAL: Duration = Duration::from_secs(1);

/// Whether the watchdog must report a stall now: after the first poll
/// (`last == 0` means none yet), once per stall, when the gap exceeds the
/// threshold. Returns the gap in milliseconds.
pub(super) fn stall_to_report(last: u64, now: u64, reported: bool) -> Option<u64> {
    if last == 0 || reported {
        return None;
    }
    let gap = now.saturating_sub(last);
    (gap > THRESHOLD_MS).then_some(gap)
}

/// The gap `poll` logs when it resumes after a reported stall.
pub(super) fn resumed_gap(previous: u64, now: u64, reported: bool) -> Option<u64> {
    (reported && previous != 0).then(|| now.saturating_sub(previous))
}

pub(crate) struct Heartbeat {
    start: Instant,
    /// Milliseconds since `start` plus one at the last poll; 0 before the first.
    last_poll: AtomicU64,
    reported: AtomicBool,
    worker_op: Mutex<&'static str>,
    /// Test clock offset, so a stall needs no real waiting.
    #[cfg(test)]
    skew: AtomicU64,
}

/// Marks the worker request in progress until dropped.
pub(crate) struct OpGuard<'a>(&'a Heartbeat);
impl Drop for OpGuard<'_> {
    fn drop(&mut self) {
        self.0.set_op("idle");
    }
}

impl Heartbeat {
    pub(crate) fn new() -> Self {
        Self {
            start: Instant::now(),
            last_poll: AtomicU64::new(0),
            reported: AtomicBool::new(false),
            worker_op: Mutex::new("idle"),
            #[cfg(test)]
            skew: AtomicU64::new(0),
        }
    }
    fn now(&self) -> u64 {
        let now = self.start.elapsed().as_millis() as u64 + 1;
        #[cfg(test)]
        let now = now + self.skew.load(Ordering::SeqCst);
        now
    }
    /// Called at `Runtime::poll` entry. A poll racing the watchdog right at the
    /// end of a stall can log a `stalled` line just before its own `resumed`
    /// line (or a `resumed` for a stall reported an instant later); this is
    /// benign, both lines carry true gaps.
    pub(crate) fn polled(&self) {
        let now = self.now();
        let previous = self.last_poll.swap(now, Ordering::SeqCst);
        if let Some(gap) = resumed_gap(previous, now, self.reported.swap(false, Ordering::SeqCst)) {
            crate::diagnostics::event(format_args!("gui poll resumed gap_ms={gap}"));
        }
    }
    fn set_op(&self, op: &'static str) {
        *crate::mutex_lock(&self.worker_op) = op;
    }
    pub(crate) fn begin(&self, op: &'static str) -> OpGuard<'_> {
        self.set_op(op);
        OpGuard(self)
    }
    /// Pretend `ms` milliseconds passed since the last poll.
    #[cfg(test)]
    pub(crate) fn pretend_stalled(&self, ms: u64) {
        self.skew.fetch_add(ms, Ordering::SeqCst);
    }
    #[cfg(test)]
    pub(crate) fn check_for_test(&self) -> Option<u64> {
        self.check()
    }
    /// One watchdog tick; returns the logged gap.
    fn check(&self) -> Option<u64> {
        let last = self.last_poll.load(Ordering::SeqCst);
        let gap = stall_to_report(last, self.now(), self.reported.load(Ordering::SeqCst))?;
        self.reported.store(true, Ordering::SeqCst);
        let op = *crate::mutex_lock(&self.worker_op);
        crate::diagnostics::event(format_args!("gui poll stalled ms={gap} worker_op={op}"));
        Some(gap)
    }
}

/// The watchdog thread; stopped and joined on drop.
pub(crate) struct Watchdog {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Watchdog {
    pub(crate) fn start(heartbeat: Arc<Heartbeat>, interval: Option<Duration>) -> Self {
        let (stop, wait) = mpsc::channel::<()>();
        let interval = interval.unwrap_or(INTERVAL);
        let thread = thread::Builder::new()
            .name("plexfreq-gui-watch".into())
            .spawn(move || {
                while matches!(
                    wait.recv_timeout(interval),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    heartbeat.check();
                }
            })
            .ok();
        Self {
            stop: Some(stop),
            thread,
        }
    }
}
impl Drop for Watchdog {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stall_is_reported_once_after_the_first_poll() {
        assert_eq!(stall_to_report(0, 10_000, false), None, "before first poll");
        assert_eq!(stall_to_report(100, 2_100, false), None, "gap at threshold");
        assert_eq!(stall_to_report(100, 2_101, false), Some(2_001));
        assert_eq!(stall_to_report(100, 9_000, true), None, "once per stall");
        assert_eq!(resumed_gap(100, 9_000, true), Some(8_900));
        assert_eq!(resumed_gap(100, 150, false), None);
        assert_eq!(resumed_gap(0, 150, true), None);
    }

    #[test]
    fn heartbeat_reports_once_and_resumes() {
        let beat = Heartbeat::new();
        assert_eq!(beat.check(), None, "no poll yet");
        beat.polled();
        assert_eq!(beat.check(), None);
        beat.pretend_stalled(3_000);
        let guard = beat.begin("browse");
        assert!(beat.check().is_some());
        assert_eq!(*crate::mutex_lock(&beat.worker_op), "browse");
        assert_eq!(beat.check(), None, "once per stall");
        drop(guard);
        assert_eq!(*crate::mutex_lock(&beat.worker_op), "idle");
        beat.polled();
        assert!(!beat.reported.load(Ordering::SeqCst));
        beat.pretend_stalled(3_000);
        assert!(beat.check().is_some(), "a new stall is reported again");
    }

    #[test]
    fn watchdog_stops_on_drop() {
        let beat = Arc::new(Heartbeat::new());
        let watchdog = Watchdog::start(beat, Some(Duration::from_millis(5)));
        thread::sleep(Duration::from_millis(20));
        drop(watchdog);
    }
}
