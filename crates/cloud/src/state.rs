//! Module-level async-safe singleton for the remote [`CloudClient`].
//!
//! Line-by-line port of `cognee/api/v1/serve/state.py`. When the
//! singleton is populated, the public V2 API functions
//! (`remember` / `recall` / `improve` / `forget`) should route through
//! the cloud instead of executing locally.
//!
//! # Implementation notes
//!
//! - Backing storage is a [`std::sync::LazyLock`] wrapping a
//!   [`tokio::sync::RwLock<Option<Arc<CloudClient>>>`].
//!   `LazyLock` is stable on edition 2024 so we avoid the
//!   `once_cell::sync::Lazy` dependency.
//! - The inner lock is [`tokio::sync::RwLock`] rather than
//!   `std::sync::RwLock` so awaiting never blocks the runtime.
//! - Readers take a short read lock, clone the [`Arc`], and drop the
//!   guard before returning. Cloning an `Arc` is cheap and never held
//!   across an `.await` boundary, which preserves reader concurrency.
//!
//! # Python-compat naming
//!
//! Python exports `get_remote_client` / `set_remote_client` /
//! `is_remote_mode`. The plan's integration hook in
//! `crates/lib/src/api/{remember,recall,improve,forget}.rs` is expected
//! to call the `*_remote_client` / `is_remote_mode` functions, so we
//! expose those names as aliases alongside the Rust-native
//! [`set_client`] / [`get_client`] / [`clear_client`] /
//! [`is_connected`] helpers.

use std::sync::{Arc, LazyLock};

use tokio::sync::RwLock;

use crate::cloud_client::CloudClient;

/// Process-wide singleton holding the remote cloud client, if any.
///
/// `LazyLock` defers construction of the `RwLock` until first access,
/// which keeps the static initialiser free of runtime state.
static CLIENT: LazyLock<RwLock<Option<Arc<CloudClient>>>> = LazyLock::new(|| RwLock::new(None));

/// Install a [`CloudClient`] as the process-wide remote client.
///
/// Replaces any previously installed client. Dropping the previous
/// `Arc` reference is safe — readers that already hold a clone keep
/// their client alive until they drop it.
pub async fn set_client(client: Arc<CloudClient>) {
    let mut guard = CLIENT.write().await;
    *guard = Some(client);
}

/// Fetch a clone of the currently installed [`CloudClient`], if any.
///
/// Cheap: clones an `Arc`. The read lock is released before the
/// `Arc` is returned, so callers can freely `await` on the result
/// without extending lock lifetime.
pub async fn get_client() -> Option<Arc<CloudClient>> {
    let guard = CLIENT.read().await;
    guard.clone()
}

/// Drop the currently installed client, if any.
///
/// After this returns, [`is_connected`] reports `false` and
/// [`get_client`] returns `None`.
pub async fn clear_client() {
    let mut guard = CLIENT.write().await;
    *guard = None;
}

/// `true` iff a client has been installed via [`set_client`] and not
/// subsequently cleared.
pub async fn is_connected() -> bool {
    CLIENT.read().await.is_some()
}

// ---------------------------------------------------------------------------
// Python-compat aliases — keep parity with `state.py`'s public surface so
// that the C4 integration hook in
// `crates/lib/src/api/{remember,recall,improve,forget}.rs` can use the
// same names the Python SDK uses.
// ---------------------------------------------------------------------------

/// Alias of [`get_client`] matching the Python SDK's function name.
///
/// Ports `cognee.api.v1.serve.state.get_remote_client`.
pub async fn get_remote_client() -> Option<Arc<CloudClient>> {
    get_client().await
}

/// Install or clear the remote client in one call.
///
/// Passing `Some(arc)` is equivalent to [`set_client`]; passing `None`
/// is equivalent to [`clear_client`]. Ports
/// `cognee.api.v1.serve.state.set_remote_client`.
pub async fn set_remote_client(client: Option<Arc<CloudClient>>) {
    let mut guard = CLIENT.write().await;
    *guard = client;
}

