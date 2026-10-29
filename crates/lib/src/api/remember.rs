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
use cognee_database::{CheckpointStore, PipelineRunRepository, RelationalDb, SessionLifecycleDb};
use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_ingestion::AddPipeline;
use cognee_llm::Llm;
use cognee_models::{DataInput, FeedbackEntry, MemoryEntry, QAEntry, TraceEntry};
use cognee_ontology::OntologyResolver;
use cognee_session::{SessionManager, SessionQAUpdate, SessionStore};
use cognee_storage::StorageTrait;
use cognee_vector::VectorDB;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::auto_improve_debounce::session_improve_due;
use super::error::ApiError;
use super::improve::{ImproveConfig, ImproveParams, improve};

/// Status of a remember operation.
///
/// **Decision 15** — library layer emits CamelCase `"PipelineRunStarted"`/
/// `"PipelineRunCompleted"`/`"PipelineRunErrored"`/`"SessionStored"` so the
/// in-process Rust SDK shares one status vocabulary with
/// `cognee_core::PipelineRunStatus`. The HTTP layer (E-01) translates this
/// to Python's lowercase wire format at the DTO boundary for strict Python
/// wire parity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RememberStatus {
    /// Pipeline has been initiated/started but has not yet finished.
    ///
    /// Currently unused by the synchronous SDK [`remember`] (which always
    /// returns a terminal state). Exists for symmetry with
    /// [`cognee_core::pipeline::PipelineRunStatus`] and for future async /
    /// HTTP background-mode emission.
    #[serde(rename = "PipelineRunStarted")]
    Started,
    /// Pipeline finished successfully.
    #[serde(rename = "PipelineRunCompleted")]
    Completed,
    /// Pipeline finished with an error.
    #[serde(rename = "PipelineRunErrored")]
    Errored,
    /// Session-mode only: data was stored in the session cache.
    #[serde(rename = "SessionStored")]
    SessionStored,
}

impl From<cognee_core::pipeline::PipelineRunStatus> for RememberStatus {
    fn from(s: cognee_core::pipeline::PipelineRunStatus) -> Self {
        use cognee_core::pipeline::PipelineRunStatus;
        match s {
            PipelineRunStatus::Initiated | PipelineRunStatus::Started => Self::Started,
            PipelineRunStatus::Completed => Self::Completed,
            PipelineRunStatus::Errored => Self::Errored,
        }
    }
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
    /// Wall-clock seconds the operation took. `None` when the operation has
    /// not produced a duration (Python parity:
    /// `RememberResult.elapsed_seconds: Optional[float]`).
    pub elapsed_seconds: Option<f64>,
    /// Content hash of the first item (Python parity for deduplication tracking).
    pub content_hash: Option<String>,
    pub items_processed: usize,
    pub items: Vec<RememberItemInfo>,
    pub error: Option<String>,
    /// Type discriminator for typed-entry remember (`"qa"` / `"trace"` /
    /// `"feedback"`). Populated by `remember_entry()` (LIB-01) in the
    /// typed-entry path; `None` for the file/text path.
    pub entry_type: Option<String>,
    /// Typed-entry id from `SessionManager`. Populated alongside
    /// [`Self::entry_type`].
    pub entry_id: Option<String>,
    #[serde(skip)]
    pub cognify_result: Option<CognifyResult>,
    #[serde(skip)]
    pub memify_result: Option<MemifyResult>,
    /// Session mode: the automatic improve was launched as a detached
    /// background task (Python's `RememberResult._task` being set). `false`
    /// when it is off, debounced, or the improve settings are invalid. Await
    /// it with [`crate::wait_for_background_tasks`].
    #[serde(skip)]
    pub improve_launched: bool,
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
        if let Some(elapsed) = self.elapsed_seconds {
            write!(f, ", elapsed={elapsed:.1}s")?;
        }
        if let Some(ref e) = self.error {
            write!(f, ", error={e:?}")?;
        }
        write!(f, ")")
    }
}

