//! The per-session turn lock.
//!
//! Mirrors Python's `session_turn_lock(user_id, session_id)`
//! (`cognee/infrastructure/locks/session_lock.py`): whole session turns of one
//! cache identity run one after another, so two quick turns cannot read the
//! same state and overwrite each other, and a session is never deleted from
//! under a turn that is still reading or writing it. Unrelated identities do
//! not wait for each other.
//!
//! One `tokio::sync::Mutex` per `(user, session)` in a process-wide registry
//! (not tied to a runtime). Scope: one process, like Python's single-worker
//! FastAPI model.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

/// Drop idle entries once the registry grows past this many identities. An
/// entry only the registry references is neither held nor waited on.
const PRUNE_THRESHOLD: usize = 1024;

type TurnKey = (String, String);
type Registry = Mutex<HashMap<TurnKey, Arc<AsyncMutex<()>>>>;

static TURN_LOCKS: OnceLock<Registry> = OnceLock::new();

/// Wait for and hold the turn lock of one `(user, session)` identity; it is
/// released when the guard drops. A search turn holds it across its whole
/// read, answer and write, and [`SessionManager::delete_session`] takes it
/// before deleting.
///
/// Not reentrant: a task that holds the guard must not delete the same session
/// through the manager.
///
/// [`SessionManager::delete_session`]: crate::SessionManager::delete_session
pub async fn lock_session_turn(user_id: impl ToString, session_id: &str) -> OwnedMutexGuard<()> {
    let key = (user_id.to_string(), session_id.to_owned());
    let mutex = {
        // lock poison is unrecoverable
        #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
        let mut locks = TURN_LOCKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap();
        if locks.len() >= PRUNE_THRESHOLD {
            locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        }
        Arc::clone(locks.entry(key).or_default())
    };
    mutex.lock_owned().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn same_identity_is_serialized_but_different_sessions_are_not() {
        let held = lock_session_turn("turn-lock-user", "s").await;
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                lock_session_turn("turn-lock-user", "s")
            )
            .await
            .is_err()
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                lock_session_turn("turn-lock-user", "other")
            )
            .await
            .is_ok()
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                lock_session_turn("someone-else", "s")
            )
            .await
            .is_ok()
        );
        drop(held);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                lock_session_turn("turn-lock-user", "s")
            )
            .await
            .is_ok()
        );
    }
}
