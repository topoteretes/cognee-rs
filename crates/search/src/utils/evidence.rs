//! Structured evidence built from the objects already placed into a
//! completion's context.
//!
//! Port of `cognee/modules/retrieval/utils/evidence.py`
//! (`chunk_context_evidence`, `graph_context_evidence`). Nothing here touches
//! a database, vector store, embedding model or LLM. The references describe
//! `used_as_context` lineage only; they do not claim that every context
//! artifact supports a particular statement in the answer.
//!
//! Not ported here: `graph_source_evidence` (the `provenance_edge_evidence`
//! sidecar lookup that adds `supports_assertion` source chunks for graph
//! edges), which waits on a Rust producer for that table.

use std::collections::HashSet;

use serde_json::Value;
use uuid::Uuid;

use crate::types::{EvidenceKind, EvidenceReference, SearchItem};

/// Prefix of a v1 source ref key (Python
/// `infrastructure/databases/provenance/constants.py` `SOURCE_REF_PREFIX`).
const SOURCE_REF_PREFIX: &str = "source_ref:v1";

/// Python `make_source_ref_key`: the stable key of one owning
/// dataset/data-item pair.
pub fn make_source_ref_key(dataset_id: Uuid, data_id: Uuid) -> String {
    format!("{SOURCE_REF_PREFIX}:{dataset_id}:{data_id}")
}

/// Python's `_string`: `str(value).strip()`, `None` when null or blank.
/// Scalars stringify as Python does (`True`/`False`, JSON numbers as is);
/// containers carry no usable id and read as `None`.
fn py_string(value: Option<&Value>) -> Option<String> {
    let text = match value? {
        Value::Null | Value::Array(_) | Value::Object(_) => return None,
        Value::String(text) => text.trim().to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(number) => number.to_string(),
    };
    (!text.is_empty()).then_some(text)
}

/// A vector score as a JSON-friendly `f64`: widening an `f32` verbatim would
/// serialize `0.1` as `0.10000000149011612`, so go through its shortest
/// decimal form instead.
fn score_to_f64(score: f32) -> Option<f64> {
    if !score.is_finite() {
        return None;
    }
    score.to_string().parse().ok()
}

/// Describe the exact vector chunks that were available to a RAG completion
/// (Python `chunk_context_evidence`).
///
/// One `segment` reference per chunk, `rank` the chunk's position in
/// `context` (items skipped for lacking an id still use up a rank, as in
/// Python), deduplicated per `(source_ref_key | data_id | dataset, chunk id)`.
/// `score` is the retriever's own relevance value for the hit; Rust vector
/// hits carry a similarity where Python's carry a distance.
pub fn chunk_context_evidence(
    context: &[SearchItem],
    dataset_id: Option<Uuid>,
) -> Vec<EvidenceReference> {
    let normalized_dataset_id = dataset_id.map(|id| id.to_string());
    let mut references = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();

    for (rank, item) in context.iter().enumerate() {
        let payload = &item.payload;
        let Some(chunk_id) = item
            .id
            .map(|id| id.to_string())
            .or_else(|| py_string(payload.get("id")))
        else {
            continue;
        };

        let data_id = py_string(payload.get("document_id"));
        let source_ref_key = dataset_id
            .zip(data_id.as_deref())
            .and_then(|(dataset, data)| {
                Uuid::parse_str(data)
                    .ok()
                    .map(|data| make_source_ref_key(dataset, data))
            });
        let dedup_scope = source_ref_key
            .clone()
            .or_else(|| data_id.clone())
            .or_else(|| normalized_dataset_id.clone())
            .unwrap_or_default();
        if !seen.insert((dedup_scope, chunk_id.clone())) {
            continue;
        }

        let mut reference = EvidenceReference::new(EvidenceKind::Segment, chunk_id.clone());
        reference.dataset_id = normalized_dataset_id.clone();
        reference.source_ref_key = source_ref_key;
        reference.data_id = data_id;
        reference.chunk_id = Some(chunk_id);
        reference.chunk_index = payload.get("chunk_index").and_then(Value::as_i64);
        reference.document_name = py_string(payload.get("document_name"));
        reference.rank = i64::try_from(rank).ok();
        reference.score = item.score.and_then(score_to_f64);
        references.push(reference);
    }
    references
}

