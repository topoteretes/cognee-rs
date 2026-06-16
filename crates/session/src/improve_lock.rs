//! Per-session improve-lock registry.
//!
//! Mirrors Python's `cognee.infrastructure.locks.session_lock` module:
//! a non-blocking claim for long-running `improve()` calls. The registry
//! is a process-global `HashSet` guarded by a sync `Mutex` so the
//! check-and-add happens atomically.
//!
//! Scope: single-process (matches Python's default single-worker FastAPI
//! model). For multi-process deployments, layer a distributed lock on top —
//! the call sites are factored so that is a local change.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

static IMPROVING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashSet<String>> {
    IMPROVING.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Atomically claim the improve-lock for `session_id`.
///
/// Returns `true` iff we got it (i.e. no other caller is currently
/// improving this session). Empty `session_id` is a no-op — always
/// returns `true` (matches Python's `if not session_id: return True`).
/// Caller MUST release the lock when done; prefer [`ImproveLockGuard`].
pub fn try_acquire_improve_lock(session_id: &str) -> bool {
    if session_id.is_empty() {
        return true;
    }
    // lock poison is unrecoverable
    let mut set = registry().lock().unwrap();
    set.insert(session_id.to_string())
}

/// Release the improve-lock for `session_id`. Idempotent.
///
/// Empty `session_id` is a no-op (mirrors Python's early-return).
pub fn release_improve_lock(session_id: &str) {
    if session_id.is_empty() {
        return;
    }
    // lock poison is unrecoverable
    registry().lock().unwrap().remove(session_id);
}

/// RAII guard that releases the improve-lock on drop.
///
/// Use [`ImproveLockGuard::acquire`] to claim and wrap the lock so that
/// any early return or panic automatically releases it (matches Python's
/// `try/finally`).
///
/// The guard stores a `String`, not a `MutexGuard`, so it is `Send` and
/// can safely be held across `.await` points.
pub struct ImproveLockGuard(Option<String>);

impl ImproveLockGuard {
    /// Attempt to acquire the lock for `session_id`.
    ///
    /// Returns `Some(guard)` if the lock was acquired, `None` if another
    /// task already holds it.
    pub fn acquire(session_id: &str) -> Option<Self> {
        if try_acquire_improve_lock(session_id) {
            Some(Self(Some(session_id.to_string())))
        } else {
            None
        }
    }
}

impl Drop for ImproveLockGuard {
    fn drop(&mut self) {
        if let Some(ref s) = self.0.take() {
            release_improve_lock(s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_session_id_always_acquires() {
        assert!(try_acquire_improve_lock(""));
        assert!(try_acquire_improve_lock(""));
        // releasing an empty id is a no-op and should not panic
        release_improve_lock("");
    }

    #[test]
    fn second_acquire_returns_false() {
        let sid = format!("test-lock-second-{}", uuid::Uuid::new_v4());
        assert!(try_acquire_improve_lock(&sid), "first acquire must succeed");
        assert!(
            !try_acquire_improve_lock(&sid),
            "second acquire must fail while first is held"
        );
        release_improve_lock(&sid);
    }

    #[test]
    fn improve_lock_excludes_concurrent() {
        let sid = format!("test-lock-excl-{}", uuid::Uuid::new_v4());

        // First acquire succeeds.
        assert!(try_acquire_improve_lock(&sid));
        // Second acquire fails.
        assert!(!try_acquire_improve_lock(&sid));
        // After release, acquire succeeds again.
        release_improve_lock(&sid);
        assert!(try_acquire_improve_lock(&sid));
        // Cleanup.
        release_improve_lock(&sid);
    }

    #[test]
    fn guard_releases_on_drop() {
        let sid = format!("test-lock-guard-{}", uuid::Uuid::new_v4());
        {
            let guard = ImproveLockGuard::acquire(&sid);
            assert!(guard.is_some(), "guard must be acquired");
            // While guard is held, a second acquire must fail.
            assert!(!try_acquire_improve_lock(&sid));
        } // guard drops here
        // After drop, acquire must succeed again.
        assert!(try_acquire_improve_lock(&sid));
        release_improve_lock(&sid);
    }

    #[test]
    fn guard_acquire_fails_when_held() {
        let sid = format!("test-lock-guard-fail-{}", uuid::Uuid::new_v4());
        let _g1 = ImproveLockGuard::acquire(&sid).expect("first guard");
        let g2 = ImproveLockGuard::acquire(&sid);
        assert!(g2.is_none(), "second guard must not be acquired");
    }

    #[test]
    fn empty_session_id_guard_always_acquires() {
        let g1 = ImproveLockGuard::acquire("");
        assert!(g1.is_some());
        // A second guard on "" must also return Some (no-op semantics)
        let g2 = ImproveLockGuard::acquire("");
        assert!(g2.is_some());
    }
}
