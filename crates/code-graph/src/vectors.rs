//! The opt-in vector index of a code graph load (`index_vectors`).
//!
//! Port of what Python's `add_data_points(..., graph_only=False)` adds to a
//! code graph node write (`add_code_graph_data_points` with
//! `index_vectors=True`, `cognee/tasks/code_graph/extract_code_graph.py`):
//! after the graph write, each written node's index fields are embedded into
//! its `{type}_{field}` collection (`index_data_points`; every code node
//! indexes `name`, a `NodeSet` indexes nothing), and the retrieval texts of
//! the structural edges the write added are embedded into
//! `EdgeType_relationship_name` (`index_graph_edges`). The typed relations
//! (`calls`, `imports`, ...) are written straight through the graph engine
//! in Python, so they are not indexed, here either.
//!
//! The default load is graph-only and never reaches this module: no
//! embedding call, no vector collection.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use cognee_embedding::EmbeddingEngine;
use cognee_graph::EdgeData;
use cognee_models::{EdgeType, HasDataPoint, IndexRefs, index_schema_payload};
use cognee_vector::{BulkLoadGuard, VectorDB, VectorPoint};
use serde_json::Value;
use uuid::Uuid;

use crate::error::{CodeGraphError, CodeGraphResult};
use crate::models::{CODE_INDEX_FIELDS, CodeNode, CodeNodeType};

/// Where an `index_vectors` load embeds and writes its vectors.
#[derive(Clone)]
pub struct CodeGraphVectors {
    /// Embeds the index fields and the edge retrieval texts.
    pub embedding_engine: Arc<dyn EmbeddingEngine>,
    /// Receives the vectors.
    pub vector_db: Arc<dyn VectorDB>,
    /// The dataset the load writes into, recorded on each vector payload
    /// (`dataset_id`), as cognify's index pass records it.
    pub dataset_id: Option<Uuid>,
}

impl std::fmt::Debug for CodeGraphVectors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeGraphVectors")
            .field("dataset_id", &self.dataset_id)
            .finish_non_exhaustive()
    }
}

/// What one index pass wrote: points per `(type, field)` collection.
pub type IndexedCollections = BTreeMap<(String, String), usize>;

/// Index the nodes a code graph write stored and the structural edges it
/// wrote. Python's `index_data_points(nodes)` then
/// `index_graph_edges(edges)` inside `add_data_points`, run after the graph
/// write, so a node's vector never exists without its graph row.
///
/// Nodes are grouped by class into `{class}_name`; a node id given twice
/// keeps its first occurrence, as Python's storage walk
/// (`get_graph_from_model`, then `deduplicate_nodes_and_edges`) hands
/// `index_data_points` each node once, first occurrence first. Edge
/// texts are the edges' non-blank `edge_text`, else their relationship name
/// ([`EdgeType::retrieval_text`]); blank ones are dropped, and texts that
/// share an [`EdgeType`] id are written once, with their counts summed.
///
/// # Errors
///
/// [`CodeGraphError::Embedding`] when the embedding engine fails,
/// [`CodeGraphError::Vector`] when the vector store does.
pub async fn index_code_graph_vectors(
    vectors: &CodeGraphVectors,
    nodes: &[&CodeNode],
    edges: &[EdgeData],
) -> CodeGraphResult<IndexedCollections> {
    let vector_db = vectors.vector_db.as_ref();
    // A store that rejects the bulk-load hint is written without it, and the
    // scope is closed on success and failure alike (cognify's index pass).
    let bulk = match BulkLoadGuard::begin(vector_db).await {
        Ok(guard) => Some(guard),
        Err(error) => {
            tracing::warn!("code graph index: vector store rejected the bulk-load scope: {error}");
            None
        }
    };
    let indexed = index_in_scope(vectors, nodes, edges).await;
    if let Some(bulk) = bulk
        && let Err(error) = bulk.finish().await
    {
        tracing::warn!("code graph index: vector store failed to finish its bulk load: {error}");
    }
    indexed
}

/// One point to write: id, embedded text, payload source.
struct Entry<'a> {
    id: Uuid,
    text: String,
    source: &'a cognee_models::DataPoint,
}

