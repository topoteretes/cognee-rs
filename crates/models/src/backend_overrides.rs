use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Per-call backend configuration overrides.
///
/// Mirrors Python's `vector_db_config` / `graph_db_config` parameters on
/// `add()`, `cognify()`, and `memify()`. When present, the pipeline should
/// use the specified backend configuration instead of the default.
///
/// For now the configuration is stored as a flat provider name + params map.
/// Actual backend instantiation from these configs is left to the caller or
/// a future factory function.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BackendOverrides {
    /// Optional per-call vector DB configuration.
    pub vector_db_config: Option<BackendConfig>,

    /// Optional per-call graph DB configuration.
    pub graph_db_config: Option<BackendConfig>,
}

/// Configuration for dynamically selecting/creating a backend.
///
/// Mirrors Python's dict-based `vector_db_config` / `graph_db_config`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    /// Provider name (e.g. `"qdrant"`, `"ladybug"`, `"pgvector"`).
    pub provider: String,

    /// Arbitrary provider-specific parameters.
    pub params: HashMap<String, serde_json::Value>,
}
