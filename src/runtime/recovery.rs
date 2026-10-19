//! Background health probes for the worker (stale-queue stage 1).
//!
//! Probes are started only after something failed, run on the shared I/O
//! runtime and report into a lossless queue that the worker drains at the
//! top of every loop iteration; the worker never waits for them. Their rate
//! is bounded per minute and by concurrency, with headroom reserved for the
//! track the user is trying to play.
use crate::{plex::Probe, Core};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

/// Probes per rolling minute, all sources together.
const BUDGET: usize = 10;
/// Of which this many are kept for current-track failures.
const PRIORITY_RESERVE: usize = 3;
const WINDOW: Duration = Duration::from_secs(60);
/// Concurrent probes; one slot is kept for current-track failures.
const SLOTS: usize = 2;
/// Wait for a current-track verdict before reporting the original failure.
pub(super) const VERDICT_DEADLINE: Duration = Duration::from_secs(8);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Class {
    /// The current track failed: the user is waiting on it.
    Priority,
    /// A cache download or gapless pre-load failed: record only.
    Background,
}

struct Outcome {
    epoch: u64,
    key: String,
    probe: Probe,
}

/// Pushes the probe outcome when the task ends, even by abort or panic, so a
/// slot can never stay occupied.
struct Report {
    results: Arc<Mutex<VecDeque<Outcome>>>,
    epoch: u64,
    key: String,
    probe: Probe,
}
impl Drop for Report {
    fn drop(&mut self) {
        crate::mutex_lock(&self.results).push_back(Outcome {
            epoch: self.epoch,
            key: std::mem::take(&mut self.key),
            probe: self.probe,
        });
    }
}

/// A current-track failure waiting for its verdict.
pub(super) struct Waiting {
    pub key: String,
    pub occurrence: u64,
    pub acted: u64,
    pub paused: bool,
    pub back: bool,
    pub message: &'static str,
    pub deadline: Instant,
}

pub(super) struct Recovery {
    pub epoch_handle: Arc<AtomicU64>,
    epoch: u64,
    results: Arc<Mutex<VecDeque<Outcome>>>,
    jobs: HashMap<String, (crate::rt::Task, Class)>,
    started: VecDeque<Instant>,
    /// Probes are refused for this epoch after the server rejected the token.
    suspended: Option<u64>,
    pub waiting: Option<Waiting>,
    /// A `waiting` failure invalidated by an epoch change, for the worker to
    /// report.
    pub orphaned: Option<Waiting>,
    /// Gapless preparation is held back for this occurrence after its
    /// successor failed to pre-load, until that successor's verdict.
    pub hold_prepare: Option<(u64, String)>,
}

