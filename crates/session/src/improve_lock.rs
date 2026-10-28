//! The session claim an `improve()` run holds on top of its dataset lock.
//!
//! Mirrors the improve half of Python's
//! `cognee/infrastructure/locks/session_lock.py` (`acquire_improve_lock_many`,
//! `release_improve_lock_many`, `improve_lock_keys`). The claim **waits**:
//! two improves that share a session queue one after the other instead of the
//! second one skipping.
//!
//! One `tokio::sync::Mutex` per key, in a process-wide registry. A run takes
//! all of its keys in sorted order, so two runs that share any key queue one
//! after the other and can never hold one key each while waiting for the
//! other's. Tokio's mutex is fair, so waiters are let in in arrival order. The
//! registry is not tied to a runtime: runs on different runtimes queue on the
//! same claim.
//!
//! Scope: one process (Python's single-worker FastAPI model). A multi-process
//! deployment needs a distributed lock on top.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

/// Drop idle entries once the registry grows past this many keys. An entry
/// only the registry references is neither held nor waited on.
const PRUNE_THRESHOLD: usize = 1024;

type Registry = Mutex<HashMap<String, Arc<AsyncMutex<()>>>>;

static IMPROVE_LOCKS: OnceLock<Registry> = OnceLock::new();

fn registry() -> &'static Registry {
    IMPROVE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The mutexes of `keys`: empty keys dropped, deduplicated, in sorted order.
fn improve_locks<I, S>(keys: I) -> Vec<Arc<AsyncMutex<()>>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut keys: Vec<String> = keys
        .into_iter()
        .map(|k| k.as_ref().to_string())
        .filter(|k| !k.is_empty())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    // lock poison is unrecoverable
    #[allow(clippy::unwrap_used, reason = "lock poison is unrecoverable")]
    let mut locks = registry().lock().unwrap();
    if locks.len() >= PRUNE_THRESHOLD {
        locks.retain(|_, lock| Arc::strong_count(lock) > 1);
    }
    keys.into_iter()
        .map(|key| Arc::clone(locks.entry(key).or_default()))
        .collect()
}

/// The keys one improve run holds, released together on drop.
///
/// Released in reverse acquisition order, exactly once: the claim owns the
/// guards, so a run cannot release a key it does not hold or release one twice
/// (Python raises on that; here it cannot be written).
#[derive(Debug, Default)]
pub struct ImproveClaim {
    guards: Vec<OwnedMutexGuard<()>>,
}

impl ImproveClaim {
    /// How many keys the claim holds.
    pub fn len(&self) -> usize {
        self.guards.len()
    }

    /// Whether the claim holds no key (a run with no sessions).
    pub fn is_empty(&self) -> bool {
        self.guards.is_empty()
    }
}

impl Drop for ImproveClaim {
    fn drop(&mut self) {
        while let Some(guard) = self.guards.pop() {
            drop(guard);
        }
    }
}

/// Wait until every key in `keys` is free, then hold them all.
///
/// Empty keys are ignored and duplicates count once; the rest are taken in
/// sorted order. There is no timeout. Cancel-safe: if the returned future is
/// dropped while it waits, the keys already taken are released (in reverse)
/// before it goes.
pub async fn acquire_improve_lock_many<I, S>(keys: I) -> ImproveClaim
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut claim = ImproveClaim::default();
    for lock in improve_locks(keys) {
        claim.guards.push(lock.lock_owned().await);
    }
    claim
}

