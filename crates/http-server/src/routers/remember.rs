//! `POST /api/v1/remember` — run the remember pipeline (add + cognify + optional memify).
//!
//! Python parity: `cognee/api/v1/remember/routers/get_remember_router.py`.
//!
//! The endpoint accepts a multipart form body with optional file parts (`data`)
//! and optional text fields (`datasetName`, `datasetId`, `node_set`,
//! `run_in_background`, `custom_prompt`, `chunks_per_batch`).

use axum::{
    Json, Router,
    extract::{Multipart, State},
    http::StatusCode,
    response::IntoResponse,
    routing::post,
};
use cognee_cognify::{
    ChunkStrategy, CognifyConfig, MemifyConfig, cognify as run_cognify, run_memify,
};
use cognee_database::{IngestDb, NoopPipelineRunRepository, SessionLifecycleDb, UserDb};
use cognee_ingestion::{AddParams, AddPipeline};
use cognee_models::memory::{FeedbackEntry, MemoryEntry, QAEntry, TraceEntry};
use cognee_models::{Data, DataInput};
use cognee_ontology::{NoOpOntologyResolver, OntologyResolver};
use cognee_search::SessionManager;
use cognee_session::{SessionError, SessionQAUpdate};
use std::sync::Arc;
use uuid::Uuid;

use crate::auth::AuthenticatedUser;
use crate::components::ComponentHandles;
use crate::dto::remember::{
    RememberFormDTO, RememberItemDTO, RememberResultDTO, UploadedFilePart, WireRememberStatus,
};
use crate::dto::remember_entry::RememberEntryRequestDTO;
use crate::error::{ApiError, ValidationDetails};
use crate::middleware::validation::Json as ValidatedJson;
use crate::multipart::{MultipartOpts, UploadGuard, check_filename_traversal, parse_multipart};
use crate::pipelines::dispatch::{DispatchOutcome, box_pipeline_future, dispatch_pipeline};
use crate::routers::feedback;
use crate::state::AppState;

// ─── parse_remember_multipart ─────────────────────────────────────────────────

/// Parse the multipart body into a [`RememberFormDTO`] + uploaded file parts.
async fn parse_remember_multipart(
    multipart: Multipart,
    request_id: &str,
) -> Result<(RememberFormDTO, Vec<UploadedFilePart>, UploadGuard), ApiError> {
    let opts = MultipartOpts::default();
    let parsed = parse_multipart(multipart, &opts, request_id).await?;
    let guard = UploadGuard::new(parsed.spool_dir.clone());

    let mut form = RememberFormDTO::default();
    let mut files: Vec<UploadedFilePart> = Vec::new();

    // ── text fields ───────────────────────────────────────────────────────────
    if let Some(vals) = parsed.fields.get("datasetName")
        && let Some(v) = vals.first()
    {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            form.dataset_name = Some(trimmed.to_owned());
        }
    }

    if let Some(vals) = parsed.fields.get("datasetId")
        && let Some(v) = vals.first()
    {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            match Uuid::parse_str(trimmed) {
                Ok(id) => {
                    form.dataset_id = crate::dto::util::DatasetIdRef(Some(id));
                }
                Err(_) => {
                    return Err(ApiError::BadRequest("Invalid datasetId UUID".into()));
                }
            }
        }
    }

    if let Some(vals) = parsed.fields.get("node_set") {
        let non_empty: Vec<String> = vals
            .iter()
            .filter(|s| !s.trim().is_empty())
            .cloned()
            .collect();
        form.node_set = if non_empty.is_empty() {
            None
        } else {
            Some(non_empty)
        };
    }

    if let Some(vals) = parsed.fields.get("run_in_background")
        && let Some(v) = vals.first()
    {
        let v = v.trim().to_ascii_lowercase();
        form.run_in_background = Some(v == "true" || v == "1");
    }

    if let Some(vals) = parsed.fields.get("custom_prompt")
        && let Some(v) = vals.first()
    {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            form.custom_prompt = Some(trimmed.to_owned());
        }
    }

    // Python parity: `session_id: Optional[str] = Form(default=None, examples=[""])`
    // (`get_remember_router.py:34`). Empty string is treated as `None`.
    if let Some(vals) = parsed.fields.get("session_id")
        && let Some(v) = vals.first()
    {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            form.session_id = Some(trimmed.to_owned());
        }
    }

    if let Some(vals) = parsed.fields.get("chunks_per_batch")
        && let Some(v) = vals.first()
        && let Ok(n) = v.trim().parse::<u32>()
    {
        form.chunks_per_batch = Some(n);
    }

    // ── file parts ────────────────────────────────────────────────────────────
    if let Some(spooled) = parsed.files.get("data") {
        for sf in spooled {
            if let Some(ref fname) = sf.filename {
                check_filename_traversal(fname)?;
            }
            files.push(UploadedFilePart {
                file_name: sf.filename.clone(),
                content_type: sf.content_type.clone(),
                temp_path: sf.path.clone(),
                byte_count: sf.byte_count,
            });
        }
    }

    Ok((form, files, guard))
}

