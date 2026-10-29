//! Per-dataset locks: one registry that serializes every writer of one
//! dataset — the create-and-grant sequence of an `add`, and (when a
//! `DeleteService` is wired with the registry) every dataset-scoped delete.
//!
//! Python keeps one such registry for pipeline runs *and* deletes
//! (`cognee/infrastructure/locks/dataset_lock.py:1-8@37f788bd2`), keyed on the
//! dataset row id, so a delete waits for an in-flight run on the dataset and
//! two deletes of one dataset never interleave.
//!
//! # Create-and-grant
//!
//! # Why this exists
//!
//! A dataset row and its ACL rows cannot be written in one transaction:
//! [`AclDb`](cognee_database::AclDb) is a separate trait over a possibly
//! separate store, and a production implementation is a downstream type that
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
//! A create locks the *deterministic* dataset id
//! (`cognee_ingestion::generate_dataset_id`, `uuid5(name, owner, tenant)`)
//! rather than a row that may not exist yet. That is the whole point: the contended moment is the one where two callers have both decided
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

//! # Re-entrancy
//!
//! The mutexes are not re-entrant, so a task that holds a dataset's lock and
//! then reaches code that locks it again (a delete inside an operation that
//! already holds the dataset, an `add` inside it, …) would wait on itself
//! forever. Python solves this with a `ContextVar` of held dataset ids
//! (`held_datasets`, `dataset_lock.py:69-75, 103-105@37f788bd2`). The Rust
//! equivalent is a tokio task-local set:
//!
//! - [`DatasetLocks::hold`] acquires the ids the current task does not already
//!   hold and runs a future inside a scope that marks them held;
//! - [`DatasetLocks::lock`] (and a nested `hold`) inside that scope returns at
//!   once, holding nothing — the outer scope's lock keeps excluding everybody
//!   else.
//!
//! A guard taken with plain [`DatasetLocks::lock`] does **not** mark the id held
//! (a guard cannot open a task-local scope), so code that may re-enter must use
//! [`DatasetLocks::hold`]. And unlike Python's `create_task`, `tokio::spawn`
//! does not inherit task-locals: re-entrancy covers work awaited in the holding
//! task (the pipelines run their tasks there), not work spawned off it.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use uuid::Uuid;

/// Prune dead entries once the map grows past this many keys.
///
/// Entries are `Weak`, so a key whose last guard dropped costs one empty map
/// slot until something prunes it. Pruning on every acquire would make the
/// common case O(keys); pruning only when the map has grown keeps it amortized
/// while still bounding the map by the number of *live* locks.
const PRUNE_THRESHOLD: usize = 64;

/// Distinguishes registries in the task-local held set, so holding dataset `X`
/// in one registry never lets a task skip `X`'s lock in another.
static NEXT_REGISTRY_ID: AtomicU64 = AtomicU64::new(1);

tokio::task_local! {
    /// `(registry, dataset id)` pairs the current task holds through
    /// [`DatasetLocks::hold`].
    static HELD: Arc<HashSet<(u64, Uuid)>>;
}

/// A registry of per-dataset locks. Share it via `Arc`; see the module docs
/// for what it protects, how it is keyed and how re-entrancy works.
#[derive(Debug)]
pub struct DatasetLocks {
    /// This registry's identity in the task-local held set.
    registry_id: u64,
    /// Live locks by dataset id. `Weak` so an entry disappears once its last
    /// guard drops — a long-lived server must not accumulate one mutex per
    /// dataset it has ever been asked about.
    locks: Mutex<HashMap<Uuid, Weak<AsyncMutex<()>>>>,
}

impl Default for DatasetLocks {
    fn default() -> Self {
        Self {
            registry_id: NEXT_REGISTRY_ID.fetch_add(1, Ordering::Relaxed),
            locks: Mutex::new(HashMap::new()),
        }
    }
}

/// The process-wide registry ([`DatasetLocks::global`]).
static GLOBAL: std::sync::LazyLock<Arc<DatasetLocks>> =
    std::sync::LazyLock::new(|| Arc::new(DatasetLocks::new()));

impl DatasetLocks {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// The process-wide registry — Python's one `dataset_lock` registry. It is
    /// the default of every writer that is not wired with its own: `add`
    /// (`AddPipeline`), `cognify` and `memify` (always), `improve` and
    /// dataset-scoped deletes (`DeleteService`), so in one process they queue
    /// per dataset. A host that keeps its own registry wires it into the add
    /// pipeline, the delete service and `improve`; `cognify` and `memify`
    /// still take this one, so a host sharing everything wires this one.
    pub fn global() -> Arc<Self> {
        Arc::clone(&GLOBAL)
    }

    /// Whether the current task holds `dataset_id` in this registry through an
    /// enclosing [`Self::hold`].
    pub fn is_held(&self, dataset_id: Uuid) -> bool {
        HELD.try_with(|held| held.contains(&(self.registry_id, dataset_id)))
            .unwrap_or(false)
    }

    /// Acquire the lock for `dataset_id`, waiting if another task holds it.
    ///
    /// Inside a [`Self::hold`] scope that already holds `dataset_id`, returns
    /// at once with a guard that holds nothing (re-entrant).
    ///
    /// Hold the returned guard across the entire critical section — for a
    /// create, the lookup-insert-grant sequence including the compensating
    /// rollback; releasing it between the insert and the grant reopens exactly
    /// the window the lock exists to close.
    pub async fn lock(&self, dataset_id: Uuid) -> DatasetLockGuard {
        if self.is_held(dataset_id) {
            return DatasetLockGuard { _guard: None };
        }
        let lock = self.lock_arc(dataset_id);
        DatasetLockGuard {
            _guard: Some(lock.lock_owned().await),
        }
    }

