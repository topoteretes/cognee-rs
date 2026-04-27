//! HTTP-side pipeline dispatcher.
//!
//! `dispatch_pipeline` is the single entry point for all four P3 routers
//! (cognify, memify, remember, improve). It owns:
//!
//! 1. **Deterministic ID computation** per §4 of `docs/http-server/pipelines.md`:
//!    - `pipeline_id = uuid5(OID, "{user_id}{pipeline_name}{dataset_id}")`
//!    - `pipeline_run_id = uuid5(OID, "{pipeline_id}_{dataset_id}")`
//! 2. **`run_in_background` branching**: `false` → `register_inline`, awaits to
//!    completion; `true` → `register_background`, returns immediately.
//! 3. **`RegistryError` → `ApiError::Internal`** mapping.
//!
//! The `work` closure is a pre-boxed `PipelineFuture` supplied by the caller.
//! Library functions do not receive a `TaskContext` from the dispatcher in P3 —
//! they create their own context internally. The dispatcher's only concern is
//! lifecycle tracking via the registry.

use std::future::Future;

use uuid::Uuid;

use cognee_core::pipeline_run_registry::{PipelineFuture, RunHandle, RunOutcome, RunSpec};

use crate::{auth::AuthenticatedUser, error::ApiError, state::AppState};

// ─── Public ID helpers ────────────────────────────────────────────────────────

/// `pipeline_id = uuid5(OID, "{user_id}{pipeline_name}{dataset_id}")`
///
/// Matches [Python's `generate_pipeline_id`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/utils/generate_pipeline_id.py).
///
/// `dataset_id` defaults to `Uuid::nil()` when absent (ad-hoc paths).
pub fn pipeline_id(user_id: Uuid, dataset_id: Uuid, pipeline_name: &str) -> Uuid {
    let s = format!("{}{}{}", user_id, pipeline_name, dataset_id);
    Uuid::new_v5(&Uuid::NAMESPACE_OID, s.as_bytes())
}

/// `pipeline_run_id = uuid5(OID, "{pipeline_id}_{dataset_id}")`
///
/// Matches [Python's `generate_pipeline_run_id`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/utils/generate_pipeline_run_id.py).
///
/// Note: this id is **not unique across separate runs** of the same pipeline —
/// Python intentionally reuses it so a re-cognify of the same dataset returns
/// the same `pipeline_run_id`. The `id` column in `pipeline_runs` is the true
/// PK; multiple rows can share the same `pipeline_run_id`.
pub fn pipeline_run_id(pipeline_id: Uuid, dataset_id: Uuid) -> Uuid {
    let s = format!("{}_{}", pipeline_id, dataset_id);
    Uuid::new_v5(&Uuid::NAMESPACE_OID, s.as_bytes())
}

// ─── DispatchOutcome ─────────────────────────────────────────────────────────

/// Outcome of a `dispatch_pipeline` call.
#[derive(Debug)]
pub enum DispatchOutcome {
    /// `run_in_background = false` — the work ran to completion.
    Blocking { outcome: RunOutcome },
    /// `run_in_background = true` — the work was spawned; use the handle to
    /// subscribe.
    Background { handle: RunHandle },
}

// ─── dispatch_pipeline ───────────────────────────────────────────────────────

/// Register a pipeline future with the registry and branch on
/// `run_in_background`.
///
/// # Arguments
///
/// * `state` — shared app state; `state.pipelines` is the registry.
/// * `user` — authenticated caller; used for the deterministic pipeline id.
/// * `pipeline_name` — `"cognify_pipeline"`, `"memify_pipeline"`, etc.
/// * `dataset_id` — `Some(uuid)` for dataset-scoped runs; `None` for ad-hoc
///   paths (none in P3, included for completeness).
/// * `run_in_background` — when `true`, spawns and returns a `RunHandle`;
///   when `false`, awaits the work to completion and returns a `RunOutcome`.
/// * `work` — a boxed `Send + 'static` future whose output is a generic
///   `Result`.  Library functions that already have this signature (`cognify`,
///   `memify`, `remember`, `improve`) can be boxed directly:
///   `Box::pin(async move { cognify(...).await.map_err(|e| Box::new(e) as _) })`.
///
/// # Errors
///
/// Registry errors (capacity full, shutdown) map to `ApiError::Internal`.
pub async fn dispatch_pipeline(
    state: &AppState,
    user: &AuthenticatedUser,
    pipeline_name: &str,
    dataset_id: Option<Uuid>,
    run_in_background: bool,
    work: PipelineFuture,
) -> Result<DispatchOutcome, ApiError> {
    // ── Deterministic IDs (§4 of pipelines.md) ────────────────────────────────
    let ds_id = dataset_id.unwrap_or_else(Uuid::nil);
    let pid = pipeline_id(user.id, ds_id, pipeline_name);
    let prid = dataset_id.map(|d| pipeline_run_id(pid, d));

    let spec = RunSpec {
        run_id: prid,
        pipeline_name: pipeline_name.to_owned(),
        user_id: Some(user.id),
        dataset_id,
    };

    if run_in_background {
        let handle = state
            .pipelines
            .register_background(spec, work)
            .await
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("registry error: {e}")))?;
        Ok(DispatchOutcome::Background { handle })
    } else {
        let outcome = state
            .pipelines
            .register_inline(spec, work)
            .await
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("registry error: {e}")))?;
        Ok(DispatchOutcome::Blocking { outcome })
    }
}

