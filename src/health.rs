//! Per-item server health learned in the background (stale-queue stage 1).
//!
//! Tracks are only checked after something failed for them: a cache-ahead
//! download, a gapless pre-load or the current stream. A confirmed-missing
//! item is marked Gone for a while, and queue navigation then steps over it
//! like a fresh failure. Nothing here performs I/O: the runtime runs the
//! probes and feeds the outcomes back.
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

/// How long a Gone mark is trusted: a library rescan can restore an item.
pub(crate) const GONE_TTL: Duration = Duration::from_secs(10 * 60);
/// No re-probe of a key whose last probe was inconclusive.
pub(crate) const UNKNOWN_COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(crate) struct Health {
    gone: HashMap<String, Instant>,
    attempts: HashMap<String, Instant>,
    /// Shared with the runtime: results tagged with an older epoch are
    /// discarded. Bumped whenever the marks stop describing the queue.
    epoch: Arc<AtomicU64>,
    /// Keys Core wants probed (a synchronous metadata 404), drained by the
    /// runtime, which owns the probe budget.
    pending: Vec<String>,
    /// A navigation stepped over Gone items and found nothing playable.
    notice: bool,
}

/// Marks set aside while a queue replacement is planned.
pub(crate) struct Saved {
    gone: HashMap<String, Instant>,
    attempts: HashMap<String, Instant>,
}

impl Health {
    pub(crate) fn set_epoch(&mut self, epoch: Arc<AtomicU64>) {
        self.epoch = epoch;
    }
    #[cfg(test)]
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }
    pub(crate) fn is_gone(&self, key: &str) -> bool {
        self.gone.get(key).is_some_and(|at| at.elapsed() < GONE_TTL)
    }
    pub(crate) fn gone_len(&self) -> usize {
        self.gone.len()
    }
    /// Whether a probe of `key` may run now (not Gone, not cooling down).
    pub(crate) fn may_probe(&self, key: &str) -> bool {
        !self.is_gone(key)
            && !self
                .attempts
                .get(key)
                .is_some_and(|at| at.elapsed() < UNKNOWN_COOLDOWN)
    }
    pub(crate) fn mark_gone(&mut self, key: &str) {
        self.attempts.remove(key);
        self.gone.insert(key.to_owned(), Instant::now());
    }
    pub(crate) fn mark_unknown(&mut self, key: &str) {
        self.attempts.insert(key.to_owned(), Instant::now());
    }
    /// The user explicitly asked for this item again: retry it.
    pub(crate) fn forget(&mut self, key: &str) {
        self.gone.remove(key);
        self.attempts.remove(key);
    }
    pub(crate) fn request_probe(&mut self, key: &str) {
        if !self.pending.iter().any(|pending| pending == key) {
            self.pending.push(key.to_owned());
        }
    }
    pub(crate) fn take_pending(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending)
    }
    pub(crate) fn set_notice(&mut self) {
        self.notice = true;
    }
    pub(crate) fn take_notice(&mut self) -> bool {
        std::mem::take(&mut self.notice)
    }
    /// Clear the marks for a replacement that may still fail.
    pub(crate) fn set_aside(&mut self) -> Saved {
        Saved {
            gone: std::mem::take(&mut self.gone),
            attempts: std::mem::take(&mut self.attempts),
        }
    }
    /// The replacement failed: the old queue and its marks stay.
    pub(crate) fn restore(&mut self, saved: Saved) {
        self.gone = saved.gone;
        self.attempts = saved.attempts;
    }
    /// The replacement succeeded: the set-aside marks are dropped for good.
    pub(crate) fn commit(&mut self, _saved: Saved) {
        self.pending.clear();
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gone_marks_expire_and_block_reprobes() {
        let mut health = Health::default();
        health.mark_gone("1");
        assert!(health.is_gone("1"));
        assert!(!health.may_probe("1"));
        health.gone.insert("1".into(), Instant::now() - GONE_TTL);
        assert!(!health.is_gone("1"));
        assert!(health.may_probe("1"));
        health.mark_unknown("2");
        assert!(!health.may_probe("2"));
        health.forget("2");
        assert!(health.may_probe("2"));
    }

    #[test]
    fn replacement_restores_marks_on_failure_and_bumps_the_epoch_on_success() {
        let mut health = Health::default();
        health.mark_gone("1");
        let saved = health.set_aside();
        assert!(!health.is_gone("1"));
        health.restore(saved);
        assert!(health.is_gone("1"));
        let epoch = health.epoch();
        let saved = health.set_aside();
        health.commit(saved);
        assert!(!health.is_gone("1"));
        assert_eq!(health.epoch(), epoch + 1);
    }
}
