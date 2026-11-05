//! Structured evidence built from the objects already placed into a
//! completion's context.
//!
//! Port of `cognee/modules/retrieval/utils/evidence.py`
//! (`chunk_context_evidence`, `graph_context_evidence`). Nothing here touches
//! a database, vector store, embedding model or LLM. The references describe
//! `used_as_context` lineage only; they do not claim that every context
//! artifact supports a particular statement in the answer.
//!
//! [`graph_source_evidence`] is the one function here that reads a database:
//! it resolves graph edges to the chunks that support them through the
//! `provenance_edge_evidence` sidecar and adds `supports_assertion` segment
//! references. [`append_source_evidence_text`] renders those as the legacy
//! `Evidence:` text block.

use std::collections::HashSet;

use cognee_database::SearchHistoryDb;
use serde_json::Value;
use uuid::Uuid;

use crate::types::{EvidenceKind, EvidenceReference, EvidenceRole, SearchItem, SearchOutput};

/// Python `get_edge_evidence_records` defaults.
const SOURCE_PER_EDGE_LIMIT: usize = 5;
const SOURCE_TOTAL_LIMIT: usize = 50;
/// Python `append_source_evidence_text` lists at most this many sources.
const SOURCE_TEXT_LIMIT: usize = 5;

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

/// Resolve graph edges to their source chunks through the relational
/// evidence sidecar (Python `graph_source_evidence`,
/// `modules/retrieval/utils/evidence.py:176`).
///
/// Returns one `supports_assertion` segment reference per active supporting
/// chunk of the `graph_edge` references in `context_evidence`, ranked after
/// them. Never fails: references are optional, so a storage error (a database
/// that has not migrated yet, say) is logged at debug level and yields none,
/// as Python's lookup does.
pub async fn graph_source_evidence(
    database: &dyn SearchHistoryDb,
    context_evidence: &[EvidenceReference],
    dataset_id: Uuid,
) -> Vec<EvidenceReference> {
    let edge_ids: Vec<Uuid> = context_evidence
        .iter()
        .filter(|reference| reference.kind == EvidenceKind::GraphEdge)
        .filter_map(|reference| Uuid::parse_str(&reference.artifact_id).ok())
        .collect();
    if edge_ids.is_empty() {
        return Vec::new();
    }

    let records = match database
        .get_edge_evidence_records(
            dataset_id,
            &edge_ids,
            SOURCE_PER_EDGE_LIMIT,
            SOURCE_TOTAL_LIMIT,
        )
        .await
    {
        Ok(records) => records,
        Err(error) => {
            tracing::debug!(%error, "unable to resolve graph edge evidence");
            return Vec::new();
        }
    };

    let rank_offset = context_evidence.len();
    records
        .into_iter()
        .enumerate()
        .map(|(rank, record)| {
            let chunk_id = record.chunk_id.to_string();
            let mut reference = EvidenceReference::new(EvidenceKind::Segment, chunk_id.clone());
            reference.role = EvidenceRole::SupportsAssertion;
            reference.assertion_id = Some(record.edge_id.to_string());
            reference.dataset_id = Some(dataset_id.to_string());
            reference.source_ref_key = Some(make_source_ref_key(dataset_id, record.data_id));
            reference.data_id = Some(record.data_id.to_string());
            reference.chunk_id = Some(chunk_id);
            reference.chunk_index = record.chunk_index.map(i64::from);
            reference.document_name = record.document_name;
            reference.rank = i64::try_from(rank_offset + rank).ok();
            reference
        })
        .collect()
}

