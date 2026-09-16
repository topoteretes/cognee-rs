//! Per-dataset-identity locks that serialize "look the dataset up, create it if
//! missing, grant the owner's ACL rows" against itself.
//!
//! # Why this exists
//!
//! A dataset row and its ACL rows cannot be written in one transaction:
//! [`AclDb`](cognee_database::AclDb) is a separate trait over a possibly
//! separate store, and the production implementation is a closed newtype that
//! may not share a database with the metadata. Every create path therefore
//! writes the row first and grants second, and compensates a failed grant by
//! revoking what landed and dropping the row again.
//!
//! That compensation is correct in isolation. What it is not is *isolated*:
//! between the insert and the grant there is a window in which another caller
//! can observe a row that is about to be rolled back. Two known ways to lose
//! by it, both requiring a failing (or merely slow) ACL write plus a
//! simultaneous second request:
//!
//! 1. A second `POST /v1/datasets` for the same name short-circuits on the
//!    existing row and answers `200` for a dataset the first request then
//!    deletes.
//! 2. Worse: a concurrent `POST /v1/add` for the same `datasetName` resolves
//!    that row and ingests into it. The rollback then deletes the dataset out
//!    from under data that was just written to it.
//!
//! Holding one of these locks across the whole lookup-insert-grant-compensate
//! sequence closes both, because every writer of a given dataset identity is
//! then serialized against every other: a caller either sees a row whose grants
//! are committed, or sees no row at all and creates its own.
//!
//! # The key is the identity, not the row
//!
//! Locks are keyed on the *deterministic* dataset id
//! ([`generate_dataset_id`](crate::generate_dataset_id), `uuid5(name, owner,
//! tenant)`) rather than on a row that may not exist yet. That is the whole
//! point: the contended moment is the one where two callers have both decided
//! the dataset is missing, and there is no row to key on.
//!
//! A keyed map, not one global mutex — otherwise every concurrent create in the
//! process serializes behind one lock regardless of which dataset it names.
//!
//! # Scope: one process
//!
//! This is an in-process lock. It does nothing about two server replicas racing
//! on a shared database, which needs either an advisory lock in the store or a
//! store that can hold the row and its ACL rows in one transaction. What it
//! does cover is the case the HTTP server actually has: concurrent requests on
//! one instance.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use uuid::Uuid;

use crate::generate_dataset_id;

/// Prune dead entries once the map grows past this many keys.
///
/// Entries are `Weak`, so a key whose last guard dropped costs one empty map
/// slot until something prunes it. Pruning on every acquire would make the
/// common case O(keys); pruning only when the map has grown keeps it amortized
/// while still bounding the map by the number of *live* locks.
const PRUNE_THRESHOLD: usize = 64;

/// A registry of per-dataset-identity locks. Cheap to clone via `Arc`; see the
/// module docs for what it protects and why it is keyed the way it is.
#[derive(Debug, Default)]
pub struct DatasetLocks {
    /// Live locks by dataset id. `Weak` so an entry disappears once its last
    /// guard drops — a long-lived server must not accumulate one mutex per
    /// dataset name it has ever been asked about.
    locks: Mutex<HashMap<Uuid, Weak<AsyncMutex<()>>>>,
}

impl DatasetLocks {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquire the lock for `dataset_id`, waiting if another task holds it.
    ///
    /// Hold the returned guard across the entire lookup-insert-grant sequence,
    /// including the compensating rollback — releasing it between the insert
    /// and the grant reopens exactly the window the lock exists to close.
    pub async fn lock(&self, dataset_id: Uuid) -> DatasetLockGuard {
        let lock = self.lock_arc(dataset_id);
        DatasetLockGuard {
            _guard: lock.lock_owned().await,
        }
    }

    /// Acquire the lock for the dataset `name` identifies under this owner and
    /// tenant — the same `uuid5` identity a create would write.
    ///
    /// Use this on the by-name paths, which are the ones that can decide a
    /// dataset is missing and race to create it.
    pub async fn lock_for_name(
        &self,
        name: &str,
        owner_id: Uuid,
        tenant_id: Option<Uuid>,
    ) -> DatasetLockGuard {
        self.lock(generate_dataset_id(name, owner_id, tenant_id))
            .await
    }