/// The improve settings an automatic improve runs with, or `None` when it
/// does not run: Python's `self_improvement and auto_improve_enabled()`, on
/// both remember paths. `improve.auto_enabled = false` is the kill switch an
/// explicit `self_improvement = true` does not override; invalid settings
/// (`None`) skip the improve, never the ingestion.
fn auto_improve_config(
    self_improvement: bool,
    improve_config: Option<ImproveConfig>,
) -> Option<ImproveConfig> {
    improve_config.filter(|config| self_improvement && config.auto_enabled)
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// One-call add + cognify + optional improve.
///
/// **Permanent Memory Mode** (no `session_id`):
/// 1. `add()` to ingest data.
/// 2. `cognify()` to extract knowledge graph.
/// 3. If `self_improvement=true` and `improve_config.auto_enabled`, `memify()`
///    to enrich with triplet embeddings. `IMPROVE_AUTO_ENABLED=false` is the
///    kill switch on both paths, as in Python (`remember.py`:
///    `self_improvement and auto_improve_enabled()`).
///
/// **Session Memory Mode** (with `session_id`):
/// 1. Convert data inputs to text.
/// 2. Store in session cache as Q&A entry.
/// 3. If `self_improvement=true` and `improve_config` allows it
///    (`auto_enabled`, then the debounce of `debounce_entries` /
///    `debounce_seconds`), launch `improve(session_ids=[session_id])` as a
///    **detached** background task in the process-wide
///    [`BackgroundTasks`](crate::BackgroundTasks) registry, as Python does.
///    Its failures are logged, never returned. A short-lived host drains the
///    registry ([`crate::wait_for_background_tasks`]) before exiting.
///
/// `improve_config` is the validated improve settings
/// ([`Settings::auto_improve_config`](crate::config::Settings::auto_improve_config));
/// `None` (invalid settings) skips the automatic improve — an improve-only
/// knob never blocks ingestion. `personalization_enabled` is handed to that
/// improve.
///
/// The permanent mode is synchronous and returns a fully-populated
/// [`RememberResult`]; background dispatch of it is a host-side concern
/// (e.g. the HTTP server via `PipelineRunRegistry::register_background`).
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
    db: Option<Arc<dyn RelationalDb>>,
    session_store: Option<Arc<dyn SessionStore>>,
    session_manager: Option<Arc<SessionManager>>,
    checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    cognify_config: Arc<CognifyConfig>,
    improve_config: Option<ImproveConfig>,
    personalization_enabled: bool,
) -> Result<RememberResult, ApiError> {
    let start = Instant::now();

    // Mirrors Python `send_telemetry("cognee.remember", ...)` from
    // cognee/api/v1/remember/remember.py:624.
    #[cfg(feature = "telemetry")]
    {
        let data_size_bytes: usize = data
            .iter()
            .map(|d| match d {
                DataInput::Text(s) => s.len(),
                _ => 0,
            })
            .sum();
        let item_count = data.len();
        let mode = if session_id.is_some() {
            "session"
        } else {
            "permanent"
        };
        cognee_telemetry::send_telemetry(
            "cognee.remember",
            owner_id,
            Some(serde_json::json!({
                "mode": mode,
                "data_size_bytes": data_size_bytes,
                "item_count": item_count,
                "session_id": session_id,
            })),
        );
    }

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
            improve_config,
            personalization_enabled,
            start,
        )
        .await;
    }

    // -- Permanent Memory Mode --
    // The same kill switch as the session path: an explicit
    // `self_improvement = true` does not override `auto_enabled = false`, and
    // invalid improve settings skip the self-improvement, never the ingestion.
    let self_improvement = auto_improve_config(self_improvement, improve_config).is_some();
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
    db: Option<Arc<dyn RelationalDb>>,
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

    // OSS build has no DB-backed user lookup (there is no `users` table in
    // the OSS schema), so we always fall back to `None`. `cognify()`
    // then uses `user_id.to_string()` as the provenance stamp, matching
    // Python's unauthenticated-run behaviour.
    let user_email: Option<String> = None;

    // Clone the optional DB handle so memify (which now requires it per
    // LIB-06 Decision 1) can still reach the relational connection after
    // cognify consumes its copy.
    let db_for_memify = db.clone();

    // LIB-06-03: `cognify()` now requires `Arc<dyn RelationalDb>` and an
    // `Arc<dyn CpuPool>` (Decision 1).
    let database = db
        .clone()
        .ok_or_else(|| ApiError::Cognify("cognify requires a relational database".to_string()))?;
    let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(
        cognee_core::RayonThreadPool::with_default_threads()
            .map_err(|e| ApiError::Cognify(format!("failed to construct thread pool: {e}")))?,
    );

    // Cognify.
    // Gap 08-07: persist the four-state `pipeline_runs` trail through the
    // real SeaORM repo when a database is available; embedded callers fall
    // back to the no-op repo.
    let pipeline_run_repo: Arc<dyn PipelineRunRepository> = Arc::clone(&database).pipeline_runs();
    let cognify_result = cognify(
        data_items,
        dataset_id,
        Some(owner_id),
        user_email,
        tenant_id,
        llm,
        storage,
        Arc::clone(&graph_db),
        Arc::clone(&vector_db),
        Arc::clone(&embedding_engine),
        database,
        Arc::clone(&pipeline_run_repo),
        thread_pool,
        ontology_resolver,
        cognify_config,
    )
    .await
    .map_err(|e| ApiError::Cognify(e.to_string()))?;

    // Optional self-improvement via memify.
    let memify_result = if self_improvement {
        let config = MemifyConfig::default();
        match db_for_memify {
            Some(database) => match cognee_core::RayonThreadPool::with_default_threads() {
                Ok(pool) => {
                    let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(pool);
                    let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
                        Arc::clone(&database).pipeline_runs();
                    match run_memify(
                        Arc::clone(&graph_db),
                        Arc::clone(&vector_db),
                        Arc::clone(&embedding_engine),
                        thread_pool,
                        database,
                        pipeline_run_repo,
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
                }
                Err(e) => {
                    warn!("memify phase skipped (non-fatal): rayon pool init: {e}");
                    None
                }
            },
            None => {
                warn!(
                    "memify phase skipped: a relational database connection is required by the \
                     LIB-06 executor-routed memify"
                );
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
    db: Option<Arc<dyn RelationalDb>>,
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
        elapsed_seconds: Some(elapsed),
        content_hash: outcome.content_hash,
        items_processed: outcome.items_processed,
        items: outcome.items,
        error: None,
        entry_type: None,
        entry_id: None,
        cognify_result: Some(outcome.cognify_result),
        memify_result: outcome.memify_result,
        improve_launched: false,
    })
}

// ---------------------------------------------------------------------------
// Session mode
// ---------------------------------------------------------------------------

/// Session-mode remember: store data as Q&A text in the session cache, then
/// launch the automatic improve when it is due (see [`remember`]).
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
    db: Option<Arc<dyn RelationalDb>>,
    session_store: Option<Arc<dyn SessionStore>>,
    session_manager: Option<Arc<SessionManager>>,
    checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    cognify_config: Arc<CognifyConfig>,
    improve_config: Option<ImproveConfig>,
    personalization_enabled: bool,
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
    // With session memory switched off this is not an error: Python's
    // `_add_to_session` logs and returns, and remember still reports the
    // session and runs the improve bridge.
    match store
        .create_qa_entry(session_id, Some(&user_id_str), "", &combined_text, None)
        .await
    {
        Ok(_) => info!(
            session_id = session_id,
            text_len = combined_text.len(),
            "remember: stored data in session cache"
        ),
        Err(cognee_session::SessionError::Disabled) => {
            warn!("remember: session cache not available (enable CACHING=true)");
        }
        Err(e) => return Err(e.into()),
    }

    let improve_launched = launch_session_auto_improve(SessionAutoImprove {
        dataset_name: dataset_name.to_string(),
        dataset_id: None,
        session_id: session_id.to_string(),
        owner_id,
        tenant_id,
        self_improvement,
        improve_config,
        personalization_enabled,
        llm,
        storage,
        graph_db,
        vector_db,
        embedding_engine,
        ontology_resolver,
        db,
        session_store,
        session_manager,
        checkpoint_store,
        add_pipeline,
        cognify_config,
    })
    .await;

    let elapsed = start.elapsed().as_secs_f64();

    Ok(RememberResult {
        status: RememberStatus::SessionStored,
        dataset_name: dataset_name.to_string(),
        dataset_id: None,
        session_ids: Some(vec![session_id.to_string()]),
        pipeline_run_id: None,
        elapsed_seconds: Some(elapsed),
        content_hash: None,
        items_processed: data.len(),
        items: vec![],
        error: None,
        entry_type: None,
        entry_id: None,
        cognify_result: None,
        memify_result: None,
        improve_launched,
    })
}

