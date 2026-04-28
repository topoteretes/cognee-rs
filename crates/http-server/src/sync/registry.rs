//! In-memory `SyncRegistry` keyed by `user_id`.
//!
//! The registry is the *optimistic* layer of the "one running sync per user"
//! rule (the *authoritative* layer is the DB query). Insertion is atomic —
//! two threads racing through `try_register` will see exactly one win.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use tokio::task::AbortHandle;
use uuid::Uuid;

/// One in-flight sync. Stored behind an `Arc` so `snapshot_for` does not need
/// the entire entry to live across the call.
pub struct RunningSync {
    pub run_id: String,
    pub user_id: Uuid,
    pub dataset_ids: Vec<Uuid>,
    pub dataset_names: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub progress_percentage: AtomicU32,
    pub abort: Option<AbortHandle>,
}

/// Read-only snapshot of a running sync, used by handlers and conflict
/// responses.
#[derive(Debug, Clone)]
pub struct RunningSyncSnapshot {
    pub run_id: String,
    pub user_id: Uuid,
    pub dataset_ids: Vec<Uuid>,
    pub dataset_names: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub progress_percentage: u32,
}

/// Returned by [`SyncRegistry::try_register`] when the user already has a
/// running sync. Carries a snapshot of the existing run so the caller can
/// build the 409 conflict response.
#[derive(Debug, Clone)]
pub struct AlreadyRunning(pub RunningSyncSnapshot);

/// In-memory registry: `user_id → RunningSync`.
#[derive(Clone)]
pub struct SyncRegistry {
    inner: Arc<DashMap<Uuid, Arc<RunningSync>>>,
}

impl Default for SyncRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncRegistry {
    /// Build an empty registry.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
        }
    }

    /// Atomically insert a running sync for `user_id` if no sync is currently
    /// in flight for that user. On collision, returns a snapshot of the
    /// existing run.
    pub fn try_register(&self, user_id: Uuid, run: RunningSync) -> Result<(), AlreadyRunning> {
        match self.inner.entry(user_id) {
            Entry::Occupied(occ) => Err(AlreadyRunning(snapshot(occ.get()))),
            Entry::Vacant(vac) => {
                vac.insert(Arc::new(run));
                Ok(())
            }
        }
    }

    /// Return a snapshot of the running sync for `user_id`, if any.
    pub fn snapshot_for(&self, user_id: Uuid) -> Option<RunningSyncSnapshot> {
        self.inner
            .get(&user_id)
            .map(|entry| snapshot(entry.value()))
    }

    /// Drop the entry for `user_id`. Idempotent.
    pub fn complete(&self, user_id: Uuid) {
        self.inner.remove(&user_id);
    }

    /// Update the progress percentage on the slot for `user_id` (no-op when
    /// the slot is gone).
    pub fn update_progress(&self, user_id: Uuid, pct: u32) {
        if let Some(entry) = self.inner.get(&user_id) {
            entry.progress_percentage.store(pct, Ordering::Relaxed);
        }
    }

    /// Iterate every running sync (used by graceful shutdown).
    pub fn snapshot_all(&self) -> Vec<RunningSyncSnapshot> {
        self.inner
            .iter()
            .map(|entry| snapshot(entry.value()))
            .collect()
    }

    /// Abort every in-flight task and clear the registry. Returns the run
    /// ids that were aborted so callers can mark them `failed` in the DB.
    pub fn abort_all(&self) -> Vec<String> {
        let mut aborted = Vec::new();
        let keys: Vec<Uuid> = self.inner.iter().map(|e| *e.key()).collect();
        for key in keys {
            if let Some((_uid, entry)) = self.inner.remove(&key) {
                if let Some(handle) = entry.abort.as_ref() {
                    handle.abort();
                }
                aborted.push(entry.run_id.clone());
            }
        }
        aborted
    }
}

fn snapshot(run: &RunningSync) -> RunningSyncSnapshot {
    RunningSyncSnapshot {
        run_id: run.run_id.clone(),
        user_id: run.user_id,
        dataset_ids: run.dataset_ids.clone(),
        dataset_names: run.dataset_names.clone(),
        created_at: run.created_at,
        progress_percentage: run.progress_percentage.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn build_run(user: Uuid) -> RunningSync {
        RunningSync {
            run_id: format!("run-{user}"),
            user_id: user,
            dataset_ids: vec![],
            dataset_names: vec![],
            created_at: Utc::now(),
            progress_percentage: AtomicU32::new(0),
            abort: None,
        }
    }

    #[test]
    fn try_register_atomic_on_collision() {
        let reg = SyncRegistry::new();
        let user = Uuid::new_v4();
        assert!(reg.try_register(user, build_run(user)).is_ok());
        let conflict = reg
            .try_register(user, build_run(user))
            .expect_err("second insert must fail");
        assert_eq!(conflict.0.user_id, user);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn try_register_only_one_winner_under_concurrency() {
        let reg = Arc::new(SyncRegistry::new());
        let user = Uuid::new_v4();

        let mut handles = Vec::new();
        for _ in 0..32 {
            let reg2 = Arc::clone(&reg);
            handles.push(tokio::spawn(async move {
                reg2.try_register(user, build_run(user)).is_ok()
            }));
        }
        let mut wins = 0_u32;
        for h in handles {
            if h.await.expect("join task") {
                wins += 1;
            }
        }
        assert_eq!(wins, 1, "exactly one concurrent inserter wins");
    }

    #[test]
    fn snapshot_for_returns_none_after_complete() {
        let reg = SyncRegistry::new();
        let user = Uuid::new_v4();
        reg.try_register(user, build_run(user)).expect("insert");
        assert!(reg.snapshot_for(user).is_some());
        reg.complete(user);
        assert!(reg.snapshot_for(user).is_none());
    }

    #[test]
    fn update_progress_persists_until_complete() {
        let reg = SyncRegistry::new();
        let user = Uuid::new_v4();
        reg.try_register(user, build_run(user)).expect("insert");
        reg.update_progress(user, 42);
        let snap = reg.snapshot_for(user).expect("snap");
        assert_eq!(snap.progress_percentage, 42);
    }
}
