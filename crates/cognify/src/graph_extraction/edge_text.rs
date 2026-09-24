//! Default `edge_text` for graph edges.
//!
//! Port of the `edge_text` half of Python's `ensure_default_edge_properties`
//! (`cognee/modules/graph/utils/prepare_edges_for_storage.py:32-131`), which
//! `add_data_points` runs over every edge before writing it
//! (`add_data_points.py:128`). An edge with no nonblank `edge_text` gets
//! `"{source label} {relationship} {target label}."`, so every stored edge
//! carries a sentence. Downstream that sentence is:
//!
//! - the key of the edge's `EdgeType_relationship_name` row (retrieval text is
//!   `edge_text` first, `relationship_name` second — `EdgeType::retrieval_text`),
//!   so ids only line up with Python's when both stamp the same text;
//! - the bullet the hybrid retriever renders under an entity.
//!
//! Also here: the `"Document chunk mentions {name}: {description}"` text
//! Python puts on a chunk→entity `contains` edge (`_link_chunk_to_entity`,
//! `expand_with_nodes_and_edges.py:115-133`).
//!
//! The `edge_object_id` / `feedback_weight` defaults the Python function also
//! fills are not ported here.
//!
//! **One deliberate divergence.** Python labels an endpoint missing from its
//! node set with the raw node id (after a warning). Here such an edge is left
//! without `edge_text` instead: a sentence naming a UUID is useless to the
//! model, and the relationship-name fallback it keeps is what Rust wrote
//! before this stamp existed.

use std::borrow::Cow;
use std::collections::HashMap;

use cognee_graph::EdgeData;
use serde_json::Value;
use uuid::Uuid;

use crate::graph_integration::GraphEdgePair;

/// Maximum characters of a label taken from an index field.
///
/// Python `_trim_preview`'s `max_length=80`.
const PREVIEW_MAX_CHARS: usize = 80;

/// The property the stamped text is stored under.
const EDGE_TEXT_KEY: &str = "edge_text";

/// Prefix of the text on a chunk→entity `contains` edge.
///
/// Mirrors the search crate's `CONTAINS_FACT_PREFIX` (`facts.py:10`), which the
/// hybrid retriever strips back off when it turns such an edge into a fact.
pub const CHUNK_MENTIONS_PREFIX: &str = "Document chunk mentions ";

/// Endpoint labels, keyed by node id.
///
/// Python's `_get_node_label` takes the first nonblank `metadata.index_fields`
/// value, trimmed by `_trim_preview`. Every node type cognify writes has a
/// single index field (`DocumentChunk.text`, `TextSummary.text`,
/// `Document.name`, `Entity.name`, `EntityType.name`), so callers insert that
/// field's value and this map applies the trim.
#[derive(Debug, Default)]
pub struct EdgeLabels {
    labels: HashMap<Uuid, String>,
}

impl EdgeLabels {
    /// Record `id`'s label from its index-field value. A blank value records
    /// nothing, as Python's `_get_node_label` skips it.
    pub fn insert(&mut self, id: Uuid, index_field_value: &str) {
        let label = trim_preview(index_field_value);
        if !label.is_empty() {
            self.labels.insert(id, label);
        }
    }

    fn get(&self, id: &str) -> Option<&str> {
        let id = Uuid::parse_str(id).ok()?;
        self.labels.get(&id).map(String::as_str)
    }
}

/// Python `_trim_preview`: collapse whitespace runs to single spaces, then
/// keep the first [`PREVIEW_MAX_CHARS`] characters.
fn trim_preview(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(PREVIEW_MAX_CHARS)
        .collect()
}

/// Python `_build_fallback_edge_text`: `"{source} {relationship} {target}."`
/// with the trimmed relationship's `_` mapped to spaces, or `"related to"`
/// when the relationship is blank.
fn fallback_edge_text(source_label: &str, relationship_name: &str, target_label: &str) -> String {
    let relationship = relationship_name.trim().replace('_', " ");
    let relationship = if relationship.is_empty() {
        "related to"
    } else {
        relationship.as_str()
    };
    format!("{source_label} {relationship} {target_label}.")
}

/// The `edge_text` of a chunk→entity `contains` edge, or `None` when the
/// entity has no description (the edge then takes the generic fallback).
///
/// Python `_link_chunk_to_entity`: `"Document chunk mentions {name}:
/// {description}"`, with the description stripped.
pub fn chunk_mentions_text(entity_name: &str, description: &str) -> Option<String> {
    let description = description.trim();
    (!description.is_empty())
        .then(|| format!("{CHUNK_MENTIONS_PREFIX}{entity_name}: {description}"))
}

/// The sentence for an edge lacking `edge_text`, or `None` when either
/// endpoint has no label (see the module docs).
fn default_edge_text(
    labels: &EdgeLabels,
    source_id: &str,
    relationship_name: &str,
    target_id: &str,
) -> Option<String> {
    let source = labels.get(source_id)?;
    let target = labels.get(target_id)?;
    Some(fallback_edge_text(source, relationship_name, target))
}

/// Whether an existing `edge_text` value is nonblank text.
fn has_edge_text(value: Option<&str>) -> bool {
    value.is_some_and(|text| !text.trim().is_empty())
}

