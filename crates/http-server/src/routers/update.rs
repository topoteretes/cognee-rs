//! `PATCH /api/v1/update` — replace an existing document and re-cognify.
//!
//! Python parity: `cognee/api/v1/update/routers/get_update_router.py`.
//! Rust delegation: composition of `cognee-delete` (soft-delete the old item)
//! + `cognee-ingestion::AddPipeline` (re-ingest the new multipart payload)
//! + `cognee-cognify::cognify` (re-extract the knowledge graph).

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Multipart, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::patch,
};
use cognee_cognify::{ChunkStrategy, CognifyConfig, cognify as run_cognify};
use cognee_database::{AclDb, IngestDb, NoopPipelineRunRepository, UserDb, ops as db_ops};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope};
use cognee_ingestion::{AddParams, AddPipeline};
use cognee_models::DataInput;
use cognee_ontology::{NoOpOntologyResolver, OntologyResolver};
use uuid::Uuid;

use crate::auth::AuthenticatedUser;
use crate::components::ComponentHandles;
use crate::dto::pipeline_run::PipelineRunInfoDTO;
use crate::dto::update::UpdateQuery;
use crate::error::ApiError;
use crate::multipart::{MultipartOpts, UploadGuard, check_filename_traversal, parse_multipart};
use crate::permissions::check_permission_via_handles;
use crate::pipelines::dispatch::{DispatchOutcome, box_pipeline_future, dispatch_pipeline};
use crate::state::AppState;

// ─── UpdateDispatchError ──────────────────────────────────────────────────────

/// Boxed-future-compatible error type for the update pipeline path.
///
/// `dispatch_pipeline` expects `Box<dyn Error + Send + Sync>`; this wrapper
/// carries the underlying message back to the registry so it surfaces in the
/// `RunPhase::Errored { message }` payload.
#[derive(Debug)]
struct UpdateDispatchError(String);

impl std::fmt::Display for UpdateDispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UpdateDispatchError {}

// ─── patch_update handler ─────────────────────────────────────────────────────

