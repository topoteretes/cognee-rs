//! Semantic attribute constant names for cognee search telemetry.
//!
//! These constants match the Python SDK's `tracing.py` semantic attributes
//! and are intended to be used as span field names or event attributes so
//! that OpenTelemetry exporters can aggregate them consistently.

/// The cognee search type (e.g. "GraphCompletion", "Chunks").
pub const COGNEE_SEARCH_TYPE: &str = "cognee.search.type";

/// The number of results returned by a retriever.
pub const COGNEE_RESULT_COUNT: &str = "cognee.result.count";

/// A short human-readable summary of the result (truncated).
pub const COGNEE_RESULT_SUMMARY: &str = "cognee.result.summary";

/// The retriever class or struct name handling this request.
pub const COGNEE_RETRIEVER: &str = "cognee.retrieval.retriever";

/// The backing database system (e.g. "qdrant", "ladybug", "sqlite").
pub const COGNEE_DB_SYSTEM: &str = "cognee.db.system";

/// The LLM model identifier used for generation.
pub const COGNEE_LLM_MODEL: &str = "cognee.llm.model";

/// The vector collection name queried.
pub const COGNEE_VECTOR_COLLECTION: &str = "cognee.vector.collection";

/// The cognify pipeline task name.
pub const COGNEE_PIPELINE_TASK_NAME: &str = "cognee.pipeline.task_name";

/// The user or tenant identifier for the request.
pub const COGNEE_USER_ID: &str = "cognee.user.id";

/// The session identifier for multi-turn interactions.
pub const COGNEE_SESSION_ID: &str = "cognee.session.id";
