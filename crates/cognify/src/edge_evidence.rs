//! Capture of `provenance_edge_evidence` rows: which chunk supports which
//! graph edge.
//!
//! Port of Python's `modules/provenance/edge_evidence/{buffer,capture}.py`
//! (called from `add_data_points.py:449`). Evidence is captured only where
//! document chunks are stored (the cognify storage task); edges written by
//! `improve()` enrichment, session bridging or conflict review carry none, as
//! in Python. `evidence_kind` is the extension point for adding them.
//!
//! Two kinds of support are recorded per chunk:
//!
//! - `extracted`: edges the chunk's graph extraction produced (Python's
//!   `chunk._provenance_edges`; here [`ArtifactProducers`], which remembers
//!   every chunk that produced each merged edge). That includes an edge an
//!   earlier run already wrote to the graph and this run only claims: Python
//!   appends to `_provenance_edges` before the existing-edge filter
//!   (`expand_with_nodes_and_edges.py:283-302`), so such a chunk still cites it;
//! - `structural`: edges of the storage batch rooted at the chunk, such as
//!   `contains` and `is_part_of`.
//!
//! An ontology edge carries no evidence, even though the chunk whose entity
//! pulled it in owns it: Python appends to `_provenance_edges` only in
//! `_add_extracted_edges` (`expand_with_nodes_and_edges.py:290-302`), never
//! in `_add_ontology_edges`. [`ArtifactProducers::edges`] lists extractors
//! only, so such a chunk is never cited for it.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use cognee_database::EdgeEvidence;
use cognee_graph::EdgeData;
use cognee_models::DocumentChunk;
use cognee_utils::id_generation::generate_edge_object_id;
use uuid::Uuid;

use crate::graph_integration::ArtifactProducers;

/// `evidence_kind` of an edge the chunk's extraction produced.
pub const KIND_EXTRACTED: &str = "extracted";
/// `evidence_kind` of an edge rooted at the chunk by the storage walk.
pub const KIND_STRUCTURAL: &str = "structural";

/// Who and when a batch of evidence rows is attributed to.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EvidenceScope {
    pub tenant_id: Option<Uuid>,
    pub user_id: Uuid,
    pub dataset_id: Uuid,
    pub pipeline_run_id: Option<Uuid>,
    pub captured_at: DateTime<Utc>,
}

/// Python `_evidence_id`: a uuid5 of the row's identity, so capturing the same
/// support twice is one row.
#[allow(clippy::too_many_arguments)]
fn evidence_id(
    scope: &EvidenceScope,
    data_id: Uuid,
    chunk_id: Uuid,
    edge_id: Uuid,
    evidence_kind: &str,
) -> Uuid {
    let empty = String::new();
    let parts = [
        scope
            .tenant_id
            .map_or_else(|| empty.clone(), |id| id.to_string()),
        scope.user_id.to_string(),
        scope.dataset_id.to_string(),
        data_id.to_string(),
        scope
            .pipeline_run_id
            .map_or_else(|| empty.clone(), |id| id.to_string()),
        chunk_id.to_string(),
        edge_id.to_string(),
        evidence_kind.to_string(),
    ];
    let encoded = parts.join("\u{1f}");
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("cognee:edge-evidence:v1:{encoded}").as_bytes(),
    )
}

/// Python's confidence filter: a JSON number that is not a bool.
fn confidence(
    properties: &HashMap<std::borrow::Cow<'static, str>, serde_json::Value>,
) -> Option<f64> {
    properties
        .get("confidence")
        .and_then(serde_json::Value::as_f64)
}

