//! Bounded background metadata refresh. Never borrows the live Core/audio actor.
use crate::{daily::DownloadPlan, model::Item, Core, Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
    thread,
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
    pub group: String,
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
    pub pending: Option<String>,
    pub error: String,
    attempts: BTreeMap<String, u64>,
    seen: BTreeSet<String>,
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
    pub fn due(&self, group: &str, plan: &DownloadPlan, policy: &Policy, now: u64) -> bool {
        self.pending.is_none()
            && self
                .attempts
                .get(group)
                .is_none_or(|last| now.saturating_sub(*last) >= 900)
            && (!self.seen.contains(group)
                || now.saturating_sub(plan.refreshed_at) >= u64::from(policy.interval_hours) * 3600)
    }
    pub fn submit(
        &mut self,
        dir: PathBuf,
        namespace: String,
        group: String,
        plan: DownloadPlan,
        now: u64,
    ) -> Result<()> {
        if self.sender.is_none() {
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
                                planner.collect_download(
                                    &job.plan.kind,
                                    &job.plan.key,
                                    job.plan.minutes,
                                )
                            })();
                            let _ = output.send(Outcome { job, result });
                        }
                    })?,
            );
            self.sender = Some(sender);
            self.results = Some(results);
        }
        let epoch = self.epoch.load(Ordering::SeqCst);
        self.sender
            .as_ref()
            .ok_or(Error::Input("Cache worker unavailable"))?
            .try_send(Job {
                epoch,
                namespace,
                group: group.clone(),
                plan,
                dir,
            })
            .map_err(|_| Error::Input("Cache worker unavailable"))?;
        self.pending = Some(group.clone());
        self.attempts.insert(group, now);
        self.error.clear();
        Ok(())
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
    fn drop(&mut self) {
        self.cancel();
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
