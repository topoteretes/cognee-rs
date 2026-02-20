use std::collections::{HashMap, HashSet};

use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_vector::VectorDB;

use crate::graph_retrieval::rank_edge_score;
use crate::types::SearchError;

const DEFAULT_WIDE_SEARCH_TOP_K: usize = 20;
const SEARCH_COLLECTIONS: [(&str, &str); 5] = [
    ("Entity", "name"),
    ("Entity", "description"),
    ("TextSummary", "text"),
    ("DocumentChunk", "text"),
    ("Triplet", "embeddable_text"),
];

#[derive(Debug, Clone)]
pub struct GraphRetrievalConfig {
    pub top_k: usize,
    pub wide_search_top_k: usize,
    pub triplet_distance_penalty: f32,
}

impl Default for GraphRetrievalConfig {
    fn default() -> Self {
        Self {
            top_k: 10,
            wide_search_top_k: DEFAULT_WIDE_SEARCH_TOP_K,
            triplet_distance_penalty: 0.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RankedGraphEdge {
    pub source_id: String,
    pub target_id: String,
    pub relationship_name: String,
    pub score: f32,
    pub source_name: String,
    pub target_name: String,
}

pub async fn brute_force_triplet_search<V: VectorDB, E: EmbeddingEngine, G: GraphDBTrait>(
    query: &str,
    vector_db: &V,
    embedding_engine: &E,
    graph_db: &G,
    config: &GraphRetrievalConfig,
) -> Result<Vec<RankedGraphEdge>, SearchError> {
    let query_vectors = embedding_engine.embed(&[query]).await?;
    let query_vector = query_vectors.into_iter().next().ok_or_else(|| {
        SearchError::InvalidInput("embedding engine returned no vectors".to_string())
    })?;

    let mut node_scores = HashMap::<String, f32>::new();
    let mut candidate_node_ids = HashSet::<String>::new();

    for (data_type, field_name) in SEARCH_COLLECTIONS {
        if !vector_db.has_collection(data_type, field_name).await? {
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
            match data_type {
                "Entity" => {
                    let entity_id = result.id.to_string();
                    candidate_node_ids.insert(entity_id.clone());
                    let entry = node_scores.entry(entity_id).or_insert(result.score);
                    *entry = entry.max(result.score);
                }
                "Triplet" => {
                    let penalty_adjusted_score = result.score - config.triplet_distance_penalty;
                    if let Some(source_id) =
                        result.metadata.get("source_id").and_then(|v| v.as_str())
                    {
                        candidate_node_ids.insert(source_id.to_string());
                        let entry = node_scores
                            .entry(source_id.to_string())
                            .or_insert(penalty_adjusted_score);
                        *entry = entry.max(penalty_adjusted_score);
                    }
                    if let Some(target_id) =
                        result.metadata.get("target_id").and_then(|v| v.as_str())
                    {
                        candidate_node_ids.insert(target_id.to_string());
                        let entry = node_scores
                            .entry(target_id.to_string())
                            .or_insert(penalty_adjusted_score);
                        *entry = entry.max(penalty_adjusted_score);
                    }
                }
                _ => {}
            }
        }
    }

    if candidate_node_ids.is_empty() {
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

    let mut ranked_edges = graph_edges
        .into_iter()
        .filter_map(|(source_id, target_id, relationship_name, _properties)| {
            if !candidate_node_ids.contains(&source_id) && !candidate_node_ids.contains(&target_id)
            {
                return None;
            }

            let source_score = node_scores.get(&source_id).copied().unwrap_or(0.0);
            let target_score = node_scores.get(&target_id).copied().unwrap_or(0.0);

            let source_name = node_names
                .get(&source_id)
                .cloned()
                .unwrap_or(source_id.clone());
            let target_name = node_names
                .get(&target_id)
                .cloned()
                .unwrap_or(target_id.clone());

            Some(RankedGraphEdge {
                source_id,
                target_id,
                relationship_name,
                score: rank_edge_score(source_score, target_score),
                source_name,
                target_name,
            })
        })
        .collect::<Vec<_>>();

    ranked_edges.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(ranked_edges.into_iter().take(config.top_k).collect())
}