async fn index_in_scope(
    vectors: &CodeGraphVectors,
    nodes: &[&CodeNode],
    edges: &[EdgeData],
) -> CodeGraphResult<IndexedCollections> {
    let mut stats = IndexedCollections::new();

    // Nodes: one collection per class and index field, each node once, its
    // first occurrence (the storage walk's dedup).
    let mut seen: HashSet<Uuid> = HashSet::with_capacity(nodes.len());
    let mut collections: BTreeMap<&'static str, Vec<Entry<'_>>> = BTreeMap::new();
    for node in nodes {
        let id = node.id();
        if !seen.insert(id) {
            continue;
        }
        let class_name = node
            .node_type()
            .map_or("CodeGraphEntity", CodeNodeType::class_name);
        collections.entry(class_name).or_default().push(Entry {
            id,
            text: node.name().to_string(),
            source: node.data_point(),
        });
    }
    for field in CODE_INDEX_FIELDS {
        for (class_name, entries) in &collections {
            let points: Vec<&Entry<'_>> = entries.iter().collect();
            write_collection(vectors, class_name, field, &points).await?;
            stats.insert((class_name.to_string(), field.to_string()), points.len());
        }
    }

    // Edges: one EdgeType per distinct retrieval text (Python's
    // `create_edge_type_datapoints`), folded by id.
    let edge_types = edge_types(edges, vectors.dataset_id);
    if !edge_types.is_empty() {
        let points: Vec<Entry<'_>> = edge_types
            .iter()
            .map(|edge_type| Entry {
                id: edge_type.base.id,
                text: edge_type.relationship_name.clone(),
                source: &edge_type.base,
            })
            .collect();
        let points: Vec<&Entry<'_>> = points.iter().collect();
        write_collection(vectors, "EdgeType", "relationship_name", &points).await?;
        stats.insert(
            ("EdgeType".to_string(), "relationship_name".to_string()),
            points.len(),
        );
    }
    Ok(stats)
}

/// The [`EdgeType`] rows of `edges`: one per distinct non-blank retrieval
/// text, counted, folded by id (two texts normalising to one id sum their
/// counts), in text order.
fn edge_types(edges: &[EdgeData], dataset_id: Option<Uuid>) -> Vec<EdgeType> {
    let texts = edges.iter().map(|(_, _, relationship, properties)| {
        EdgeType::retrieval_text(
            properties.get("edge_text").and_then(Value::as_str),
            relationship,
        )
    });
    EdgeType::fold_texts(texts, dataset_id)
}

/// Embed `points` and write them to the `data_type`/`field` collection,
/// creating it first when missing, a window at a time.
async fn write_collection(
    vectors: &CodeGraphVectors,
    data_type: &str,
    field: &str,
    points: &[&Entry<'_>],
) -> CodeGraphResult<()> {
    if points.is_empty() {
        return Ok(());
    }
    let engine = vectors.embedding_engine.as_ref();
    let vector_db = vectors.vector_db.as_ref();
    let vector_error =
        |error: cognee_vector::VectorDBError| CodeGraphError::Vector(error.to_string());
    if !vector_db
        .has_collection(data_type, field)
        .await
        .map_err(vector_error)?
    {
        vector_db
            .create_collection(data_type, field, engine.dimension())
            .await
            .map_err(vector_error)?;
    }
    vector_db
        .announce_bulk_write(data_type, field, points.len())
        .await
        .map_err(vector_error)?;
    let window = cognee_vector::write_window_points(engine.dimension());
    for window_points in points.chunks(window) {
        let texts: Vec<&str> = window_points
            .iter()
            .map(|point| point.text.as_str())
            .collect();
        let embedded = cognee_embedding::embed_in_batches(engine, &texts)
            .await
            .map_err(|error| CodeGraphError::Embedding(error.to_string()))?;
        let rows: Vec<VectorPoint> = window_points
            .iter()
            .zip(embedded)
            .map(|(point, vector)| VectorPoint {
                id: point.id,
                vector,
                metadata: index_schema_payload(
                    point.source,
                    point.text.clone(),
                    &IndexRefs::default(),
                    vectors.dataset_id,
                ),
            })
            .collect();
        vector_db
            .index_points(data_type, field, &rows)
            .await
            .map_err(vector_error)?;
    }
    tracing::info!(
        "Indexed {} code graph point(s) into {data_type}_{field}.",
        points.len()
    );
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use std::borrow::Cow;
    use std::collections::HashMap;

    use super::*;

    fn edge(relationship: &str, edge_text: Option<&str>) -> EdgeData {
        let mut properties = HashMap::new();
        if let Some(text) = edge_text {
            properties.insert(Cow::Borrowed("edge_text"), Value::from(text));
        }
        ("a".into(), "b".into(), relationship.into(), properties)
    }

    #[test]
    fn edge_types_count_texts_and_fall_back_to_the_relationship() {
        let edges = vec![
            edge("part_of", Some("foo part of shop.")),
            edge("part_of", Some("foo part of shop.")),
            edge("part_of", Some("  ")),
            edge("  ", None),
        ];
        let types = edge_types(&edges, None);
        let found: Vec<(&str, i32)> = types
            .iter()
            .map(|edge_type| (edge_type.relationship_name.as_str(), edge_type.count()))
            .collect();
        assert_eq!(found, vec![("foo part of shop.", 2), ("part_of", 1)]);
        assert_eq!(
            types[0].base.id,
            EdgeType::deterministic_id("foo part of shop.")
        );
    }
}
