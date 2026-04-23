//! Bidirectional session-graph bridge -- `improve()`.
//!
//! Four-stage pipeline:
//! 1. Apply feedback weights from session Q&A entries to graph nodes/edges.
//! 2. Extract feedback knowledge: run cognify on session Q&A text.
//! 3. Default enrichment: reuse existing memify() for triplet embeddings.
//! 4. Sync graph context to session store.
//!
//! Equivalent to Python's `cognee.api.v1.improve.improve()`.

use std::sync::Arc;

use cognee_cognify::{CognifyConfig, MemifyConfig, MemifyResult, run_memify};
use cognee_database::DatabaseConnection;
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::Llm;
use cognee_session::SessionStore;
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;
use tracing::{info, warn};
use uuid::Uuid;

use super::error::ApiError;

/// Result of an `improve()` operation.
#[derive(Debug, Clone)]
pub struct ImproveResult {
    /// Names of stages that were executed.
    pub stages_run: Vec<String>,
    /// Result of the memify (triplet embedding) stage.
    pub memify_result: Option<MemifyResult>,
    /// Number of feedback entries processed in stage 1.
    pub feedback_entries_processed: usize,
    /// Number of session Q&A texts persisted to graph in stage 2.
    pub sessions_persisted: usize,
    /// Number of edges synced to session cache in stage 4.
    pub edges_synced: usize,
}

/// Bidirectional session-graph bridge.
///
/// # Stage 1: Apply Feedback Weights (only when `session_ids` provided)
/// Reads session Q&A entries with feedback scores and updates the
/// `feedback_weight` property on referenced graph nodes/edges.
///
/// # Stage 2: Persist Session Q&A to Graph (only when `session_ids` provided)
/// Runs cognify on session Q&A text to extract entities/relationships.
/// **Currently a stub** -- logs the intent but does not perform LLM extraction.
///
/// # Stage 3: Default Enrichment (always runs)
/// Calls existing `memify()` to extract triplets and index them in vector DB.
///
/// # Stage 4: Sync Graph to Session Cache (only when `session_ids` provided)
/// Stores a summary of graph edges in the session store as graph context.
/// **Currently a stub** -- logs the intent but does not perform the sync.
#[allow(clippy::too_many_arguments)]
pub async fn improve(
    _dataset_name: &str,
    session_ids: Option<Vec<String>>,
    node_name: Option<Vec<String>>,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    feedback_alpha: f64,
    _llm: Arc<dyn Llm>,
    _storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    _db: Option<Arc<DatabaseConnection>>,
    session_store: Option<Arc<dyn SessionStore>>,
    _cognify_config: &CognifyConfig,
) -> Result<ImproveResult, ApiError> {
    let mut result = ImproveResult {
        stages_run: Vec::new(),
        memify_result: None,
        feedback_entries_processed: 0,
        sessions_persisted: 0,
        edges_synced: 0,
    };

    let has_sessions = session_ids.as_ref().is_some_and(|ids| !ids.is_empty());

    // ---- Stage 1: Apply Feedback Weights ----
    if has_sessions {
        let session_ids = session_ids
            .as_ref()
            .expect("has_sessions is true only when session_ids is Some with non-empty vec");
        result.feedback_entries_processed = stage1_apply_feedback_weights(
            session_ids,
            owner_id,
            feedback_alpha,
            &*graph_db,
            session_store.as_deref(),
        )
        .await?;
        result.stages_run.push("apply_feedback_weights".to_string());
    }

    // ---- Stage 2: Persist Session Q&A to Graph ----
    if has_sessions {
        let session_ids = session_ids
            .as_ref()
            .expect("has_sessions is true only when session_ids is Some with non-empty vec");
        result.sessions_persisted =
            stage2_persist_sessions(session_ids, session_store.as_deref()).await?;
        result.stages_run.push("persist_sessions".to_string());
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
                "improve stage 3: memify complete"
            );
            result.memify_result = Some(mr);
        }
        Err(e) => {
            warn!("improve stage 3 (memify) failed: {e}");
            return Err(ApiError::Memify(e.to_string()));
        }
    }
    result.stages_run.push("memify".to_string());

    // ---- Stage 4: Sync Graph to Session Cache ----
    if has_sessions {
        let session_ids = session_ids
            .as_ref()
            .expect("has_sessions is true only when session_ids is Some with non-empty vec");
        result.edges_synced =
            stage4_sync_graph_to_session(session_ids, session_store.as_deref()).await?;
        result.stages_run.push("sync_graph_to_session".to_string());
    }

    Ok(result)
}

