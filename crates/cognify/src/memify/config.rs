//! Configuration for the memify pipeline.

use serde::{Deserialize, Serialize};

use super::error::MemifyError;

/// Configuration for the memify pipeline.
///
/// Controls how graph triplets are fetched, filtered, and embedded
/// during the memify process. Embedding batch size is not stored here;
/// it is read from `EmbeddingEngine::batch_size()` at runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemifyConfig {
    /// Number of triplets to fetch and process per batch.
    /// Default: 100
    pub triplet_batch_size: usize,

    /// Optional filter to restrict processing to nodes of a specific type.
    /// When `None`, all node types are included.
    pub node_type_filter: Option<String>,

    /// Optional filter to restrict processing to nodes with specific names.
    /// When `None`, all node names are included.
    pub node_name_filter: Option<Vec<String>>,

    /// Logical operator for combining `node_name_filter` entries.
    /// Must be `"OR"` or `"AND"`. Default: `"OR"`.
    pub node_name_filter_operator: String,
}

impl Default for MemifyConfig {
    fn default() -> Self {
        Self {
            triplet_batch_size: 100,
            node_type_filter: None,
            node_name_filter: None,
            node_name_filter_operator: "OR".to_string(),
        }
    }
}

impl MemifyConfig {
    /// Set the number of triplets processed per batch.
    pub fn with_triplet_batch_size(mut self, size: usize) -> Self {
        self.triplet_batch_size = size;
        self
    }

    /// Set the node type filter.
    pub fn with_node_type_filter(mut self, node_type: String) -> Self {
        self.node_type_filter = Some(node_type);
        self
    }

    /// Set the node name filter.
    pub fn with_node_name_filter(mut self, names: Vec<String>) -> Self {
        self.node_name_filter = Some(names);
        self
    }

    /// Set the logical operator for combining node name filter entries.
    pub fn with_node_name_filter_operator(mut self, operator: String) -> Self {
        self.node_name_filter_operator = operator;
        self
    }

    /// Validate configuration parameters.
    ///
    /// Returns an error if any parameters are invalid.
    pub fn validate(&self) -> Result<(), MemifyError> {
        if self.triplet_batch_size == 0 {
            return Err(MemifyError::ConfigError(
                "triplet_batch_size must be greater than 0".to_string(),
            ));
        }

        if self.node_name_filter_operator != "OR" && self.node_name_filter_operator != "AND" {
            return Err(MemifyError::ConfigError(
                "node_name_filter_operator must be \"OR\" or \"AND\"".to_string(),
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
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
    fn test_builder_methods() {
        let config = MemifyConfig::default()
            .with_triplet_batch_size(50)
            .with_node_type_filter("Person".to_string())
            .with_node_name_filter(vec!["Alice".to_string(), "Bob".to_string()])
            .with_node_name_filter_operator("AND".to_string());

        assert_eq!(config.triplet_batch_size, 50);
        assert_eq!(config.node_type_filter, Some("Person".to_string()));
        assert_eq!(
            config.node_name_filter,
            Some(vec!["Alice".to_string(), "Bob".to_string()])
        );
        assert_eq!(config.node_name_filter_operator, "AND");
    }

    #[test]
    fn test_validate_success() {
        let config = MemifyConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_zero_batch_size() {
        let config = MemifyConfig {
            triplet_batch_size: 0,
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(matches!(err, MemifyError::ConfigError(_)));
        assert!(err.to_string().contains("triplet_batch_size"));
    }

    #[test]
    fn test_validate_invalid_operator() {
        let config = MemifyConfig {
            node_name_filter_operator: "XOR".to_string(),
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(matches!(err, MemifyError::ConfigError(_)));
        assert!(err.to_string().contains("node_name_filter_operator"));
    }

    #[test]
    fn test_validate_valid_operators() {
        let config_or = MemifyConfig::default();
        assert!(config_or.validate().is_ok());

        let config_and = MemifyConfig {
            node_name_filter_operator: "AND".to_string(),
            ..Default::default()
        };
        assert!(config_and.validate().is_ok());
    }
}