// ---------------------------------------------------------------------------
// Typed-entry dispatch (`remember_entry`) — LIB-01 / Decision 2 / Decision 5
// ---------------------------------------------------------------------------

/// The inputs of a session `remember()`'s automatic improve
/// ([`launch_session_auto_improve`]): the run's identity, its gate and the
/// handles the detached improve owns.
pub struct SessionAutoImprove {
    /// Dataset the session is bridged into.
    pub dataset_name: String,
    /// The dataset's id when the host already resolved it; `None` resolves
    /// `dataset_name` among the owner's datasets.
    pub dataset_id: Option<Uuid>,
    /// The session to bridge.
    pub session_id: String,
    /// Owner of the session and the dataset.
    pub owner_id: Uuid,
    /// Tenant of the dataset.
    pub tenant_id: Option<Uuid>,
    /// The caller's `self_improvement` flag.
    pub self_improvement: bool,
    /// The validated improve settings; `None` (invalid settings) skips.
    pub improve_config: Option<ImproveConfig>,
    /// Handed to the improve.
    pub personalization_enabled: bool,
    /// LLM handle.
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
    /// Metadata DB connection.
    pub db: Option<Arc<dyn RelationalDb>>,
    /// Session backing store.
    pub session_store: Option<Arc<dyn SessionStore>>,
    /// Session manager (also the debounce state).
    pub session_manager: Option<Arc<SessionManager>>,
    /// Checkpoint store (stage watermarks).
    pub checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    /// Add pipeline of the cognify-backed stages (and its dataset locks).
    pub add_pipeline: Arc<AddPipeline>,
    /// Cognify configuration of the cognify-backed stages.
    pub cognify_config: Arc<CognifyConfig>,
}