/// The edge's own object id when the item carries one, else Python's
/// deterministic `generate_edge_object_id`.
fn edge_object_id(payload: &Value, source: &str, target: &str, relationship: &str) -> String {
    let stored = py_string(payload.get("edge_object_id")).or_else(|| {
        py_string(
            payload
                .get("attributes")
                .and_then(|attributes| attributes.get("edge_object_id")),
        )
    });
    stored.unwrap_or_else(|| {
        cognee_utils::id_generation::generate_edge_object_id(source, target, relationship)
            .to_string()
    })
}

/// Describe the exact graph nodes and edges rendered into graph context
/// (Python `graph_context_evidence`).
///
/// Every context item is one triplet (`source_id`, `target_id`,
/// `relationship`). Distinct nodes come first as `graph_node` references in
/// order of first appearance (labelled with the node's name), then the
/// distinct edges as `graph_edge` references; ranks run on across both. An
/// item lacking a source or target id is skipped, one lacking a relationship
/// name still contributes its nodes.
pub fn graph_context_evidence(
    context: &[SearchItem],
    dataset_id: Option<Uuid>,
) -> Vec<EvidenceReference> {
    let normalized_dataset_id = dataset_id.map(|id| id.to_string());
    let mut nodes: Vec<(String, Option<String>)> = Vec::new();
    let mut edges: Vec<(String, String, String, String)> = Vec::new();
    let mut seen_node_ids: HashSet<String> = HashSet::new();
    let mut seen_edge_ids: HashSet<String> = HashSet::new();

    for item in context {
        let payload = &item.payload;
        let (Some(source_id), Some(target_id)) = (
            py_string(payload.get("source_id")),
            py_string(payload.get("target_id")),
        ) else {
            continue;
        };

        for (node_id, name_key) in [(&source_id, "source_name"), (&target_id, "target_name")] {
            if seen_node_ids.insert(node_id.clone()) {
                nodes.push((node_id.clone(), py_string(payload.get(name_key))));
            }
        }

        // Python: `relationship_type or relationship_name or edge_text`.
        let Some(relationship_name) = py_string(payload.get("relationship"))
            .or_else(|| py_string(payload.get("relationship_name")))
            .or_else(|| py_string(payload.get("edge_text")))
        else {
            continue;
        };
        let edge_id = edge_object_id(payload, &source_id, &target_id, &relationship_name);
        if !seen_edge_ids.insert(edge_id.clone()) {
            continue;
        }
        edges.push((edge_id, source_id, target_id, relationship_name));
    }

    let node_count = nodes.len();
    let mut references = Vec::with_capacity(node_count + edges.len());
    for (rank, (node_id, label)) in nodes.into_iter().enumerate() {
        let mut reference = EvidenceReference::new(EvidenceKind::GraphNode, node_id);
        reference.dataset_id = normalized_dataset_id.clone();
        reference.label = label;
        reference.rank = i64::try_from(rank).ok();
        references.push(reference);
    }
    for (rank, (edge_id, source_id, target_id, relationship_name)) in edges.into_iter().enumerate()
    {
        let mut reference = EvidenceReference::new(EvidenceKind::GraphEdge, edge_id);
        reference.dataset_id = normalized_dataset_id.clone();
        reference.source_node_id = Some(source_id);
        reference.target_node_id = Some(target_id);
        reference.relationship_name = Some(relationship_name);
        reference.rank = i64::try_from(node_count + rank).ok();
        references.push(reference);
    }
    references
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::types::EvidenceRole;

    fn chunk(id: Uuid, payload: Value, score: f32) -> SearchItem {
        SearchItem {
            id: Some(id),
            score: Some(score),
            payload,
        }
    }

    fn triplet(source: &str, target: &str, relationship: &str, extra: Value) -> SearchItem {
        let mut payload = json!({
            "source_id": source,
            "target_id": target,
            "relationship": relationship,
            "source_name": source.to_uppercase(),
            "target_name": target.to_uppercase(),
        });
        if let (Value::Object(base), Value::Object(extra)) = (&mut payload, extra) {
            base.extend(extra);
        }
        SearchItem {
            id: None,
            score: None,
            payload,
        }
    }

    /// `test_rag_context_evidence_uses_exact_retrieved_chunks`.
    #[test]
    fn rag_context_evidence_uses_exact_retrieved_chunks() {
        let dataset_id = Uuid::new_v4();
        let data_id = Uuid::new_v4();
        let chunk_id = Uuid::new_v4();
        let context = vec![chunk(
            chunk_id,
            json!({
                "document_id": data_id.to_string(),
                "document_name": "report.pdf",
                "chunk_index": 3,
                "text": "Revenue grew twelve percent.",
            }),
            0.125,
        )];

        let evidence = chunk_context_evidence(&context, Some(dataset_id));

        assert_eq!(evidence.len(), 1);
        let reference = &evidence[0];
        assert_eq!(reference.kind, EvidenceKind::Segment);
        assert_eq!(reference.role, EvidenceRole::UsedAsContext);
        assert_eq!(reference.artifact_id, chunk_id.to_string());
        assert_eq!(reference.dataset_id, Some(dataset_id.to_string()));
        assert_eq!(
            reference.source_ref_key,
            Some(format!("source_ref:v1:{dataset_id}:{data_id}"))
        );
        assert_eq!(reference.data_id, Some(data_id.to_string()));
        assert_eq!(reference.chunk_id, Some(chunk_id.to_string()));
        assert_eq!(reference.chunk_index, Some(3));
        assert_eq!(reference.document_name.as_deref(), Some("report.pdf"));
        assert_eq!(reference.rank, Some(0));
        assert_eq!(reference.score, Some(0.125));
    }

    #[test]
    fn chunk_evidence_without_dataset_or_data_id_has_no_source_ref_key() {
        let context = vec![chunk(Uuid::new_v4(), json!({"text": "t"}), 0.1)];
        let evidence = chunk_context_evidence(&context, None);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].dataset_id, None);
        assert_eq!(evidence[0].source_ref_key, None);
        // An f32 score keeps its short decimal form on the wire.
        assert_eq!(
            serde_json::to_value(&evidence[0]).unwrap()["score"],
            json!(0.1)
        );
    }

    #[test]
    fn chunk_evidence_deduplicates_and_keeps_rank_positions() {
        let id = Uuid::new_v4();
        let other = Uuid::new_v4();
        let context = vec![
            chunk(id, json!({}), 0.9),
            // Same chunk again: dropped, but it still used up rank 1.
            chunk(id, json!({}), 0.8),
            chunk(other, json!({}), 0.7),
            // No id anywhere: skipped, rank 3 is consumed.
            SearchItem {
                id: None,
                score: None,
                payload: json!({}),
            },
        ];
        let evidence = chunk_context_evidence(&context, None);
        let ranks: Vec<_> = evidence.iter().map(|r| r.rank).collect();
        assert_eq!(ranks, vec![Some(0), Some(2)]);
    }

    #[test]
    fn chunk_evidence_falls_back_to_payload_id_and_rejects_bool_index() {
        let context = vec![SearchItem {
            id: None,
            score: None,
            payload: json!({"id": "chunk-x", "chunk_index": true}),
        }];
        let evidence = chunk_context_evidence(&context, None);
        assert_eq!(evidence[0].artifact_id, "chunk-x");
        assert_eq!(evidence[0].chunk_index, None);
    }

    /// `test_graph_context_evidence_uses_exact_nodes_and_edge`.
    #[test]
    fn graph_context_evidence_uses_exact_nodes_and_edge() {
        let dataset_id = Uuid::new_v4();
        let context = vec![triplet(
            "node-a",
            "node-b",
            "relates_to",
            json!({"edge_object_id": "edge-1"}),
        )];

        let evidence = graph_context_evidence(&context, Some(dataset_id));

        let shape: Vec<_> = evidence
            .iter()
            .map(|r| (r.kind, r.artifact_id.as_str()))
            .collect();
        assert_eq!(
            shape,
            vec![
                (EvidenceKind::GraphNode, "node-a"),
                (EvidenceKind::GraphNode, "node-b"),
                (EvidenceKind::GraphEdge, "edge-1"),
            ]
        );
        assert!(
            evidence
                .iter()
                .all(|r| r.role == EvidenceRole::UsedAsContext)
        );
        assert!(
            evidence
                .iter()
                .all(|r| r.dataset_id == Some(dataset_id.to_string()))
        );
        assert_eq!(evidence[0].label.as_deref(), Some("NODE-A"));
        assert_eq!(evidence[1].label.as_deref(), Some("NODE-B"));
        assert_eq!(evidence[2].source_node_id.as_deref(), Some("node-a"));
        assert_eq!(evidence[2].target_node_id.as_deref(), Some("node-b"));
        assert_eq!(evidence[2].relationship_name.as_deref(), Some("relates_to"));
        // Ranks run on across nodes then edges.
        let ranks: Vec<_> = evidence.iter().map(|r| r.rank).collect();
        assert_eq!(ranks, vec![Some(0), Some(1), Some(2)]);
    }

    /// `test_graph_context_evidence_deduplicates_shared_nodes`.
    #[test]
    fn graph_context_evidence_deduplicates_shared_nodes() {
        let context = vec![
            triplet(
                "node-a",
                "node-b",
                "first",
                json!({"edge_object_id": "edge-1"}),
            ),
            triplet(
                "node-b",
                "node-c",
                "second",
                json!({"edge_object_id": "edge-2"}),
            ),
        ];
        let evidence = graph_context_evidence(&context, None);
        let ids = |kind| {
            evidence
                .iter()
                .filter(|r| r.kind == kind)
                .map(|r| r.artifact_id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(EvidenceKind::GraphNode), ["node-a", "node-b", "node-c"]);
        assert_eq!(ids(EvidenceKind::GraphEdge), ["edge-1", "edge-2"]);
    }

    #[test]
    fn graph_edge_without_stored_id_uses_the_deterministic_edge_object_id() {
        let context = vec![triplet("Alice", "Bob", "works at", json!({}))];
        let evidence = graph_context_evidence(&context, None);
        let edge = evidence
            .iter()
            .find(|r| r.kind == EvidenceKind::GraphEdge)
            .unwrap();
        // Python: uuid5(NAMESPACE_OID, "alice" + "works_at" + "bob" normalized).
        assert_eq!(edge.artifact_id, "1251b503-c597-5421-a919-c20fc3db093a");
    }

    #[test]
    fn graph_items_without_ids_or_relationship_degrade_like_python() {
        let no_target = SearchItem {
            id: None,
            score: None,
            payload: json!({"source_id": "a", "relationship": "r"}),
        };
        let no_relationship = SearchItem {
            id: None,
            score: None,
            payload: json!({"source_id": "a", "target_id": "b"}),
        };
        let evidence = graph_context_evidence(&[no_target, no_relationship], None);
        // The first is skipped outright; the second still yields its nodes.
        let kinds: Vec<_> = evidence.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            vec![EvidenceKind::GraphNode, EvidenceKind::GraphNode]
        );
    }

    #[test]
    fn empty_context_has_no_evidence() {
        assert!(chunk_context_evidence(&[], None).is_empty());
        assert!(graph_context_evidence(&[], None).is_empty());
    }
}