// ─── post_remember ────────────────────────────────────────────────────────────

/// `POST /api/v1/remember`
///
/// Accepts a multipart form body with optional file parts and dispatches the
/// remember pipeline (add + cognify + optional memify).
///
/// Either `datasetName` or `datasetId` must be supplied.
///
/// On error in blocking mode, returns `409 Conflict` per Python parity
/// (unlike cognify/memify which return 500).
pub async fn post_remember(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    multipart: Multipart,
) -> Result<impl IntoResponse, ApiError> {
    // Capture wall-clock start to populate `RememberResultDTO.elapsed_seconds`
    // (Python parity — `RememberResult.elapsed_seconds`).
    let started = std::time::Instant::now();
    let request_id = Uuid::new_v4().to_string();
    let (form, files, _guard) = parse_remember_multipart(multipart, &request_id).await?;

    let session_id = form.session_id.clone();

    // ── Dataset resolution ────────────────────────────────────────────────────
    let db = state.components().map(|c| c.database.clone());
    let dataset_id_opt = form.dataset_id.as_option();

    let (dataset_id, dataset_name) = if let Some(id) = dataset_id_opt {
        let name = if let Some(ref db) = db {
            match db.get_dataset(id).await {
                Ok(Some(ds)) => ds.name,
                Ok(None) => {
                    return Err(ApiError::NotFound(format!("Dataset {id} not found")));
                }
                Err(e) => {
                    return Err(ApiError::Internal(anyhow::anyhow!(
                        "DB error looking up dataset {id}: {e}"
                    )));
                }
            }
        } else {
            id.to_string()
        };
        (id, name)
    } else if let Some(ref name) = form.dataset_name {
        let id = if let Some(ref db) = db {
            match db.get_dataset_by_name(name, user.id, user.tenant_id).await {
                Ok(Some(ds)) => ds.id,
                Ok(None) => {
                    // Python auto-creates the dataset on remember — generate a new id.
                    Uuid::new_v4()
                }
                Err(e) => {
                    return Err(ApiError::Internal(anyhow::anyhow!(
                        "DB error looking up dataset '{name}': {e}"
                    )));
                }
            }
        } else {
            Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes())
        };
        (id, name.clone())
    } else {
        // Python raises HTTPException(400, detail="...") → body uses "detail" key.
        return Err(ApiError::BadRequest(
            "Either datasetId or datasetName must be provided.".into(),
        ));
    };

    let run_in_background = form.run_in_background.unwrap_or(false);

    // ── Build data inputs for the add phase ───────────────────────────────────
    let mut inputs: Vec<DataInput> = Vec::new();
    for part in &files {
        let path = part.temp_path.to_string_lossy().to_string();
        inputs.push(DataInput::FilePath(format!("file://{path}")));
    }
    if inputs.is_empty() {
        inputs.push(DataInput::Text(String::new()));
    }

    // ── Components required for the full remember pipeline ──────────────────
    // The previous no-components branch was a stub that always succeeded;
    // per gap-03, missing handles now surface through the 409 catch-all
    // (Python parity) so the response never falsely reports success.
    let components = state
        .components()
        .ok_or_else(|| ApiError::DeprecatedConflict("An error occurred during remember.".into()))?;
    let db_arc = components.database.clone();
    let storage_arc = components.storage.clone();

    let add_params = AddParams {
        node_set: form.node_set.clone(),
        ..AddParams::default()
    };

    // LIB-06: AddPipeline now routes through `pipeline::execute` and
    // requires graph/vector/thread_pool. Missing handles surface as
    // 409 catch-all to match Python parity.
    let Some(graph_db) = components.graph_db.clone() else {
        return Err(ApiError::DeprecatedConflict(
            "An error occurred during remember.".into(),
        ));
    };
    let Some(vector_db) = components.vector_db.clone() else {
        return Err(ApiError::DeprecatedConflict(
            "An error occurred during remember.".into(),
        ));
    };
    let Some(thread_pool) = components.thread_pool.clone() else {
        return Err(ApiError::DeprecatedConflict(
            "An error occurred during remember.".into(),
        ));
    };

    // Gap 08-07: HTTP `dispatch_pipeline` already writes the
    // four-state `pipeline_runs` trail through `ScopedRunWatcher`.
    // Pass a no-op repo so `AddPipeline`'s inner `DbPipelineWatcher`
    // does not double-write.
    let pipeline = AddPipeline::new(
        Arc::clone(&storage_arc),
        db_arc.clone() as Arc<dyn cognee_database::IngestDb>,
    )
    .with_acl_db(db_arc.clone() as Arc<dyn cognee_database::AclDb>)
    .with_thread_pool(Arc::clone(&thread_pool))
    .with_graph_db(Arc::clone(&graph_db))
    .with_vector_db(Arc::clone(&vector_db))
    .with_database(Arc::clone(&db_arc))
    .with_pipeline_run_repo(NoopPipelineRunRepository::arc());

    // Run add synchronously — errors map to 409 {"error": "An error occurred
    // during remember."} per Python parity (not {"detail": "..."}).
    // Python catches all exceptions here and returns JSONResponse({"error": ...}).
    let data_items: Vec<Data> = pipeline
        .add_with_params(inputs, &dataset_name, user.id, user.tenant_id, &add_params)
        .await
        .map_err(|_e| ApiError::DeprecatedConflict("An error occurred during remember.".into()))?;

    let items_processed = data_items.len();
    let content_hash_first = data_items.first().map(|d| d.content_hash.clone());
    let item_dtos: Vec<RememberItemDTO> = data_items
        .iter()
        .map(|d| RememberItemDTO {
            name: Some(d.name.clone()),
            content_hash: Some(d.content_hash.clone()),
            token_count: (d.token_count >= 0).then_some(d.token_count),
        })
        .collect();

    // ── Build CognifyConfig matching CLI defaults ─────────────────────────────
    let mut cognify_cfg = CognifyConfig::default().with_chunk_strategy(ChunkStrategy::Paragraph);
    if let Some(batch) = form.chunks_per_batch {
        cognify_cfg = cognify_cfg.with_chunks_per_batch(batch.max(1) as usize);
    }
    if let Some(ref prompt) = form.custom_prompt {
        cognify_cfg = cognify_cfg.with_custom_prompt(prompt.clone());
    }
    let cognify_cfg = Arc::new(cognify_cfg);

    // ── Cognify + memify (+ session bookkeeping) dispatch ─────────────────────
    let components_owned = state.components().cloned();
    let user_for_run = user.clone();
    let cognify_cfg_for_run = Arc::clone(&cognify_cfg);
    let data_items_for_run = data_items.clone();
    let session_id_for_run = session_id.clone();

    let work = box_pipeline_future(async move {
        let Some(components) = components_owned else {
            return Err(RememberDispatchError(
                "Component handles not initialized; cannot run remember pipeline".to_string(),
            ));
        };
        run_remember_cognify_memify(
            &components,
            &user_for_run,
            dataset_id,
            data_items_for_run,
            session_id_for_run.as_deref(),
            &cognify_cfg_for_run,
        )
        .await
    });

    let outcome = dispatch_pipeline(
        &state,
        &user,
        "remember_pipeline",
        Some(dataset_id),
        run_in_background,
        work,
    )
    .await
    .map_err(|_e| ApiError::DeprecatedConflict("An error occurred during remember.".into()))?;

    // ── Map outcome → wire status + run id ────────────────────────────────────
    // Decision 15: HTTP wire emits Python-parity lowercase strings.
    use cognee_core::pipeline_run_registry::RunPhase;
    let (status, pipeline_run_id, error_msg) = match outcome {
        DispatchOutcome::Background { handle } => {
            (WireRememberStatus::Running, handle.run_id, None)
        }
        DispatchOutcome::Blocking { outcome } => match outcome.phase {
            RunPhase::Completed | RunPhase::Pending => {
                let wire = if session_id.is_some() {
                    WireRememberStatus::SessionStored
                } else {
                    WireRememberStatus::Completed
                };
                (wire, outcome.run_id, None)
            }
            RunPhase::Errored { message } => {
                // Surface the underlying cause through the 409 catch-all
                // envelope (Python parity for `/api/v1/remember`).
                tracing::error!(
                    dataset_id = %dataset_id,
                    "remember pipeline errored: {}",
                    message
                );
                return Err(ApiError::DeprecatedConflict(
                    "An error occurred during remember.".into(),
                ));
            }
            RunPhase::Running => (WireRememberStatus::Running, outcome.run_id, None::<String>),
        },
    };

    // ── Populate response DTO from real pipeline output ──────────────────────
    let session_ids = session_id.as_ref().map(|s| vec![s.clone()]);
    let result = RememberResultDTO {
        status,
        pipeline_run_id: Some(pipeline_run_id),
        dataset_id: Some(dataset_id),
        dataset_name,
        items_processed: items_processed as u32,
        elapsed_seconds: Some(started.elapsed().as_secs_f64()),
        session_ids,
        content_hash: content_hash_first,
        items: if item_dtos.is_empty() {
            None
        } else {
            Some(item_dtos)
        },
        error: error_msg,
        // Typed-entry-only fields — `None` on the file/text path
        // (Python parity: `entry_type` / `entry_id` are absent when
        // remember was invoked with file/text data, not a `MemoryEntry`).
        entry_type: None,
        entry_id: None,
    };

    Ok((StatusCode::OK, Json(result)))
}

