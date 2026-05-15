//! Stage 2 of `improve()` — persist session Q&A text into the permanent
//! knowledge graph.
//!
//! Ported from:
//! - `/tmp/cognee-python/cognee/tasks/memify/extract_user_sessions.py`
//! - `/tmp/cognee-python/cognee/tasks/memify/cognify_session.py`
//!
//! For each session ID:
//! 1. Load all Q&A entries.
//! 2. Concatenate into a single string matching the Python format
//!    (`"Session ID: <sid>\n\nQuestion: <q>\n\nAnswer: <a>\n\n"`).
//! 3. Run `AddPipeline::add_with_params(...)` with
//!    `node_set = ["user_sessions_from_cache"]`.
//! 4. Run `cognify(...)` on the resulting data rows to extract entities and
//!    relationships.
//!
//! Empty sessions are silently skipped; individual session failures do not
//! abort the loop (matches Python's try/except per-session).

use std::sync::Arc;

use cognee_core::CpuPool;
use cognee_database::DatabaseConnection;
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_ingestion::{AddParams, AddPipeline};
use cognee_llm::Llm;
use cognee_models::DataInput;
use cognee_ontology::OntologyResolver;
use cognee_session::SessionStore;
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;
use thiserror::Error;
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::CognifyConfig;
use crate::error::CognifyError;
use crate::tasks::cognify;

/// Node-set tag attached to session-derived data; matches Python
/// `cognify_session.py:32`.
pub const USER_SESSIONS_NODE_SET: &str = "user_sessions_from_cache";

/// Error type for Stage 2 (`persist_sessions_in_knowledge_graph`).
#[derive(Debug, Error)]
pub enum PersistSessionsError {
    #[error("Session error: {0}")]
    Session(#[from] cognee_session::SessionError),

    #[error("Ingestion error: {0}")]
    Ingestion(String),

    #[error("Cognify error: {0}")]
    Cognify(#[from] CognifyError),
}

/// Summary of a Stage 2 run.
#[derive(Debug, Clone, Default)]
pub struct PersistSessionsResult {
    /// Number of sessions whose text was successfully persisted to the graph.
    pub sessions_persisted: usize,
    /// Number of sessions that were skipped (empty).
    pub sessions_skipped: usize,
    /// Number of sessions that failed to persist (non-fatal; logged).
    pub sessions_failed: usize,
}

/// Concatenate all Q&A entries of a session into a single string.
///
/// Matches Python `extract_user_sessions.py:62-67`:
/// ```text
/// Session ID: {sid}
///
/// Question: {q}
///
/// Answer: {a}
///
/// Question: {q2}
///
/// Answer: {a2}
///
/// ```
fn concat_session_entries(session_id: &str, entries: &[cognee_session::SessionQAEntry]) -> String {
    let mut buf = format!("Session ID: {session_id}\n\n");
    for e in entries {
        buf.push_str(&format!(
            "Question: {}\n\nAnswer: {}\n\n",
            e.question, e.answer
        ));
    }
    buf
}

/// Persist session Q&A text into the permanent graph.
#[allow(clippy::too_many_arguments)]
pub async fn persist_sessions_in_knowledge_graph(
    session_ids: &[String],
    dataset_name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    session_store: Arc<dyn SessionStore>,
    add_pipeline: &AddPipeline,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    database: Arc<DatabaseConnection>,
    thread_pool: Arc<dyn CpuPool>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    cognify_config: &CognifyConfig,
) -> Result<PersistSessionsResult, PersistSessionsError> {
    let user_id_str = owner_id.to_string();
    let mut result = PersistSessionsResult::default();

    for sid in session_ids {
        let entries = session_store
            .get_all_qa_entries(sid, Some(&user_id_str))
            .await?;
        if entries.is_empty() {
            info!(
                session_id = sid,
                "persist_sessions: empty session, skipping"
            );
            result.sessions_skipped += 1;
            continue;
        }

        let buf = concat_session_entries(sid, &entries);
        if buf.trim().is_empty() {
            result.sessions_skipped += 1;
            continue;
        }

        let params = AddParams {
            node_set: Some(vec![USER_SESSIONS_NODE_SET.to_string()]),
            ..Default::default()
        };

        let add_result = match add_pipeline
            .add_with_params(
                vec![DataInput::Text(buf)],
                dataset_name,
                owner_id,
                tenant_id,
                &params,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                warn!(session_id = sid, "persist_sessions: add failed: {e}");
                result.sessions_failed += 1;
                continue;
            }
        };

        if add_result.is_empty() {
            warn!(session_id = sid, "persist_sessions: add returned no rows");
            result.sessions_failed += 1;
            continue;
        }

        // Each Data row in add_result belongs to exactly one dataset; use the
        // first row's dataset lookup by querying via name. AddPipeline does
        // not currently return the dataset_id directly, so we derive it from
        // the node_set-tagged Data entries through the same helper that
        // cognify_dataset_refs uses: look up by name/owner.
        let dataset_id = match cognee_database::ops::datasets::get_dataset_by_name(
            database.as_ref(),
            dataset_name,
            owner_id,
            tenant_id,
        )
        .await
        {
            Ok(Some(ds)) => ds.id,
            Ok(None) => {
                warn!(
                    session_id = sid,
                    dataset_name = dataset_name,
                    "persist_sessions: dataset lookup returned None"
                );
                result.sessions_failed += 1;
                continue;
            }
            Err(e) => {
                warn!(
                    session_id = sid,
                    "persist_sessions: dataset lookup failed: {e}"
                );
                result.sessions_failed += 1;
                continue;
            }
        };

        match cognify(
            add_result,
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
            Arc::clone(&thread_pool),
            Arc::clone(&ontology_resolver),
            cognify_config,
        )
        .await
        {
            Ok(_) => {
                info!(session_id = sid, "persist_sessions: session persisted");
                result.sessions_persisted += 1;
            }
            Err(e) => {
                warn!(
                    session_id = sid,
                    "persist_sessions: cognify failed (non-fatal): {e}"
                );
                result.sessions_failed += 1;
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cognee_session::SessionQAEntry;
    use uuid::Uuid;

    fn mk_entry(q: &str, a: &str) -> SessionQAEntry {
        SessionQAEntry {
            id: Uuid::new_v4(),
            session_id: "s1".into(),
            user_id: None,
            question: q.into(),
            answer: a.into(),
            context: None,
            created_at: chrono::Utc::now(),
            feedback_text: None,
            feedback_score: None,
            used_graph_element_ids: None,
            memify_metadata: None,
        }
    }

    #[test]
    fn concat_format_matches_python() {
        let entries = vec![mk_entry("q1", "a1"), mk_entry("q2", "a2")];
        let out = concat_session_entries("sid-1", &entries);
        let expected =
            "Session ID: sid-1\n\nQuestion: q1\n\nAnswer: a1\n\nQuestion: q2\n\nAnswer: a2\n\n";
        assert_eq!(out, expected);
    }

    #[test]
    fn concat_empty_entries() {
        let out = concat_session_entries("sid-empty", &[]);
        assert_eq!(out, "Session ID: sid-empty\n\n");
    }
}
