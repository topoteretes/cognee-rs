//! One-call add + cognify + optional improve -- `remember()`.
//!
//! Composition of `add()` -> `cognify()` -> optionally `improve()` (via `memify`),
//! with session-mode support.
//!
//! Equivalent to Python's `cognee.api.v1.remember.remember()`.

use std::sync::Arc;
use std::time::Instant;

use cognee_cognify::cognify;
use cognee_cognify::{CognifyConfig, CognifyResult, MemifyConfig, MemifyResult, run_memify};
use cognee_database::DatabaseConnection;
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_ingestion::AddPipeline;
use cognee_llm::Llm;
use cognee_models::DataInput;
use cognee_ontology::OntologyResolver;
use cognee_session::SessionStore;
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;
use serde::{Deserialize, Serialize};
use tracing::info;
use uuid::Uuid;

use super::error::ApiError;

/// Status of a remember operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RememberStatus {
    Completed,
    Errored,
    SessionStored,
}

/// Per-item information in the remember result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RememberItemInfo {
    pub id: Option<Uuid>,
    pub name: Option<String>,
    pub content_hash: Option<String>,
    pub mime_type: Option<String>,
}

/// Result of a `remember()` call.
#[derive(Debug, Clone)]
pub struct RememberResult {
    pub status: RememberStatus,
    pub dataset_name: String,
    pub dataset_id: Option<Uuid>,
    pub session_ids: Option<Vec<String>>,
    pub elapsed_seconds: f64,
    pub items_processed: usize,
    pub items: Vec<RememberItemInfo>,
    pub cognify_result: Option<CognifyResult>,
    pub memify_result: Option<MemifyResult>,
    pub error: Option<String>,
}

/// One-call add + cognify + optional improve.
///
/// **Permanent Memory Mode** (no `session_id`):
/// 1. `add()` to ingest data
/// 2. `cognify()` to extract knowledge graph
/// 3. If `self_improvement=true`, `memify()` to enrich with triplet embeddings
///
/// **Session Memory Mode** (with `session_id`):
/// 1. Convert data inputs to text
/// 2. Store in session cache as Q&A entry
/// 3. If `self_improvement=true`, run `memify()` for enrichment
#[allow(clippy::too_many_arguments)]
pub async fn remember(
    data: Vec<DataInput>,
    dataset_name: &str,
    session_id: Option<&str>,
    self_improvement: bool,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    add_pipeline: &AddPipeline,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    db: Option<Arc<DatabaseConnection>>,
    session_store: Option<Arc<dyn SessionStore>>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    cognify_config: &CognifyConfig,
) -> Result<RememberResult, ApiError> {
    let start = Instant::now();

    // -- Session Memory Mode --
    if let Some(sid) = session_id {
        return remember_session(
            &data,
            dataset_name,
            sid,
            self_improvement,
            owner_id,
            &*graph_db,
            &*vector_db,
            &*embedding_engine,
            session_store.as_deref(),
            start,
        )
        .await;
    }

    // -- Permanent Memory Mode --
    let data_items = add_pipeline
        .add(data, dataset_name, owner_id, tenant_id)
        .await
        .map_err(|e| ApiError::Ingestion(e.to_string()))?;

    let items: Vec<RememberItemInfo> = data_items
        .iter()
        .map(|d| RememberItemInfo {
            id: Some(d.id),
            name: Some(d.name.clone()),
            content_hash: Some(d.content_hash.clone()),
            mime_type: Some(d.mime_type.clone()),
        })
        .collect();

    let dataset_id = cognee_ingestion::generate_dataset_id(dataset_name, owner_id, tenant_id);

    // Cognify.
    let cognify_result = cognify(
        data_items,
        dataset_id,
        Some(owner_id),
        tenant_id,
        llm,
        storage,
        Arc::clone(&graph_db),
        Arc::clone(&vector_db),
        Arc::clone(&embedding_engine),
        db,
        ontology_resolver,
        cognify_config,
    )
    .await
    .map_err(|e| ApiError::Cognify(e.to_string()))?;

    // Optional self-improvement via memify.
    let memify_result = if self_improvement {
        let config = MemifyConfig::default();
        match run_memify(
            &*graph_db,
            &*vector_db,
            &*embedding_engine,
            Some(dataset_id),
            Some(owner_id),
            tenant_id,
            &config,
        )
        .await
        {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!("memify phase failed (non-fatal): {e}");
                None
            }
        }
    } else {
        None
    };

    let elapsed = start.elapsed().as_secs_f64();

    Ok(RememberResult {
        status: RememberStatus::Completed,
        dataset_name: dataset_name.to_string(),
        dataset_id: Some(dataset_id),
        session_ids: None,
        elapsed_seconds: elapsed,
        items_processed: items.len(),
        items,
        cognify_result: Some(cognify_result),
        memify_result,
        error: None,
    })
}

/// Session-mode remember: store data as Q&A text in the session cache.
#[allow(clippy::too_many_arguments)]
async fn remember_session(
    data: &[DataInput],
    dataset_name: &str,
    session_id: &str,
    self_improvement: bool,
    owner_id: Uuid,
    graph_db: &dyn GraphDBTrait,
    vector_db: &dyn VectorDB,
    embedding_engine: &dyn EmbeddingEngine,
    session_store: Option<&dyn SessionStore>,
    start: Instant,
) -> Result<RememberResult, ApiError> {
    let store = session_store.ok_or_else(|| {
        ApiError::InvalidArgument(
            "session_id provided but no session_store is available".to_string(),
        )
    })?;

    // Convert data inputs to text representation.
    let texts: Vec<String> = data
        .iter()
        .map(|di| match di {
            DataInput::Text(t) => t.clone(),
            DataInput::FilePath(p) => format!("[file: {}]", p),
            other => format!("{:?}", other),
        })
        .collect();

    let combined_text = texts.join("\n\n");
    let user_id_str = owner_id.to_string();

    // Store as a Q&A entry (question="" since this is ingestion, not a query).
    store
        .create_qa_entry(session_id, Some(&user_id_str), "", &combined_text, None)
        .await?;

    info!(
        session_id = session_id,
        text_len = combined_text.len(),
        "remember: stored data in session cache"
    );

    // Optional self-improvement via memify.
    let memify_result = if self_improvement {
        let config = MemifyConfig::default();
        match run_memify(
            graph_db,
            vector_db,
            embedding_engine,
            None,
            Some(owner_id),
            None,
            &config,
        )
        .await
        {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!("memify phase in session mode failed (non-fatal): {e}");
                None
            }
        }
    } else {
        None
    };

    let elapsed = start.elapsed().as_secs_f64();

    Ok(RememberResult {
        status: RememberStatus::SessionStored,
        dataset_name: dataset_name.to_string(),
        dataset_id: None,
        session_ids: Some(vec![session_id.to_string()]),
        elapsed_seconds: elapsed,
        items_processed: data.len(),
        items: vec![],
        cognify_result: None,
        memify_result,
        error: None,
    })
}
