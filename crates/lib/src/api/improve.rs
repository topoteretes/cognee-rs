//! Bidirectional session-graph bridge — `improve()`.
//!
//! Multi-stage pipeline matching Python `cognee.api.v1.improve.improve()`:
//! 1. Apply feedback weights from session Q&A entries to graph nodes/edges.
//! 2. Persist session Q&A text into the permanent knowledge graph (Stage 2),
//!    then persist agent trace steps (Stage 2b), then distill each session's
//!    Q&A into curated, entity-anchored lesson documents tagged with the
//!    `session_learnings` node-set (Stage 2c).
//! 3. Default enrichment: reuse `memify()` for triplet embeddings.
//! 4. Sync recent graph edges into the session's `graph_context`.
//!
//! Each stage is wrapped in a warning-only handler so that a failure in one
//! stage does not abort subsequent stages (matches Python semantics).

use std::sync::Arc;

use cognee_cognify::memify::sync_graph_session::DEFAULT_MAX_LINES;
use cognee_cognify::{
    CognifyConfig, MemifyConfig, MemifyResult, apply_feedback_weights_pipeline,
    distill_sessions_in_knowledge_graph, persist_sessions_in_knowledge_graph, run_memify,
    sync_graph_to_session,
};
use cognee_database::{CheckpointStore, PipelineRunRepository, RelationalDb};
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_ingestion::{AddParams, AddPipeline};
use cognee_llm::Llm;
use cognee_models::DataInput;
use cognee_models::dataset_scope::maybe_with_dataset;
use cognee_ontology::OntologyResolver;
use cognee_session::{ImproveLockGuard, SessionManager, SessionStore};
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;
use tracing::{info, warn};
use uuid::Uuid;

use super::error::ApiError;

/// Result of an `improve()` operation.
#[derive(Debug, Clone, Default)]
pub struct ImproveResult {
    /// Names of stages that were executed.
    pub stages_run: Vec<String>,
    /// Result of the memify (triplet embedding) stage, if it ran.
    pub memify_result: Option<MemifyResult>,
    /// Number of feedback QA entries that were processed (Stage 1).
    pub feedback_entries_processed: usize,
    /// Number of feedback QA entries whose graph updates all applied cleanly.
    pub feedback_entries_applied: usize,
    /// Number of sessions whose Q&A text was persisted to the graph (Stage 2).
    pub sessions_persisted: usize,
    /// Number of sessions that produced >= 1 distilled lesson (Stage 2c).
    pub sessions_distilled: usize,
    /// Total number of distilled lesson documents published (Stage 2c).
    pub lessons_published: usize,
    /// Total number of edges newly synced into session contexts (Stage 4).
    pub edges_synced: usize,
}

