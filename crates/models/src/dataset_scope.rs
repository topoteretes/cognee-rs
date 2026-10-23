//! The dataset an operation runs in, for per-dataset storage.
//!
//! With `ENABLE_BACKEND_ACCESS_CONTROL` every dataset has its own graph and
//! vector database (`docs/design/per-dataset-storage.md`). The engines the
//! component layer hands out are then routing engines: each call forwards to
//! the engine of the dataset named by this scope. It is the Rust counterpart
//! of Python's `current_dataset_id` / `graph_db_config` / `vector_db_config`
//! `ContextVar`s (`cognee/context_global_variables.py`), set by
//! `set_database_global_context_variables`.
//!
//! A scope is attached to a future, so it follows that future wherever it runs,
//! including into a `tokio::spawn` that runs the scoped future itself. A spawn
//! *inside* a scoped future starts a new task without it; wrap the spawned
//! future in [`propagate`] when it touches a graph or vector engine.
//!
//! Without per-dataset storage nothing reads the scope, so entering one is
//! harmless: operations enter it unconditionally.
//!
//! On wasm32 (no tokio; see `docs/spike-wasm-config1.md`) there is no
//! per-dataset storage, and the scope is always empty.

use std::future::Future;

use uuid::Uuid;

#[cfg(not(target_arch = "wasm32"))]
tokio::task_local! {
    static CURRENT_DATASET: Uuid;
}

/// Run `future` with `dataset_id` as the current dataset.
///
/// A nested scope for another dataset replaces this one until it ends.
#[cfg(not(target_arch = "wasm32"))]
pub fn with_dataset<F: Future>(dataset_id: Uuid, future: F) -> impl Future<Output = F::Output> {
    CURRENT_DATASET.scope(dataset_id, future)
}

/// Run `future` with `dataset_id` as the current dataset (a no-op on wasm32).
#[cfg(target_arch = "wasm32")]
pub fn with_dataset<F: Future>(_dataset_id: Uuid, future: F) -> impl Future<Output = F::Output> {
    future
}

/// The current dataset, or `None` outside every scope.
#[cfg(not(target_arch = "wasm32"))]
pub fn current_dataset() -> Option<Uuid> {
    CURRENT_DATASET.try_with(|id| *id).ok()
}

/// The current dataset: always `None` on wasm32.
#[cfg(target_arch = "wasm32")]
pub fn current_dataset() -> Option<Uuid> {
    None
}

/// `future` in `dataset_id`'s scope when there is one, unscoped otherwise —
/// for an operation whose dataset is optional (an ad-hoc memify, an improve
/// whose dataset does not exist yet).
pub async fn maybe_with_dataset<F: Future>(dataset_id: Option<Uuid>, future: F) -> F::Output {
    match dataset_id {
        Some(id) => with_dataset(id, future).await,
        None => future.await,
    }
}

/// `future` in the caller's dataset scope, for a future that will run in
/// another task (`tokio::spawn`), which does not inherit it. Outside a scope
/// the future is returned to run unscoped.
pub fn propagate<F: Future>(future: F) -> impl Future<Output = F::Output> {
    let dataset = current_dataset();
    async move {
        match dataset {
            Some(id) => with_dataset(id, future).await,
            None => future.await,
        }
    }
}

// The task-local scope is native-only; wasm32 has no tokio to test it with.
#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_scope_is_visible_inside_and_gone_after() {
        let id = Uuid::new_v4();
        assert_eq!(current_dataset(), None);
        let seen = with_dataset(id, async { current_dataset() }).await;
        assert_eq!(seen, Some(id));
        assert_eq!(current_dataset(), None);
    }

    #[tokio::test]
    async fn an_inner_scope_wins_until_it_ends() {
        let outer = Uuid::new_v4();
        let inner = Uuid::new_v4();
        let (in_inner, after_inner) = with_dataset(outer, async {
            let in_inner = with_dataset(inner, async { current_dataset() }).await;
            (in_inner, current_dataset())
        })
        .await;
        assert_eq!(in_inner, Some(inner));
        assert_eq!(after_inner, Some(outer));
    }

    #[tokio::test]
    async fn a_spawned_scoped_future_keeps_its_scope() {
        let id = Uuid::new_v4();
        let seen = tokio::spawn(with_dataset(id, async { current_dataset() }))
            .await
            .unwrap();
        assert_eq!(seen, Some(id));
    }

    #[tokio::test]
    async fn a_spawn_inside_a_scope_needs_propagate() {
        let id = Uuid::new_v4();
        let (bare, propagated) = with_dataset(id, async {
            let bare = tokio::spawn(async { current_dataset() }).await.unwrap();
            let propagated = tokio::spawn(propagate(async { current_dataset() }))
                .await
                .unwrap();
            (bare, propagated)
        })
        .await;
        assert_eq!(bare, None);
        assert_eq!(propagated, Some(id));
    }
}
