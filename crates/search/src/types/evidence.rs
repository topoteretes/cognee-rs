//! Structured identifiers for the artifacts a completion's context was built
//! from (`include_references`).
//!
//! Port of Python's `EvidenceReference`
//! (`cognee/modules/search/models/EvidenceReference.py`). A reference carries
//! identifiers and small locators only; source text stays in the vector or
//! graph store.
//!
//! **Wire shape.** Python's model has a camelCase alias generator, but every
//! wire path dumps it with `model_dump(mode="json")` and no `by_alias`
//! (`search.py:781-807`, `normalize_search_payload.py:129-130`), so the output
//! is snake_case with every field present (unset ones `null`). This type
//! serializes the same way; deserialization additionally accepts the camelCase
//! aliases (Python's `populate_by_name`).

use serde::{Deserialize, Serialize};

/// What kind of artifact an [`EvidenceReference`] points at (Python's
/// `EvidenceKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// A document chunk.
    Segment,
    /// A graph node.
    GraphNode,
    /// A graph edge.
    GraphEdge,
    /// A triplet (source, relationship, target) as one artifact.
    Triplet,
}

/// How the artifact relates to the answer (Python's `EvidenceRole`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRole {
    /// The artifact was placed in the completion's context.
    #[default]
    UsedAsContext,
    /// The artifact is a recorded source of an asserted fact.
    SupportsAssertion,
}

/// Stable identifier for an artifact that was included in completion context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceReference {
    pub kind: EvidenceKind,
    #[serde(alias = "artifactId")]
    pub artifact_id: String,
    #[serde(default)]
    pub role: EvidenceRole,

    #[serde(default, alias = "datasetId")]
    pub dataset_id: Option<String>,
    #[serde(default, alias = "sourceRefKey")]
    pub source_ref_key: Option<String>,

    #[serde(default, alias = "dataId")]
    pub data_id: Option<String>,
    #[serde(default, alias = "chunkId")]
    pub chunk_id: Option<String>,
    #[serde(default, alias = "chunkIndex")]
    pub chunk_index: Option<i64>,
    #[serde(default, alias = "documentName")]
    pub document_name: Option<String>,

    #[serde(default, alias = "sourceNodeId")]
    pub source_node_id: Option<String>,
    #[serde(default, alias = "targetNodeId")]
    pub target_node_id: Option<String>,
    #[serde(default, alias = "relationshipName")]
    pub relationship_name: Option<String>,
    #[serde(default, alias = "assertionId")]
    pub assertion_id: Option<String>,
    #[serde(default)]
    pub label: Option<String>,

    #[serde(default)]
    pub rank: Option<i64>,
    #[serde(default)]
    pub score: Option<f64>,
}

impl EvidenceReference {
    /// A reference of `kind` to `artifact_id` with the default role and no
    /// locators; the builders fill the rest.
    #[must_use]
    pub fn new(kind: EvidenceKind, artifact_id: impl Into<String>) -> Self {
        Self {
            kind,
            artifact_id: artifact_id.into(),
            role: EvidenceRole::default(),
            dataset_id: None,
            source_ref_key: None,
            data_id: None,
            chunk_id: None,
            chunk_index: None,
            document_name: None,
            source_node_id: None,
            target_node_id: None,
            relationship_name: None,
            assertion_id: None,
            label: None,
            rank: None,
            score: None,
        }
    }
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

    #[test]
    fn serializes_snake_case_with_every_field_present() {
        let reference = EvidenceReference::new(EvidenceKind::GraphEdge, "e1");
        assert_eq!(
            serde_json::to_value(&reference).unwrap(),
            json!({
                "kind": "graph_edge",
                "artifact_id": "e1",
                "role": "used_as_context",
                "dataset_id": null,
                "source_ref_key": null,
                "data_id": null,
                "chunk_id": null,
                "chunk_index": null,
                "document_name": null,
                "source_node_id": null,
                "target_node_id": null,
                "relationship_name": null,
                "assertion_id": null,
                "label": null,
                "rank": null,
                "score": null,
            })
        );
    }

    #[test]
    fn role_serializes_as_python_literals() {
        assert_eq!(
            serde_json::to_value(EvidenceRole::SupportsAssertion).unwrap(),
            json!("supports_assertion")
        );
        assert_eq!(
            serde_json::to_value(EvidenceKind::GraphNode).unwrap(),
            json!("graph_node")
        );
    }

    #[test]
    fn deserializes_camel_case_aliases_and_defaults() {
        let reference: EvidenceReference = serde_json::from_value(json!({
            "kind": "segment",
            "artifactId": "c1",
            "chunkIndex": 2,
            "documentName": "doc.txt",
        }))
        .unwrap();
        assert_eq!(reference.artifact_id, "c1");
        assert_eq!(reference.role, EvidenceRole::UsedAsContext);
        assert_eq!(reference.chunk_index, Some(2));
        assert_eq!(reference.document_name.as_deref(), Some("doc.txt"));
    }
}
