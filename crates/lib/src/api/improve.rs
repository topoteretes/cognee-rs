//! Bidirectional session-graph bridge — `improve()`.
//!
//! Four-stage pipeline matching Python `cognee.api.v1.improve.improve()`:
//! 1. Apply feedback weights from session Q&A entries to graph nodes/edges.
//! 2. Persist session Q&A text into the permanent knowledge graph.
//! 3. Default enrichment: reuse `memify()` for triplet embeddings.
//! 4. Sync recent graph edges into the session's `graph_context`.
//!
//! Each stage is wrapped in a warning-only handler so that a failure in one
//! stage does not abort subsequent stages (matches Python semantics).

use std::sync::Arc;

use cognee_cognify::memify::sync_graph_session::DEFAULT_MAX_LINES;
use cognee_cognify::{
    CognifyConfig, MemifyConfig, MemifyResult, apply_feedback_weights_pipeline,
    persist_sessions_in_knowledge_graph, run_memify, sync_graph_to_session,
};
use cognee_database::{CheckpointStore, DatabaseConnection};
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_ingestion::AddPipeline;
use cognee_llm::Llm;
use cognee_ontology::OntologyResolver;
use cognee_session::{SessionManager, SessionStore};
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
    pub db: Option<Arc<DatabaseConnection>>,
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
        // E-05 v2 power-user fields — currently informational; the orchestrator
        // does not yet branch on them. Accepting them here keeps the struct
        // shape Python-parity-aligned for HTTP plumbing.
        extraction_tasks: _extraction_tasks,
        enrichment_tasks: _enrichment_tasks,
        data: _data,
    } = params;

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

    // ---- Stage 1: Apply Feedback Weights ----
    if has_sessions {
        let sids = session_ids
            .as_ref()
            .expect("has_sessions guarantees session_ids is Some with non-empty vec");
        match (session_store.as_ref(), session_manager.as_ref()) {
            (Some(store), Some(mgr)) => {
                match apply_feedback_weights_pipeline(
                    sids,
                    owner_id,
                    feedback_alpha,
                    &*graph_db,
                    Arc::clone(store),
                    Arc::clone(mgr),
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
        let sids = session_ids
            .as_ref()
            .expect("has_sessions guarantees session_ids is Some with non-empty vec");
        // LIB-06-03: `persist_sessions_in_knowledge_graph` now requires
        // `Arc<DatabaseConnection>` and `Arc<dyn CpuPool>`.
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
                    "improve stage 2: session_store, add_pipeline, and DatabaseConnection are required; skipping persist_sessions"
                );
            }
        }
    }

    // ---- Stage 3: Default Enrichment (always) ----
    let memify_config = if let Some(names) = node_name {
        MemifyConfig::default().with_node_name_filter(names)
    } else {
        MemifyConfig::default()
    };
    match db.as_ref() {
        Some(database) => match cognee_core::RayonThreadPool::with_default_threads() {
            Ok(pool) => {
                let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(pool);
                match run_memify(
                    Arc::clone(&graph_db),
                    Arc::clone(&vector_db),
                    Arc::clone(&embedding_engine),
                    thread_pool,
                    Arc::clone(database),
                    None,
                    Some(owner_id),
                    tenant_id,
                    &memify_config,
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

    // ---- Stage 4: Sync Graph to Session Cache ----
    //
    // Stage 4 always runs when sessions are present (background dispatch is host-side).
    if has_sessions {
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
                let dataset_id_opt = cognee_database::ops::datasets::get_dataset_by_name(
                    dbc.as_ref(),
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
        assert_eq!(result.edges_synced, 0);
    }
}