/// The automatic improve of a session `remember()` — Python's session
/// branch after `_add_to_session`: gate (`self_improvement` and
/// `improve.auto_enabled`), debounce, then `improve(session_ids=[session])`
/// as a **detached** task in the process-wide background registry
/// ([`crate::wait_for_background_tasks`] drains it). Its failures are logged,
/// never returned. `true` when the improve was launched.
///
/// [`remember`] calls this after storing the entry; a host that stores the
/// session entry itself (the HTTP server) calls it the same way.
pub async fn launch_session_auto_improve(request: SessionAutoImprove) -> bool {
    let SessionAutoImprove {
        dataset_name,
        dataset_id,
        session_id,
        owner_id,
        tenant_id,
        self_improvement,
        improve_config,
        personalization_enabled,
        llm,
        storage,
        graph_db,
        vector_db,
        embedding_engine,
        ontology_resolver,
        db,
        session_store,
        session_manager,
        checkpoint_store,
        add_pipeline,
        cognify_config,
    } = request;

    let Some(config) = auto_improve_config(self_improvement, improve_config) else {
        if self_improvement {
            debug!("remember: automatic improve disabled or unavailable");
        }
        return false;
    };
    // Debounce: bridge only after enough new entries or enough time since the
    // last automatic improve of this session. The default thresholds fire
    // every time and read nothing.
    let user_id = owner_id.to_string();
    if !session_improve_due(session_manager.as_deref(), &config, &user_id, &session_id).await {
        return false;
    }

    // Detached, as Python's `asyncio.create_task(_session_improve())`: the
    // session write already succeeded, so a failed bridge is only logged.
    crate::background_tasks::spawn_background_task(async move {
        let result = improve(ImproveParams {
            dataset_name,
            dataset_id,
            session_ids: Some(vec![session_id.clone()]),
            node_name: None,
            owner_id,
            tenant_id,
            feedback_alpha: None,
            config,
            personalization_enabled,
            llm,
            storage,
            graph_db,
            vector_db,
            embedding_engine,
            ontology_resolver,
            db,
            session_store,
            session_manager,
            add_pipeline: Some(add_pipeline.as_ref()),
            checkpoint_store,
            cognify_config: &cognify_config,
            extraction_tasks: None,
            enrichment_tasks: None,
            data: None,
            build_global_context_index: false,
            build_truth_subspace: false,
            run_in_background: false,
        })
        .await;
        match result {
            Ok(result) if result.status() == super::improve::ImproveStatus::Errored => {
                warn!(
                    session_id,
                    stages = %result.stage_summary(),
                    "remember: session improve reported errors (non-fatal)"
                );
            }
            Ok(result) => info!(
                session_id,
                status = result.status().as_str(),
                stages = %result.stage_summary(),
                "remember: session bridged to permanent graph"
            ),
            // A fatal-stage abort (`ApiError::ImproveAborted`) included.
            Err(e) => warn!(
                session_id,
                "remember: session improve failed (non-fatal): {e}"
            ),
        }
    });
    true
}