    /// Run `fut` holding the locks of every id in `dataset_ids`.
    ///
    /// Ids the current task already holds (an enclosing `hold`) are not
    /// re-acquired; the rest are taken in ascending id order, so two `hold`s
    /// over overlapping sets cannot deadlock each other. Inside `fut`,
    /// [`Self::lock`] and nested `hold`s on these ids are re-entrant. The locks
    /// are released when `fut` completes (or is dropped).
    pub async fn hold<F>(&self, dataset_ids: impl IntoIterator<Item = Uuid>, fut: F) -> F::Output
    where
        F: Future,
    {
        let mut to_take: Vec<Uuid> = dataset_ids
            .into_iter()
            .filter(|id| !self.is_held(*id))
            .collect();
        to_take.sort_unstable();
        to_take.dedup();
        if to_take.is_empty() {
            return fut.await;
        }

        let mut guards = Vec::with_capacity(to_take.len());
        for id in &to_take {
            guards.push(self.lock_arc(*id).lock_owned().await);
        }

        let mut held: HashSet<(u64, Uuid)> = HELD
            .try_with(|held| held.as_ref().clone())
            .unwrap_or_default();
        held.extend(to_take.iter().map(|id| (self.registry_id, *id)));
        let output = HELD.scope(Arc::new(held), fut).await;
        drop(guards);
        output
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

/// Hold on one dataset, released on drop.
///
/// Owns the mutex `Arc` as well as the guard, which is what keeps the
/// registry's `Weak` entry alive for as long as anyone holds the lock. A
/// re-entrant acquisition (see [`DatasetLocks::lock`]) holds nothing.
#[derive(Debug)]
pub struct DatasetLockGuard {
    _guard: Option<OwnedMutexGuard<()>>,
}

impl DatasetLockGuard {
    /// Whether this guard holds the lock itself, rather than riding on an
    /// enclosing [`DatasetLocks::hold`] of the same dataset.
    pub fn is_reentrant(&self) -> bool {
        self._guard.is_none()
    }
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

    /// Scenario: a task holds dataset A through `hold`, then locks A again
    /// with `lock` and with a nested `hold` (what a hooked delete inside an
    /// operation that already holds the dataset does).
    /// Expected: no self-deadlock — both return at once, the inner guard is
    /// re-entrant, and another task still cannot take A meanwhile.
    /// Verification: the whole body runs under a timeout (a deadlock fails it
    /// rather than hanging), and a concurrent `lock(A)` from a spawned task
    /// is still pending while the scope is open.
    #[tokio::test]
    async fn hold_is_reentrant_for_the_holding_task_only() {
        let locks = Arc::new(DatasetLocks::new());
        let a = Uuid::new_v4();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), {
            let locks = Arc::clone(&locks);
            async move {
                locks
                    .hold([a], async {
                        assert!(locks.is_held(a));
                        let inner = locks.lock(a).await;
                        assert!(inner.is_reentrant(), "inner lock must ride on the hold");
                        let nested = locks.hold([a], async { 7 }).await;
                        assert_eq!(nested, 7);

                        // Another task is excluded while the scope is open.
                        let other = {
                            let locks = Arc::clone(&locks);
                            tokio::spawn(async move {
                                let _g = locks.lock(a).await;
                            })
                        };
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        assert!(!other.is_finished(), "another task must wait for the hold");
                        // Wrapped: the handle is a future, and yielding it bare
                        // from an async block reads like a missing `.await`.
                        Some(other)
                    })
                    .await
            }
        })
        .await
        .expect("a re-entrant lock must not deadlock its holder");
        // Released after the scope: the waiting task now gets the lock.
        let other = outcome.expect("the scope returns the waiting task");
        tokio::time::timeout(std::time::Duration::from_secs(5), other)
            .await
            .expect("the hold must release on scope exit")
            .unwrap();
        assert!(!locks.is_held(a), "nothing is held outside the scope");
    }

    /// Scenario: a task holds A in registry 1 and locks A in registry 2.
    /// Expected: registry 2 is not satisfied by registry 1's hold — the held
    /// set is keyed per registry.
    /// Verification: a second task holds A in registry 2; the first task's
    /// `lock(A)` on registry 2 inside its registry-1 hold must wait.
    #[tokio::test]
    async fn the_held_set_is_per_registry() {
        let one = DatasetLocks::new();
        let two = Arc::new(DatasetLocks::new());
        let a = Uuid::new_v4();
        let _blocker = two.lock(a).await;
        one.hold([a], async {
            assert!(!two.is_held(a));
            let waited =
                tokio::time::timeout(std::time::Duration::from_millis(50), two.lock(a)).await;
            assert!(
                waited.is_err(),
                "another registry's lock must not be skipped"
            );
        })
        .await;
    }

    /// Scenario: two tasks `hold` the same two ids, listed in opposite order.
    /// Expected: no deadlock — `hold` takes ids in ascending order whatever
    /// order they were given in.
    /// Verification: both complete under a timeout.
    #[tokio::test]
    async fn hold_takes_multiple_ids_in_a_fixed_order() {
        let locks = Arc::new(DatasetLocks::new());
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut handles = Vec::new();
        for ids in [[a, b], [b, a]] {
            let locks = Arc::clone(&locks);
            handles.push(tokio::spawn(async move {
                for _ in 0..50 {
                    locks.hold(ids, tokio::task::yield_now()).await;
                }
            }));
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for h in handles {
                h.await.unwrap();
            }
        })
        .await
        .expect("opposite-order holds must not deadlock");
    }
}
