//! One process-wide async runtime for network, D-Bus and HTTP I/O.
//!
//! A lazily created static (rather than a runtime owned by `runtime::Runtime`)
//! because `Core` is constructed standalone by tests, `pf_new`/`pf_new_inspect`
//! and the plan-refresh worker's `Core::inspect`, and `Cache`/audio readers are
//! used without a `Runtime` too. The runtime lives for the process; lifetime of
//! the work running on it is owned explicitly by `Task`/`Halter` holders, whose
//! `Drop` stops (and where required waits for) their tasks.
//!
//! It is a current-thread scheduler driven by one dedicated `plexfreq-rt`
//! thread: the crate enables tokio's `rt` but not `rt-multi-thread`. Its tasks
//! are I/O bound; filesystem work and libpulse samples go to the blocking pool
//! (`unblock`), and futures driven by [`block_on`] are polled on the calling
//! (Core/refresh) thread, so JSON decoding never runs on the I/O thread.
//!
//! Never call [`block_on`] or [`Task::join`] on the runtime thread itself:
//! nothing else could drive the I/O they wait for. Both detect it.
use std::{
    cell::Cell,
    future::Future,
    sync::{mpsc, OnceLock},
    time::Duration,
};
use tokio::{runtime::Handle, sync::watch};

/// Blocking pool bound: DNS resolution (getaddrinfo), cache/artwork
/// filesystem writes and libpulse diagnostic samples.
const BLOCKING: usize = 8;

static RUNTIME: OnceLock<Handle> = OnceLock::new();
thread_local! {
    static RUNTIME_THREAD: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn handle() -> &'static Handle {
    RUNTIME.get_or_init(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(BLOCKING)
            .thread_name("plexfreq-rt-blk")
            .enable_all()
            .build()
            .expect("PlexFreq async runtime cannot start");
        let handle = runtime.handle().clone();
        std::thread::Builder::new()
            .name("plexfreq-rt".into())
            .spawn(move || {
                RUNTIME_THREAD.with(|flag| flag.set(true));
                runtime.block_on(std::future::pending::<()>());
            })
            .expect("PlexFreq async runtime thread cannot start");
        handle
    })
}

/// Whether the caller is the runtime thread, where nothing may block.
pub(crate) fn on_runtime_thread() -> bool {
    RUNTIME_THREAD.with(Cell::get)
}