/// Alias of [`is_connected`] matching the Python SDK's function name.
///
/// Ports `cognee.api.v1.serve.state.is_remote_mode`.
pub async fn is_remote_mode() -> bool {
    is_connected().await
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    // Every test mutates the same process-wide client singleton, which is
    // also touched by the env tests in `serve`/`disconnect`. Under the default
    // multi-threaded test runner those raced. We serialize against ALL of them
    // via the crate-wide [`crate::ENV_TEST_LOCK`], held synchronously across a
    // local `block_on` (never across an `.await`) — the same pattern the other
    // modules use.

    fn dummy_client() -> Arc<CloudClient> {
        CloudClient::new("https://example.com", "test-key")
            .expect("valid key should construct client")
    }

    /// Run `body` on a fresh current-thread runtime while holding the
    /// crate-wide singleton/env lock, so no other test mutates the global
    /// client concurrently. The guard is held by this synchronous frame across
    /// `block_on`, not across any `.await`, so there is no `await_holding_lock`
    /// hazard.
    fn with_singleton_lock<F, Fut>(body: F)
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let _guard = crate::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime for state tests")
            .block_on(body());
    }

    #[test]
    fn set_get_clear_round_trip() {
        with_singleton_lock(|| async {
            clear_client().await;
            assert!(get_client().await.is_none());

            let client = dummy_client();
            set_client(client.clone()).await;

            let fetched = get_client().await.expect("client should be set");
            assert!(
                Arc::ptr_eq(&fetched, &client),
                "get_client must return the same Arc"
            );

            clear_client().await;
            assert!(get_client().await.is_none());
        });
    }

    #[test]
    fn is_connected_tracks_client_presence() {
        with_singleton_lock(|| async {
            clear_client().await;
            assert!(!is_connected().await);

            set_client(dummy_client()).await;
            assert!(is_connected().await);

            clear_client().await;
            assert!(!is_connected().await);
        });
    }

    #[test]
    fn set_client_replaces_previous() {
        with_singleton_lock(|| async {
            clear_client().await;
            let first = dummy_client();
            set_client(first.clone()).await;

            let second =
                CloudClient::new("https://other.example.com", "other-key").expect("valid key");
            set_client(second.clone()).await;

            let fetched = get_client().await.expect("should be set");
            assert!(
                Arc::ptr_eq(&fetched, &second),
                "second set_client must replace the first"
            );
            assert!(
                !Arc::ptr_eq(&fetched, &first),
                "first client should no longer be installed"
            );

            clear_client().await;
        });
    }

    #[test]
    fn python_compat_aliases_match_canonical_helpers() {
        with_singleton_lock(|| async {
            clear_client().await;
            assert!(!is_remote_mode().await);
            assert!(get_remote_client().await.is_none());

            set_remote_client(Some(dummy_client())).await;
            assert!(is_remote_mode().await);
            assert!(get_remote_client().await.is_some());
            // The canonical helpers must see the same state.
            assert!(is_connected().await);
            assert!(get_client().await.is_some());

            set_remote_client(None).await;
            assert!(!is_remote_mode().await);
            assert!(get_remote_client().await.is_none());
        });
    }

    #[test]
    fn concurrent_reads_are_non_exclusive() {
        with_singleton_lock(|| async {
            use std::sync::atomic::{AtomicUsize, Ordering};
            use tokio::sync::Barrier;
            use tokio::time::{Duration, sleep};

            clear_client().await;
            set_client(dummy_client()).await;

            // Two tasks both acquire read access via `get_client`. If the
            // lock were exclusive on the read side, the second task could
            // not enter until the first finished — the barrier would
            // deadlock. `tokio::sync::RwLock` allows parallel reads, so
            // both tasks reach the barrier and complete.
            let barrier = Arc::new(Barrier::new(2));
            let counter = Arc::new(AtomicUsize::new(0));

            let b1 = Arc::clone(&barrier);
            let c1 = Arc::clone(&counter);
            let t1 = tokio::spawn(async move {
                let _ = get_client().await;
                c1.fetch_add(1, Ordering::SeqCst);
                b1.wait().await;
            });

            let b2 = Arc::clone(&barrier);
            let c2 = Arc::clone(&counter);
            let t2 = tokio::spawn(async move {
                let _ = get_client().await;
                c2.fetch_add(1, Ordering::SeqCst);
                b2.wait().await;
            });

            // Don't wait forever — 5s is ample for two Arc clones.
            let joined = tokio::time::timeout(Duration::from_secs(5), async {
                let (r1, r2) = tokio::join!(t1, t2);
                r1.expect("task 1 must not panic");
                r2.expect("task 2 must not panic");
            })
            .await;

            assert!(
                joined.is_ok(),
                "two concurrent readers must both finish; barrier would deadlock on an exclusive lock"
            );
            assert_eq!(counter.load(Ordering::SeqCst), 2);

            // Tiny pause to ensure the tasks' drop runs before we move on
            // (not strictly required, just tidy).
            sleep(Duration::from_millis(1)).await;
            clear_client().await;
        });
    }
}