/// Dispatch a typed [`MemoryEntry`] to the appropriate `SessionManager`
/// method.
///
/// Mirrors Python's `_dispatch_session_entry` at
/// `cognee/api/v1/remember/remember.py:190-313`. The `entry_type` /
/// `entry_id` fields on the returned [`RememberResult`] are populated for
/// **all three** branches; the HTTP DTO (E-02) carries them through to the
/// wire.
///
/// **Behavior**:
/// - Empty `session_id` returns `Err(ApiError::InvalidArgument)` (Python
///   parity: `ValueError` → HTTP 400 at the handler boundary).
/// - Session memory switched off (`session.caching`, Python's
///   `CACHING=false`) returns `Err(ApiError::Session(SessionError::Disabled))`
///   before any other work, for every entry type (Python parity: the
///   "Session cache unavailable" `RuntimeError`).
/// - Best-effort pre-upsert via [`SessionLifecycleDb::ensure_and_touch_session`];
///   any failure is logged at `debug` level and swallowed (Python parity:
///   `try/except` around the pre-upsert at `remember.py:232-253`).
/// - `MemoryEntry::Qa` → [`SessionManager::save_qa`]; if any of
///   `feedback_text` / `feedback_score` / `used_graph_element_ids` is set,
///   a follow-up [`SessionManager::update_qa`] applies the partial update.
///   `entry_type = "qa"`, `entry_id = qa_id`.
/// - `MemoryEntry::Trace` → [`SessionManager::add_agent_trace_step`].
///   `method_params.unwrap_or(Value::Null)` is passed because the Rust
///   signature requires a non-`Option` value. Feedback generation happens
///   inside `SessionManager` based on `generate_feedback_with_llm` and
///   whether an LLM is wired. `entry_type = "trace"`, `entry_id = trace_id`.
/// - `MemoryEntry::Feedback` → [`SessionManager::add_feedback`]. On
///   `Ok(true)` the result reports `RememberStatus::SessionStored` with
///   `entry_id = qa_id`. On `Ok(false)` the result reports
///   `RememberStatus::Errored` with `error = Some("add_feedback: QA <id>
///   not found in session <sid>")`. `entry_type = "feedback"`.
///
/// **LLM feedback**: When `TraceEntry::generate_feedback_with_llm` is `true`
/// and `llm` is `Some`, the trace entry's `session_feedback` is produced by
/// calling the LLM with the
/// `agent_trace_feedback_summary_system` prompt (Python parity); the call is
/// bounded by an 8-second timeout. On timeout, LLM error, missing `llm`
/// handle, or empty `method_return_value`, the implementation falls back to
/// the deterministic Python-parity strings (`"<origin> succeeded."` /
/// `"<origin> failed. Reason: ..."` / `"<origin> failed."`). When
/// `generate_feedback_with_llm` is `false`, the deterministic fallback is
/// recorded regardless — also Python parity
/// (`session_manager.py:289-294`).
#[allow(clippy::too_many_arguments)]
pub async fn remember_entry(
    entry: MemoryEntry,
    dataset_name: &str,
    session_id: &str,
    owner_id: Uuid,
    _tenant_id: Option<Uuid>,
    db: Option<Arc<dyn RelationalDb>>,
    _session_store: Option<Arc<dyn SessionStore>>,
    session_manager: Option<Arc<SessionManager>>,
    llm: Option<Arc<dyn Llm>>,
) -> Result<RememberResult, ApiError> {
    let start = Instant::now();

    if session_id.is_empty() {
        return Err(ApiError::InvalidArgument(
            "session_id is required for typed memory entries".to_string(),
        ));
    }

    let base_session_manager = session_manager.ok_or_else(|| {
        ApiError::InvalidArgument("SessionManager is required for typed memory entries".to_string())
    })?;
    // Python's `_dispatch_session_entry` raises "Session cache unavailable"
    // before anything else: no session_records upsert, no feedback LLM call.
    if !base_session_manager.is_available() {
        return Err(cognee_session::SessionError::Disabled.into());
    }
    let sm = if let Some(llm) = llm.clone() {
        Arc::new(base_session_manager.as_ref().clone().with_llm(llm))
    } else {
        base_session_manager
    };

    // Best-effort pre-upsert of the session_records row. Mirrors Python's
    // try/except at remember.py:232-253 — any failure is logged at debug
    // and we proceed without a dataset_id binding.
    if let Some(ref database) = db
        && let Err(exc) = SessionLifecycleDb::ensure_and_touch_session(
            database.as_ref(),
            session_id,
            owner_id,
            None,
        )
        .await
    {
        debug!(
            session_id = session_id,
            "remember_entry: pre-upsert session_record failed (non-fatal): {exc}"
        );
    }

    let user_id_str = owner_id.to_string();
    let entry_type_str = entry.type_str();

    let mut status = RememberStatus::SessionStored;
    let entry_id: Option<String>;
    let mut error: Option<String> = None;

    match entry {
        MemoryEntry::Qa(q) => {
            let QAEntry {
                question,
                answer,
                context,
                feedback_text,
                feedback_score,
                used_graph_element_ids,
            } = q;

            let qa_id = sm
                .save_qa(
                    Some(session_id),
                    Some(&user_id_str),
                    &question,
                    &answer,
                    Some(context.as_str()),
                    // used_graph_element_ids handled by the follow-up update below
                    // when the raw JSON value needs schema-validation first.
                    None,
                )
                .await?;

            // Follow-up partial update when any of the optional fields are
            // present. Composes existing methods rather than widening
            // `save_qa`'s public signature (see task §3 rationale).
            if feedback_text.is_some()
                || feedback_score.is_some()
                || used_graph_element_ids.is_some()
            {
                let used_graph_element_ids_typed = match used_graph_element_ids {
                    Some(value) => Some(Some(serde_json::from_value(value).map_err(|e| {
                        ApiError::InvalidArgument(format!(
                            "used_graph_element_ids does not match {{node_ids:[], edge_ids:[]}} shape: {e}"
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

                sm.update_qa(Some(session_id), Some(&user_id_str), &qa_id, updates)
                    .await?;
            }

            entry_id = Some(qa_id);
        }

        MemoryEntry::Trace(t) => {
            let TraceEntry {
                origin_function,
                status: trace_status,
                method_params,
                method_return_value,
                memory_query,
                memory_context,
                error_message,
                generate_feedback_with_llm,
            } = t;

            let trace_id = sm
                .add_agent_trace_step(
                    &user_id_str,
                    Some(session_id),
                    &origin_function,
                    &trace_status,
                    &memory_query,
                    &memory_context,
                    method_params.unwrap_or(serde_json::Value::Null),
                    method_return_value,
                    &error_message,
                    generate_feedback_with_llm,
                )
                .await?;

            entry_id = Some(trace_id);
        }

        MemoryEntry::Feedback(f) => {
            let FeedbackEntry {
                qa_id,
                feedback_text,
                feedback_score,
            } = f;

            let ok = sm
                .add_feedback(
                    Some(session_id),
                    Some(&user_id_str),
                    &qa_id,
                    feedback_text.as_deref(),
                    feedback_score,
                )
                .await?;

            if !ok {
                status = RememberStatus::Errored;
                error = Some(format!(
                    "add_feedback: QA {qa_id} not found in session {session_id}"
                ));
            }
            // Python parity: entry_id is set to the input qa_id even on
            // not-found (remember.py:307: `result.entry_id = entry.qa_id`).
            entry_id = Some(qa_id);
        }
    }

    info!(
        session_id = session_id,
        entry_type = entry_type_str,
        entry_id = entry_id.as_deref().unwrap_or(""),
        status = ?status,
        "remember_entry: dispatched typed memory entry"
    );

    Ok(RememberResult {
        status,
        dataset_name: dataset_name.to_string(),
        dataset_id: None,
        session_ids: Some(vec![session_id.to_string()]),
        pipeline_run_id: None,
        elapsed_seconds: Some(start.elapsed().as_secs_f64()),
        content_hash: None,
        items_processed: 0,
        items: vec![],
        error,
        entry_type: Some(entry_type_str.to_string()),
        entry_id,
        cognify_result: None,
        memify_result: None,
        improve_launched: false,
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    // Python `remember.py`: `auto_improve = self_improvement and
    // auto_improve_enabled()`, the one gate of both paths.
    #[test]
    fn auto_improve_needs_self_improvement_and_auto_enabled() {
        let enabled = ImproveConfig::default();
        assert!(enabled.auto_enabled, "Python's default");
        let disabled = ImproveConfig {
            auto_enabled: false,
            ..ImproveConfig::default()
        };
        assert_eq!(
            auto_improve_config(true, Some(enabled.clone())),
            Some(enabled.clone())
        );
        assert_eq!(auto_improve_config(false, Some(enabled)), None);
        assert_eq!(auto_improve_config(true, Some(disabled)), None);
        assert_eq!(auto_improve_config(true, None), None);
    }

    #[test]
    fn remember_status_serde_roundtrip_errored() {
        let s = RememberStatus::Errored;
        let j = serde_json::to_string(&s).expect("serialize");
        assert_eq!(j, "\"PipelineRunErrored\"");
        let back: RememberStatus = serde_json::from_str(&j).expect("deserialize");
        assert_eq!(back, RememberStatus::Errored);
    }

    #[test]
    fn remember_status_serializes_to_pipeline_run_camelcase() {
        // Decision 15 (LIB-06): library status emits CamelCase
        // "PipelineRun*" / "SessionStored" — matches the
        // `cognee_core::PipelineRunStatus` family. HTTP-side translation
        // (E-01) maps these to Python's lowercase wire format.
        assert_eq!(
            serde_json::to_string(&RememberStatus::Started).expect("ser"),
            "\"PipelineRunStarted\""
        );
        assert_eq!(
            serde_json::to_string(&RememberStatus::Completed).expect("ser"),
            "\"PipelineRunCompleted\""
        );
        assert_eq!(
            serde_json::to_string(&RememberStatus::Errored).expect("ser"),
            "\"PipelineRunErrored\""
        );
        assert_eq!(
            serde_json::to_string(&RememberStatus::SessionStored).expect("ser"),
            "\"SessionStored\""
        );
    }

    #[test]
    fn remember_status_deserializes_from_pipeline_run_camelcase() {
        for (s, expected) in [
            ("\"PipelineRunStarted\"", RememberStatus::Started),
            ("\"PipelineRunCompleted\"", RememberStatus::Completed),
            ("\"PipelineRunErrored\"", RememberStatus::Errored),
            ("\"SessionStored\"", RememberStatus::SessionStored),
        ] {
            let got: RememberStatus = serde_json::from_str(s).expect("deserialize");
            assert_eq!(got, expected, "for input {s}");
        }
    }

    #[test]
    fn remember_status_from_pipeline_run_status_translation_table() {
        use cognee_core::pipeline::PipelineRunStatus;
        // Exhaustive match — adding a variant to PipelineRunStatus forces
        // this test to be updated.
        assert_eq!(
            RememberStatus::from(PipelineRunStatus::Initiated),
            RememberStatus::Started
        );
        assert_eq!(
            RememberStatus::from(PipelineRunStatus::Started),
            RememberStatus::Started
        );
        assert_eq!(
            RememberStatus::from(PipelineRunStatus::Completed),
            RememberStatus::Completed
        );
        assert_eq!(
            RememberStatus::from(PipelineRunStatus::Errored),
            RememberStatus::Errored
        );
    }

    #[test]
    fn remember_result_elapsed_seconds_serializes_as_null_when_none() {
        let mut r = sample_result(RememberStatus::Completed);
        r.elapsed_seconds = None;
        let v = r.to_dict();
        let obj = v.as_object().expect("object");
        assert!(
            obj.contains_key("elapsed_seconds"),
            "elapsed_seconds key should be present even when None (Python parity)"
        );
        assert!(
            obj.get("elapsed_seconds").is_some_and(|v| v.is_null()),
            "elapsed_seconds should serialize as null when None"
        );
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
            elapsed_seconds: Some(1.23),
            content_hash: None,
            items_processed: 0,
            items: Vec::new(),
            error: None,
            entry_type: None,
            entry_id: None,
            cognify_result: None,
            memify_result: None,
            improve_launched: false,
        }
    }
}
