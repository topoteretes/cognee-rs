use std::future::Future;
use std::pin::Pin;

use crate::error::CoreError;
/// Dyn-compatible interface for a CPU-bound thread pool.
///
/// Use the blanket [`CpuPoolExt::spawn`] method for ergonomic, generic usage.
/// Implement only [`CpuPool::spawn_raw`] in concrete types.
pub trait CpuPool: Send + Sync {
    /// Spawn a task on the pool and return a future that resolves once the task
    /// finishes.  The future does **not** borrow `self`; the task is enqueued
    /// immediately when `spawn_raw` is called.
    fn spawn_raw(
        &self,
        task: Box<dyn FnOnce() + Send + 'static>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CoreError>> + Send + 'static>>;
}
/// Ergonomic extension for [`CpuPool`] that adds a generic `spawn` with a
/// return value.  Auto-implemented for every `T: CpuPool`.
pub trait CpuPoolExt: CpuPool {
    /// Spawn a CPU-intensive closure on the thread pool and await its result
    /// asynchronously.
    ///
    /// The closure is executed on a rayon (or other CPU) worker thread while
    /// the caller's async task yields. Useful for blocking work that would
    /// otherwise stall the Tokio executor.
    ///
    /// Returns `Err(CoreError::TaskAborted)` if the worker panicked or the pool
    /// was shut down before the result could be delivered.
    fn spawn<F, R>(
        &self,
        f: F,
    ) -> Pin<Box<dyn Future<Output = Result<R, CoreError>> + Send + 'static>>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel::<R>();

        let task: Box<dyn FnOnce() + Send + 'static> = Box::new(move || {
            let result = f();
            // If the receiver was dropped the caller gave up; that's fine.
            let _ = tx.send(result);
        });

        let raw_fut = self.spawn_raw(task);

        Box::pin(async move {
            // Wait for the task to complete (raw_fut resolves after the closure
            // returns), then retrieve the value from the oneshot channel.
            raw_fut.await?;
            rx.await.map_err(|_| CoreError::TaskAborted {
                reason: "task result channel dropped (task panicked or pool shut down)".into(),
            })
        })
    }
}

impl<T: CpuPool + ?Sized> CpuPoolExt for T {}
/// A [`CpuPool`] backed by a dedicated [`rayon::ThreadPool`].
///
/// Provides direct access to the underlying pool via [`RayonThreadPool::rayon_pool`]
/// for callers that want to use rayon's parallel iterators directly.
pub struct RayonThreadPool {
    pool: rayon::ThreadPool,
}

impl RayonThreadPool {
    /// Create a pool with a specific number of threads.
    pub fn new(num_threads: usize) -> Result<Self, CoreError> {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .build()
            .map_err(|e| CoreError::ThreadPoolBuild(e.to_string()))?;
        Ok(Self { pool })
    }

    /// Create a pool with rayon's default thread count (one per logical CPU).
    pub fn with_default_threads() -> Result<Self, CoreError> {
        let pool = rayon::ThreadPoolBuilder::new()
            .build()
            .map_err(|e| CoreError::ThreadPoolBuild(e.to_string()))?;
        Ok(Self { pool })
    }

    /// Direct access to the underlying [`rayon::ThreadPool`], e.g. for
    /// `pool.install(|| { ... })` or parallel iterators scoped to this pool.
    pub fn rayon_pool(&self) -> &rayon::ThreadPool {
        &self.pool
    }
}

impl CpuPool for RayonThreadPool {
    fn spawn_raw(
        &self,
        task: Box<dyn FnOnce() + Send + 'static>,
    ) -> Pin<Box<dyn Future<Output = Result<(), CoreError>> + Send + 'static>> {
        let (tx, rx) = tokio::sync::oneshot::channel::<Result<(), String>>();

        self.pool.spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task));
            match result {
                Ok(()) => {
                    let _ = tx.send(Ok(()));
                }
                Err(panic_payload) => {
                    let msg = panic_payload
                        .downcast_ref::<String>()
                        .map(|s| s.as_str())
                        .or_else(|| panic_payload.downcast_ref::<&str>().copied())
                        .unwrap_or("unknown panic")
                        .to_string();
                    let _ = tx.send(Err(msg));
                }
            }
        });

        Box::pin(async move {
            match rx.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(panic_msg)) => Err(CoreError::TaskAborted {
                    reason: format!("task panicked: {panic_msg}"),
                }),
                Err(_) => Err(CoreError::TaskAborted {
                    reason: "pool shut down before task completed".into(),
                }),
            }
        })
    }
}