// ─── Remember execution helpers ──────────────────────────────────────────────

/// Boxed-future-compatible error type for the remember pipeline path.
///
/// `dispatch_pipeline` expects `Box<dyn Error + Send + Sync>`; this wrapper
/// carries the underlying message back to the registry so it surfaces in the
/// `RunPhase::Errored { message }` payload.
#[derive(Debug)]
struct RememberDispatchError(String);

impl std::fmt::Display for RememberDispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RememberDispatchError {}

/// Drive the cognify + memify legs of the remember pipeline, plus optional
/// session bookkeeping when `session_id` is set.
///
/// Mirrors the permanent-mode composition in
/// `cognee_lib::api::remember::remember()`: cognify the just-ingested
/// `Data` rows, then run `memify` to populate the `("Triplet","text")`
/// vector collection so `SearchType::TripletCompletion` can serve queries.
///
/// When `session_id` is set, also appends a Q&A entry to the session store
/// (mirrors Python `_session_qa_entry` / library `remember_session`). The
/// session bookkeeping happens after cognify+memify succeed so a session
/// row is only created when the permanent leg landed cleanly.
async fn run_remember_cognify_memify(
    components: &ComponentHandles,
    user: &AuthenticatedUser,
    dataset_id: Uuid,
    data_items: Vec<Data>,
    session_id: Option<&str>,
    cognify_config: &CognifyConfig,
) -> Result<(), RememberDispatchError> {
    // ── Pull required backend handles ─────────────────────────────────────────
    let llm = components
        .llm
        .clone()
        .ok_or_else(|| RememberDispatchError("llm not wired in ComponentHandles".into()))?;
    let graph_db = components
        .graph_db
        .clone()
        .ok_or_else(|| RememberDispatchError("graph_db not wired in ComponentHandles".into()))?;
    let vector_db = components
        .vector_db
        .clone()
        .ok_or_else(|| RememberDispatchError("vector_db not wired in ComponentHandles".into()))?;
    let embedding_engine = components.embedding_engine.clone().ok_or_else(|| {
        RememberDispatchError("embedding_engine not wired in ComponentHandles".into())
    })?;
    let thread_pool = components
        .thread_pool
        .clone()
        .ok_or_else(|| RememberDispatchError("thread_pool not wired in ComponentHandles".into()))?;

    let storage = components.storage.clone();
    let database = components.database.clone();
    // Default to a pass-through resolver when no ontology is configured — same
    // behaviour as the CLI's `--ontology-file` fallback.
    let ontology_resolver: Arc<dyn OntologyResolver> = components
        .ontology_resolver
        .clone()
        .unwrap_or_else(|| Arc::new(NoOpOntologyResolver::new()));

    // Best-effort user_email lookup for provenance stamping (matches CLI).
    let user_email = database
        .get_user(user.id)
        .await
        .ok()
        .flatten()
        .map(|u| u.email);

    // Gap 08-07: the outer `dispatch_pipeline` already wires the
    // four-state `pipeline_runs` trail. Hand the inner cognify/memify a
    // no-op repo so their `DbPipelineWatcher` does not double-write.
    let cognify_repo = NoopPipelineRunRepository::arc();

    // ── Cognify leg ───────────────────────────────────────────────────────────
    run_cognify(
        data_items,
        dataset_id,
        Some(user.id),
        user_email,
        user.tenant_id,
        Arc::clone(&llm),
        storage,
        Arc::clone(&graph_db),
        Arc::clone(&vector_db),
        Arc::clone(&embedding_engine),
        Arc::clone(&database),
        cognify_repo,
        Arc::clone(&thread_pool),
        ontology_resolver,
        cognify_config,
    )
    .await
    .map_err(|e| RememberDispatchError(format!("cognify failed: {e}")))?;

    // ── Memify leg ────────────────────────────────────────────────────────────
    // Python's `remember()` always runs the improve/memify pass after
    // cognify so `SearchType::TripletCompletion` has a populated triplet
    // collection. We mirror that here for parity.
    let memify_config = MemifyConfig::default();
    let memify_repo = NoopPipelineRunRepository::arc();
    run_memify(
        graph_db,
        vector_db,
        embedding_engine,
        thread_pool,
        Arc::clone(&database),
        memify_repo,
        Some(dataset_id),
        Some(user.id),
        user.tenant_id,
        &memify_config,
    )
    .await
    .map_err(|e| RememberDispatchError(format!("memify failed: {e}")))?;

    // ── Optional session bookkeeping ──────────────────────────────────────────
    if let Some(sid) = session_id
        && let Some(session_store) = components.session_store.clone()
    {
        // Best-effort pre-upsert of the `session_records` row so the
        // session is bound to the owner even when the store backend doesn't
        // create it eagerly. Failures are non-fatal (Python parity).
        if let Err(exc) =
            SessionLifecycleDb::ensure_and_touch_session(database.as_ref(), sid, user.id, None)
                .await
        {
            tracing::debug!(
                session_id = sid,
                "remember: pre-upsert session_record failed (non-fatal): {exc}"
            );
        }

        let user_id_str = user.id.to_string();
        let combined_text = format!("(remember dataset_id={dataset_id})");
        if let Err(e) = session_store
            .create_qa_entry(sid, Some(&user_id_str), "", &combined_text, None)
            .await
        {
            return Err(RememberDispatchError(format!(
                "session create_qa_entry failed: {e}"
            )));
        }
    }

    Ok(())
}