/// `PATCH /api/v1/update` — Replace an existing document and re-cognify the dataset.
///
/// Sequence (Python parity: `update()` in `cognee/api/v1/update/`):
/// 1. Parse the multipart payload and validate filenames for traversal.
/// 2. Resolve the target `Data` and its `Dataset` from the query string.
/// 3. Enforce `write` permission on the dataset.
/// 4. Soft-delete the old `Data` row (cascading through graph + vector).
/// 5. Re-ingest the new multipart files under the same dataset.
/// 6. Re-run cognify on the dataset to refresh the knowledge graph.
///
/// The whole sequence is wrapped in a single `dispatch_pipeline` call so the
/// `pipeline_runs` registry owns the lifecycle row.  Inner pipelines
/// (`AddPipeline`, `cognify`) use `NoopPipelineRunRepository::arc()` so they
/// do not produce a second set of registry rows.
pub async fn patch_update(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    Query(query): Query<UpdateQuery>,
    multipart: Multipart,
) -> Result<axum::response::Response, ApiError> {
    let request_id = Uuid::new_v4().to_string();
    let opts = MultipartOpts::default();
    let parsed = parse_multipart(multipart, &opts, &request_id).await?;
    let _guard = UploadGuard::new(parsed.spool_dir.clone());

    // Validate uploaded files for traversal.
    if let Some(files) = parsed.files.get("data") {
        for f in files {
            if let Some(ref name) = f.filename {
                check_filename_traversal(name)?;
            }
        }
    }

    // ── Resolve components ────────────────────────────────────────────────────
    let Some(components) = state.components() else {
        return Err(ApiError::Internal(anyhow::anyhow!(
            "Component handles not initialized"
        )));
    };
    let components_arc = components.clone();
    let db = components_arc.database.clone();

    // ── Look up target Data and Dataset ──────────────────────────────────────
    let target_data = db
        .get_data(query.data_id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("get_data error: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("data {} not found", query.data_id)))?;
    let _ = target_data; // Loaded purely to confirm existence; the IDs come from query.

    let dataset_id = query.dataset_id;
    let dataset = db
        .get_dataset(dataset_id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("get_dataset error: {e}")))?
        .ok_or_else(|| ApiError::NotFound(format!("dataset {dataset_id} not found")))?;
    let dataset_name = dataset.name;

    // ── Permission gate (write ACL on the dataset) ────────────────────────────
    check_permission_via_handles(&components_arc, user.id, dataset_id, "write").await?;

    // ── Collect re-ingest inputs from the multipart payload ──────────────────
    let mut inputs: Vec<DataInput> = Vec::new();
    let empty_files = Vec::new();
    for part in parsed.files.get("data").unwrap_or(&empty_files) {
        let path = part.path.to_string_lossy().to_string();
        inputs.push(DataInput::FilePath(format!("file://{path}")));
    }
    if inputs.is_empty() {
        // Allow an empty PATCH: the old item gets deleted and an empty dataset
        // touch is registered, matching the behaviour of `POST /add` with no
        // files.
        inputs.push(DataInput::Text(String::new()));
    }

    // ── Build the boxed pipeline future ──────────────────────────────────────
    let run_in_background = false; // PATCH /update always runs inline (Python parity).
    let user_for_run = user.clone();
    let dataset_name_for_run = dataset_name.clone();
    let data_id_for_run = query.data_id;
    let dataset_id_for_run = dataset_id;
    let components_for_run = components_arc.clone();

    let work = box_pipeline_future(async move {
        run_update_pipeline(
            &components_for_run,
            &user_for_run,
            data_id_for_run,
            dataset_id_for_run,
            &dataset_name_for_run,
            inputs,
        )
        .await
    });

    let outcome = dispatch_pipeline(
        &state,
        &user,
        "update_pipeline",
        Some(dataset_id),
        run_in_background,
        work,
    )
    .await?;

    // ── Build response (HashMap<Uuid, PipelineRunInfoDTO>) ───────────────────
    let mut response: HashMap<Uuid, PipelineRunInfoDTO> = HashMap::new();
    match outcome {
        DispatchOutcome::Blocking { outcome } => {
            use cognee_core::pipeline_run_registry::RunPhase;
            match outcome.phase {
                RunPhase::Completed | RunPhase::Pending => {
                    response.insert(
                        query.data_id,
                        PipelineRunInfoDTO {
                            status: "PipelineRunCompleted".into(),
                            pipeline_run_id: outcome.run_id,
                            dataset_id,
                            dataset_name,
                            payload: None,
                            error: None,
                            data_ingestion_info: None,
                        },
                    );
                }
                RunPhase::Errored { message } => {
                    return Err(ApiError::WriteEndpointError {
                        error: "Pipeline run errored".into(),
                        detail: Some(message),
                        status: StatusCode::INTERNAL_SERVER_ERROR,
                    });
                }
                RunPhase::Running => {
                    response.insert(
                        query.data_id,
                        PipelineRunInfoDTO {
                            status: "PipelineRunStarted".into(),
                            pipeline_run_id: outcome.run_id,
                            dataset_id,
                            dataset_name,
                            payload: None,
                            error: None,
                            data_ingestion_info: None,
                        },
                    );
                }
            }
        }
        DispatchOutcome::Background { handle } => {
            response.insert(
                query.data_id,
                PipelineRunInfoDTO {
                    status: "PipelineRunStarted".into(),
                    pipeline_run_id: handle.run_id,
                    dataset_id,
                    dataset_name,
                    payload: None,
                    error: None,
                    data_ingestion_info: None,
                },
            );
        }
    }

    Ok((StatusCode::OK, Json(response)).into_response())
}

// ─── run_update_pipeline ─────────────────────────────────────────────────────

/// Drive the soft-delete → re-ingest → re-cognify chain for a single update.
async fn run_update_pipeline(
    components: &ComponentHandles,
    user: &AuthenticatedUser,
    data_id: Uuid,
    dataset_id: Uuid,
    dataset_name: &str,
    inputs: Vec<DataInput>,
) -> Result<(), UpdateDispatchError> {
    // ── Step 1: soft-delete the old item ─────────────────────────────────────
    let scope = DeleteScope::Data {
        owner_id: user.id,
        data_id,
        dataset_name: Some(dataset_name.to_owned()),
        delete_dataset_if_empty: false,
    };
    let delete_request = DeleteRequest {
        scope,
        mode: DeleteMode::Soft,
        memory_only: false,
    };
    components
        .delete_service
        .execute(&delete_request)
        .await
        .map_err(|e| UpdateDispatchError(format!("delete failed: {e}")))?;

    // ── Step 2: re-ingest via AddPipeline ────────────────────────────────────
    let graph_db = components
        .graph_db
        .clone()
        .ok_or_else(|| UpdateDispatchError("graph_db not wired in ComponentHandles".into()))?;
    let vector_db = components
        .vector_db
        .clone()
        .ok_or_else(|| UpdateDispatchError("vector_db not wired in ComponentHandles".into()))?;
    let thread_pool = components
        .thread_pool
        .clone()
        .ok_or_else(|| UpdateDispatchError("thread_pool not wired in ComponentHandles".into()))?;

    let storage = components.storage.clone();
    let database = components.database.clone();

    let pipeline = AddPipeline::new(storage.clone(), database.clone() as Arc<dyn IngestDb>)
        .with_acl_db(database.clone() as Arc<dyn AclDb>)
        .with_thread_pool(thread_pool.clone())
        .with_graph_db(graph_db.clone())
        .with_vector_db(vector_db.clone())
        .with_database(database.clone())
        .with_pipeline_run_repo(NoopPipelineRunRepository::arc());

    let params = AddParams::default();
    pipeline
        .add_with_params(inputs, dataset_name, user.id, user.tenant_id, &params)
        .await
        .map_err(|e| UpdateDispatchError(format!("re-add failed: {e}")))?;

    // ── Step 3: re-cognify the dataset ───────────────────────────────────────
    let llm = components
        .llm
        .clone()
        .ok_or_else(|| UpdateDispatchError("llm not wired in ComponentHandles".into()))?;
    let embedding_engine = components.embedding_engine.clone().ok_or_else(|| {
        UpdateDispatchError("embedding_engine not wired in ComponentHandles".into())
    })?;

    let ontology_resolver: Arc<dyn OntologyResolver> = components
        .ontology_resolver
        .clone()
        .unwrap_or_else(|| Arc::new(NoOpOntologyResolver::new()));

    let data_items = db_ops::datasets::get_dataset_data(&database, dataset_id)
        .await
        .map_err(|e| UpdateDispatchError(format!("get_dataset_data failed: {e}")))?;

    let user_email = database
        .get_user(user.id)
        .await
        .ok()
        .flatten()
        .map(|u| u.email);

    let mut cognify_config = CognifyConfig::default().with_chunk_strategy(ChunkStrategy::Paragraph);
    if let Some(ref t) = components.transcriber {
        cognify_config = cognify_config.with_transcriber(Arc::clone(t));
    }

    run_cognify(
        data_items,
        dataset_id,
        Some(user.id),
        user_email,
        user.tenant_id,
        llm,
        storage,
        graph_db,
        vector_db,
        embedding_engine,
        database,
        NoopPipelineRunRepository::arc(),
        thread_pool,
        ontology_resolver,
        &cognify_config,
    )
    .await
    .map_err(|e| UpdateDispatchError(format!("cognify failed: {e}")))?;

    Ok(())
}

// ─── router ──────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new().route("/", patch(patch_update))
}

