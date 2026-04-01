use tokio::runtime::{Builder, EnterGuard, Handle, Runtime};

use crate::error::CoreError;

/// A wrapper around a Tokio [`Runtime`].
///
/// Provides convenient constructors for common configurations and exposes the
/// most-used entry points (`block_on`, `spawn`, `handle`, `enter`).
pub struct AsyncRuntime {
    inner: Runtime,
}

impl AsyncRuntime {
    /// Create a multi-threaded runtime with Tokio's defaults (one worker thread
    /// per logical CPU, all feature flags enabled).
    pub fn new() -> Result<Self, CoreError> {
        let rt = Runtime::new().map_err(|e| CoreError::Runtime(e.to_string()))?;
        Ok(Self { inner: rt })
    }

    /// Create a multi-threaded runtime with an explicit worker-thread count.
    pub fn multi_thread(num_workers: usize) -> Result<Self, CoreError> {
        let rt = Builder::new_multi_thread()
            .worker_threads(num_workers)
            .enable_all()
            .build()
            .map_err(|e| CoreError::Runtime(e.to_string()))?;
        Ok(Self { inner: rt })
    }

    /// Create a single-threaded (current-thread) runtime.
    pub fn current_thread() -> Result<Self, CoreError> {
        let rt = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| CoreError::Runtime(e.to_string()))?;
        Ok(Self { inner: rt })
    }

    /// Return a cloneable [`Handle`] to this runtime.
    pub fn handle(&self) -> Handle {
        self.inner.handle().clone()
    }

    /// Block the current thread until `future` completes and return its output.
    pub fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.inner.block_on(future)
    }

    /// Spawn a future onto the runtime without waiting for its result.
    pub fn spawn<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.inner.spawn(future)
    }

    /// Enter the runtime context so that async code can call `Handle::current()`.
    pub fn enter(&self) -> EnterGuard<'_> {
        self.inner.enter()
    }
}