/// Stamp a default `edge_text` on every structural edge that lacks one.
///
/// Returns how many edges had to be left bare for want of a label.
pub fn ensure_default_edge_text(edges: &mut [EdgeData], labels: &EdgeLabels) -> usize {
    let mut unlabelled = 0;
    for (source_id, target_id, relationship_name, properties) in edges {
        if has_edge_text(properties.get(EDGE_TEXT_KEY).and_then(Value::as_str)) {
            continue;
        }
        match default_edge_text(labels, source_id, relationship_name, target_id) {
            Some(text) => {
                properties.insert(Cow::Borrowed(EDGE_TEXT_KEY), Value::String(text));
            }
            None => unlabelled += 1,
        }
    }
    unlabelled
}

/// [`ensure_default_edge_text`] for extracted edges, whose properties are
/// strings.
///
/// Returns how many edges had to be left bare for want of a label.
pub fn ensure_default_edge_text_for_pairs(
    edges: &mut [GraphEdgePair],
    labels: &EdgeLabels,
) -> usize {
    let mut unlabelled = 0;
    for edge in edges {
        if has_edge_text(edge.properties.get(EDGE_TEXT_KEY).map(String::as_str)) {
            continue;
        }
        match default_edge_text(
            labels,
            &edge.source_entity_id.to_string(),
            &edge.relationship_name,
            &edge.target_entity_id.to_string(),
        ) {
            Some(text) => {
                edge.properties.insert(EDGE_TEXT_KEY.to_string(), text);
            }
            None => unlabelled += 1,
        }
    }
    unlabelled
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use serde_json::json;

    fn edge(source: Uuid, target: Uuid, relationship: &str) -> EdgeData {
        (
            source.to_string(),
            target.to_string(),
            relationship.to_string(),
            HashMap::from([(Cow::Borrowed("updated_at"), json!("now"))]),
        )
    }

    fn edge_text(edge: &EdgeData) -> Option<&str> {
        edge.3.get(EDGE_TEXT_KEY).and_then(Value::as_str)
    }

    #[test]
    fn fallback_matches_pythons_build_fallback_edge_text() {
        assert_eq!(
            fallback_edge_text("Alice", "is_a", "person"),
            "Alice is a person."
        );
        // Only `_` is mapped and case is kept, as Python's `.replace("_", " ")`.
        assert_eq!(
            fallback_edge_text("Alice", " WORKS-AT ", "Acme"),
            "Alice WORKS-AT Acme."
        );
        assert_eq!(
            fallback_edge_text("Alice", "  ", "Acme"),
            "Alice related to Acme."
        );
    }

    #[test]
    fn labels_are_whitespace_collapsed_and_cut_to_80_chars() {
        let id = Uuid::new_v4();
        let mut labels = EdgeLabels::default();
        labels.insert(id, &format!("  Alice\n\tsat  {}", "x".repeat(100)));
        let label = labels.get(&id.to_string()).unwrap();
        assert!(label.starts_with("Alice sat x"), "{label}");
        assert_eq!(label.chars().count(), PREVIEW_MAX_CHARS);

        labels.insert(Uuid::new_v4(), "   ");
        assert_eq!(labels.labels.len(), 1);
    }

    #[test]
    fn chunk_mentions_text_needs_a_description() {
        assert_eq!(
            chunk_mentions_text("alice", "  A curious girl. "),
            Some("Document chunk mentions alice: A curious girl.".to_string())
        );
        assert_eq!(chunk_mentions_text("alice", "   "), None);
    }

    #[test]
    fn stamps_only_bare_edges_whose_endpoints_are_labelled() {
        let (chunk, alice, person, stranger) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let mut labels = EdgeLabels::default();
        labels.insert(chunk, "Alice sat by the March Hare.");
        labels.insert(alice, "alice");
        labels.insert(person, "person");

        let mut kept = edge(chunk, alice, "contains");
        kept.3.insert(
            Cow::Borrowed(EDGE_TEXT_KEY),
            json!("Document chunk mentions alice: A girl."),
        );
        let mut edges = vec![
            edge(alice, person, "is_a"),
            kept,
            edge(chunk, alice, "contains"),
            edge(alice, stranger, "knows"),
        ];

        let unlabelled = ensure_default_edge_text(&mut edges, &labels);

        assert_eq!(unlabelled, 1);
        assert_eq!(edge_text(&edges[0]), Some("alice is a person."));
        assert_eq!(
            edge_text(&edges[1]),
            Some("Document chunk mentions alice: A girl.")
        );
        assert_eq!(
            edge_text(&edges[2]),
            Some("Alice sat by the March Hare. contains alice.")
        );
        assert_eq!(edge_text(&edges[3]), None);
    }

    #[test]
    fn stamps_extracted_edges_with_blank_text() {
        let (alice, acme) = (Uuid::new_v4(), Uuid::new_v4());
        let mut labels = EdgeLabels::default();
        labels.insert(alice, "alice");
        labels.insert(acme, "acme");

        let mut blank = GraphEdgePair::new(alice, acme, "works_at".to_string());
        blank.add_property("edge_text", "");
        let mut described = GraphEdgePair::new(alice, acme, "founded".to_string());
        described.add_property("edge_text", "Alice founded Acme in 1999.");
        let mut edges = vec![blank, described];

        assert_eq!(ensure_default_edge_text_for_pairs(&mut edges, &labels), 0);
        assert_eq!(edges[0].properties["edge_text"], "alice works at acme.");
        assert_eq!(
            edges[1].properties["edge_text"],
            "Alice founded Acme in 1999."
        );
    }
}
