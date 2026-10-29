//! Process-wide registry of fire-and-forget background tasks — Python's
//! `cognee.infrastructure.background_tasks`.
//!
//! A session `remember()` launches its improve as a detached tokio task. A
//! short-lived host (the CLI, a script around a binding) would drop its
//! runtime, and the task with it, as soon as the call returned. This registry
//! is the one place to wait for that work: [`spawn_background_task`] counts a
//! task in, [`wait_for_background_tasks`] waits until every counted task is
//! done. The CLI drains it before exiting and the bindings before closing a
//! handle.
//!
//! Nothing here cancels a task: a timeout reports `false` and leaves the work
//! running.

use std::future::Future;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::Notify;

/// A set of detached tasks that can be waited for. [`BackgroundTasks::global`]
/// is the process-wide one; a separate instance keeps tests independent.
#[derive(Debug, Default)]
pub struct BackgroundTasks {
    running: AtomicUsize,
    finished: Notify,
}

/// Decrements the count when the task ends, however it ends (completion,
/// panic, or the runtime dropping it).
struct Running<'a>(&'a BackgroundTasks);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.running.fetch_sub(1, Ordering::AcqRel);
        self.0.finished.notify_waiters();
    }
}

static GLOBAL: LazyLock<BackgroundTasks> = LazyLock::new(BackgroundTasks::default);

impl BackgroundTasks {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// The process-wide registry.
    pub fn global() -> &'static Self {
        &GLOBAL
    }

    /// Spawn `future` on the current tokio runtime and count it until it
    /// ends. Must be called from inside a runtime.
    ///
    /// A task that panics is logged at `error` (Python's done-callback logs
    /// the exception of a failed background task); nobody joins a detached
    /// task, so the panic would otherwise vanish.
    pub fn spawn<F>(&'static self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.running.fetch_add(1, Ordering::AcqRel);
        let guard = Running(self);
        tokio::spawn(async move {
            let _guard = guard;
            if let Err(error) = tokio::spawn(future).await
                && error.is_panic()
            {
                tracing::error!("background task panicked: {}", panic_message(error));
            }
        });
    }

    /// Number of counted tasks that have not finished yet.
    pub fn pending(&self) -> usize {
        self.running.load(Ordering::Acquire)
    }

    /// Wait until every counted task has finished, including tasks spawned
    /// while waiting. `true` when everything finished, `false` when
    /// `timeout` elapsed with work still running; nothing is cancelled.
    pub async fn wait(&self, timeout: Option<Duration>) -> bool {
        let deadline = timeout.map(|t| tokio::time::Instant::now() + t);
        loop {
            // Registered before the count is read, so a task ending in
            // between still wakes this wait.
            let finished = self.finished.notified();
            let pending = self.pending();
            if pending == 0 {
                return true;
            }
            match deadline {
                None => finished.await,
                Some(deadline) => {
                    if tokio::time::timeout_at(deadline, finished).await.is_err() {
                        tracing::warn!(
                            pending,
                            timeout = ?timeout,
                            "wait_for_background_tasks: task(s) still running after the timeout"
                        );
                        return false;
                    }
                }
            }
        }
    }
}

/// The message of a panicked task's payload, when it is a string.
fn panic_message(error: tokio::task::JoinError) -> String {
    let payload = error.into_panic();
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string())
}

/// Spawn `future` as a detached task in the process-wide registry
/// (Python's `register_background_task`).
pub fn spawn_background_task<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    BackgroundTasks::global().spawn(future);
}

/// Number of process-wide background tasks still running.
pub fn pending_background_tasks() -> usize {
    BackgroundTasks::global().pending()
}

/// Wait for every process-wide background task (Python's
/// `cognee.wait_for_background_tasks()`); see [`BackgroundTasks::wait`].
pub async fn wait_for_background_tasks(timeout: Option<Duration>) -> bool {
    BackgroundTasks::global().wait(timeout).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
mod tests {
    //! Ported from Python's `cognee/tests/unit/infrastructure/test_background_tasks.py`.

    use std::sync::{Arc, Mutex};

    use super::*;

    fn registry() -> &'static BackgroundTasks {
        Box::leak(Box::new(BackgroundTasks::new()))
    }

    #[tokio::test]
    async fn drain_awaits_registered_tasks() {
        let tasks = registry();
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let finished = Arc::new(Mutex::new(Vec::new()));
        for name in ["a", "b"] {
            let (release, finished) = (Arc::clone(&release), Arc::clone(&finished));
            tasks.spawn(async move {
                let _permit = release.acquire().await;
                finished.lock().unwrap().push(name);
            });
        }
        assert_eq!(tasks.pending(), 2);
        let releaser = Arc::clone(&release);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            releaser.add_permits(2);
        });
        assert!(tasks.wait(Some(Duration::from_secs(5))).await);
        let mut done = finished.lock().unwrap().clone();
        done.sort_unstable();
        assert_eq!(done, ["a", "b"]);
        assert_eq!(tasks.pending(), 0);
    }

    #[tokio::test]
    async fn drain_waits_for_tasks_spawned_while_draining() {
        let tasks = registry();
        let order = Arc::new(Mutex::new(Vec::new()));
        let parent_order = Arc::clone(&order);
        tasks.spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            parent_order.lock().unwrap().push("parent");
            let child_order = Arc::clone(&parent_order);
            tasks.spawn(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                child_order.lock().unwrap().push("child");
            });
        });
        assert!(tasks.wait(Some(Duration::from_secs(5))).await);
        assert_eq!(*order.lock().unwrap(), ["parent", "child"]);
    }

    #[tokio::test]
    async fn drain_timeout_reports_false_and_cancels_nothing() {
        let tasks = registry();
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let held = Arc::clone(&release);
        tasks.spawn(async move {
            let _permit = held.acquire().await;
        });
        assert!(!tasks.wait(Some(Duration::from_millis(50))).await);
        assert_eq!(tasks.pending(), 1, "the slow task keeps running");
        release.add_permits(1);
        assert!(tasks.wait(Some(Duration::from_secs(5))).await);
        assert_eq!(tasks.pending(), 0);
    }

    #[tokio::test]
    async fn failed_task_does_not_fail_the_drain() {
        let tasks = registry();
        tasks.spawn(async { panic!("background failure") });
        assert!(tasks.wait(Some(Duration::from_secs(5))).await);
        assert_eq!(tasks.pending(), 0);
    }

    #[tokio::test]
    async fn drain_with_nothing_registered_returns_immediately() {
        assert!(registry().wait(Some(Duration::ZERO)).await);
    }

    // A panicking task is counted out like any other (and logged), so a drain
    // does not hang on it.
    #[tokio::test]
    async fn a_panicking_task_is_counted_out() {
        let tasks = registry();
        tasks.spawn(async { panic!("boom") });
        assert!(tasks.wait(Some(Duration::from_secs(5))).await);
        assert_eq!(tasks.pending(), 0);
    }
}