/// Stage 1: Read session Q&A entries and apply feedback weights to graph elements.
async fn stage1_apply_feedback_weights(
    session_ids: &[String],
    owner_id: Uuid,
    feedback_alpha: f64,
    graph_db: &dyn GraphDBTrait,
    session_store: Option<&dyn SessionStore>,
) -> Result<usize, ApiError> {
    let store = match session_store {
        Some(s) => s,
        None => {
            warn!("improve stage 1: no session_store provided; skipping feedback weights");
            return Ok(0);
        }
    };

    let user_id_str = owner_id.to_string();
    let mut processed = 0;

    for session_id in session_ids {
        let entries = store
            .get_all_qa_entries(session_id, Some(&user_id_str))
            .await?;

        for entry in &entries {
            // Check if this entry has feedback information in context.
            // The context field may contain JSON with feedback_score and
            // used_graph_element_ids. This is a best-effort parse.
            if let Some(ctx) = &entry.context
                && let Ok(ctx_val) = serde_json::from_str::<serde_json::Value>(ctx)
            {
                let feedback_score = ctx_val.get("feedback_score").and_then(|v| v.as_f64());
                let element_ids = ctx_val
                    .get("used_graph_element_ids")
                    .and_then(|v| v.as_array());

                if let (Some(score), Some(ids)) = (feedback_score, element_ids) {
                    let weight_delta = feedback_alpha * score;
                    for id_val in ids {
                        if let Some(node_id) = id_val.as_str() {
                            // Try to update the node property.
                            if let Err(e) = graph_db
                                .update_node_property(
                                    node_id,
                                    "feedback_weight",
                                    serde_json::json!(weight_delta),
                                )
                                .await
                            {
                                warn!(
                                    node_id = node_id,
                                    "Failed to update feedback_weight on node: {e}"
                                );
                            }
                        }
                    }
                    processed += 1;
                }
            }
        }
    }

    info!(
        processed = processed,
        "improve stage 1: feedback weights applied"
    );
    Ok(processed)
}

/// Stage 2: Persist session Q&A text to the knowledge graph.
///
/// **Stub implementation** -- logs the operation but does not yet perform
/// LLM-based entity extraction on session text. Full implementation would
/// call `cognify()` on the concatenated Q&A text with a `node_set` tag.
async fn stage2_persist_sessions(
    session_ids: &[String],
    session_store: Option<&dyn SessionStore>,
) -> Result<usize, ApiError> {
    let _store = match session_store {
        Some(s) => s,
        None => {
            warn!("improve stage 2: no session_store provided; skipping session persistence");
            return Ok(0);
        }
    };

    // TODO: For each session, load Q&A entries, concatenate text, and run
    // cognify() with node_set="user_sessions_from_cache". This requires
    // wiring up the full cognify pipeline with appropriate parameters.
    info!(
        session_count = session_ids.len(),
        "improve stage 2: session persistence not yet implemented (stub)"
    );

    Ok(0)
}

/// Stage 4: Sync graph context to session cache.
///
/// **Stub implementation** -- logs the operation but does not yet read
/// graph edges or store them as session graph context. Full implementation
/// would read recent edges from the graph DB and store structured summaries
/// in each session via `SessionStore`.
async fn stage4_sync_graph_to_session(
    session_ids: &[String],
    session_store: Option<&dyn SessionStore>,
) -> Result<usize, ApiError> {
    let _store = match session_store {
        Some(s) => s,
        None => {
            warn!("improve stage 4: no session_store provided; skipping graph sync");
            return Ok(0);
        }
    };

    // TODO: Read recent edges from graph DB, format as structured summaries,
    // and store in each session's graph context. This requires:
    // - GraphDBTrait method to get edges since a timestamp or checkpoint
    // - SessionStore method to set_graph_context()
    info!(
        session_count = session_ids.len(),
        "improve stage 4: graph-to-session sync not yet implemented (stub)"
    );

    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn improve_result_default_fields() {
        let result = ImproveResult {
            stages_run: vec![],
            memify_result: None,
            feedback_entries_processed: 0,
            sessions_persisted: 0,
            edges_synced: 0,
        };
        assert!(result.stages_run.is_empty());
        assert!(result.memify_result.is_none());
    }
}
