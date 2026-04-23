//! Memify pipeline orchestration.
//!
//! The `memify` function extracts triplets from an existing knowledge graph
//! and indexes them into the vector database for semantic search.

use cognee_embedding::EmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_vector::VectorDB;
use tracing::info;
use uuid::Uuid;

use super::config::MemifyConfig;
use super::error::MemifyError;
use super::extract_triplets::extract_triplets_from_graph_db;
use super::index_triplets::{IndexResult, index_triplets};

/// Result of the memify pipeline.
#[derive(Debug, Clone)]
pub struct MemifyResult {
    /// Number of triplets extracted from the graph.
    pub triplet_count: usize,

    /// Details about vector indexing.
    pub index_result: IndexResult,
}

/// Run the memify pipeline: extract triplets from the graph and index them.
///
/// # Algorithm
/// 1. Validate configuration
/// 2. Extract triplets from the graph database
/// 3. If no triplets found, return early with zeros
/// 4. Index triplets into the vector database
/// 5. Return summary result
///
/// # Arguments
/// * `graph_db` - Graph database containing the knowledge graph
/// * `vector_db` - Vector database for storing triplet embeddings
/// * `embedding_engine` - Engine to generate text embeddings
/// * `dataset_id` - Optional dataset ID for metadata tagging
/// * `user_id` - Optional user ID for metadata tagging
/// * `tenant_id` - Optional tenant ID for metadata tagging
/// * `config` - Pipeline configuration
///
/// # Returns
/// A `MemifyResult` with counts of extracted and indexed triplets.
pub async fn memify(
    graph_db: &dyn GraphDBTrait,
    vector_db: &dyn VectorDB,
    embedding_engine: &dyn EmbeddingEngine,
    dataset_id: Option<Uuid>,
    user_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
    config: &MemifyConfig,
) -> Result<MemifyResult, MemifyError> {
    // 1. Validate configuration.
    config.validate()?;

    // 2. Extract triplets from the graph database (or use custom data).
    let triplets = if let Some(ref custom_data) = config.custom_data {
        // When custom data is provided, convert JSON values to Triplet objects.
        // Each value should be a JSON object with "source_node", "relationship_name",
        // and "target_node" fields. UUIDs are generated deterministically from the
        // text values.
        use cognee_models::Triplet;
        let mut custom_triplets = Vec::new();
        for value in custom_data {
            let source = value
                .get("source_node")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let relationship = value
                .get("relationship_name")
                .and_then(|v| v.as_str())
                .unwrap_or("related_to")
                .to_string();
            let target = value
                .get("target_node")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let source_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, source.to_lowercase().as_bytes());
            let target_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, target.to_lowercase().as_bytes());
            let text = format!("{source}-\u{203A}{relationship}-\u{203A}{target}");
            custom_triplets.push(
                Triplet::new(source_id, target_id, relationship, text).with_names(source, target),
            );
        }
        info!(
            "Using {} custom triplets instead of graph extraction",
            custom_triplets.len()
        );
        custom_triplets
    } else {
        extract_triplets_from_graph_db(graph_db, config).await?
    };

    // 3. If empty, return early with zeros.
    if triplets.is_empty() {
        info!("No triplets extracted from graph; nothing to index");
        return Ok(MemifyResult {
            triplet_count: 0,
            index_result: IndexResult {
                indexed_count: 0,
                batch_count: 0,
            },
        });
    }

    // 4. Index triplets into the vector database.
    let index_result = index_triplets(
        &triplets,
        vector_db,
        embedding_engine,
        dataset_id,
        user_id,
        tenant_id,
    )
    .await?;

    // 5. Log summary and return.
    info!(
        "Memify complete: {} triplets extracted, {} indexed",
        triplets.len(),
        index_result.indexed_count
    );

    Ok(MemifyResult {
        triplet_count: triplets.len(),
        index_result,
    })
}