// ─── Convenience: box a future into PipelineFuture ───────────────────────────

/// Box a `Send + 'static` future (whose error is convertible to
/// `Box<dyn Error + Send + Sync>`) into the registry's `PipelineFuture` type.
///
/// Usage:
/// ```rust,ignore
/// let work = box_pipeline_future(async move {
///     cognify(...).await.map_err(|e| Box::new(e) as _)
/// });
/// ```
pub fn box_pipeline_future<F, E>(fut: F) -> PipelineFuture
where
    F: Future<Output = Result<(), E>> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    Box::pin(async move {
        fut.await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    })
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    // Verify the deterministic ID functions match Python's algorithm.

    #[test]
    fn pipeline_id_is_deterministic() {
        let user_id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let dataset_id = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();

        let a = pipeline_id(user_id, dataset_id, "cognify_pipeline");
        let b = pipeline_id(user_id, dataset_id, "cognify_pipeline");
        assert_eq!(a, b, "same inputs must produce same output");
    }

    #[test]
    fn pipeline_id_differs_on_name() {
        let user_id = Uuid::new_v4();
        let dataset_id = Uuid::new_v4();

        let cognify = pipeline_id(user_id, dataset_id, "cognify_pipeline");
        let memify = pipeline_id(user_id, dataset_id, "memify_pipeline");
        assert_ne!(cognify, memify);
    }

    #[test]
    fn pipeline_run_id_is_deterministic() {
        let pid = Uuid::new_v4();
        let did = Uuid::new_v4();
        assert_eq!(pipeline_run_id(pid, did), pipeline_run_id(pid, did));
    }

    #[test]
    fn pipeline_run_id_differs_on_dataset() {
        let pid = Uuid::new_v4();
        let did1 = Uuid::new_v4();
        let did2 = Uuid::new_v4();
        assert_ne!(pipeline_run_id(pid, did1), pipeline_run_id(pid, did2));
    }

    #[tokio::test]
    async fn dispatch_blocking_returns_outcome() {
        // Build a minimal AppState with a no-op registry.
        let state = AppState::build(crate::config::HttpServerConfig::default())
            .await
            .expect("AppState::build");

        let user = AuthenticatedUser {
            id: Uuid::new_v4(),
            email: "test@example.com".into(),
            is_superuser: false,
            is_verified: true,
            is_active: true,
            tenant_id: Some(Uuid::new_v4()),
            auth_method: crate::auth::AuthMethod::DefaultUser,
        };

        let work = box_pipeline_future(async move { Ok::<(), std::io::Error>(()) });

        let result = dispatch_pipeline(
            &state,
            &user,
            "test_pipeline",
            Some(Uuid::new_v4()),
            false, // blocking
            work,
        )
        .await
        .expect("dispatch should succeed");

        assert!(matches!(result, DispatchOutcome::Blocking { .. }));
    }

    #[tokio::test]
    async fn dispatch_background_returns_handle() {
        let state = AppState::build(crate::config::HttpServerConfig::default())
            .await
            .expect("AppState::build");

        let user = AuthenticatedUser {
            id: Uuid::new_v4(),
            email: "test@example.com".into(),
            is_superuser: false,
            is_verified: true,
            is_active: true,
            tenant_id: Some(Uuid::new_v4()),
            auth_method: crate::auth::AuthMethod::DefaultUser,
        };

        let dataset_id = Uuid::new_v4();

        // The background work parks for a moment.
        let work = box_pipeline_future(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            Ok::<(), std::io::Error>(())
        });

        let result = dispatch_pipeline(
            &state,
            &user,
            "test_pipeline",
            Some(dataset_id),
            true, // background
            work,
        )
        .await
        .expect("dispatch should succeed");

        let handle = match result {
            DispatchOutcome::Background { handle } => handle,
            _ => panic!("expected Background variant"),
        };

        // The returned handle carries the deterministic run_id.
        let user_id = user.id;
        let pid = pipeline_id(user_id, dataset_id, "test_pipeline");
        let expected_prid = pipeline_run_id(pid, dataset_id);
        assert_eq!(handle.run_id, expected_prid);
    }

    #[tokio::test]
    async fn dispatch_with_dataset_id_none_uses_nil() {
        let state = AppState::build(crate::config::HttpServerConfig::default())
            .await
            .expect("AppState::build");

        let user = AuthenticatedUser {
            id: Uuid::new_v4(),
            email: "test@example.com".into(),
            is_superuser: false,
            is_verified: true,
            is_active: true,
            tenant_id: Some(Uuid::new_v4()),
            auth_method: crate::auth::AuthMethod::DefaultUser,
        };

        // dataset_id=None path (ad-hoc): run_id becomes None → registry
        // auto-generates one.
        let work = box_pipeline_future(async { Ok::<(), std::io::Error>(()) });

        let result = dispatch_pipeline(
            &state,
            &user,
            "adhoc_pipeline",
            None, // no dataset_id
            false,
            work,
        )
        .await
        .expect("dispatch with None dataset_id should succeed");

        assert!(matches!(result, DispatchOutcome::Blocking { .. }));
    }
}
