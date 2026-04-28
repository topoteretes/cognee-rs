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
