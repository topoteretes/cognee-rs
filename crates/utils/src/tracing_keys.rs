//! Span attribute key constants matching Python's
//! [`cognee/modules/observability/tracing.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/tracing.py).
//!
//! These keys are the source of truth for instrumentation across the workspace
//! so spans surfaced via `/api/v1/activity/spans` look identical to Python's
//! exporter. Cross-SDK parity tests assert byte-for-byte equality (P8).

/// Database backend identifier (e.g. `"sqlite"`, `"postgres"`).
pub const COGNEE_DB_SYSTEM: &str = "cognee.db.system";

/// Raw query text or canonical method name.
pub const COGNEE_DB_QUERY: &str = "cognee.db.query";

/// Number of rows returned by a query.
pub const COGNEE_DB_ROW_COUNT: &str = "cognee.db.row_count";

/// LLM model name (e.g. `"gpt-4o-mini"`).
pub const COGNEE_LLM_MODEL: &str = "cognee.llm.model";

/// LLM provider (`"openai"`, `"ollama"`, ...).
pub const COGNEE_LLM_PROVIDER: &str = "cognee.llm.provider";

/// Active search mode (`"GraphCompletion"`, `"RagCompletion"`, ...).
pub const COGNEE_SEARCH_TYPE: &str = "cognee.search.type";

/// Pipeline name (`"cognify_pipeline"`, `"add_pipeline"`, ...).
pub const COGNEE_PIPELINE_NAME: &str = "cognee.pipeline.name";

/// Per-task instrumentation key.
pub const COGNEE_PIPELINE_TASK_NAME: &str = "cognee.pipeline.task_name";

/// `remember()` / `improve()` operation mode (`"session"` vs `"permanent"`).
pub const COGNEE_OPERATION_MODE: &str = "cognee.operation.mode";

/// `recall()` query routing scope.
pub const COGNEE_RECALL_SCOPE: &str = "cognee.recall.scope";

/// `forget()` target.
pub const COGNEE_FORGET_TARGET: &str = "cognee.forget.target";

/// Dataset name in scope for the current operation.
pub const COGNEE_DATASET_NAME: &str = "cognee.dataset.name";

/// Session-aware operation identifier.
pub const COGNEE_SESSION_ID: &str = "cognee.session.id";

// --- Vector / DB extras ----------------------------------------------------

/// The vector collection name queried (e.g. `"DocumentChunk_text"`).
pub const COGNEE_VECTOR_COLLECTION: &str = "cognee.vector.collection";

/// Number of results returned by a vector search call. Distinct from
/// [`COGNEE_DB_ROW_COUNT`] — vector search has *similarity hits* rather
/// than *rows*. Mirrors Python's `cognee.vector.result_count` (LanceDB
/// adapter).
pub const COGNEE_VECTOR_RESULT_COUNT: &str = "cognee.vector.result_count";

// --- Search retrieval ------------------------------------------------------

/// The number of results returned by a retriever (search-orchestrator
/// level, distinct from [`COGNEE_VECTOR_RESULT_COUNT`] which is
/// adapter-level).
pub const COGNEE_RESULT_COUNT: &str = "cognee.result.count";

/// A short human-readable summary of the search result (truncated).
pub const COGNEE_RESULT_SUMMARY: &str = "cognee.result.summary";

/// The retriever class or struct name handling this request
/// (e.g. `"GraphCompletionRetriever"`).
pub const COGNEE_RETRIEVER: &str = "cognee.retrieval.retriever";

/// The natural-language query text (truncated to 500 chars for PII
/// control). Apply [`crate::redact::redact`] before recording.
pub const COGNEE_SEARCH_QUERY: &str = "cognee.search.query";

/// Recall result source — `"session"`, `"graph"`, or `"cloud"`.
pub const COGNEE_RECALL_SOURCE: &str = "cognee.recall.source";

/// Number of session Q&A entries that matched the keyword search.
pub const COGNEE_SESSION_ENTRY_COUNT: &str = "cognee.session.entry_count";

// --- Identity --------------------------------------------------------------

/// The user or tenant identifier for the request.
pub const COGNEE_USER_ID: &str = "cognee.user.id";

// --- Data lifecycle --------------------------------------------------------

/// The number of data items affected by a delete operation.
pub const COGNEE_DATA_ITEM_COUNT: &str = "cognee.data.item_count";
