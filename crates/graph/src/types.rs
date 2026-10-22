//! Type definitions for graph database operations.
//!
//! Type aliases for graph data structures:
//! - NodeData: arbitrary key-value properties
//! - EdgeData: (source_id, target_id, relationship_name, properties)
//! - GraphNode: (node_id, properties)

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;

/// Node data: arbitrary key-value properties
/// Uses Cow<'static, str> for keys to avoid allocating static strings
pub type NodeData = HashMap<Cow<'static, str>, serde_json::Value>;

/// Graph node: (node_id, properties)
pub type GraphNode = (String, NodeData);

/// Edge data: (source_id, target_id, relationship_name, properties)
pub type EdgeData = (
    String,
    String,
    String,
    HashMap<Cow<'static, str>, serde_json::Value>,
);

/// Parse a `created_at` / `updated_at` property into a UTC timestamp for the
/// adapters' dedicated audit columns.
///
/// [`cognee_models::DataPoint`] declares both as `i64` **milliseconds since the
/// epoch** (matching Python's
/// `DataPoint.created_at: int = int(datetime.now(timezone.utc).timestamp() * 1000)`),
/// so the epoch-ms form is the one every real write carries. RFC 3339 strings are
/// accepted too because hand-rolled node structs — the plain `#[derive(Serialize)]`
/// structs used by tests and pre-`DataPoint` call sites — emit
/// `Utc::now().to_rfc3339()`.
///
/// Returns `None` when the value is missing or is neither shape, letting the
/// caller substitute the write time.
///
/// Public so graph adapters outside this crate store audit timestamps by the
/// same rule as the built-in ones.
pub fn parse_audit_timestamp(
    value: Option<&serde_json::Value>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    match value? {
        // `as_i64` never matches a JSON bool, so `true` cannot become a date.
        serde_json::Value::Number(number) => {
            chrono::DateTime::from_timestamp_millis(number.as_i64()?)
        }
        serde_json::Value::String(text) => chrono::DateTime::parse_from_rfc3339(text)
            .ok()
            .map(|parsed| parsed.with_timezone(&chrono::Utc)),
        _ => None,
    }
}

/// Structured graph edge for easier construction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    /// Source node ID
    pub source_id: String,
    /// Target node ID
    pub target_id: String,
    /// Relationship name (edge label)
    pub relationship_name: String,
    /// Edge properties
    pub properties: HashMap<Cow<'static, str>, serde_json::Value>,
}

impl GraphEdge {
    /// Create a new graph edge
    pub fn new(source_id: String, target_id: String, relationship_name: String) -> Self {
        Self {
            source_id,
            target_id,
            relationship_name,
            properties: HashMap::new(),
        }
    }

    /// Create a new graph edge with properties
    pub fn with_properties(
        source_id: String,
        target_id: String,
        relationship_name: String,
        properties: HashMap<Cow<'static, str>, serde_json::Value>,
    ) -> Self {
        Self {
            source_id,
            target_id,
            relationship_name,
            properties,
        }
    }

    /// Convert to EdgeData tuple
    pub fn to_edge_data(self) -> EdgeData {
        (
            self.source_id,
            self.target_id,
            self.relationship_name,
            self.properties,
        )
    }

    /// Create from EdgeData tuple
    pub fn from_edge_data(edge: EdgeData) -> Self {
        Self {
            source_id: edge.0,
            target_id: edge.1,
            relationship_name: edge.2,
            properties: edge.3,
        }
    }
}

impl From<GraphEdge> for EdgeData {
    fn from(edge: GraphEdge) -> Self {
        edge.to_edge_data()
    }
}

impl From<EdgeData> for GraphEdge {
    fn from(edge: EdgeData) -> Self {
        GraphEdge::from_edge_data(edge)
    }
}

#[cfg(all(test, any(feature = "ladybug", feature = "postgres")))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::parse_audit_timestamp;
    use serde_json::json;

    /// Regression: both adapters only tried `as_str().and_then(parse_rfc3339)`,
    /// so every `DataPoint`-shaped write (epoch-ms `i64`) silently fell back to
    /// `Utc::now()` and the stored creation time was the write time.
    #[test]
    fn epoch_millis_are_accepted() {
        let parsed = parse_audit_timestamp(Some(&json!(1_768_164_683_000_i64)))
            .expect("epoch-ms integers must parse");
        assert_eq!(parsed.timestamp_millis(), 1_768_164_683_000);
    }

    #[test]
    fn rfc3339_strings_are_still_accepted() {
        let parsed = parse_audit_timestamp(Some(&json!("2026-01-11T20:51:23+00:00")))
            .expect("RFC 3339 strings must keep parsing");
        assert_eq!(parsed.timestamp_millis(), 1_768_164_683_000);
    }

    #[test]
    fn other_shapes_yield_none_so_the_caller_substitutes_now() {
        for value in [
            json!(null),
            // `true` must not become the epoch + 1 ms.
            json!(true),
            json!("not a date"),
            json!(1.5),
            json!([]),
        ] {
            assert!(
                parse_audit_timestamp(Some(&value)).is_none(),
                "unexpectedly parsed {value}"
            );
        }
        assert!(parse_audit_timestamp(None).is_none());
    }
}