impl Recovery {
    pub(super) fn new() -> Self {
        Self {
            epoch_handle: Arc::new(AtomicU64::new(0)),
            epoch: 0,
            results: Arc::default(),
            jobs: HashMap::new(),
            started: VecDeque::new(),
            suspended: None,
            waiting: None,
            orphaned: None,
            hold_prepare: None,
        }
    }
    /// Follow Core's epoch: a queue/server change or rebuild invalidates
    /// every running probe and pending verdict.
    pub(super) fn sync(&mut self) {
        let epoch = self.epoch_handle.load(Ordering::SeqCst);
        if epoch != self.epoch {
            self.epoch = epoch;
            self.jobs.clear();
            if let Some(waiting) = self.waiting.take() {
                self.orphaned = Some(waiting);
            }
            self.hold_prepare = None;
        }
    }
    /// Invalidate everything now (Core rebuild).
    pub(super) fn reset(&mut self) {
        self.epoch_handle.fetch_add(1, Ordering::SeqCst);
        self.sync();
    }
    /// Probes started in the last minute, and probes running now.
    fn used(&mut self) -> (usize, usize) {
        while self
            .started
            .front()
            .is_some_and(|at| at.elapsed() >= WINDOW)
        {
            self.started.pop_front();
        }
        (self.started.len(), self.jobs.len())
    }
    /// Start a probe of `key`, or join the one already running. Returns
    /// whether a verdict will arrive.
    pub(super) fn request(&mut self, core: &Core, key: &str, class: Class) -> bool {
        self.sync();
        if let Some((_, running)) = self.jobs.get_mut(key) {
            if class == Class::Priority {
                *running = Class::Priority;
            }
            return true;
        }
        if self.suspended == Some(self.epoch) || !core.may_probe(key) {
            return false;
        }
        let (total, running) = self.used();
        let (budget, slots) = match class {
            Class::Priority => (BUDGET, SLOTS),
            Class::Background => (BUDGET - PRIORITY_RESERVE, SLOTS - 1),
        };
        if total >= budget || running >= slots {
            return false;
        }
        let Some(probe) = core.probe(key) else {
            return false;
        };
        let report = Report {
            results: self.results.clone(),
            epoch: self.epoch,
            key: key.to_owned(),
            probe: Probe::Unknown,
        };
        let task = crate::rt::spawn(async move {
            let mut report = report;
            report.probe = probe.await;
        });
        crate::diagnostics::event(format_args!(
            "health probe started key={key} priority={}",
            class == Class::Priority
        ));
        self.started.push_back(Instant::now());
        self.jobs.insert(key.to_owned(), (task, class));
        true
    }
    /// Apply finished probes to Core; returns the current-epoch outcomes.
    pub(super) fn drain(&mut self, core: &mut Core) -> Vec<(String, Probe)> {
        self.sync();
        let finished: Vec<_> = crate::mutex_lock(&self.results).drain(..).collect();
        let mut applied = Vec::new();
        for outcome in finished {
            if outcome.epoch != self.epoch {
                continue;
            }
            self.jobs.remove(&outcome.key);
            crate::diagnostics::event(format_args!(
                "health probe result key={} outcome={:?}",
                outcome.key, outcome.probe
            ));
            if outcome.probe == Probe::Auth {
                self.suspended = Some(self.epoch);
            }
            core.record_probe(&outcome.key, outcome.probe);
            applied.push((outcome.key, outcome.probe));
        }
        applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core(dir: &std::path::Path) -> Core {
        crate::store::save(
            dir,
            &crate::store::Settings {
                client_identifier: "fixture-client".into(),
                // Nothing listens here: probes end quickly as Unknown.
                server_url: "http://127.0.0.1:9/".into(),
                server_token: "fixture-token".into(),
                cache: crate::cache::Config {
                    enabled: false,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
        Core::new(dir.into()).unwrap()
    }

    #[test]
    fn a_slot_and_budget_stay_reserved_for_the_current_track() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = core(dir.path());
        let mut recovery = Recovery::new();
        core.set_health_epoch(recovery.epoch_handle.clone());
        assert!(recovery.request(&core, "1", Class::Background));
        assert!(
            !recovery.request(&core, "2", Class::Background),
            "one background slot"
        );
        assert!(
            recovery.request(&core, "3", Class::Priority),
            "the reserved slot"
        );
        assert!(
            !recovery.request(&core, "4", Class::Priority),
            "both slots busy"
        );
        // Joining a running probe needs no slot.
        assert!(recovery.request(&core, "1", Class::Priority));
        let end = Instant::now() + Duration::from_secs(10);
        let mut outcomes = Vec::new();
        while outcomes.len() < 2 && Instant::now() < end {
            outcomes.extend(recovery.drain(&mut core));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(outcomes.len(), 2, "every started probe reports");
        assert!(outcomes.iter().all(|(_, probe)| *probe == Probe::Unknown));
        // Inconclusive keys cool down; the budget then limits new keys.
        assert!(!recovery.request(&core, "1", Class::Priority));
        let mut started = 2;
        for key in 10..40 {
            if recovery.request(&core, &key.to_string(), Class::Priority) {
                started += 1;
            }
            recovery.jobs.clear();
        }
        assert_eq!(started, BUDGET);
    }

    #[test]
    fn background_probes_leave_budget_for_the_current_track() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = core(dir.path());
        let mut recovery = Recovery::new();
        core.set_health_epoch(recovery.epoch_handle.clone());
        let mut started = 0;
        for key in 10..40 {
            if recovery.request(&core, &key.to_string(), Class::Background) {
                started += 1;
            }
            recovery.jobs.clear();
        }
        assert_eq!(started, BUDGET - PRIORITY_RESERVE);
        assert!(recovery.request(&core, "99", Class::Priority));
    }

    #[test]
    fn a_new_epoch_discards_running_probes_and_waiting_verdicts() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = core(dir.path());
        let mut recovery = Recovery::new();
        core.set_health_epoch(recovery.epoch_handle.clone());
        assert!(recovery.request(&core, "1", Class::Priority));
        recovery.waiting = Some(Waiting {
            key: "1".into(),
            occurrence: 1,
            acted: 0,
            paused: false,
            back: false,
            message: "Audio decoding or streaming failed",
            deadline: Instant::now(),
        });
        recovery.reset();
        assert!(recovery.waiting.is_none());
        // Handed to the worker, which still reports it if the track is current.
        assert_eq!(recovery.orphaned.take().map(|w| w.key), Some("1".into()));
        assert!(recovery.jobs.is_empty());
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            recovery.drain(&mut core).is_empty(),
            "stale results are dropped"
        );
    }
}
