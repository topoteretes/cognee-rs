use std::collections::{HashMap, HashSet};

use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_vector::VectorDB;
use tracing::debug;

use crate::graph_retrieval::rank_edge_score;
use crate::types::SearchError;

const DEFAULT_WIDE_SEARCH_TOP_K: usize = 100;

/// Default cosine distance assigned to graph elements (nodes or edges) that have no
/// vector match for the current query. Matches Python's `triplet_distance_penalty` default
/// of 3.5 in `brute_force_triplet_search.py`.
pub const DEFAULT_TRIPLET_DISTANCE_PENALTY: f32 = 3.5;

/// Collections searched to find candidate graph nodes and edge-type distances.
/// Each entry is (data_type, field_name).
///
/// Note: "Entity_description" and "Triplet_embeddable_text" are intentionally excluded here
/// because they don't match the default Python collection set used in brute_force_triplet_search.
/// The "EdgeType_relationship_name" collection provides per-relationship-name distances.
const SEARCH_COLLECTIONS: [(&str, &str); 5] = [
    ("Entity", "name"),
    ("TextSummary", "text"),
    ("EntityType", "name"), // matches Python default collection list
    ("DocumentChunk", "text"),
    ("EdgeType", "relationship_name"),
];

#[derive(Debug, Clone)]
pub struct GraphRetrievalConfig {
    pub top_k: usize,
    pub wide_search_top_k: usize,
    /// Default cosine distance used for nodes/edges not found in vector search.
    /// Matches Python's `triplet_distance_penalty` semantics (default 3.5).
    pub triplet_distance_penalty: f32,
}

impl Default for GraphRetrievalConfig {
    fn default() -> Self {
        Self {
            top_k: 5,
            wide_search_top_k: DEFAULT_WIDE_SEARCH_TOP_K,
            triplet_distance_penalty: DEFAULT_TRIPLET_DISTANCE_PENALTY,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RankedGraphEdge {
    pub source_id: String,
    pub target_id: String,
    pub relationship_name: String,
    /// Total triplet distance (lower = better match).
    /// Sum of source_node_distance + edge_distance + target_node_distance.
    pub score: f32,
    pub source_name: String,
    pub target_name: String,
    /// Dataset ID of the source or target entity, for context scoping.
    pub dataset_id: Option<String>,
}

pub async fn brute_force_triplet_search(
    query: &str,
    vector_db: &dyn VectorDB,
    embedding_engine: &dyn EmbeddingEngine,
    graph_db: &dyn GraphDBTrait,
    config: &GraphRetrievalConfig,
) -> Result<Vec<RankedGraphEdge>, SearchError> {
    let query_vectors = embedding_engine.embed(&[query]).await?;
    let query_vector = query_vectors.into_iter().next().ok_or_else(|| {
        SearchError::InvalidInput("embedding engine returned no vectors".to_string())
    })?;

    // node_id -> cosine distance (lower = better)
    let mut node_distances = HashMap::<String, f32>::new();
    let mut candidate_node_ids = HashSet::<String>::new();
    let mut node_dataset_ids = HashMap::<String, String>::new();

    // relationship_name -> cosine distance (lower = better)
    // Keyed by relationship_name because edge_type_id is NOT stored in graph edge
    // properties by cognify. The EdgeType vector points store relationship_name in
    // their metadata (confirmed in cognify tasks.rs).
    let mut edge_type_distances = HashMap::<String, f32>::new();

    for (data_type, field_name) in SEARCH_COLLECTIONS {
        if !vector_db.has_collection(data_type, field_name).await? {
            debug!("vector collection {data_type}/{field_name} does not exist — skipping");
            continue;
        }

        let results = vector_db
            .search_similar(
                data_type,
                field_name,
                &query_vector,
                config.wide_search_top_k,
            )
            .await?;

        for result in results {
            // Convert Qdrant cosine similarity to cosine distance: distance = 1 - similarity
            let distance = 1.0 - result.score;

            if data_type == "EdgeType" && field_name == "relationship_name" {
                // Edge distances keyed by relationship_name from vector point metadata.
                // edge_type_id is NOT stored in graph edge properties, so we key by
                // relationship_name to match graph edges at scoring time.
                if let Some(rel_name) = result
                    .metadata
                    .get("relationship_name")
                    .and_then(|v| v.as_str())
                {
                    let entry = edge_type_distances
                        .entry(rel_name.to_string())
                        .or_insert(distance);
                    if distance < *entry {
                        *entry = distance;
                    }
                }
            } else {
                // Node distances keyed by vector point ID.
                // Use min to merge across collections (lower distance = better match).
                let node_id = result.id.to_string();
                candidate_node_ids.insert(node_id.clone());
                let entry = node_distances.entry(node_id.clone()).or_insert(distance);
                if distance < *entry {
                    *entry = distance;
                }
                if let Some(dataset_id) = result.metadata.get("dataset_id").and_then(|v| v.as_str())
                {
                    node_dataset_ids
                        .entry(node_id)
                        .or_insert_with(|| dataset_id.to_string());
                }
            }
        }
    }

    if candidate_node_ids.is_empty() {
        debug!("no candidate nodes found from vector search — returning empty");
        return Ok(vec![]);
    }

    let (graph_nodes, graph_edges) = graph_db.get_graph_data().await?;

    let node_names: HashMap<String, String> = graph_nodes
        .into_iter()
        .map(|(node_id, properties)| {
            let name = properties
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or(node_id.as_str())
                .to_string();
            (node_id, name)
        })
        .collect();

    let default_penalty = config.triplet_distance_penalty;

    let mut ranked_edges = graph_edges
        .into_iter()
        .filter_map(|(source_id, target_id, relationship_name, _properties)| {
            // Only consider edges where at least one endpoint was found in vector search
            if !candidate_node_ids.contains(&source_id) && !candidate_node_ids.contains(&target_id)
            {
                return None;
            }

            // Unmatched nodes get the default penalty distance (not 0.0)
            let source_dist = node_distances
                .get(&source_id)
                .copied()
                .unwrap_or(default_penalty);
            let target_dist = node_distances
                .get(&target_id)
                .copied()
                .unwrap_or(default_penalty);

            // Look up edge distance by relationship_name.
            // Unmatched edge types also get the default penalty distance.
            let edge_dist = edge_type_distances
                .get(&relationship_name)
                .copied()
                .unwrap_or(default_penalty);

            let source_name = node_names
                .get(&source_id)
                .cloned()
                .unwrap_or(source_id.clone());
            let target_name = node_names
                .get(&target_id)
                .cloned()
                .unwrap_or(target_id.clone());

            let dataset_id = node_dataset_ids
                .get(&source_id)
                .or_else(|| node_dataset_ids.get(&target_id))
                .cloned();

            Some(RankedGraphEdge {
                source_id,
                target_id,
                relationship_name,
                score: rank_edge_score(source_dist, target_dist, edge_dist),
                source_name,
                target_name,
                dataset_id,
            })
        })
        .collect::<Vec<_>>();

    // Sort ascending: lowest total distance = best match (matches Python heapq.nsmallest)
    ranked_edges.sort_by(|left, right| {
        left.score
            .partial_cmp(&right.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(ranked_edges.into_iter().take(config.top_k).collect())
}
