//! UserPreference - one user's preference node in a dataset's graph.
//!
//! Mirrors Python's `UserPreference` (`cognee/modules/user_preferences/models.py`).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::DataPoint;
use crate::has_datapoint::HasDataPoint;

/// At most one node per (user, dataset): what that user cares about.
///
/// `text` holds the stated preference lines only (newest first; the render
/// header is added at read time). `turn_counter` is the one clock of weight
/// decay: it advances once per processed session turn, rated or not.
/// `text_watermark` is the `created_at` of the newest session-context entry
/// folded into `text`.
///
/// An internal node (Python's `InternalDataPoint`): never embedded (no index
/// fields) and never surfaced in retrieval output (`is_internal`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserPreference {
    /// Base data point fields (id, timestamps, metadata, etc.)
    #[serde(flatten)]
    pub base: DataPoint,

    /// The user, as a string id.
    pub user_id: String,
    /// The dataset, as a string id.
    pub dataset_id: String,
    /// Stated preference lines, newest first.
    #[serde(default)]
    pub text: String,
    /// Session turns processed so far.
    #[serde(default)]
    pub turn_counter: i64,
    /// `created_at` of the newest entry folded into `text`.
    #[serde(default)]
    pub text_watermark: String,
    /// Always `true`: the marker of an internal node.
    #[serde(default = "internal")]
    pub is_internal: bool,
}

fn internal() -> bool {
    true
}

impl UserPreference {
    /// The node type.
    pub const TYPE: &'static str = "UserPreference";

    /// The preference node `id` of `user_id` in `dataset_id`.
    pub fn new(
        id: Uuid,
        user_id: impl Into<String>,
        dataset_id: impl Into<String>,
        text: impl Into<String>,
        turn_counter: i64,
        text_watermark: impl Into<String>,
    ) -> Self {
        let mut base = DataPoint::with_metadata(
            Self::TYPE,
            None,
            HashMap::from([("index_fields".to_string(), serde_json::json!([]))]),
        );
        base.id = id;
        Self {
            base,
            user_id: user_id.into(),
            dataset_id: dataset_id.into(),
            text: text.into(),
            turn_counter,
            text_watermark: text_watermark.into(),
            is_internal: true,
        }
    }
}

impl HasDataPoint for UserPreference {
    fn data_point(&self) -> &DataPoint {
        &self.base
    }
    fn data_point_mut(&mut self) -> &mut DataPoint {
        &mut self.base
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
    use crate::is_internal_node;

    // Python `test_user_preference_dump_is_internal`.
    #[test]
    fn a_preference_node_is_internal_and_not_embedded() {
        let node = UserPreference::new(Uuid::nil(), "u", "d", "Be brief", 3, "w");
        let value = serde_json::to_value(&node).unwrap();
        assert!(is_internal_node(value.as_object().unwrap()));
        assert_eq!(value["type"], "UserPreference");
        assert_eq!(value["metadata"]["index_fields"], serde_json::json!([]));
        assert_eq!(value["turn_counter"], 3);
        assert_eq!(value["text_watermark"], "w");
    }
}