/// The claim keys of one improve run: `session:{user_id}:{session_id}` for
/// every non-empty session id.
///
/// Session keys carry the user id because session state is scoped per
/// `(user_id, session_id)`: two users who both call a session "chat" never
/// block each other. The dataset is not a key; improve holds the dataset lock
/// for the whole run, like a pipeline run.
pub fn improve_lock_keys<I, S>(session_ids: I, user_id: &str) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    session_ids
        .into_iter()
        .filter(|sid| !sid.as_ref().is_empty())
        .map(|sid| format!("session:{user_id}:{}", sid.as_ref()))
        .collect()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    //! Ported from Python's
    //! `cognee/tests/unit/infrastructure/locks/test_improve_lock.py`. Every test
    //! uses its own keys: the registry is process-wide.

    use std::time::Duration;

    use super::*;

    fn key(name: &str) -> String {
        format!("session:{}:{name}", uuid::Uuid::new_v4())
    }

    async fn settle() {
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
    }

    async fn within<F: std::future::Future>(fut: F) -> F::Output {
        tokio::time::timeout(Duration::from_secs(1), fut)
            .await
            .expect("must not wait")
    }

    #[tokio::test]
    async fn a_run_sharing_a_key_waits_for_the_holder() {
        let (a, b, c) = (key("a"), key("b"), key("c"));
        let holder = acquire_improve_lock_many([&a, &c]).await;
        let waiter = tokio::spawn(async move {
            let _claim = acquire_improve_lock_many([b, c]).await;
        });
        settle().await;
        assert!(!waiter.is_finished());

        drop(holder);
        within(waiter).await.unwrap();
    }

    #[tokio::test]
    async fn runs_sharing_no_key_do_not_wait() {
        let _first = acquire_improve_lock_many([key("a"), key("c1")]).await;
        let second = within(acquire_improve_lock_many([key("b"), key("c2")])).await;
        assert_eq!(second.len(), 2);
    }

    #[tokio::test]
    async fn waiters_run_in_arrival_order() {
        let c = key("c");
        let holder = acquire_improve_lock_many([&c]).await;
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        for name in ["first", "second", "third"] {
            let (c, order) = (c.clone(), Arc::clone(&order));
            tasks.push(tokio::spawn(async move {
                let _claim = acquire_improve_lock_many([c]).await;
                order.lock().unwrap().push(name);
            }));
            settle().await;
        }
        drop(holder);
        for task in tasks {
            within(task).await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), ["first", "second", "third"]);
    }

    /// Keys are taken in sorted order whatever order the caller lists them in.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn key_order_cannot_deadlock_two_runs() {
        let (a, c) = (key("a"), key("c"));
        let mut runs = Vec::new();
        for keys in [[a.clone(), c.clone()], [c, a]] {
            runs.push(tokio::spawn(async move {
                for _ in 0..50 {
                    let _claim = acquire_improve_lock_many(&keys).await;
                    settle().await;
                }
            }));
        }
        for run in runs {
            within(run).await.unwrap();
        }
    }

    #[tokio::test]
    async fn a_cancelled_wait_releases_the_keys_it_already_took() {
        // `c` sorts before `z`: it is taken, then the wait on `z` blocks.
        let id = uuid::Uuid::new_v4();
        let (c, z) = (format!("session:{id}:c"), format!("session:{id}:z"));
        let holder = acquire_improve_lock_many([&z]).await;
        let waiter = tokio::spawn({
            let (c, z) = (c.clone(), z.clone());
            async move { acquire_improve_lock_many([c, z]).await }
        });
        settle().await;
        assert!(!waiter.is_finished());
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());

        within(acquire_improve_lock_many([&c])).await;
        drop(holder);
    }

    #[tokio::test]
    async fn a_dropped_claim_releases_every_key() {
        let (a, b) = (key("a"), key("b"));
        drop(acquire_improve_lock_many([&a, &b]).await);
        let again = within(acquire_improve_lock_many([&b, &a])).await;
        assert_eq!(again.len(), 2);
    }

    #[tokio::test]
    async fn empty_and_duplicate_keys() {
        assert!(
            within(acquire_improve_lock_many(Vec::<String>::new()))
                .await
                .is_empty()
        );
        assert!(within(acquire_improve_lock_many(["", ""])).await.is_empty());
        let a = key("a");
        assert_eq!(within(acquire_improve_lock_many([&a, &a])).await.len(), 1);
    }

    #[test]
    fn the_claim_can_be_used_from_more_than_one_runtime() {
        let c = key("c");
        for _ in 0..2 {
            let c = c.clone();
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let holder = acquire_improve_lock_many([&c]).await;
                    let waiter = tokio::spawn({
                        let c = c.clone();
                        async move {
                            let _claim = acquire_improve_lock_many([c]).await;
                        }
                    });
                    settle().await;
                    assert!(!waiter.is_finished());
                    drop(holder);
                    within(waiter).await.unwrap();
                });
        }
    }

    /// One registry for every runtime: a run on another runtime (another
    /// thread) queues behind the holder instead of getting its own lock.
    #[test]
    fn a_claim_held_on_one_runtime_blocks_a_run_on_another() {
        let x = key("x");
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (let_go_tx, let_go_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn({
            let x = x.clone();
            move || {
                tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap()
                    .block_on(async move {
                        let _claim = acquire_improve_lock_many([x]).await;
                        held_tx.send(()).unwrap();
                        let_go_rx.recv().unwrap();
                    });
            }
        });
        held_rx.recv_timeout(Duration::from_secs(5)).unwrap();

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let waiter = tokio::spawn(async move {
                    let _claim = acquire_improve_lock_many([x]).await;
                });
                tokio::time::sleep(Duration::from_millis(200)).await;
                assert!(!waiter.is_finished());
                let_go_tx.send(()).unwrap();
                tokio::time::timeout(Duration::from_secs(5), waiter)
                    .await
                    .expect("released on the other runtime")
                    .unwrap();
            });
        holder.join().unwrap();
    }

    #[test]
    fn lock_keys_carry_the_user_and_drop_empty_ids() {
        assert_eq!(
            improve_lock_keys(["chat", "", "other"], "u1"),
            ["session:u1:chat", "session:u1:other"]
        );
        assert!(improve_lock_keys(Vec::<String>::new(), "u1").is_empty());
    }

    #[test]
    fn idle_entries_are_pruned_past_the_threshold() {
        let held = key("held");
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let _claim = rt.block_on(acquire_improve_lock_many([&held]));
        for i in 0..PRUNE_THRESHOLD {
            let _ = improve_locks([format!("idle-{}-{i}", uuid::Uuid::new_v4())]);
        }
        let _ = improve_locks([key("trigger")]);
        let locks = registry().lock().unwrap();
        assert!(locks.len() <= PRUNE_THRESHOLD);
        assert!(locks.contains_key(&held), "a held key is never pruned");
    }
}