// ─── post_remember_entry ──────────────────────────────────────────────────────

/// `POST /api/v1/remember/entry` — typed memory-entry dispatch.
///
/// Python parity: `cognee/api/v1/remember/routers/get_remember_router.py:115-164`.
///
/// **Inline replication** of `cognee_lib::api::remember::remember_entry`
/// (`crates/lib/src/api/remember.rs:603-792`) to work around the
/// `cognee-http-server` ↔ `cognee-lib` cycle constraint
/// (`Cargo.toml:35-37`). The library facade is the canonical in-process
/// Rust SDK entry point for non-HTTP callers; this handler is a parallel
/// implementation that mirrors the same `match` on the `MemoryEntry`
/// variants byte-for-byte. **See also**: if Python's
/// `_dispatch_session_entry` ever changes shape, both this handler **and**
/// `cognee_lib::api::remember::remember_entry` must be updated.
///
/// Status codes match Python:
/// - `200` — success.
/// - `400` — empty `session_id` (validation envelope) or unknown
///   discriminator (`ValidatedJson` rejects at deserialization time).
/// - `503` — session cache not configured (`Python RuntimeError → 503`).
/// - `409 {"error": "An error occurred during remember."}` — catch-all.
#[utoipa::path(
    post,
    path = "/api/v1/remember/entry",
    tag = "remember",
    request_body = RememberEntryRequestDTO,
    responses(
        (status = 200, description = "typed entry stored", body = RememberResultDTO),
        (status = 400, description = "validation error", body = serde_json::Value),
        (status = 401, description = "unauthorized"),
        (status = 409, description = "catch-all", body = serde_json::Value),
        (status = 503, description = "session cache unavailable", body = serde_json::Value),
    )
)]
#[tracing::instrument(
    name = "cognee.api.remember_entry",
    skip(state, payload),
    fields(
        endpoint = "POST /v1/remember/entry",
        cognee.user_id = %user.id,
        entry_type = tracing::field::Empty,
    ),
)]
pub async fn post_remember_entry(
    user: AuthenticatedUser,
    State(state): State<AppState>,
    ValidatedJson(payload): ValidatedJson<RememberEntryRequestDTO>,
) -> Result<Json<RememberResultDTO>, ApiError> {
    let started = std::time::Instant::now();

    // ── Pre-handler validation: empty session_id ─────────────────────────
    // Python's library raises `ValueError("session_id is required ...")` →
    // 400. The HTTP layer additionally requires the Pydantic-style
    // validation envelope (Decision 7, task §5).
    if payload.session_id.trim().is_empty() {
        return Err(ApiError::Validation(ValidationDetails {
            detail: serde_json::json!([{
                "loc": ["body", "session_id"],
                "msg": "session_id is required for typed memory entries",
                "type": "value_error",
            }]),
            body: None,
        }));
    }

    // Record the entry_type discriminator on the current span for telemetry.
    let entry_type_str = payload.entry.type_str();
    tracing::Span::current().record("entry_type", entry_type_str);

    // ── Resolve required handles from ComponentHandles ───────────────────
    let components = state
        .components()
        .ok_or_else(|| ApiError::DeprecatedConflict("An error occurred during remember.".into()))?;
    let session_manager: Arc<SessionManager> = components
        .session_manager
        .clone()
        .ok_or_else(|| ApiError::ServiceUnavailable("Session cache is not configured.".into()))?;
    let database = components.database.clone();

    // ── Best-effort pre-upsert of the session_records row ────────────────
    // Mirrors `crates/lib/src/api/remember.rs:625-641` — log-and-swallow at
    // debug level on failure (non-fatal).
    if let Err(exc) = SessionLifecycleDb::ensure_and_touch_session(
        database.as_ref(),
        &payload.session_id,
        user.id,
        None,
    )
    .await
    {
        tracing::debug!(
            session_id = %payload.session_id,
            "post_remember_entry: pre-upsert session_record failed (non-fatal): {exc}"
        );
    }

    // ── Dispatch by variant — mirrors crates/lib/src/api/remember.rs:650-769
    let user_id_str = user.id.to_string();
    let mut wire_status = WireRememberStatus::SessionStored;
    let mut error_msg: Option<String> = None;

    let entry_id: String = match payload.entry {
        MemoryEntry::Qa(QAEntry {
            question,
            answer,
            context,
            feedback_text,
            feedback_score,
            used_graph_element_ids,
        }) => {
            let qa_id = session_manager
                .save_qa(
                    Some(&payload.session_id),
                    Some(&user_id_str),
                    &question,
                    &answer,
                    Some(context.as_str()),
                )
                .await
                .map_err(map_session_err)?;

            // Follow-up partial update when any of the optional fields
            // are present (mirrors `crates/lib/src/api/remember.rs:674-696`).
            if feedback_text.is_some()
                || feedback_score.is_some()
                || used_graph_element_ids.is_some()
            {
                let used_graph_element_ids_typed = match used_graph_element_ids {
                    Some(value) => Some(Some(serde_json::from_value(value).map_err(|e| {
                        ApiError::BadRequest(format!(
                            "used_graph_element_ids does not match \
                             {{node_ids:[], edge_ids:[]}} shape: {e}"
                        ))
                    })?)),
                    None => None,
                };

                let updates = SessionQAUpdate {
                    feedback_text: feedback_text.map(Some),
                    feedback_score: feedback_score.map(Some),
                    used_graph_element_ids: used_graph_element_ids_typed,
                    ..Default::default()
                };

                session_manager
                    .update_qa(
                        Some(&payload.session_id),
                        Some(&user_id_str),
                        &qa_id,
                        updates,
                    )
                    .await
                    .map_err(map_session_err)?;
            }

            qa_id
        }

        MemoryEntry::Trace(TraceEntry {
            origin_function,
            status: trace_status,
            method_params,
            method_return_value,
            memory_query,
            memory_context,
            error_message,
            generate_feedback_with_llm,
        }) => {
            // Generate (or look up the deterministic fallback for) the
            // `session_feedback` string before persisting the trace step.
            //
            // Parity bump vs. Python: when `generate_feedback_with_llm` is
            // `false`, Python still records the deterministic fallback
            // (`session_manager.py:289-294`). The Rust handler previously
            // wrote `""`; we now match Python on both branches.
            let session_feedback: String = if generate_feedback_with_llm {
                if let Some(llm) = components.llm.as_ref() {
                    feedback::generate_session_feedback(
                        llm.as_ref(),
                        &origin_function,
                        &trace_status,
                        method_return_value.as_ref(),
                        &error_message,
                    )
                    .await
                } else {
                    tracing::warn!(
                        session_id = %payload.session_id,
                        "post_remember_entry: generate_feedback_with_llm=true \
                         but ComponentHandles.llm is None; using deterministic \
                         fallback"
                    );
                    feedback::fallback_feedback(&origin_function, &trace_status, &error_message)
                }
            } else {
                feedback::fallback_feedback(&origin_function, &trace_status, &error_message)
            };

            session_manager
                .add_agent_trace_step(
                    &user_id_str,
                    Some(&payload.session_id),
                    &origin_function,
                    &trace_status,
                    &memory_query,
                    &memory_context,
                    method_params.unwrap_or(serde_json::Value::Null),
                    method_return_value,
                    &error_message,
                    &session_feedback,
                )
                .await
                .map_err(map_session_err)?
        }

        MemoryEntry::Feedback(FeedbackEntry {
            qa_id,
            feedback_text,
            feedback_score,
        }) => {
            let ok = session_manager
                .add_feedback(
                    Some(&payload.session_id),
                    Some(&user_id_str),
                    &qa_id,
                    feedback_text.as_deref(),
                    feedback_score,
                )
                .await
                .map_err(map_session_err)?;

            if !ok {
                wire_status = WireRememberStatus::Errored;
                error_msg = Some(format!(
                    "add_feedback: QA {qa_id} not found in session {}",
                    payload.session_id,
                ));
            }
            // Python parity: entry_id is set to the input qa_id even on
            // not-found (remember.py:307: `result.entry_id = entry.qa_id`).
            qa_id
        }
    };

    let result = RememberResultDTO {
        status: wire_status,
        pipeline_run_id: None,
        dataset_id: None,
        dataset_name: payload.dataset_name,
        items_processed: 0,
        elapsed_seconds: Some(started.elapsed().as_secs_f64()),
        session_ids: Some(vec![payload.session_id]),
        content_hash: None,
        items: None,
        error: error_msg,
        entry_type: Some(entry_type_str.to_string()),
        entry_id: Some(entry_id),
    };

    Ok(Json(result))
}

