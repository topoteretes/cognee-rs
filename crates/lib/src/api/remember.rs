//! One-call add + cognify + optional improve -- `remember()`.
//!
//! Composition of `add()` -> `cognify()` -> optionally `improve()` (via `memify`),
//! with session-mode support.
//!
//! Equivalent to Python's `cognee.api.v1.remember.remember()`.

use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use cognee_cognify::cognify;
use cognee_cognify::{CognifyConfig, CognifyResult, MemifyConfig, MemifyResult, run_memify};
use cognee_database::{CheckpointStore, DatabaseConnection};
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_ingestion::AddPipeline;
use cognee_llm::Llm;
use cognee_models::DataInput;
use cognee_ontology::OntologyResolver;
use cognee_session::{SessionManager, SessionStore};
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use uuid::Uuid;

use super::error::ApiError;
use super::improve::improve;

/// Status of a remember operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RememberStatus {
    /// Pipeline finished successfully.
    Completed,
    /// Pipeline finished with an error.
    Errored,
    /// Session-mode only: data was stored in the session cache.
    SessionStored,
}

/// Per-item information in the remember result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RememberItemInfo {
    pub id: Option<Uuid>,
    pub name: Option<String>,
    pub content_hash: Option<String>,
    /// Token count (None when not yet computed).
    pub token_count: Option<i64>,
    /// Size of the raw data in bytes (None when unknown).
    pub data_size: Option<i64>,
    pub mime_type: Option<String>,
}

/// Result of a `remember()` call.
///
/// All fields are populated before the function returns — `remember()` is
/// strictly synchronous.
#[derive(Debug, Clone, Serialize)]
pub struct RememberResult {
    pub status: RememberStatus,
    pub dataset_name: String,
    pub dataset_id: Option<Uuid>,
    pub session_ids: Option<Vec<String>>,
    pub pipeline_run_id: Option<Uuid>,
    pub elapsed_seconds: f64,
    /// Content hash of the first item (Python parity for deduplication tracking).
    pub content_hash: Option<String>,
    pub items_processed: usize,
    pub items: Vec<RememberItemInfo>,
    pub error: Option<String>,
    #[serde(skip)]
    pub cognify_result: Option<CognifyResult>,
    #[serde(skip)]
    pub memify_result: Option<MemifyResult>,
}

impl RememberResult {
    /// Serialize to a plain JSON value (Python `to_dict()` parity).
    pub fn to_dict(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    /// `true` if status is `Completed` or `SessionStored` (Python `__bool__`).
    pub fn is_success(&self) -> bool {
        matches!(
            self.status,
            RememberStatus::Completed | RememberStatus::SessionStored
        )
    }

    /// `true` always — every `RememberStatus` variant is terminal.
    ///
    /// `remember()` is synchronous; the result is always in a terminal state
    /// by the time the function returns.
    pub fn done(&self) -> bool {
        true
    }
}

impl fmt::Display for RememberResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RememberResult(status={:?}, dataset={:?}",
            self.status, self.dataset_name
        )?;
        if let Some(ref ids) = self.session_ids {
            if ids.len() == 1 {
                write!(f, ", session_id={:?}", ids[0])?;
            } else {
                write!(f, ", session_ids={ids:?}")?;
            }
        }
        if let Some(id) = self.dataset_id {
            write!(f, ", dataset_id={id}")?;
        }
        if let Some(id) = self.pipeline_run_id {
            write!(f, ", pipeline_run_id={id}")?;
        }
        if self.items_processed > 0 {
            write!(f, ", items={}", self.items_processed)?;
        }
        if let Some(ref h) = self.content_hash {
            write!(f, ", content_hash={h:?}")?;
        }
        write!(f, ", elapsed={:.1}s", self.elapsed_seconds)?;
        if let Some(ref e) = self.error {
            write!(f, ", error={e:?}")?;
        }
        write!(f, ")")
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// One-call add + cognify + optional improve.
///
/// **Permanent Memory Mode** (no `session_id`):
/// 1. `add()` to ingest data.
/// 2. `cognify()` to extract knowledge graph.
/// 3. If `self_improvement=true`, `memify()` to enrich with triplet embeddings.
///
/// **Session Memory Mode** (with `session_id`):
/// 1. Convert data inputs to text.
/// 2. Store in session cache as Q&A entry.
/// 3. If `self_improvement=true`, run `improve(session_ids=[session_id])`
///    inline. Failures are logged but never surface as an error to the caller
///    (matches Python `_session_improve()` semantics).
///
/// This function is strictly synchronous — it always returns a
/// fully-populated [`RememberResult`]. Background dispatch is a host-side
/// concern (e.g. the HTTP server via `PipelineRunRegistry::register_background`).
#[allow(clippy::too_many_arguments)]
pub async fn remember(
    data: Vec<DataInput>,
    dataset_name: &str,
    session_id: Option<&str>,
    self_improvement: bool,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    add_pipeline: Arc<AddPipeline>,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    db: Option<Arc<DatabaseConnection>>,
    session_store: Option<Arc<dyn SessionStore>>,
    session_manager: Option<Arc<SessionManager>>,
    checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    cognify_config: Arc<CognifyConfig>,
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
            tenant_id,
            add_pipeline,
            llm,
            storage,
            graph_db,
            vector_db,
            embedding_engine,
            db,
            session_store,
            session_manager,
            checkpoint_store,
            ontology_resolver,
            cognify_config,
            start,
        )
        .await;
    }

    // -- Permanent Memory Mode --
    remember_permanent_blocking(
        data,
        dataset_name,
        self_improvement,
        owner_id,
        tenant_id,
        &add_pipeline,
        llm,
        storage,
        graph_db,
        vector_db,
        embedding_engine,
        db,
        ontology_resolver,
        &cognify_config,
        start,
    )
    .await
}

