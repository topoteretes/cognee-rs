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
use tokio::sync::Mutex as AsyncMutex;
use tracing::{info, warn};
use uuid::Uuid;

use super::error::ApiError;
use super::improve::improve;

/// Status of a remember operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RememberStatus {
    /// Pipeline spawned and still running (background mode).
    Running,
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

/// Inner state shared between a `RememberResult` handle and its background task.
///
/// The background task writes `status`, `error`, `elapsed_seconds`, etc. as it
/// runs; callers observe the latest state by calling `await_completion()`.
#[derive(Debug, Default)]
struct RememberResultInner {
    status: Option<RememberStatus>,
    error: Option<String>,
    elapsed_seconds: f64,
    dataset_id: Option<Uuid>,
    pipeline_run_id: Option<Uuid>,
    items: Vec<RememberItemInfo>,
    items_processed: usize,
    content_hash: Option<String>,
    cognify_result: Option<CognifyResult>,
    memify_result: Option<MemifyResult>,
    join_handle: Option<tokio::task::JoinHandle<()>>,
}

/// Result of a `remember()` call.
///
/// Acts as a lightweight promise wrapper:
/// * Blocking mode — fields are populated before the function returns.
/// * Background mode — fields carry partial/initial state; call
///   [`RememberResult::await_completion`] to block until the background
///   pipeline finishes and refresh fields from the shared inner state.
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
    /// Shared state for background-mode awaiting. `None` for blocking results.
    #[serde(skip)]
    inner: Option<Arc<AsyncMutex<RememberResultInner>>>,
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

    /// `true` if the pipeline has finished (success, error, or stored).
    /// `Running` is the only status that returns `false`.
    pub fn done(&self) -> bool {
        self.status != RememberStatus::Running
    }

    /// Wait for the background task (if any) and refresh fields from shared
    /// inner state. Mirrors Python's `await result`.
    ///
    /// Safe to call on blocking results — it is a no-op in that case.
    pub async fn await_completion(mut self) -> Result<Self, ApiError> {
        let Some(inner) = self.inner.clone() else {
            return Ok(self);
        };

        // Take the handle out of inner to release the lock before awaiting it.
        let handle = {
            let mut guard = inner.lock().await;
            guard.join_handle.take()
        };
        if let Some(h) = handle {
            // If the task panicked, return the JoinError; otherwise discard its
            // () output — the task writes all state into `inner`.
            h.await?;
        }

        let guard = inner.lock().await;
        if let Some(s) = guard.status {
            self.status = s;
        }
        if let Some(ref e) = guard.error {
            self.error = Some(e.clone());
        }
        if guard.elapsed_seconds > 0.0 {
            self.elapsed_seconds = guard.elapsed_seconds;
        }
        if guard.dataset_id.is_some() {
            self.dataset_id = guard.dataset_id;
        }
        if guard.pipeline_run_id.is_some() {
            self.pipeline_run_id = guard.pipeline_run_id;
        }
        if !guard.items.is_empty() {
            self.items = guard.items.clone();
            self.items_processed = guard.items_processed;
        }
        if guard.content_hash.is_some() {
            self.content_hash = guard.content_hash.clone();
        }
        if guard.cognify_result.is_some() {
            self.cognify_result = guard.cognify_result.clone();
        }
        if guard.memify_result.is_some() {
            self.memify_result = guard.memify_result.clone();
        }
        Ok(self)
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
/// When `run_in_background` is `true`, the whole pipeline is spawned on a tokio
/// task and the returned `RememberResult` has `status = Running`. Call
/// [`RememberResult::await_completion`] to block until the task finishes.
///
/// **Session Memory Mode** (with `session_id`):
/// 1. Convert data inputs to text.
/// 2. Store in session cache as Q&A entry.
/// 3. If `self_improvement=true`, spawn `improve(session_ids=[session_id])`
///    in the background. The bridge failures are logged but never surface
///    as an error to the caller (matches Python `_session_improve()`).
#[allow(clippy::too_many_arguments)]
pub async fn remember(
    data: Vec<DataInput>,
    dataset_name: &str,
    session_id: Option<&str>,
    self_improvement: bool,
    run_in_background: bool,
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
    if run_in_background {
        return remember_permanent_background(
            data,
            dataset_name.to_string(),
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
        session_store,
        session_manager,
        checkpoint_store,
        ontology_resolver,
        &cognify_config,
        start,
    )
    .await
}

// ---------------------------------------------------------------------------
// Permanent mode: blocking
// ---------------------------------------------------------------------------

/// Outcome of the permanent-mode pipeline, shared between the blocking and
/// background implementations.
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
    _session_store: Option<Arc<dyn SessionStore>>,
    _session_manager: Option<Arc<SessionManager>>,
    _checkpoint_store: Option<Arc<dyn CheckpointStore>>,
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
        inner: None,
    })
}