/// Append the causal source references to a text answer as a legacy
/// `Evidence:` block (Python `append_source_evidence_text`,
/// `evidence.py:219`).
///
/// Only `supports_assertion` segments count, at most five distinct
/// `(data_id, chunk_id)` of them. A text answer and a list of texts are
/// extended; any other output is returned as it came.
pub fn append_source_evidence_text(
    output: SearchOutput,
    evidence: &[EvidenceReference],
) -> SearchOutput {
    let mut bullets: Vec<String> = Vec::new();
    let mut seen: HashSet<(Option<&str>, Option<&str>)> = HashSet::new();
    for reference in evidence
        .iter()
        .filter(|reference| {
            reference.kind == EvidenceKind::Segment
                && reference.role == EvidenceRole::SupportsAssertion
        })
        .take(SOURCE_TEXT_LIMIT)
    {
        if !seen.insert((reference.data_id.as_deref(), reference.chunk_id.as_deref())) {
            continue;
        }
        let chunk_number = reference
            .chunk_index
            .map_or_else(|| "unknown".to_string(), |index| (index + 1).to_string());
        // Python `reference.document_name or "unknown"`: empty is unknown too.
        let document_name = reference
            .document_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or("unknown");
        let mut identifiers = Vec::new();
        if let Some(data_id) = reference.data_id.as_deref().filter(|id| !id.is_empty()) {
            identifiers.push(format!("data_id: {data_id}"));
        }
        if let Some(chunk_id) = reference.chunk_id.as_deref().filter(|id| !id.is_empty()) {
            identifiers.push(format!("chunk_id: {chunk_id}"));
        }
        let suffix = if identifiers.is_empty() {
            String::new()
        } else {
            format!(" ({})", identifiers.join(", "))
        };
        bullets.push(format!(
            "- chunk {chunk_number} of document {document_name}{suffix}"
        ));
    }
    if bullets.is_empty() {
        return output;
    }
    let block = format!("Evidence:\n{}", bullets.join("\n"));

    match output {
        SearchOutput::Text(text) => SearchOutput::Text(format!("{text}\n\n{block}")),
        SearchOutput::Texts(texts) => SearchOutput::Texts(
            texts
                .into_iter()
                .map(|text| format!("{text}\n\n{block}"))
                .collect(),
        ),
        other => other,
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

    // --- source evidence (provenance_edge_evidence sidecar) -------------

    use async_trait::async_trait;
    use cognee_database::{DatabaseError, EdgeEvidenceRecord, SearchHistoryDb, SearchHistoryEntry};

    /// A history database whose evidence lookup answers from a fixed result.
    struct EvidenceDb(Result<Vec<EdgeEvidenceRecord>, DatabaseError>);

    #[async_trait]
    impl SearchHistoryDb for EvidenceDb {
        async fn log_query(
            &self,
            _: &str,
            _: &str,
            _: Option<Uuid>,
            _: Option<Uuid>,
        ) -> Result<Uuid, DatabaseError> {
            Ok(Uuid::nil())
        }
        async fn log_result(
            &self,
            _: Uuid,
            _: &str,
            _: Option<Uuid>,
            _: Option<Uuid>,
        ) -> Result<Uuid, DatabaseError> {
            Ok(Uuid::nil())
        }
        async fn get_history(
            &self,
            _: Option<Uuid>,
            _: Option<usize>,
        ) -> Result<Vec<SearchHistoryEntry>, DatabaseError> {
            Ok(Vec::new())
        }
        async fn get_edge_evidence_records(
            &self,
            _: Uuid,
            _: &[Uuid],
            _: usize,
            _: usize,
        ) -> Result<Vec<EdgeEvidenceRecord>, DatabaseError> {
            self.0.clone()
        }
    }

    fn source(
        data_id: &str,
        chunk_id: &str,
        index: Option<i64>,
        name: Option<&str>,
    ) -> EvidenceReference {
        let mut reference = EvidenceReference::new(EvidenceKind::Segment, chunk_id);
        reference.role = EvidenceRole::SupportsAssertion;
        reference.data_id = Some(data_id.to_string());
        reference.chunk_id = Some(chunk_id.to_string());
        reference.chunk_index = index;
        reference.document_name = name.map(str::to_string);
        reference
    }

    #[tokio::test]
    async fn graph_source_evidence_maps_records_to_supporting_segments() {
        let dataset_id = Uuid::new_v4();
        let edge_id = Uuid::new_v4();
        let (data_id, chunk_id) = (Uuid::new_v4(), Uuid::new_v4());
        let db = EvidenceDb(Ok(vec![EdgeEvidenceRecord {
            edge_id,
            data_id,
            chunk_id,
            chunk_index: Some(4),
            document_name: Some("report.pdf".to_string()),
        }]));
        let context = graph_context_evidence(
            &[triplet(
                "a",
                "b",
                "r",
                json!({"edge_object_id": edge_id.to_string()}),
            )],
            Some(dataset_id),
        );

        let sources = graph_source_evidence(&db, &context, dataset_id).await;

        assert_eq!(sources.len(), 1);
        let reference = &sources[0];
        assert_eq!(reference.kind, EvidenceKind::Segment);
        assert_eq!(reference.role, EvidenceRole::SupportsAssertion);
        assert_eq!(reference.artifact_id, chunk_id.to_string());
        assert_eq!(reference.assertion_id, Some(edge_id.to_string()));
        assert_eq!(reference.dataset_id, Some(dataset_id.to_string()));
        assert_eq!(
            reference.source_ref_key,
            Some(format!("source_ref:v1:{dataset_id}:{data_id}"))
        );
        assert_eq!(reference.data_id, Some(data_id.to_string()));
        assert_eq!(reference.chunk_id, Some(chunk_id.to_string()));
        assert_eq!(reference.chunk_index, Some(4));
        assert_eq!(reference.document_name.as_deref(), Some("report.pdf"));
        // Ranked after the context references (2 nodes + 1 edge).
        assert_eq!(reference.rank, Some(3));
    }

    #[tokio::test]
    async fn graph_source_evidence_is_empty_without_edges_or_on_a_storage_error() {
        let dataset_id = Uuid::new_v4();
        let failing = EvidenceDb(Err(DatabaseError::QueryError("no such table".into())));
        let context = graph_context_evidence(
            &[triplet(
                "a",
                "b",
                "r",
                json!({"edge_object_id": Uuid::new_v4().to_string()}),
            )],
            None,
        );
        assert!(
            graph_source_evidence(&failing, &context, dataset_id)
                .await
                .is_empty()
        );

        // Only graph edges are looked up: a chunk-only context never asks.
        let panics = EvidenceDb(Ok(vec![EdgeEvidenceRecord {
            edge_id: Uuid::new_v4(),
            data_id: Uuid::new_v4(),
            chunk_id: Uuid::new_v4(),
            chunk_index: None,
            document_name: None,
        }]));
        let chunks = chunk_context_evidence(&[chunk(Uuid::new_v4(), json!({}), 0.5)], None);
        assert!(
            graph_source_evidence(&panics, &chunks, dataset_id)
                .await
                .is_empty()
        );
    }

    /// `test_get_retriever_output_appends_graph_source_evidence`'s block.
    #[test]
    fn source_text_names_chunk_document_and_ids_like_python() {
        let evidence = vec![
            // A context reference is never listed.
            EvidenceReference::new(EvidenceKind::Segment, "ctx"),
            source("data-1", "chunk-1", Some(0), Some("a.txt")),
            // Repeat of the same (data, chunk): listed once.
            source("data-1", "chunk-1", Some(0), Some("a.txt")),
            source("data-2", "chunk-2", None, None),
            // An empty name is unknown too (Python `document_name or "unknown"`).
            source("data-3", "chunk-3", Some(1), Some("")),
        ];
        let out = append_source_evidence_text(SearchOutput::Text("answer".into()), &evidence);
        assert!(matches!(&out, SearchOutput::Text(text) if text
            == "answer\n\nEvidence:\n\
                - chunk 1 of document a.txt (data_id: data-1, chunk_id: chunk-1)\n\
                - chunk unknown of document unknown (data_id: data-2, chunk_id: chunk-2)\n\
                - chunk 2 of document unknown (data_id: data-3, chunk_id: chunk-3)"));

        let bare = EvidenceReference {
            data_id: None,
            chunk_id: None,
            ..source("", "", None, None)
        };
        let out = append_source_evidence_text(SearchOutput::Text("a".into()), &[bare]);
        assert!(matches!(&out, SearchOutput::Text(text)
            if text == "a\n\nEvidence:\n- chunk unknown of document unknown"));
    }

    #[test]
    fn source_text_extends_every_text_and_leaves_other_output_alone() {
        let evidence = vec![source("d", "c", Some(1), Some("n"))];
        let block = "Evidence:\n- chunk 2 of document n (data_id: d, chunk_id: c)";

        let out = append_source_evidence_text(
            SearchOutput::Texts(vec!["x".into(), "y".into()]),
            &evidence,
        );
        assert!(matches!(&out, SearchOutput::Texts(texts)
            if texts == &[format!("x\n\n{block}"), format!("y\n\n{block}")]));

        let structured = SearchOutput::Structured(json!({"a": 1}));
        assert!(matches!(
            append_source_evidence_text(structured, &evidence),
            SearchOutput::Structured(value) if value == json!({"a": 1})
        ));
        // No sources: untouched.
        assert!(matches!(
            append_source_evidence_text(SearchOutput::Text("t".into()), &[]),
            SearchOutput::Text(text) if text == "t"
        ));
    }

    #[test]
    fn source_text_lists_at_most_five_sources() {
        let evidence: Vec<_> = (0..8)
            .map(|i| source(&format!("d{i}"), &format!("c{i}"), Some(i), Some("n")))
            .collect();
        let out = append_source_evidence_text(SearchOutput::Text("t".into()), &evidence);
        let SearchOutput::Text(text) = out else {
            panic!("text stays text");
        };
        assert_eq!(text.matches("\n- chunk").count(), 5);
    }
}