// ---------------------------------------------------------------------------
// Permanent mode: blocking
// ---------------------------------------------------------------------------

/// Outcome of the permanent-mode pipeline.
struct PermanentOutcome {
    dataset_id: Uuid,
    pipeline_run_id: Uuid,
    items: Vec<RememberItemInfo>,
    items_processed: usize,
    content_hash: Option<String>,
    cognify_result: CognifyResult,
    memify_result: Option<MemifyResult>,
}

#[allow(clippy::too_many_arguments)]
async fn run_permanent_inner(
    data: Vec<DataInput>,
    dataset_name: &str,
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
    ontology_resolver: Arc<dyn OntologyResolver>,
    cognify_config: &CognifyConfig,
) -> Result<PermanentOutcome, ApiError> {
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
            token_count: (d.token_count >= 0).then_some(d.token_count),
            data_size: (d.data_size >= 0).then_some(d.data_size),
            mime_type: Some(d.mime_type.clone()),
        })
        .collect();

    let content_hash_first = items.first().and_then(|i| i.content_hash.clone());
    let items_processed = items.len();

    let dataset_id = cognee_ingestion::generate_dataset_id(dataset_name, owner_id, tenant_id);
    // The Rust cognify pipeline does not expose a pipeline run ID today;
    // synthesize one per-call to preserve Python API parity.
    let pipeline_run_id = Uuid::new_v4();

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
                warn!("memify phase failed (non-fatal): {e}");
                None
            }
        }
    } else {
        None
    };

    Ok(PermanentOutcome {
        dataset_id,
        pipeline_run_id,
        items,
        items_processed,
        content_hash: content_hash_first,
        cognify_result,
        memify_result,
    })
}

#[allow(clippy::too_many_arguments)]
async fn remember_permanent_blocking(
    data: Vec<DataInput>,
    dataset_name: &str,
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
    ontology_resolver: Arc<dyn OntologyResolver>,
    cognify_config: &CognifyConfig,
    start: Instant,
) -> Result<RememberResult, ApiError> {
    let outcome = run_permanent_inner(
        data,
        dataset_name,
        self_improvement,
        owner_id,
        tenant_id,
        add_pipeline,
        llm,
        storage,
        graph_db,
        vector_db,
        embedding_engine,
        db,
        ontology_resolver,
        cognify_config,
    )
    .await?;

    let elapsed = start.elapsed().as_secs_f64();

    Ok(RememberResult {
        status: RememberStatus::Completed,
        dataset_name: dataset_name.to_string(),
        dataset_id: Some(outcome.dataset_id),
        session_ids: None,
        pipeline_run_id: Some(outcome.pipeline_run_id),
        elapsed_seconds: elapsed,
        content_hash: outcome.content_hash,
        items_processed: outcome.items_processed,
        items: outcome.items,
        error: None,
        cognify_result: Some(outcome.cognify_result),
        memify_result: outcome.memify_result,
    })
}

// ---------------------------------------------------------------------------
// Session mode
// ---------------------------------------------------------------------------

