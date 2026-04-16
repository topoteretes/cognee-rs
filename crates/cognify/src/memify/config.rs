use serde::{Deserialize, Serialize};

use super::error::MemifyError;

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

    pub fn validate(&self) -> Result<(), MemifyError> {
        if self.triplet_batch_size == 0 {
            return Err(MemifyError::ConfigError(
                "triplet_batch_size must be > 0".into(),
            ));
        }
        Ok(())
    }
}