/// Map a `cognee_session::SessionError` to the matching `ApiError` per
/// Python parity (task §3 step 3 + task §4):
/// - `StoreError` whose message contains `"cache unavailable"` → 503
///   `{"error": "..."}` (Python `RuntimeError → 503`).
/// - everything else → 409 `{"error": "An error occurred during remember."}`.
fn map_session_err(err: SessionError) -> ApiError {
    match err {
        SessionError::StoreError(ref msg) if msg.contains("cache unavailable") => {
            ApiError::ServiceUnavailable(msg.clone())
        }
        other => {
            tracing::error!(error = %other, "remember_entry: session dispatch failed");
            ApiError::DeprecatedConflict("An error occurred during remember.".into())
        }
    }
}

// ─── router ──────────────────────────────────────────────────────────────────

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(post_remember))
        .route("/entry", post(post_remember_entry))
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, http::Request};

    async fn test_state() -> AppState {
        AppState::build(crate::config::HttpServerConfig::default())
            .await
            .expect("AppState::build")
    }

    #[tokio::test]
    async fn router_mounts_at_root() {
        // Just verify the router builds without panicking.
        let state = test_state().await;
        let _app: Router = Router::new().merge(router()).with_state(state);
    }

    /// A POST with a non-multipart content type should return 400 (bad request).
    ///
    /// axum's `Multipart` extractor rejects requests whose `Content-Type` is not
    /// `multipart/form-data` with a 400 response.
    #[tokio::test]
    async fn post_without_multipart_content_type_returns_400() {
        use tower::ServiceExt;
        let state = test_state().await;
        // Mount at "/" so the route is `/`.
        let app = Router::new().merge(router()).with_state(state);

        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{}"#))
            .unwrap();

        let resp = app.oneshot(req).await.expect("oneshot");
        // axum rejects non-multipart bodies for a Multipart extractor with 400.
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Python parity: remember's catch-all 409 body uses `{"error": "..."}`.
    /// Python uses `JSONResponse({"error": ...}, status_code=409)` not
    /// `HTTPException(409, detail=...)`, so the key is "error" not "detail".
    /// See remember.md §2.1 error table.
    #[tokio::test]
    async fn remember_catch_all_409_uses_error_key() {
        use crate::error::ApiError;
        use axum::body::to_bytes;

        // Build the response directly from the DeprecatedConflict variant.
        let resp = ApiError::DeprecatedConflict("An error occurred during remember.".into())
            .into_response();

        assert_eq!(resp.status(), StatusCode::CONFLICT);

        let body_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

        // Must use "error" key, not "detail".
        assert_eq!(
            v["error"], "An error occurred during remember.",
            "remember catch-all must use 'error' key"
        );
        assert!(
            v.get("detail").is_none(),
            "remember catch-all must NOT have 'detail' key"
        );
    }

    /// Python parity: remember's validation 400 body uses `{"detail": "..."}`.
    /// Python uses `HTTPException(400, detail="...")`.
    /// See remember.md §2.1 error table.
    #[tokio::test]
    async fn remember_validation_400_uses_detail_key() {
        use crate::error::ApiError;
        use axum::body::to_bytes;

        let resp = ApiError::BadRequest("Either datasetId or datasetName must be provided.".into())
            .into_response();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let body_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

        assert_eq!(
            v["detail"], "Either datasetId or datasetName must be provided.",
            "remember validation must use 'detail' key per Python HTTPException parity"
        );
        assert!(
            v.get("error").is_none(),
            "remember validation must NOT use 'error' key"
        );
    }

    /// After gap 03 (remember pipeline wiring) lands, a blocking POST to
    /// `/api/v1/remember` without `ComponentHandles` wired no longer falls
    /// through to a no-op stub: it returns the Python-parity 409 catch-all
    /// envelope `{"error": "An error occurred during remember."}`.
    ///
    /// This regression-protects the removal of the no-components `else`
    /// branch — if the stub future is ever re-introduced, this assertion
    /// will flip from 409 to 200.
    #[tokio::test]
    async fn post_remember_without_components_returns_409_catch_all() {
        use axum::body::to_bytes;
        use tower::ServiceExt;

        let state = test_state().await;
        // The default test state has `state.components()` == None — exactly
        // the case the removed no-components branch used to handle.
        let app = Router::new().merge(router()).with_state(state);

        let boundary = "boundary-no-components";
        let body = format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"datasetName\"\r\n\
             \r\n\
             ds\r\n\
             --{boundary}--\r\n"
        );

        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.expect("oneshot");
        assert_eq!(
            resp.status(),
            StatusCode::CONFLICT,
            "missing-components path must return 409 (Python catch-all)"
        );

        let body_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(
            v["error"], "An error occurred during remember.",
            "remember catch-all body must use 'error' key with Python parity text"
        );
        assert!(
            v.get("detail").is_none(),
            "remember catch-all body must NOT contain 'detail' key"
        );
    }
}