// ─── Inline regression-guard tests ───────────────────────────────────────────

#[cfg(test)]
mod tests {
    //! These tests live inline so the 501-regression guard cannot drift away
    //! from the handler. The whole purpose of this Tier-3 implementation is
    //! to remove the `NotImplemented` short-circuit; the guard test fails
    //! loudly if it ever returns.

    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    /// Build a minimal app state without backends for handler-shape tests.
    async fn test_state() -> AppState {
        AppState::build(crate::config::HttpServerConfig::default())
            .await
            .expect("AppState::build")
    }

    /// Bypass auth: inject a fake user and exercise `patch_update` directly.
    async fn patch_update_no_auth(
        State(state): State<AppState>,
        Query(query): Query<UpdateQuery>,
        multipart: Multipart,
    ) -> Result<axum::response::Response, ApiError> {
        let user = AuthenticatedUser {
            id: Uuid::new_v4(),
            email: "test@example.com".into(),
            is_superuser: false,
            is_verified: true,
            is_active: true,
            tenant_id: Some(Uuid::new_v4()),
            auth_method: crate::auth::AuthMethod::DefaultUser,
        };
        patch_update(user, State(state), Query(query), multipart).await
    }

    fn build_request(data_id: Uuid, dataset_id: Uuid) -> Request<Body> {
        let boundary = "updboundaryregress";
        let body_str = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"data\"; filename=\"file.txt\"\r\nContent-Type: text/plain\r\n\r\nhello world\r\n--{boundary}--\r\n"
        );
        Request::builder()
            .method("PATCH")
            .uri(format!("/?data_id={data_id}&dataset_id={dataset_id}"))
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body_str))
            .expect("request")
    }

    /// **Tier-3 regression guard**: `patch_update` MUST NOT return 501.
    ///
    /// This is the load-bearing assertion that prevents the handler from
    /// being silently reverted to its `NotImplemented` stub. The minimum
    /// app state (no components wired) is enough — the handler should fail
    /// fast with 500, which is *not* 501.
    #[tokio::test]
    async fn patch_update_does_not_return_501() {
        let state = test_state().await;
        let app = Router::new()
            .route("/", patch(patch_update_no_auth))
            .with_state(state);

        let req = build_request(Uuid::new_v4(), Uuid::new_v4());
        let resp = app.oneshot(req).await.expect("response");
        assert_ne!(
            resp.status(),
            StatusCode::NOT_IMPLEMENTED,
            "patch_update must not return 501 — Tier-3 regression guard"
        );
    }

    /// Without `ComponentHandles` wired, the handler must surface an internal
    /// error (500), proving that we reach the real pipeline-resolution path
    /// rather than short-circuiting at parse time.
    #[tokio::test]
    async fn patch_update_missing_components_returns_500() {
        let state = test_state().await;
        let app = Router::new()
            .route("/", patch(patch_update_no_auth))
            .with_state(state);

        let req = build_request(Uuid::new_v4(), Uuid::new_v4());
        let resp = app.oneshot(req).await.expect("response");
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