/// Build the evidence rows of one storage batch (Python
/// `ProvenanceBuffer.capture`).
///
/// `producers` names every edge the extraction produced (written this run or
/// already in the graph) with the chunks that produced it; `structural_edges`
/// are the edges the storage walk writes (Python's `graph_edges`, custom edges
/// included). Rows are deduplicated by id; order follows the chunks, then the
/// edge key.
pub(crate) fn build_edge_evidence<'a>(
    scope: &EvidenceScope,
    chunks: &[DocumentChunk],
    structural_edges: impl IntoIterator<Item = &'a EdgeData>,
    producers: &ArtifactProducers,
) -> Vec<EdgeEvidence> {
    let chunk_by_id: HashMap<Uuid, &DocumentChunk> =
        chunks.iter().map(|chunk| (chunk.base.id, chunk)).collect();

    // chunk id -> (kind, source, destination, relationship, confidence)
    type Support = (&'static str, Uuid, Uuid, String, Option<f64>);
    let mut by_chunk: HashMap<Uuid, Vec<Support>> = HashMap::new();
    // Sorted, so the rows (and their insert order) are deterministic.
    let mut produced: Vec<_> = producers.edges().collect();
    produced.sort_by(|a, b| a.0.cmp(b.0));
    for ((source, target, relationship), chunk_ids) in produced {
        for chunk_id in chunk_ids {
            by_chunk.entry(*chunk_id).or_default().push((
                KIND_EXTRACTED,
                *source,
                *target,
                relationship.clone(),
                None,
            ));
        }
    }
    for (source, target, relationship, properties) in structural_edges {
        let (Ok(source_id), Ok(target_id)) = (Uuid::parse_str(source), Uuid::parse_str(target))
        else {
            continue;
        };
        if chunk_by_id.contains_key(&source_id) {
            by_chunk.entry(source_id).or_default().push((
                KIND_STRUCTURAL,
                source_id,
                target_id,
                relationship.clone(),
                confidence(properties),
            ));
        }
    }

    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for chunk in chunks {
        let chunk_id = chunk.base.id;
        let Some(supports) = by_chunk.get(&chunk_id) else {
            continue;
        };
        for (kind, source, destination, relationship, confidence) in supports {
            let edge_id = generate_edge_object_id(
                &source.to_string(),
                &destination.to_string(),
                relationship,
            );
            let id = evidence_id(scope, chunk.document_id, chunk_id, edge_id, kind);
            if !seen.insert(id) {
                continue;
            }
            rows.push(EdgeEvidence {
                id,
                tenant_id: scope.tenant_id,
                user_id: scope.user_id,
                dataset_id: scope.dataset_id,
                data_id: chunk.document_id,
                pipeline_run_id: scope.pipeline_run_id,
                chunk_id,
                chunk_index: i32::try_from(chunk.chunk_index).ok(),
                edge_id,
                source_node_id: *source,
                destination_node_id: *destination,
                relationship_name: relationship.clone(),
                evidence_kind: (*kind).to_string(),
                source_task: chunk.base.source_task.clone(),
                confidence: *confidence,
                created_at: scope.captured_at,
            });
        }
    }
    rows
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use super::*;
    use crate::graph_integration::GraphEdgePair;
    use serde_json::json;

    fn chunk(index: usize, document_id: Uuid) -> DocumentChunk {
        let mut chunk = DocumentChunk::new(
            Uuid::new_v4(),
            "t".to_string(),
            1,
            index,
            String::new(),
            document_id,
        );
        chunk.base.source_task = Some("extract_chunks_from_documents".to_string());
        chunk
    }

    fn scope() -> EvidenceScope {
        EvidenceScope {
            tenant_id: None,
            user_id: Uuid::new_v4(),
            dataset_id: Uuid::new_v4(),
            pipeline_run_id: Some(Uuid::new_v4()),
            captured_at: Utc::now(),
        }
    }

    #[test]
    fn evidence_id_matches_pythons_uuid5_scheme() {
        // Python: uuid5(NAMESPACE_URL, "cognee:edge-evidence:v1:" + "\x1f".join(
        //   ("", user, dataset, data, "", chunk, edge, kind))) with every id the
        //   hyphenated str(UUID); fixed ids below so the value is reproducible.
        let id = |n: u128| Uuid::from_u128(n);
        let scope = EvidenceScope {
            tenant_id: None,
            user_id: id(1),
            dataset_id: id(2),
            pipeline_run_id: None,
            captured_at: Utc::now(),
        };
        let got = evidence_id(&scope, id(3), id(4), id(5), "extracted");
        let encoded = [
            "",
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000002",
            "00000000-0000-0000-0000-000000000003",
            "",
            "00000000-0000-0000-0000-000000000004",
            "00000000-0000-0000-0000-000000000005",
            "extracted",
        ]
        .join("\u{1f}");
        assert_eq!(
            got,
            Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("cognee:edge-evidence:v1:{encoded}").as_bytes()
            )
        );
    }

    #[test]
    fn extracted_and_structural_support_are_captured_per_chunk() {
        let document = Uuid::new_v4();
        let (c0, c1) = (chunk(0, document), chunk(1, document));
        let (alice, bob) = (Uuid::new_v4(), Uuid::new_v4());
        let edge = GraphEdgePair::new(alice, bob, "works_with");

        // Both chunks produced the merged edge; only chunk 0 contains alice.
        let mut producers = ArtifactProducers::default();
        producers.record_edge(edge.dedup_key(), c0.base.id);
        producers.record_edge(edge.dedup_key(), c1.base.id);
        let contains: EdgeData = (
            c0.base.id.to_string(),
            alice.to_string(),
            "contains".to_string(),
            HashMap::from([("confidence".into(), json!(0.5))]),
        );
        // Not chunk-rooted: ignored.
        let unrelated: EdgeData = (
            alice.to_string(),
            bob.to_string(),
            "knows".to_string(),
            HashMap::new(),
        );

        let rows = build_edge_evidence(
            &scope(),
            &[c0.clone(), c1.clone()],
            &[contains, unrelated],
            &producers,
        );

        let shape: Vec<_> = rows
            .iter()
            .map(|r| (r.chunk_id, r.evidence_kind.as_str(), r.chunk_index))
            .collect();
        assert_eq!(
            shape,
            vec![
                (c0.base.id, "extracted", Some(0)),
                (c0.base.id, "structural", Some(0)),
                (c1.base.id, "extracted", Some(1)),
            ]
        );
        let extracted = &rows[0];
        assert_eq!(
            extracted.edge_id,
            generate_edge_object_id(&alice.to_string(), &bob.to_string(), "works_with")
        );
        assert_eq!(extracted.data_id, document);
        assert_eq!(extracted.confidence, None);
        assert_eq!(
            extracted.source_task.as_deref(),
            Some("extract_chunks_from_documents")
        );
        assert_eq!(rows[1].confidence, Some(0.5));
        assert_eq!(rows[1].destination_node_id, alice);
    }

    #[test]
    fn a_repeated_support_is_one_row_and_an_unproducing_chunk_has_none() {
        let (c0, c1) = (chunk(0, Uuid::new_v4()), chunk(1, Uuid::new_v4()));
        let edge = GraphEdgePair::new(Uuid::new_v4(), Uuid::new_v4(), "r");
        let mut producers = ArtifactProducers::default();
        producers.record_edge(edge.dedup_key(), c0.base.id);
        producers.record_edge(edge.dedup_key(), c0.base.id);
        let scope = scope();

        let rows = build_edge_evidence(&scope, &[c0.clone(), c1], &[], &producers);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].chunk_id, c0.base.id);
        assert!(build_edge_evidence(&scope, &[c0], &[], &ArtifactProducers::default()).is_empty());
    }

    /// Python appends to `_provenance_edges` before the existing-edge filter
    /// (`expand_with_nodes_and_edges.py:283-302`): an edge an earlier run
    /// already wrote, which this run only claims (it is in the producers but
    /// not in the batch's written edges), is still cited by its chunk.
    #[test]
    fn an_edge_already_in_the_graph_is_still_evidence_of_its_chunk() {
        let c0 = chunk(0, Uuid::new_v4());
        let claimed = GraphEdgePair::new(Uuid::new_v4(), Uuid::new_v4(), "works_with");
        let mut producers = ArtifactProducers::default();
        producers.record_edge(claimed.dedup_key(), c0.base.id);

        let rows = build_edge_evidence(&scope(), std::slice::from_ref(&c0), &[], &producers);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].evidence_kind, KIND_EXTRACTED);
        assert_eq!(
            rows[0].edge_id,
            generate_edge_object_id(
                &claimed.source_entity_id.to_string(),
                &claimed.target_entity_id.to_string(),
                "works_with"
            )
        );
    }

    /// A chunk that only pulled an edge in from the ontology owns it but is
    /// not evidence for it; Python's `_add_ontology_edges` never appends to
    /// `_provenance_edges`. A chunk that also extracted the same edge still
    /// cites it.
    #[test]
    fn an_ontology_edge_is_not_evidence_of_the_chunk_that_pulled_it() {
        let (puller, extractor) = (chunk(0, Uuid::new_v4()), chunk(1, Uuid::new_v4()));
        let is_a = GraphEdgePair::new(Uuid::new_v4(), Uuid::new_v4(), "is_a");
        let chunks = [puller.clone(), extractor.clone()];

        let mut producers = ArtifactProducers::default();
        producers.record_ontology_edge(is_a.dedup_key(), puller.base.id);
        assert!(build_edge_evidence(&scope(), &chunks, &[], &producers).is_empty());

        producers.record_edge(is_a.dedup_key(), extractor.base.id);
        let rows = build_edge_evidence(&scope(), &chunks, &[], &producers);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].chunk_id, extractor.base.id);
        assert_eq!(rows[0].evidence_kind, KIND_EXTRACTED);
    }
}
