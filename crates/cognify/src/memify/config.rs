use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::error::MemifyError;

/// Opaque wrapper around an async task callback for the memify pipeline.
///
/// Mirrors Python's `extraction_tasks` / `enrichment_tasks` parameters.
/// The callback receives a JSON array of input data and returns a JSON array
/// of output data.
#[derive(Clone)]
#[allow(clippy::type_complexity)]
pub struct MemifyTask(
    pub  Arc<
        dyn Fn(
                Vec<serde_json::Value>,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = Result<Vec<serde_json::Value>, MemifyError>>
                        + Send,
                >,
            > + Send
            + Sync,
    >,
);

impl std::fmt::Debug for MemifyTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MemifyTask(…)")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemifyConfig {
    /// Batch size for reading triplets from the graph.
    /// Python: triplets_batch_size=100 in get_triplet_datapoints()
    ///
    /// In the Rust implementation this controls the chunk size used when
    /// batching embedding calls, NOT graph pagination (since Rust loads
    /// the full graph via get_graph_data()). Kept for future use if
    /// get_triplets_batch() is added to GraphDBTrait.
    pub triplet_batch_size: usize,

    /// Optional filter: only process nodes of this type.
    /// Python: memify(node_type=NodeSet) default
    /// When set along with node_name_filter, uses
    /// GraphDBTrait::get_nodeset_subgraph().
    pub node_type_filter: Option<String>,

    /// Optional filter: only process nodes with these names.
    /// Python: memify(node_name=None) default
    pub node_name_filter: Option<Vec<String>>,

    /// Operator for node_name filtering ("OR" or "AND").
    /// Python: get_memory_fragment(node_name_filter_operator="OR")
    pub node_name_filter_operator: String,

    /// Custom extraction tasks to run instead of (or in addition to)
    /// the default triplet extraction.
    /// Python: memify(extraction_tasks=...)
    #[serde(skip)]
    pub extraction_tasks: Option<Vec<MemifyTask>>,

    /// Custom enrichment tasks to run after extraction.
    /// Python: memify(enrichment_tasks=...)
    #[serde(skip)]
    pub enrichment_tasks: Option<Vec<MemifyTask>>,

    /// Custom input data. When provided, skip reading from the graph
    /// and use this data directly as input to the pipeline.
    /// Python: memify(data=...)
    #[serde(skip)]
    pub custom_data: Option<Vec<serde_json::Value>>,
}

impl Default for MemifyConfig {
    fn default() -> Self {
        Self {
            triplet_batch_size: 100,
            node_type_filter: None,
            node_name_filter: None,
            node_name_filter_operator: "OR".to_string(),
            extraction_tasks: None,
            enrichment_tasks: None,
            custom_data: None,
        }
    }
}

impl MemifyConfig {
    pub fn with_triplet_batch_size(mut self, size: usize) -> Self {
        self.triplet_batch_size = size;
        self
    }

    pub fn with_node_type_filter(mut self, node_type: String) -> Self {
        self.node_type_filter = Some(node_type);
        self
    }

    pub fn with_node_name_filter(mut self, names: Vec<String>) -> Self {
        self.node_name_filter = Some(names);
        self
    }

    pub fn with_node_name_filter_operator(mut self, op: String) -> Self {
        self.node_name_filter_operator = op;
        self
    }

    pub fn with_extraction_tasks(mut self, tasks: Vec<MemifyTask>) -> Self {
        self.extraction_tasks = Some(tasks);
        self
    }

    pub fn with_enrichment_tasks(mut self, tasks: Vec<MemifyTask>) -> Self {
        self.enrichment_tasks = Some(tasks);
        self
    }

    pub fn with_custom_data(mut self, data: Vec<serde_json::Value>) -> Self {
        self.custom_data = Some(data);
        self
    }

    pub fn validate(&self) -> Result<(), MemifyError> {
        if self.triplet_batch_size == 0 {
            return Err(MemifyError::ConfigError(
                "triplet_batch_size must be > 0".into(),
            ));
        }
        let op = self.node_name_filter_operator.as_str();
        if op != "OR" && op != "AND" {
            return Err(MemifyError::ConfigError(format!(
                "node_name_filter_operator must be \"OR\" or \"AND\", got \"{op}\""
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = MemifyConfig::default();
        assert_eq!(config.triplet_batch_size, 100);
        assert!(config.node_type_filter.is_none());
        assert!(config.node_name_filter.is_none());
        assert_eq!(config.node_name_filter_operator, "OR");
    }

    #[test]
    fn test_validate_valid_config() {
        let config = MemifyConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_zero_batch_size() {
        let config = MemifyConfig::default().with_triplet_batch_size(0);
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("triplet_batch_size must be > 0"),
            "expected batch size error, got: {err}"
        );
    }

    #[test]
    fn test_validate_invalid_operator() {
        let config = MemifyConfig::default().with_node_name_filter_operator("XOR".to_string());
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("node_name_filter_operator"),
            "expected operator error, got: {err}"
        );
    }

    #[test]
    fn test_validate_and_operator_accepted() {
        let config = MemifyConfig::default().with_node_name_filter_operator("AND".to_string());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_empty_node_names_vec_passes_validation() {
        // ANCHOR: pins the current behavior that an empty `node_name_filter`
        // vec passes `validate()` as-is (it is NOT coerced to `None`). If
        // future work adds such coercion, this test must be updated.
        let config = MemifyConfig::default().with_node_name_filter(vec![]);
        assert_eq!(config.node_name_filter, Some(vec![]));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_operator_case_sensitive() {
        for op in ["or", "Or", "aNd", "and", "xor", ""] {
            let config = MemifyConfig::default().with_node_name_filter_operator(op.to_string());
            let err = config.validate().unwrap_err();
            assert!(
                matches!(err, MemifyError::ConfigError(_)),
                "expected ConfigError for operator {op:?}, got: {err}"
            );
        }
        for op in ["OR", "AND"] {
            let config = MemifyConfig::default().with_node_name_filter_operator(op.to_string());
            assert!(
                config.validate().is_ok(),
                "expected operator {op:?} to pass validation"
            );
        }
    }

    #[test]
    fn test_config_large_batch_size_accepted() {
        let config = MemifyConfig::default().with_triplet_batch_size(10_000);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_builder_methods() {
        let config = MemifyConfig::default()
            .with_triplet_batch_size(50)
            .with_node_type_filter("Entity".to_string())
            .with_node_name_filter(vec!["Alice".to_string(), "Bob".to_string()])
            .with_node_name_filter_operator("AND".to_string());

        assert_eq!(config.triplet_batch_size, 50);
        assert_eq!(config.node_type_filter, Some("Entity".to_string()));
        assert_eq!(
            config.node_name_filter,
            Some(vec!["Alice".to_string(), "Bob".to_string()])
        );
        assert_eq!(config.node_name_filter_operator, "AND");
        assert!(config.validate().is_ok());
    }
}