/// Session-mode remember: store data as Q&A text in the session cache.
///
/// When `self_improvement=true`, runs `improve()` inline (synchronously).
/// Session-improve failures are logged but never propagated — matches Python's
/// `_session_improve()` semantics.
#[allow(clippy::too_many_arguments)]
async fn remember_session(
    data: &[DataInput],
    dataset_name: &str,
    session_id: &str,
    self_improvement: bool,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    add_pipeline: Arc<AddPipeline>,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    db: Option<Arc<DatabaseConnection>>,
    session_store: Option<Arc<dyn SessionStore>>,
    session_manager: Option<Arc<SessionManager>>,
    checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    cognify_config: Arc<CognifyConfig>,
    start: Instant,
) -> Result<RememberResult, ApiError> {
    let store = session_store.clone().ok_or_else(|| {
        ApiError::InvalidArgument(
            "session_id provided but no session_store is available".to_string(),
        )
    })?;

    // Convert data inputs to text representation.
    let texts: Vec<String> = data
        .iter()
        .map(|di| match di {
            DataInput::Text(t) => t.clone(),
            DataInput::FilePath(p) => format!("[file: {p}]"),
            other => format!("{other:?}"),
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

    // Optional self-improvement via improve() — inline (synchronous).
    let mut improve_error: Option<String> = None;
    if self_improvement {
        let improve_result = improve(
            dataset_name,
            Some(vec![session_id.to_string()]),
            None,
            owner_id,
            tenant_id,
            0.1, // default feedback_alpha
            llm,
            storage,
            graph_db,
            vector_db,
            embedding_engine,
            ontology_resolver,
            db,
            session_store,
            session_manager,
            Some(add_pipeline.as_ref()),
            checkpoint_store,
            &cognify_config,
        )
        .await;

        match improve_result {
            Ok(_) => {
                info!(
                    session_id = session_id,
                    "remember: session bridged to permanent graph"
                );
            }
            Err(e) => {
                // Session-improve failures are non-fatal — record and log.
                let msg = e.to_string();
                warn!(
                    session_id = session_id,
                    "remember: session improve failed (non-fatal): {msg}"
                );
                improve_error = Some(msg);
            }
        }
    }

    let elapsed = start.elapsed().as_secs_f64();

    Ok(RememberResult {
        status: RememberStatus::SessionStored,
        dataset_name: dataset_name.to_string(),
        dataset_id: None,
        session_ids: Some(vec![session_id.to_string()]),
        pipeline_run_id: None,
        elapsed_seconds: elapsed,
        content_hash: None,
        items_processed: data.len(),
        items: vec![],
        error: improve_error,
        cognify_result: None,
        memify_result: None,
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_status_serde_roundtrip_errored() {
        let s = RememberStatus::Errored;
        let j = serde_json::to_string(&s).expect("serialize");
        assert_eq!(j, "\"errored\"");
        let back: RememberStatus = serde_json::from_str(&j).expect("deserialize");
        assert_eq!(back, RememberStatus::Errored);
    }

    #[test]
    fn is_success_completed_and_session_stored() {
        let mut r = sample_result(RememberStatus::Completed);
        assert!(r.is_success());
        assert!(r.done());

        r.status = RememberStatus::SessionStored;
        assert!(r.is_success());
        assert!(r.done());
    }

    #[test]
    fn is_success_errored() {
        let r = sample_result(RememberStatus::Errored);
        assert!(!r.is_success());
        // done() is always true — every status is terminal.
        assert!(r.done());
    }

    #[test]
    fn all_statuses_are_done() {
        for status in [
            RememberStatus::Completed,
            RememberStatus::Errored,
            RememberStatus::SessionStored,
        ] {
            let r = sample_result(status);
            assert!(r.done(), "expected done() == true for {status:?}");
        }
    }

    #[test]
    fn display_format_has_status_and_dataset() {
        let r = sample_result(RememberStatus::Completed);
        let text = format!("{r}");
        assert!(text.contains("RememberResult("));
        assert!(text.contains("status=Completed"));
        assert!(text.contains("dataset="));
        assert!(text.ends_with(')'));
    }

    #[test]
    fn to_dict_omits_skipped_fields() {
        let r = sample_result(RememberStatus::Completed);
        let v = r.to_dict();
        assert!(v.is_object());
        let obj = v.as_object().expect("object");
        assert!(obj.contains_key("status"));
        assert!(obj.contains_key("dataset_name"));
        // cognify_result / memify_result are #[serde(skip)]
        assert!(!obj.contains_key("cognify_result"));
        assert!(!obj.contains_key("memify_result"));
    }

    #[test]
    fn display_formats_single_session_id() {
        let mut r = sample_result(RememberStatus::SessionStored);
        r.session_ids = Some(vec!["sess-123".to_string()]);
        let text = format!("{r}");
        assert!(text.contains("session_id=\"sess-123\""));
        assert!(!text.contains("session_ids="));
    }

    fn sample_result(status: RememberStatus) -> RememberResult {
        RememberResult {
            status,
            dataset_name: "main_dataset".to_string(),
            dataset_id: None,
            session_ids: None,
            pipeline_run_id: None,
            elapsed_seconds: 1.23,
            content_hash: None,
            items_processed: 0,
            items: Vec::new(),
            error: None,
            cognify_result: None,
            memify_result: None,
        }
    }
}
