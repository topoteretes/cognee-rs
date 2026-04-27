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

/// Bidirectional session-graph bridge.
///
/// Background dispatch is a host-side concern — this function is strictly
/// synchronous. Stage 4 always runs when sessions are present.
#[allow(clippy::too_many_arguments)]
pub async fn improve(
    dataset_name: &str,
    session_ids: Option<Vec<String>>,
    node_name: Option<Vec<String>>,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    feedback_alpha: f64,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    db: Option<Arc<DatabaseConnection>>,
    session_store: Option<Arc<dyn SessionStore>>,
    session_manager: Option<Arc<SessionManager>>,
    add_pipeline: Option<&AddPipeline>,
    checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    cognify_config: &CognifyConfig,
) -> Result<ImproveResult, ApiError> {
    let mut result = ImproveResult::default();
    let has_sessions = session_ids.as_ref().is_some_and(|ids| !ids.is_empty());

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
        match (session_store.as_ref(), add_pipeline) {
            (Some(store), Some(pipeline)) => {
                match persist_sessions_in_knowledge_graph(
                    sids,
                    dataset_name,
                    owner_id,
                    tenant_id,
                    Arc::clone(store),
                    pipeline,
                    Arc::clone(&llm),
                    Arc::clone(&storage),
                    Arc::clone(&graph_db),
                    Arc::clone(&vector_db),
                    Arc::clone(&embedding_engine),
                    db.clone(),
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
                    "improve stage 2: session_store and add_pipeline are required; skipping persist_sessions"
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
    match run_memify(
        &*graph_db,
        &*vector_db,
        &*embedding_engine,
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
                    dataset_name,
                    owner_id,
                    tenant_id,
                )
                .await
                .ok()
                .flatten()
                .map(|ds| ds.id);
                let Some(dataset_id) = dataset_id_opt else {
                    warn!(
                        dataset_name = dataset_name,
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