// ---------------------------------------------------------------------------
// Permanent mode: background
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn remember_permanent_background(
    data: Vec<DataInput>,
    dataset_name: String,
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
    _session_store: Option<Arc<dyn SessionStore>>,
    _session_manager: Option<Arc<SessionManager>>,
    _checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    cognify_config: Arc<CognifyConfig>,
    start: Instant,
) -> Result<RememberResult, ApiError> {
    let inner = Arc::new(AsyncMutex::new(RememberResultInner::default()));
    let inner_task = Arc::clone(&inner);
    let dataset_name_task = dataset_name.clone();

    let handle = tokio::spawn(async move {
        let pipeline_result = run_permanent_inner(
            data,
            &dataset_name_task,
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
        )
        .await;

        let mut guard = inner_task.lock().await;
        guard.elapsed_seconds = start.elapsed().as_secs_f64();
        match pipeline_result {
            Ok(outcome) => {
                guard.status = Some(RememberStatus::Completed);
                guard.dataset_id = Some(outcome.dataset_id);
                guard.pipeline_run_id = Some(outcome.pipeline_run_id);
                guard.items_processed = outcome.items_processed;
                guard.items = outcome.items;
                guard.content_hash = outcome.content_hash;
                guard.cognify_result = Some(outcome.cognify_result);
                guard.memify_result = outcome.memify_result;
            }
            Err(e) => {
                guard.status = Some(RememberStatus::Errored);
                guard.error = Some(e.to_string());
            }
        }
    });

    {
        let mut guard = inner.lock().await;
        guard.join_handle = Some(handle);
    }

    Ok(RememberResult {
        status: RememberStatus::Running,
        dataset_name,
        dataset_id: None,
        session_ids: None,
        pipeline_run_id: None,
        elapsed_seconds: 0.0,
        content_hash: None,
        items_processed: 0,
        items: Vec::new(),
        error: None,
        cognify_result: None,
        memify_result: None,
        inner: Some(inner),
    })
}

// ---------------------------------------------------------------------------
// Session mode
// ---------------------------------------------------------------------------

/// Session-mode remember: store data as Q&A text in the session cache.
///
/// When `self_improvement=true`, spawns a background `improve()` call with the
/// given `session_id`. Background failures are logged but never propagated.
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

    // Optional self-improvement via improve() in the background.
    let inner = if self_improvement {
        let inner = Arc::new(AsyncMutex::new(RememberResultInner::default()));
        let inner_task = Arc::clone(&inner);
        let dataset_name_task = dataset_name.to_string();
        let sid_task = session_id.to_string();
        let add_pipeline_task = Arc::clone(&add_pipeline);
        let llm_task = Arc::clone(&llm);
        let storage_task = Arc::clone(&storage);
        let graph_db_task = Arc::clone(&graph_db);
        let vector_db_task = Arc::clone(&vector_db);
        let embedding_engine_task = Arc::clone(&embedding_engine);
        let db_task = db.clone();
        let session_store_task = session_store.clone();
        let session_manager_task = session_manager.clone();
        let checkpoint_store_task = checkpoint_store.clone();
        let ontology_task = Arc::clone(&ontology_resolver);
        let cognify_config_task = Arc::clone(&cognify_config);

        let handle = tokio::spawn(async move {
            let improve_result = improve(
                &dataset_name_task,
                Some(vec![sid_task.clone()]),
                None,
                owner_id,
                tenant_id,
                0.1, // default feedback_alpha
                false,
                llm_task,
                storage_task,
                graph_db_task,
                vector_db_task,
                embedding_engine_task,
                ontology_task,
                db_task,
                session_store_task,
                session_manager_task,
                Some(add_pipeline_task.as_ref()),
                checkpoint_store_task,
                &cognify_config_task,
            )
            .await;

            let mut guard = inner_task.lock().await;
            guard.elapsed_seconds = start.elapsed().as_secs_f64();
            match improve_result {
                Ok(_) => {
                    guard.status = Some(RememberStatus::SessionStored);
                    info!(
                        session_id = %sid_task,
                        "remember: session bridged to permanent graph"
                    );
                }
                Err(e) => {
                    // Session-improve failures are non-fatal — record and log.
                    guard.error = Some(e.to_string());
                    guard.status = Some(RememberStatus::SessionStored);
                    warn!(
                        session_id = %sid_task,
                        "remember: session improve failed (non-fatal): {e}"
                    );
                }
            }
        });

        {
            let mut guard = inner.lock().await;
            guard.join_handle = Some(handle);
        }
        Some(inner)
    } else {
        None
    };

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
        error: None,
        cognify_result: None,
        memify_result: None,
        inner,
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_status_serde_roundtrip_running() {
        let s = RememberStatus::Running;
        let j = serde_json::to_string(&s).expect("serialize");
        assert_eq!(j, "\"running\"");
        let back: RememberStatus = serde_json::from_str(&j).expect("deserialize");
        assert_eq!(back, RememberStatus::Running);
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
    fn is_success_running_and_errored() {
        let mut r = sample_result(RememberStatus::Running);
        assert!(!r.is_success());
        assert!(!r.done());

        r.status = RememberStatus::Errored;
        assert!(!r.is_success());
        assert!(r.done());
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
        // cognify_result / memify_result / inner are #[serde(skip)]
        assert!(!obj.contains_key("cognify_result"));
        assert!(!obj.contains_key("memify_result"));
        assert!(!obj.contains_key("inner"));
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
            inner: None,
        }
    }
}
