//! Bounded background metadata refresh. Never borrows the live Core/audio actor.
use crate::{daily::DownloadPlan, model::Item, Core, Error, GroupId, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Policy {
    pub enabled: bool,
    pub interval_hours: u32,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_hours: 6,
        }
    }
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        if ![1, 6, 12, 24].contains(&self.interval_hours) {
            return Err(Error::Input(
                "Choose a refresh interval of 1, 6, 12 or 24 hours",
            ));
        }
        Ok(())
    }
}

pub(crate) struct Job {
    pub epoch: u64,
    pub namespace: String,
    pub group: GroupId,
    pub plan: DownloadPlan,
    pub dir: PathBuf,
}
pub(crate) struct Outcome {
    pub job: Job,
    pub result: Result<(String, Vec<Item>)>,
}
#[derive(Default)]
pub(crate) struct Scheduler {
    epoch: Arc<AtomicU64>,
    sender: Option<mpsc::SyncSender<Job>>,
    results: Option<mpsc::Receiver<Outcome>>,
    worker: Option<thread::JoinHandle<()>>,
    pub pending: Option<GroupId>,
    pub error: String,
    attempts: BTreeMap<GroupId, u64>,
    seen: BTreeSet<GroupId>,
}
impl Scheduler {
    pub fn cancel(&mut self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        self.pending = None;
    }
    pub fn reconnect(&mut self) {
        self.seen.clear();
    }
    pub fn reset(&mut self) {
        self.cancel();
        self.seen.clear();
        self.attempts.clear();
        self.error.clear();
    }
    pub fn due(&self, group: &GroupId, plan: &DownloadPlan, policy: &Policy, now: u64) -> bool {
        self.pending.is_none()
            && self
                .attempts
                .get(group)
                .is_none_or(|last| now.saturating_sub(*last) >= 900)
            && (!self.seen.contains(group)
                || now.saturating_sub(plan.refreshed_at) >= u64::from(policy.interval_hours) * 3600)
    }
    fn spawn(&mut self) -> Result<()> {
        let (sender, receiver) = mpsc::sync_channel::<Job>(1);
        let (output, results) = mpsc::channel();
        let epoch = self.epoch.clone();
        self.worker = Some(
            thread::Builder::new()
                .name("plexfreq-plan-refresh".into())
                .spawn(move || {
                    while let Ok(job) = receiver.recv() {
                        if epoch.load(Ordering::SeqCst) != job.epoch {
                            continue;
                        }
                        let result = (|| {
                            let mut planner = Core::inspect(job.dir.clone())?;
                            if planner.cache_namespace() != job.namespace {
                                return Err(Error::Input("Cache request cancelled"));
                            }
                            planner.planning_guard = Some((epoch.clone(), job.epoch));
                            planner.collect_download(job.plan.kind, &job.plan.key, job.plan.minutes)
                        })();
                        let _ = output.send(Outcome { job, result });
                    }
                })?,
        );
        self.sender = Some(sender);
        self.results = Some(results);
        Ok(())
    }
    pub fn submit(
        &mut self,
        dir: PathBuf,
        namespace: String,
        group: GroupId,
        plan: DownloadPlan,
        now: u64,
    ) -> Result<()> {
        let mut job = Job {
            epoch: self.epoch.load(Ordering::SeqCst),
            namespace,
            group: group.clone(),
            plan,
            dir,
        };
        // One respawn: a worker that died (panic) disconnects its channel.
        for _ in 0..2 {
            if self.sender.is_none() {
                self.spawn()?;
            }
            let Some(sender) = self.sender.as_ref() else {
                break;
            };
            match sender.try_send(job) {
                Ok(()) => {
                    self.pending = Some(group.clone());
                    self.attempts.insert(group, now);
                    self.error.clear();
                    return Ok(());
                }
                // A cancelled job is still draining; retry at the next poll.
                Err(mpsc::TrySendError::Full(_)) => return Ok(()),
                Err(mpsc::TrySendError::Disconnected(returned)) => {
                    job = returned;
                    self.sender = None;
                    self.results = None;
                    if let Some(worker) = self.worker.take() {
                        let _ = worker.join();
                    }
                }
            }
        }
        Err(Error::Input("Cache worker unavailable"))
    }
    pub fn poll(&mut self) -> Option<Outcome> {
        while let Some(outcome) = self.results.as_ref().and_then(|rx| rx.try_recv().ok()) {
            if outcome.job.epoch != self.epoch.load(Ordering::SeqCst) {
                continue;
            }
            self.pending = None;
            self.seen.insert(outcome.job.group.clone());
            return Some(outcome);
        }
        None
    }
}
impl Drop for Scheduler {
    /// The worker checks the cancelled epoch between requests, but a single
    /// in-flight HTTP request may still run until its timeout. Wait briefly
    /// for it to notice; otherwise detach it. It owns only a read-only
    /// `Core::inspect`, so outliving the scheduler cannot corrupt state.
    fn drop(&mut self) {
        self.cancel();
        self.sender.take();
        let Some(worker) = self.worker.take() else {
            return;
        };
        let deadline = Instant::now() + Duration::from_secs(1);
        if let Some(results) = self.results.take() {
            // Outcomes are discarded; disconnection means the worker exited.
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match results.recv_timeout(left) {
                    Ok(_) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => return,
                }
            }
        }
        let _ = worker.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, time::Duration};

    #[test]
    fn dropping_scheduler_does_not_wait_for_a_stalled_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let dir = tempfile::tempdir().unwrap();
        let settings = crate::store::Settings {
            client_identifier: "fixture-client".into(),
            server_url: url.clone(),
            server_token: "fixture".into(),
            ..Default::default()
        };
        crate::store::save(dir.path(), &settings).unwrap();
        let mut scheduler = Scheduler::default();
        let plan = DownloadPlan {
            kind: crate::DownloadKind::Playlist,
            key: "5".into(),
            minutes: 0,
            refreshed_at: 0,
        };
        scheduler
            .submit(
                dir.path().into(),
                crate::cache::namespace(&url, "fixture"),
                "playlist:5".parse().unwrap(),
                plan.clone(),
                1,
            )
            .unwrap();
        // Accept the planner's request and never answer it.
        let (_stalled, _) = listener.accept().unwrap();
        // The occupied worker must not turn a second submit into a user error.
        scheduler.pending = None;
        assert!(scheduler
            .submit(
                dir.path().into(),
                String::new(),
                "playlist:6".parse().unwrap(),
                plan.clone(),
                2
            )
            .is_ok());
        assert!(scheduler
            .submit(
                dir.path().into(),
                String::new(),
                "playlist:7".parse().unwrap(),
                plan,
                3
            )
            .is_ok());
        let started = Instant::now();
        drop(scheduler);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
