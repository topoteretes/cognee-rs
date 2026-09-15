//! The one derivation of an `EdgeType` vector-row id from a graph edge.
//!
//! Every retrieval lane that wants to line a `EdgeType_relationship_name` vector
//! hit up with the graph edge it came from has to recompute that row's point id,
//! because cognify does **not** store `edge_type_id` on the graph edge (Python
//! does not either — `CogneeGraph.add_edge` stamps it onto the in-memory edge at
//! load time, `CogneeGraph.py:58-61`).
//!
//! The row is built by `cognee-cognify`'s `add_data_points`:
//! `EdgeType::new_deterministic(retrieval_text, …)`, so its point id is
//! `EdgeType::deterministic_id(retrieval_text)` where the retrieval text is the
//! nonblank `edge_text` edge property falling back to the bare
//! `relationship_name` ([`EdgeType::retrieval_text`], a port of Python's
//! `get_edge_retrieval_text`).
//!
//! Each lane extracts `edge_text` from its own edge shape — a JSON payload in the
//! hybrid retriever, a typed `(src, tgt, rel, properties)` triple in graph
//! retrieval — and then calls [`edge_type_point_id`] for the hashing itself, so
//! the rule that has to match the writer exists in exactly one place.

use cognee_models::EdgeType;

/// Recompute the `EdgeType` vector-row point id for one graph edge.
///
/// `edge_text` is the edge's description when it has one; a `None`, empty or
/// whitespace-only value falls back to `relationship_name`. Returns `None` when
/// both are blank — cognify skips such edges when building `EdgeType` rows
/// (`tasks.rs`: `if edge_text.is_empty() { continue; }`), so there is no row to
/// match and the caller should treat the edge as unmatched.
pub(crate) fn edge_type_point_id(
    edge_text: Option<&str>,
    relationship_name: &str,
) -> Option<String> {
    let retrieval_text = EdgeType::retrieval_text(edge_text, relationship_name);
    if retrieval_text.is_empty() {
        None
    } else {
        Some(EdgeType::deterministic_id(&retrieval_text).to_string())
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
    fn prefers_the_edge_text_over_the_relationship_name() {
        let with_text = edge_type_point_id(Some("Alice works at Acme"), "works_at").unwrap();
        assert_eq!(
            with_text,
            EdgeType::deterministic_id("Alice works at Acme").to_string()
        );
        assert_ne!(
            with_text,
            EdgeType::deterministic_id("works_at").to_string(),
            "a described edge must NOT hash to the bare relation name"
        );
    }

    #[test]
    fn falls_back_to_the_relationship_name() {
        let expected = EdgeType::deterministic_id("works_at").to_string();
        assert_eq!(
            edge_type_point_id(None, "works_at").as_deref(),
            Some(&*expected)
        );
        assert_eq!(
            edge_type_point_id(Some("   "), "works_at").as_deref(),
            Some(&*expected)
        );
    }

    #[test]
    fn returns_none_when_both_sources_are_blank() {
        assert_eq!(edge_type_point_id(None, ""), None);
        assert_eq!(edge_type_point_id(Some("  "), "  "), None);
    }
}