    /// Resolve (or install) the mutex for one key, without awaiting on it.
    ///
    /// Split out so the `std` map lock is provably not held across the `await`
    /// on the async mutex — holding it there would deadlock every other key.
    #[allow(
        clippy::unwrap_used,
        reason = "lock poison is unrecoverable — the holder already panicked"
    )]
    fn lock_arc(&self, dataset_id: Uuid) -> Arc<AsyncMutex<()>> {
        let mut locks = self.locks.lock().unwrap();

        if let Some(existing) = locks.get(&dataset_id).and_then(Weak::upgrade) {
            return existing;
        }

        if locks.len() >= PRUNE_THRESHOLD {
            locks.retain(|_, weak| weak.strong_count() > 0);
        }

        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(dataset_id, Arc::downgrade(&lock));
        lock
    }
}

/// Exclusive hold on one dataset identity, released on drop.
///
/// Owns the mutex `Arc` as well as the guard, which is what keeps the
/// registry's `Weak` entry alive for as long as anyone holds the lock.
#[derive(Debug)]
pub struct DatasetLockGuard {
    _guard: OwnedMutexGuard<()>,
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "test code — panics are acceptable failures"
    )]

    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Scenario: two tasks take the lock for the same dataset id and each
    /// increments a counter in a read-modify-write straddling an `await`.
    /// Expected: no interleaving — the counter lands on 2, and neither task
    /// ever observes the other's half-finished write.
    /// Verification: record the value each task read; with real exclusion the
    /// two reads are distinct (0 then 1).
    #[tokio::test]
    async fn the_same_key_serializes() {
        let locks = Arc::new(DatasetLocks::new());
        let id = Uuid::new_v4();
        let counter = Arc::new(AtomicUsize::new(0));
        let reads = Arc::new(Mutex::new(Vec::new()));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let locks = Arc::clone(&locks);
            let counter = Arc::clone(&counter);
            let reads = Arc::clone(&reads);
            handles.push(tokio::spawn(async move {
                let _guard = locks.lock(id).await;
                let seen = counter.load(Ordering::SeqCst);
                // Yield inside the critical section: without the lock the
                // second task resumes here and reads the same value.
                tokio::task::yield_now().await;
                counter.store(seen + 1, Ordering::SeqCst);
                reads.lock().unwrap().push(seen);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(
            counter.load(Ordering::SeqCst),
            2,
            "a lost update means the two critical sections interleaved"
        );
        let mut seen = reads.lock().unwrap().clone();
        seen.sort_unstable();
        assert_eq!(seen, vec![0, 1], "each task must observe the other's write");
    }

    /// Scenario: one task holds the lock for key A while another takes key B.
    /// Expected: B is not blocked by A — the registry is a keyed map, not one
    /// global mutex, so unrelated datasets never serialize.
    /// Verification: acquire A, then acquire B from the same task. A global
    /// mutex would deadlock here and the test would hang rather than fail, so
    /// the assertion is really the completion.
    #[tokio::test]
    async fn different_keys_do_not_block_each_other() {
        let locks = DatasetLocks::new();
        let _a = locks.lock(Uuid::new_v4()).await;
        let _b = locks.lock(Uuid::new_v4()).await;
    }

    /// Scenario: a lock is taken and released, then taken again for the same
    /// key.
    /// Expected: the registry does not grow a permanent entry per key it has
    /// ever seen — the `Weak` entry is reclaimed once the last guard drops.
    /// Verification: drop the guard, then assert the stored `Weak` no longer
    /// upgrades; and that re-locking the key still works.
    #[tokio::test]
    async fn a_released_lock_is_reclaimed() {
        let locks = DatasetLocks::new();
        let id = Uuid::new_v4();

        drop(locks.lock(id).await);

        let dangling = {
            let map = locks.locks.lock().unwrap();
            map.get(&id)
                .map(|weak| weak.strong_count() > 0)
                .unwrap_or(false)
        };
        assert!(
            !dangling,
            "the entry must not keep the mutex alive after its last guard dropped"
        );

        // And the key is still usable — a reclaimed entry is replaced, not
        // resurrected as a lock nobody else can see.
        let _again = locks.lock(id).await;
    }

    /// Scenario: the same name, owner and tenant are locked through
    /// `lock_for_name`, while the equivalent `uuid5` id is locked directly.
    /// Expected: they contend — `lock_for_name` must key on the identity a
    /// create would write, not on some other hash of the name.
    /// Verification: hold the id lock, then assert `lock_for_name` cannot be
    /// acquired without waiting.
    #[tokio::test]
    async fn lock_for_name_keys_on_the_deterministic_id() {
        let locks = DatasetLocks::new();
        let owner = Uuid::new_v4();
        let id = generate_dataset_id("ds", owner, None);

        let _held = locks.lock(id).await;

        let contended = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            locks.lock_for_name("ds", owner, None),
        )
        .await;
        assert!(
            contended.is_err(),
            "lock_for_name must contend with the deterministic id it maps to"
        );
    }
}