/// Parameters for [`improve`].
///
/// All fields are required at construction time — `Default` is intentionally
/// not derived because several fields (`owner_id`, the engine handles, and
/// `cognify_config`) have no sensible default value. This forces every caller
/// to think about each dependency. Callers that omit optional behavior should
/// pass `None` explicitly for the `Option<...>` fields.
///
/// LIB-04 (Decision 8) introduced this struct to replace the previous 18
/// positional parameters. E-05 (this commit) extended it with three v2
/// power-user fields — `extraction_tasks`, `enrichment_tasks`, `data` —
/// matching Python's `ImprovePayloadDTO` field-for-field. They are pure-data
/// fields and currently informational: the orchestrator does not yet branch
/// on them, but they are accepted by the constructor so callers (especially
/// the HTTP layer) can plumb the raw payload through without dropping fields.
pub struct ImproveParams<'a> {
    /// Dataset name to operate on (Stage 2 persistence + Stage 4 lookup).
    pub dataset_name: String,
    /// Session ids that drive Stages 1, 2, and 4. `None` or empty skips them.
    pub session_ids: Option<Vec<String>>,
    /// Optional graph node-name filter applied to the memify (Stage 3) pass.
    pub node_name: Option<Vec<String>>,
    /// Owner UUID under which graph/session reads and writes are scoped.
    pub owner_id: Uuid,
    /// Optional tenant UUID for multi-tenant deployments.
    pub tenant_id: Option<Uuid>,
    /// Mixing factor for feedback weight propagation (Stage 1).
    pub feedback_alpha: f64,

    /// Optional list of extraction-task identifiers (Python parity:
    /// `extraction_tasks: Optional[List[str]]`). Currently informational —
    /// reserved for future power-user overrides matching Python's
    /// `ImproveKwargs.extraction_tasks`.
    pub extraction_tasks: Option<Vec<String>>,
    /// Optional list of enrichment-task identifiers (Python parity:
    /// `enrichment_tasks: Optional[List[str]]`). Currently informational.
    pub enrichment_tasks: Option<Vec<String>>,
    /// Optional inline text payload (Python parity: `data: Optional[str]`).
    /// Currently informational; reserved for future power-user overrides.
    pub data: Option<String>,

    /// When `true` and not running in background, build the global context
    /// index (graph summary) after Stage 3.
    ///
    /// Mirrors Python's `build_global_context_index` parameter.
    /// Default `false` (opt-in) — matches Python parity.
    pub build_global_context_index: bool,

    /// When `true` and sessions are present, build/refresh the truth-subspace
    /// centroid slots and per-chunk alignment (Stage 2d, Phase-2 join point).
    ///
    /// Mirrors Python's `build_truth_subspace` parameter. Default `false`
    /// (opt-in) — the entire truth-subspace path is inert unless this is set.
    pub build_truth_subspace: bool,

    /// When `true`, treat this as a background run: skips stages that
    /// require the prior stage to have completed synchronously (e.g. the
    /// global context index and the sync-graph stage).
    ///
    /// Background dispatch is handled by the host (HTTP server or CLI);
    /// this flag is used only for stage-skipping logic parity with Python.
    pub run_in_background: bool,

    /// LLM handle (used by Stage 2 cognify-of-session-text).
    pub llm: Arc<dyn Llm>,
    /// File storage handle.
    pub storage: Arc<dyn StorageTrait>,
    /// Graph database handle.
    pub graph_db: Arc<dyn GraphDBTrait>,
    /// Vector database handle.
    pub vector_db: Arc<dyn VectorDB>,
    /// Embedding engine handle.
    pub embedding_engine: Arc<dyn EmbeddingEngine>,
    /// Ontology resolver handle.
    pub ontology_resolver: Arc<dyn OntologyResolver>,

    /// Metadata DB connection. Required for Stage 4 (dataset lookup).
    pub db: Option<Arc<dyn RelationalDb>>,
    /// Session backing store. Required for Stages 1 and 2.
    pub session_store: Option<Arc<dyn SessionStore>>,
    /// Session manager. Required for Stages 1 and 4.
    pub session_manager: Option<Arc<SessionManager>>,
    /// Add pipeline (borrowed). Required for Stage 2.
    pub add_pipeline: Option<&'a AddPipeline>,
    /// Checkpoint store. Required for Stage 4.
    pub checkpoint_store: Option<Arc<dyn CheckpointStore>>,

    /// Borrowed cognify configuration used by Stage 2 persistence.
    pub cognify_config: &'a CognifyConfig,
}

/// Bidirectional session-graph bridge.
///
/// Background dispatch is a host-side concern — this function is strictly
/// synchronous. Stage 4 always runs when sessions are present.
///
/// All inputs are passed via [`ImproveParams`] (see Decision 8 / LIB-04).
/// The id of the owner's dataset `name` in `tenant`, or `None` when there is
/// no relational database or no such dataset. Names resolve among the owner's
/// own datasets only, as Python's `get_dataset_ids` does; a failed lookup is
/// an error, never a silent run of the graph stages outside the dataset.
async fn dataset_id_by_name(
    db: Option<&Arc<dyn RelationalDb>>,
    name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
) -> Result<Option<Uuid>, ApiError> {
    let Some(db) = db else {
        return Ok(None);
    };
    let dataset =
        cognee_database::IngestDb::get_dataset_by_name_in_tenant(&**db, name, owner_id, tenant_id)
            .await
            .map_err(|e| ApiError::Improve(format!("dataset lookup failed for '{name}': {e}")))?;
    Ok(dataset.map(|ds| ds.id))
}

