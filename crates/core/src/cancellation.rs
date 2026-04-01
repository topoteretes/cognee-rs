use std::sync::Arc;
use tokio::sync::watch;

/// Creates a linked (`CancellationHandle`, `CancellationToken`) pair.
///
/// The handle is given to the *owner* of a task; the token is passed into the
/// task itself.  Dropping the handle does **not** cancel — call
/// [`CancellationHandle::cancel`] explicitly.
pub fn cancellation_pair() -> (CancellationHandle, CancellationToken) {
    let (tx, rx) = watch::channel(false);
    (
        CancellationHandle {
            sender: Arc::new(tx),
        },
        CancellationToken { receiver: rx },
    )
}
/// Allows the owner of a task to request cancellation.
///
/// Clone-able so that multiple parties can independently cancel the same task.
#[derive(Clone)]
pub struct CancellationHandle {
    sender: Arc<watch::Sender<bool>>,
}

impl CancellationHandle {
    /// Signal cancellation to all associated [`CancellationToken`]s.
    pub fn cancel(&self) {
        // Ignore errors: all tokens have been dropped, nothing to signal.
        let _ = self.sender.send(true);
    }

    /// Returns `true` if cancellation has already been requested.
    pub fn is_cancelled(&self) -> bool {
        *self.sender.borrow()
    }
}
/// Passed into a running task so it can observe cancellation requests.
///
/// Clone-able: each clone independently tracks whether it has already seen the
/// cancellation signal (via the `watch` channel's mark-seen semantics).
#[derive(Clone)]
pub struct CancellationToken {
    receiver: watch::Receiver<bool>,
}

impl CancellationToken {
    /// Returns `true` if cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }

    /// Await until cancellation is requested.
    ///
    /// Returns immediately if already cancelled.  Also returns if the
    /// [`CancellationHandle`] is dropped without cancelling (treat as
    /// cancelled to avoid hanging forever).
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let mut rx = self.receiver.clone();
        loop {
            match rx.changed().await {
                Ok(_) => {
                    if *rx.borrow() {
                        return;
                    }
                }
                // Sender dropped — treat as cancelled so tasks don't hang.
                Err(_) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_cancellation_signal_sync() {
        let (handle, token) = cancellation_pair();

        assert!(
            !token.is_cancelled(),
            "token should not be cancelled initially"
        );

        handle.cancel();

        assert!(
            token.is_cancelled(),
            "token should be cancelled after handle.cancel()"
        );
    }

    #[tokio::test]
    async fn test_cancellation_signal_async() {
        let (handle, token) = cancellation_pair();

        assert!(!token.is_cancelled());

        handle.cancel();

        assert!(token.is_cancelled());

        // `cancelled().await` should return immediately since cancel was already called.
        let result = tokio::time::timeout(Duration::from_millis(100), token.cancelled()).await;

        assert!(
            result.is_ok(),
            "token.cancelled().await should complete immediately after cancel, not time out"
        );
    }
}