/// Drive `future` to completion from synchronous code. The future is polled
/// on the calling thread; its sockets and timers are driven by the runtime
/// thread. Called from some other async context (e.g. a test's own tokio
/// runtime) it waits on a scoped helper thread instead of panicking.
///
/// # Panics
/// On the shared runtime thread itself, where waiting would deadlock.
pub(crate) fn block_on<F>(future: F) -> F::Output
where
    F: Future + Send,
    F::Output: Send,
{
    assert!(
        !on_runtime_thread(),
        "rt::block_on called on the async runtime thread"
    );
    if Handle::try_current().is_err() {
        return handle().block_on(future);
    }
    std::thread::scope(|scope| {
        scope
            .spawn(|| handle().block_on(future))
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

/// Run blocking work (filesystem, libpulse) off the runtime thread.
pub(crate) async fn unblock<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    match handle().spawn_blocking(f).await {
        Ok(result) => result,
        Err(error) => std::panic::resume_unwind(error.into_panic()),
    }
}

/// An owned task on the shared runtime. Dropping it aborts the task;
/// [`Task::join`] waits for its future to finish or to be dropped.
pub(crate) struct Task {
    handle: tokio::task::JoinHandle<()>,
    done: mpsc::Receiver<()>,
}
pub(crate) fn spawn(future: impl Future<Output = ()> + Send + 'static) -> Task {
    let (finished, done) = mpsc::channel::<()>();
    let handle = handle().spawn(async move {
        // Declared first, so dropped last: the signal follows the future's
        // own completion or (on abort) its destructors.
        let _finished = finished;
        future.await;
    });
    Task { handle, done }
}
impl Task {
    pub(crate) fn abort(&self) {
        self.handle.abort();
    }
    /// Wait for completion; `None` waits indefinitely. Returns whether the
    /// task has finished. On the runtime thread it cannot wait: it aborts the
    /// task instead (its destructors then run on the next scheduler turn).
    pub(crate) fn join(&self, timeout: Option<Duration>) -> bool {
        if on_runtime_thread() {
            self.abort();
            return self.handle.is_finished();
        }
        match timeout {
            None => {
                let _ = self.done.recv();
                true
            }
            Some(timeout) => !matches!(
                self.done.recv_timeout(timeout),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
        }
    }
    /// After the owner signalled a cooperative stop: wait up to `grace`,
    /// then abort and wait for the destructors.
    pub(crate) fn finish(&self, grace: Duration) {
        if !self.join(Some(grace)) {
            self.abort();
            self.join(Some(Duration::from_secs(1)));
        }
    }
}
impl Drop for Task {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Cooperative stop signal for tasks that must run async cleanup (D-Bus match
/// deregistration) instead of being dropped mid-await. Dropping the `Halter`
/// halts too, so a task never outlives its owner waiting on a lost signal.
pub(crate) struct Halter(watch::Sender<bool>);
#[derive(Clone)]
pub(crate) struct Halt(watch::Receiver<bool>);
pub(crate) fn halter() -> Halter {
    Halter(watch::channel(false).0)
}
impl Halter {
    pub(crate) fn halt(&self) {
        self.0.send_replace(true);
    }
    pub(crate) fn subscribe(&self) -> Halt {
        Halt(self.0.subscribe())
    }
}
impl Halt {
    pub(crate) fn halted(&self) -> bool {
        *self.0.borrow()
    }
    pub(crate) async fn wait(&self) {
        let mut receiver = self.0.clone();
        let _ = receiver.wait_for(|halted| *halted).await;
    }
}

/// Await the next change of an optional watch; a missing or closed channel
/// never completes (callers always combine it with a fallback timer).
pub(crate) async fn changed<T>(receiver: &mut Option<watch::Receiver<T>>) {
    let open = match receiver.as_mut() {
        Some(receiver) => receiver.changed().await.is_ok(),
        None => false,
    };
    if !open {
        *receiver = None;
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn tasks_join_abort_and_block_on_from_other_contexts() {
        let halter = halter();
        let halt = halter.subscribe();
        let task = spawn(async move { halt.wait().await });
        assert!(!task.join(Some(Duration::from_millis(50))));
        halter.halt();
        assert!(task.join(Some(Duration::from_secs(2))));
        let stuck = spawn(std::future::pending());
        let started = Instant::now();
        stuck.finish(Duration::from_millis(20));
        assert!(stuck.join(Some(Duration::ZERO)));
        assert!(started.elapsed() < Duration::from_secs(1));
        // Timers are driven by the runtime thread, not by the blocked caller.
        assert_eq!(
            block_on(async {
                tokio::time::sleep(Duration::from_millis(5)).await;
                3
            }),
            3
        );
        // A foreign async context gets a helper thread instead of a panic.
        let foreign = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert_eq!(foreign.block_on(async { block_on(async { 7 }) }), 7);
        assert_eq!(block_on(unblock(|| 9)), 9);
    }

    /// Scheduling latency of the I/O thread (a proxy for the audio reader's
    /// wakeups) while cache-style filesystem work runs through `unblock`.
    /// Run with `--nocapture` for the measured numbers.
    #[test]
    fn io_thread_latency_under_filesystem_load() {
        let dir = tempfile::tempdir().unwrap();
        let probe = |label: &str| {
            let (tx, rx) = mpsc::channel();
            let task = spawn(async move {
                let mut worst = Duration::ZERO;
                let mut total = Duration::ZERO;
                for _ in 0..200 {
                    let before = Instant::now();
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    let late = before.elapsed().saturating_sub(Duration::from_millis(5));
                    worst = worst.max(late);
                    total += late;
                }
                let _ = tx.send((worst, total / 200));
            });
            let (worst, mean) = rx.recv_timeout(Duration::from_secs(30)).unwrap();
            task.join(Some(Duration::from_secs(1)));
            println!("{label}: 5 ms timer lateness worst={worst:?} mean={mean:?}");
            worst
        };
        probe("idle");
        let writers: Vec<_> = (0..3)
            .map(|n| {
                let path = dir.path().join(format!("load-{n}"));
                spawn(async move {
                    for _ in 0..4 {
                        let path = path.clone();
                        unblock(move || {
                            use std::io::Write;
                            let mut file = std::fs::File::create(&path).unwrap();
                            for _ in 0..64 {
                                file.write_all(&[7u8; 64 * 1024]).unwrap();
                            }
                            file.sync_all().unwrap();
                        })
                        .await;
                    }
                })
            })
            .collect();
        let worst = probe("4 MiB write+fsync x12 on the blocking pool");
        for writer in &writers {
            writer.join(Some(Duration::from_secs(30)));
        }
        assert!(
            worst < Duration::from_millis(250),
            "I/O thread stalled {worst:?}"
        );
    }
}