pub async fn improve(params: ImproveParams<'_>) -> Result<ImproveResult, ApiError> {
    let ImproveParams {
        dataset_name,
        session_ids,
        node_name,
        owner_id,
        tenant_id,
        feedback_alpha,
        llm,
        storage,
        graph_db,
        vector_db,
        embedding_engine,
        ontology_resolver,
        db,
        session_store,
        session_manager,
        add_pipeline,
        checkpoint_store,
        cognify_config,
        build_global_context_index,
        build_truth_subspace,
        run_in_background,
        // E-05 v2 power-user fields — currently informational; the orchestrator
        // does not yet branch on them. Accepting them here keeps the struct
        // shape Python-parity-aligned for HTTP plumbing.
        extraction_tasks: _extraction_tasks,
        enrichment_tasks: _enrichment_tasks,
        data: _data,
    } = params;

    // ---- Improve lock (parity with Python session_lock.py:136-150) ----
    //
    // When exactly one session is targeted, acquire a per-session lock so
    // that concurrent `improve()` calls on the same session don't duplicate
    // work (e.g. auto-improve + idle-watcher + SessionEnd firing at once).
    // Multi-session improves skip the lock — the pattern is rare and locking
    // N sessions atomically is messy (matches Python comment verbatim).
    //
    // The guard holds a `String`, not a `MutexGuard`, so it is Send-safe
    // across `.await` points.
    let _improve_guard = if let Some(ref sids) = session_ids {
        if sids.len() == 1 {
            match ImproveLockGuard::acquire(&sids[0]) {
                Some(g) => Some(g),
                None => {
                    info!(
                        session_id = %sids[0],
                        "improve: session already being improved, skipping"
                    );
                    // Parity with Python `return {}` — return empty result.
                    return Ok(ImproveResult::default());
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    let mut result = ImproveResult::default();
    let has_sessions = session_ids.as_ref().is_some_and(|ids| !ids.is_empty());

    // Wrap the body in a `cognee.api.improve` OTEL span for parity with
    // Python's `cognee.api.v1.improve.improve()` (gap 03 / task 03-07).
    // Attribute names mirror the analytics payload below and the Python
    // span's verbose names (`dataset`, `session_count`, `run_in_background`).
    let session_count = session_ids.as_ref().map(|v| v.len()).unwrap_or(0);
    let span = tracing::info_span!(
        "cognee.api.improve",
        dataset = %dataset_name,
        session_count = session_count,
        run_in_background = false,
    );
    let _enter = span.enter();

    // Mirrors Python `send_telemetry("cognee.improve", ...)` from
    // cognee/api/v1/improve/improve.py:91.
    #[cfg(feature = "telemetry")]
    {
        cognee_telemetry::send_telemetry(
            "cognee.improve",
            owner_id,
            Some(serde_json::json!({
                "dataset": dataset_name.clone(),
                "session_count": session_count,
                "session_ids": session_ids.clone(),
                "run_in_background": false,
                "cognee_version": env!("CARGO_PKG_VERSION"),
            })),
        );
    }

    // The dataset the graph-reading stages (1, 3, 3b) run in. Under per-dataset
    // storage (`ENABLE_BACKEND_ACCESS_CONTROL`) the graph they read and write
    // is that dataset's own, as Python's memify runs in the dataset's database
    // context. Stages 2–2d scope themselves (add, cognify, truth subspace). A
    // dataset that does not exist yet has no graph to weight; stage 2 may
    // create it, so the later stages look it up again.
    let stage1_dataset =
        dataset_id_by_name(db.as_ref(), &dataset_name, owner_id, tenant_id).await?;

    // ---- Stage 1: Apply Feedback Weights ----
    if has_sessions {
        #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
        let sids = session_ids
            .as_ref()
            .expect("has_sessions guarantees session_ids is Some with non-empty vec");
        match (session_store.as_ref(), session_manager.as_ref()) {
            (Some(store), Some(mgr)) => {
                match maybe_with_dataset(
                    stage1_dataset,
                    apply_feedback_weights_pipeline(
                        sids,
                        owner_id,
                        feedback_alpha,
                        &*graph_db,
                        Arc::clone(store),
                        Arc::clone(mgr),
                    ),
                )
                .await
                {
                    Ok(r) => {
                        info!(
                            processed = r.processed,
                            applied = r.applied,
                            skipped = r.skipped,
                            "improve stage 1 (feedback_weights) complete"
                        );
                        result.feedback_entries_processed = r.processed;
                        result.feedback_entries_applied = r.applied;
                        result.stages_run.push("apply_feedback_weights".to_string());
                    }
                    Err(e) => {
                        warn!("improve stage 1 (feedback_weights) failed (non-fatal): {e}");
                    }
                }
            }
            _ => {
                warn!(
                    "improve stage 1: session_store and session_manager are required; skipping feedback_weights"
                );
            }
        }
    }

    // ---- Stage 2: Persist Session Q&A to Graph ----
    if has_sessions {
        #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
        let sids = session_ids
            .as_ref()
            .expect("has_sessions guarantees session_ids is Some with non-empty vec");
        // LIB-06-03: `persist_sessions_in_knowledge_graph` now requires
        // `Arc<dyn RelationalDb>` and `Arc<dyn CpuPool>`.
        let stage2_db = db.clone();
        match (session_store.as_ref(), add_pipeline, stage2_db) {
            (Some(store), Some(pipeline), Some(database)) => {
                let thread_pool: Arc<dyn cognee_core::CpuPool> =
                    match cognee_core::RayonThreadPool::with_default_threads() {
                        Ok(pool) => Arc::new(pool),
                        Err(e) => {
                            warn!(
                                "improve stage 2: failed to construct thread pool: {e}; skipping persist_sessions"
                            );
                            return Ok(result);
                        }
                    };
                let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
                    Arc::clone(&database).pipeline_runs();
                match persist_sessions_in_knowledge_graph(
                    sids,
                    &dataset_name,
                    owner_id,
                    tenant_id,
                    Arc::clone(store),
                    pipeline,
                    Arc::clone(&llm),
                    Arc::clone(&storage),
                    Arc::clone(&graph_db),
                    Arc::clone(&vector_db),
                    Arc::clone(&embedding_engine),
                    database,
                    pipeline_run_repo,
                    thread_pool,
                    Arc::clone(&ontology_resolver),
                    cognify_config,
                )
                .await
                {
                    Ok(r) => {
                        info!(
                            persisted = r.sessions_persisted,
                            skipped = r.sessions_skipped,
                            failed = r.sessions_failed,
                            "improve stage 2 (persist_sessions) complete"
                        );
                        result.sessions_persisted = r.sessions_persisted;
                        result.stages_run.push("persist_sessions".to_string());
                    }
                    Err(e) => {
                        warn!("improve stage 2 (persist_sessions) failed (non-fatal): {e}");
                    }
                }
            }
            _ => {
                warn!(
                    "improve stage 2: session_store, add_pipeline, and the relational database are required; skipping persist_sessions"
                );
            }
        }
    }

    // ---- Stage 2b: Persist Agent Trace Steps ----
    //
    // Mirrors Python's `_persist_session_traces` (improve.py:166-176).
    // Reads `session_feedback` from each trace step and cognifies it into the
    // permanent graph so that the plugin's tool-call activity reaches permanent
    // memory — not just QA entries.
    //
    // Scoped-down 0.1.0 implementation: collects trace `session_feedback` text
    // (the per-step LLM-generated feedback string) and runs it through the
    // add→cognify path with node_set `"agent_trace_feedbacks"`.
    //
    // TODO(parity): Python's `persist_agent_trace_feedbacks_in_knowledge_graph_pipeline`
    // uses per-step metadata (origin_function, status, method_params). The full
    // parity pass should introduce a dedicated `persist_trace_feedbacks_in_knowledge_graph`
    // function in cognee-cognify that preserves per-step provenance.
    if has_sessions {
        #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
        let sids = session_ids
            .as_ref()
            .expect("has_sessions guarantees session_ids is Some with non-empty vec");

        // Collect all trace feedback texts across the sessions.
        let mut trace_texts: Vec<String> = Vec::new();
        if let Some(mgr) = session_manager.as_ref() {
            let user_id_str = owner_id.to_string();
            for sid in sids {
                match mgr
                    .get_agent_trace_session(&user_id_str, Some(sid), None)
                    .await
                {
                    Ok(steps) => {
                        for step in &steps {
                            if !step.session_feedback.is_empty() {
                                trace_texts.push(format!(
                                    "Session: {sid}\nFunction: {}\nStatus: {}\nFeedback: {}",
                                    step.origin_function, step.status, step.session_feedback,
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            session_id = sid,
                            "improve stage 2b: could not read trace steps (non-fatal): {e}"
                        );
                    }
                }
            }
        }

        if !trace_texts.is_empty() {
            let stage2b_db = db.clone();
            let combined_text = trace_texts.join("\n\n");
            match (add_pipeline, stage2b_db) {
                (Some(pipeline), Some(database)) => {
                    match cognee_core::RayonThreadPool::with_default_threads() {
                        Ok(pool) => {
                            let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(pool);
                            let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
                                Arc::clone(&database).pipeline_runs();
                            let add_params = AddParams {
                                node_set: Some(vec!["agent_trace_feedbacks".to_string()]),
                                ..Default::default()
                            };
                            match pipeline
                                .add_with_params(
                                    vec![DataInput::Text(combined_text)],
                                    &dataset_name,
                                    owner_id,
                                    tenant_id,
                                    &add_params,
                                )
                                .await
                                .map_err(|e| e.to_string())
                            {
                                Ok(data_rows) if !data_rows.is_empty() => {
                                    // Resolve dataset_id the same way persist_sessions does.
                                    let dataset_id_opt =
                                        cognee_database::IngestDb::get_dataset_by_name_in_tenant(
                                            &*database,
                                            &dataset_name,
                                            owner_id,
                                            tenant_id,
                                        )
                                        .await
                                        .ok()
                                        .flatten()
                                        .map(|ds| ds.id);
                                    if let Some(dataset_id) = dataset_id_opt {
                                        match cognee_cognify::tasks::cognify(
                                            data_rows,
                                            dataset_id,
                                            Some(owner_id),
                                            None,
                                            tenant_id,
                                            Arc::clone(&llm),
                                            Arc::clone(&storage),
                                            Arc::clone(&graph_db),
                                            Arc::clone(&vector_db),
                                            Arc::clone(&embedding_engine),
                                            Arc::clone(&database),
                                            pipeline_run_repo,
                                            thread_pool,
                                            Arc::clone(&ontology_resolver),
                                            cognify_config,
                                        )
                                        .await
                                        {
                                            Ok(_) => {
                                                info!(
                                                    trace_items = trace_texts.len(),
                                                    "improve stage 2b (persist_trace_steps) complete"
                                                );
                                            }
                                            Err(e) => {
                                                warn!(
                                                    "improve stage 2b: cognify of trace steps failed (non-fatal): {e}"
                                                );
                                            }
                                        }
                                    } else {
                                        warn!(
                                            "improve stage 2b: dataset lookup returned None; trace steps not cognified"
                                        );
                                    }
                                }
                                Ok(_) => {
                                    warn!(
                                        "improve stage 2b: add returned no rows; trace steps not cognified"
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        "improve stage 2b: add of trace text failed (non-fatal): {e}"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            warn!("improve stage 2b: rayon pool init failed (non-fatal): {e}");
                        }
                    }
                }
                _ => {
                    warn!(
                        "improve stage 2b: add_pipeline and the relational database are required; trace steps not cognified"
                    );
                }
            }
        }
        // Always push the stage name so stages_run stays consistent with Python,
        // even when no traces were present or cognification was skipped/failed.
        result.stages_run.push("persist_trace_steps".to_string());
    }

    // ---- Stage 2c: Distill Sessions into Learnings ----
    //
    // Mirrors Python's `_distill_sessions` (improve.py:190-201). Each session's
    // Q&A is curated into durable, entity-anchored lesson documents tagged with
    // the `session_learnings` node-set and cognified into the graph. Fail-open
    // per session; a session with no non-empty Q&A yields nothing.
    //
    // Scope cut vs. Python (locked): only Q&A is consumed — there is no
    // `SessionContextEntry` gated-guidance corpus in Rust — and Python's stage
    // 2b2 (`_extract_agent_context`) is intentionally NOT ported.
    //
    // Unlike Stage 2b, the stage name is pushed onto `stages_run` ONLY when a
    // lesson was actually published (Python improve.py:200-201).
    if has_sessions {
        #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
        let sids = session_ids
            .as_ref()
            .expect("has_sessions guarantees session_ids is Some with non-empty vec");
        let stage2c_db = db.clone();
        match (session_store.as_ref(), add_pipeline, stage2c_db) {
            (Some(store), Some(pipeline), Some(database)) => {
                match cognee_core::RayonThreadPool::with_default_threads() {
                    Ok(pool) => {
                        let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(pool);
                        let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
                            Arc::clone(&database).pipeline_runs();
                        let r = distill_sessions_in_knowledge_graph(
                            sids,
                            &dataset_name,
                            owner_id,
                            tenant_id,
                            Arc::clone(store),
                            pipeline,
                            Arc::clone(&llm),
                            Arc::clone(&storage),
                            Arc::clone(&graph_db),
                            Arc::clone(&vector_db),
                            Arc::clone(&embedding_engine),
                            database,
                            pipeline_run_repo,
                            thread_pool,
                            Arc::clone(&ontology_resolver),
                            cognify_config,
                        )
                        .await;
                        info!(
                            sessions_distilled = r.sessions_distilled,
                            lessons_published = r.lessons_published,
                            "improve stage 2c (distill_sessions) complete"
                        );
                        result.sessions_distilled = r.sessions_distilled;
                        result.lessons_published = r.lessons_published;
                        // Conditional push: only when a lesson was published.
                        if r.sessions_distilled > 0 {
                            result.stages_run.push("distill_sessions".to_string());
                        }
                    }
                    Err(e) => {
                        warn!(
                            "improve stage 2c: rayon pool init failed (non-fatal): {e}; skipping distill_sessions"
                        );
                    }
                }
            }
            _ => {
                warn!(
                    "improve stage 2c: session_store, add_pipeline, and the relational database are required; skipping distill_sessions"
                );
            }
        }
    }

    // ---- Stage 2d: Build Truth Subspace (opt-in) ----
    //
    // Phase-2 join point. Mirrors Python's `build_truth_subspace` stage
    // (improve.py:207-221). Gated on `has_sessions && build_truth_subspace`, so
    // it is fully inert (zero graph/vector I/O) unless the caller opts in. Never
    // aborts improve() — `build_truth_subspace` is infallible and internal
    // failures merely warn.
    if has_sessions && build_truth_subspace {
        #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
        let sids = session_ids
            .as_ref()
            .expect("has_sessions guarantees session_ids is Some with non-empty vec");
        if let Some(database) = db.as_ref() {
            // Resolve the dataset the same way Stage 4 does.
            match cognee_database::IngestDb::get_dataset_by_name_in_tenant(
                &**database,
                &dataset_name,
                owner_id,
                tenant_id,
            )
            .await
            {
                Ok(Some(ds)) => {
                    let ts = cognee_cognify::build_truth_subspace(
                        ds.id,
                        sids,
                        Arc::clone(&graph_db),
                        Arc::clone(&vector_db),
                        Arc::clone(&embedding_engine),
                        cognee_cognify::DEFAULT_K,
                    )
                    .await;
                    info!(
                        anchors = ts.anchors,
                        nodes_scored = ts.nodes_scored,
                        truth_epoch = ts.truth_epoch,
                        "improve stage 2d (build_truth_subspace) complete"
                    );
                    result.stages_run.push("build_truth_subspace".to_string());
                }
                Ok(None) => warn!("improve stage 2d: dataset not found; skipping"),
                Err(e) => {
                    warn!("improve stage 2d: dataset lookup failed (non-fatal): {e}")
                }
            }
        } else {
            warn!("improve stage 2d: relational database required; skipping build_truth_subspace");
        }
    }

    // ---- Stage 3: Default Enrichment (always) ----
    let later_dataset = dataset_id_by_name(db.as_ref(), &dataset_name, owner_id, tenant_id).await?;
    let memify_config = if let Some(names) = node_name {
        MemifyConfig::default().with_node_name_filter(names)
    } else {
        MemifyConfig::default()
    };
    match db.as_ref() {
        Some(database) => match cognee_core::RayonThreadPool::with_default_threads() {
            Ok(pool) => {
                let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(pool);
                let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
                    Arc::clone(database).pipeline_runs();
                match maybe_with_dataset(
                    later_dataset,
                    run_memify(
                        Arc::clone(&graph_db),
                        Arc::clone(&vector_db),
                        Arc::clone(&embedding_engine),
                        thread_pool,
                        Arc::clone(database),
                        pipeline_run_repo,
                        None,
                        Some(owner_id),
                        tenant_id,
                        &memify_config,
                    ),
                )
                .await
                {
                    Ok(mr) => {
                        info!(
                            triplets = mr.triplet_count,
                            "improve stage 3 (memify) complete"
                        );
                        result.memify_result = Some(mr);
                        result.stages_run.push("memify".to_string());
                    }
                    Err(e) => {
                        warn!("improve stage 3 (memify) failed (non-fatal): {e}");
                    }
                }
            }
            Err(e) => {
                warn!("improve stage 3 (memify) failed (non-fatal): rayon pool init: {e}");
            }
        },
        None => {
            warn!(
                "improve stage 3: a relational database connection is required by the LIB-06 \
                 executor-routed memify; skipping memify"
            );
        }
    }

    // ---- Stage 3b: Global Context Index (opt-in) ----
    //
    // Mirrors Python's `_build_global_context_index` (improve.py:201-213).
    // When `build_global_context_index` is `true` and not running in background:
    // build a graph summary and store it in the session graph-context so the
    // search side can prepend it as background knowledge.
    //
    // Partial 0.1.0 implementation: retrieves graph summaries already stored as
    // TextSummary nodes and concatenates them as the global context. Python's full
    // implementation (`global_context_index_pipeline`) also builds bucket and root
    // summaries via an LLM pass.
    // TODO(parity): implement bucket/root summary indexing via a dedicated
    // `global_context_index_pipeline` function in cognee-cognify that mirrors
    // Python's `bucketing_strategy="graph"` / `max_bucket_size=4` pass.
    if build_global_context_index {
        if run_in_background {
            warn!(
                "improve stage 3b: global context index skipped in background mode \
                 because ordered background pipeline chaining is not supported"
            );
        } else if let Some(sm) = session_manager.as_ref() {
            // Partial 0.1.0: read all graph edges via `get_graph_data()` and format
            // as "source_id → relationship → target_id" lines, then store as the
            // global context so any session can prepend it as background knowledge.
            // TODO(parity): replace with a full `global_context_index_pipeline` that
            // uses LLM bucket/root summarisation (`bucketing_strategy="graph"`,
            // `max_bucket_size=4`) matching Python's `_build_global_context_index`.
            match maybe_with_dataset(later_dataset, graph_db.get_graph_data()).await {
                Ok((_nodes, edges)) if !edges.is_empty() => {
                    let global_context = edges
                        .iter()
                        .map(|(src, tgt, rel, _props)| format!("{src} → {rel} → {tgt}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let user_id_str = owner_id.to_string();
                    // Store under a synthetic global-context key so any session can read it.
                    let global_session_key = "_global_context_index";
                    match sm
                        .set_graph_context(
                            Some(global_session_key),
                            Some(&user_id_str),
                            &global_context,
                        )
                        .await
                    {
                        Ok(()) => {
                            info!(
                                edges = edges.len(),
                                "improve stage 3b (global_context_index) complete"
                            );
                            result.stages_run.push("global_context_index".to_string());
                        }
                        Err(e) => {
                            warn!(
                                "improve stage 3b: failed to store global context (non-fatal): {e}"
                            );
                        }
                    }
                }
                Ok(_) => {
                    info!("improve stage 3b: graph has no edges; skipping global_context_index");
                }
                Err(e) => {
                    warn!("improve stage 3b: failed to load graph data (non-fatal): {e}");
                }
            }
        } else {
            warn!("improve stage 3b: session_manager is required; skipping global_context_index");
        }
    }

    // ---- Stage 4: Sync Graph to Session Cache ----
    //
    // Stage 4 always runs when sessions are present (background dispatch is host-side).
    if has_sessions {
        #[allow(clippy::expect_used, reason = "invariant is upheld by construction")]
        let sids = session_ids
            .as_ref()
            .expect("has_sessions guarantees session_ids is Some with non-empty vec");
        match (
            db.as_ref(),
            session_manager.as_ref(),
            checkpoint_store.as_ref(),
        ) {
            (Some(dbc), Some(mgr), Some(ckstore)) => {
                // Stage 4 requires a dataset UUID. Resolve from the name.
                let dataset_id_opt = cognee_database::IngestDb::get_dataset_by_name_in_tenant(
                    &**dbc,
                    &dataset_name,
                    owner_id,
                    tenant_id,
                )
                .await
                .ok()
                .flatten()
                .map(|ds| ds.id);
                let Some(dataset_id) = dataset_id_opt else {
                    warn!(
                        dataset_name = %dataset_name,
                        "improve stage 4: dataset not found; skipping sync_graph_to_session"
                    );
                    return Ok(result);
                };

                let user_id_str = owner_id.to_string();
                let mut total_synced = 0usize;
                let mut any_ran = false;
                for sid in sids {
                    match sync_graph_to_session(
                        &user_id_str,
                        sid,
                        dataset_id,
                        dbc.as_ref(),
                        mgr.as_ref(),
                        ckstore.as_ref(),
                        DEFAULT_MAX_LINES,
                    )
                    .await
                    {
                        Ok(r) => {
                            info!(
                                session_id = sid,
                                synced = r.synced,
                                total = r.total,
                                "improve stage 4: session synced"
                            );
                            total_synced += r.synced;
                            any_ran = true;
                        }
                        Err(e) => {
                            warn!(
                                session_id = sid,
                                "improve stage 4 failed for session (non-fatal): {e}"
                            );
                        }
                    }
                }
                result.edges_synced = total_synced;
                if any_ran {
                    result.stages_run.push("sync_graph_to_session".to_string());
                }
            }
            _ => {
                warn!(
                    "improve stage 4: db, session_manager, and checkpoint_store are required; skipping sync_graph_to_session"
                );
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn improve_result_default_fields() {
        let result = ImproveResult::default();
        assert!(result.stages_run.is_empty());
        assert!(result.memify_result.is_none());
        assert_eq!(result.feedback_entries_processed, 0);
        assert_eq!(result.feedback_entries_applied, 0);
        assert_eq!(result.sessions_persisted, 0);
        assert_eq!(result.sessions_distilled, 0);
        assert_eq!(result.lessons_published, 0);
        assert_eq!(result.edges_synced, 0);
    }
}
