//! E2E test: delete preview counts must match actual execution counts.
//!
//! Runs the full pipeline (add -> cognify -> memify -> delete) using real
//! backends (SQLite, Ladybug, Qdrant, ONNX embeddings, OpenAI-compatible LLM),
//! then verifies that `DeleteService::preview()` returns the same counts as
//! `DeleteService::execute()` for `DeleteScope::Dataset` with `DeleteMode::Soft`.
//!
//! Required environment variables:
//!   OPENAI_URL, OPENAI_TOKEN, OPENAI_MODEL,
//!   COGNEE_E2E_EMBED_MODEL_PATH, COGNEE_E2E_TOKENIZER_PATH

use std::sync::Arc;

use cognee_cognify::memify::{MemifyConfig, memify};
use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::{DatabaseConnection, DeleteDb, IngestDb, connect, initialize, ops};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_embedding::{EmbeddingEngine, config::OnnxEmbeddingConfig, onnx::OnnxEmbeddingEngine};
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_ingestion::AddPipeline;
use cognee_llm::{Llm, OpenAIAdapter};
use cognee_models::DataInput;
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::{QdrantAdapter, VectorDB};
use tempfile::TempDir;
use uuid::Uuid;

mod test_data;
mod test_utils;

use test_utils::require_env;

/// Extract the embedding model directory from `COGNEE_E2E_EMBED_MODEL_PATH`.
fn get_embedding_model_dir() -> String {
    if let Ok(model_path) = std::env::var("COGNEE_E2E_EMBED_MODEL_PATH")
        && let Some(parent) = std::path::Path::new(&model_path).parent()
    {
        return parent.to_string_lossy().to_string();
    }
    "./target/models".to_string()
}

#[tokio::test]
async fn test_delete_preview_counts_match_execution() {
    // ── Environment ─────────────────────────────────────────────────────────
    let _ = require_env("OPENAI_URL");
    let _ = require_env("OPENAI_TOKEN");
    let _ = require_env("OPENAI_MODEL");
    let _ = require_env("COGNEE_E2E_EMBED_MODEL_PATH");

    // ── Infrastructure setup ────────────────────────────────────────────────
    let temp_dir = TempDir::new().expect("temp dir");

    // Local file storage
    let storage: Arc<dyn StorageTrait> =
        Arc::new(LocalStorage::new(temp_dir.path().join("storage")));
    storage.initialize().await.expect("storage.initialize");

    // SQLite metadata database
    let db_path = temp_dir.path().join("cognee.db");
    std::fs::File::create(&db_path).expect("create sqlite db file");
    let db_url = format!("sqlite://{}", db_path.display());
    let db = connect(&db_url).await.expect("connect");
    initialize(&db).await.expect("initialize");
    let database: Arc<DatabaseConnection> = Arc::new(db);

    // Ladybug graph database
    let graph_path = temp_dir.path().join("graph").to_string_lossy().to_string();
    let graph_db: Arc<dyn GraphDBTrait> = Arc::new(
        LadybugAdapter::new(&graph_path)
            .await
            .expect("LadybugAdapter::new"),
    );
    graph_db.initialize().await.expect("graph_db.initialize");

    // Qdrant vector database (BGE-Small dimension = 384)
    let vector_db: Arc<dyn VectorDB> =
        Arc::new(QdrantAdapter::new(temp_dir.path().join("qdrant"), 384));

    // ONNX embedding engine
    let model_dir = get_embedding_model_dir();
    let embedding_engine: Arc<dyn EmbeddingEngine> =
        match OnnxEmbeddingEngine::new(OnnxEmbeddingConfig::bge_small(&model_dir)) {
            Ok(engine) => Arc::new(engine),
            Err(e) => {
                eprintln!("Skipping test: failed to load embedding model: {}", e);
                eprintln!(
                    "   Ensure model is at {}/BGE-Small-v1.5-model_quantized.onnx",
                    model_dir
                );
                return;
            }
        };

    // OpenAI-compatible LLM
    let llm: Arc<dyn Llm> = Arc::new(
        OpenAIAdapter::new(
            require_env("OPENAI_MODEL"),
            require_env("OPENAI_TOKEN"),
            Some(require_env("OPENAI_URL")),
        )
        .expect("OpenAIAdapter::new"),
    );

    let owner_id = Uuid::nil();
    let dataset_name = "preview_test";

    // ── Step 1: Ingest 2 documents ──────────────────────────────────────────
    let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>);
    let data_items = ingest
        .add(
            vec![
                DataInput::Text(test_data::TEST_TEXT_TECHCORP.to_string()),
                DataInput::Text(test_data::TEST_TEXT_RESEARCH.to_string()),
            ],
            dataset_name,
            owner_id,
            None,
        )
        .await
        .expect("ingest.add");

    assert_eq!(data_items.len(), 2, "Expected 2 ingested data items");
    println!("Ingested {} data item(s)", data_items.len());

    // ── Step 2: Cognify (summarization=false, triplet_embeddings=false) ─────
    let config = CognifyConfig::default()
        .with_summarization(false)
        .with_triplet_embeddings(false);

    let dataset = ops::datasets::get_dataset_by_name(&database, dataset_name, owner_id, None)
        .await
        .expect("get_dataset_by_name")
        .expect("dataset should exist after ingest");

    let cognify_result = match cognify(
        data_items,
        dataset.id,
        Some(owner_id),
        None,
        None,
        llm.clone() as Arc<dyn Llm>,
        storage.clone() as Arc<dyn StorageTrait>,
        graph_db.clone() as Arc<dyn GraphDBTrait>,
        vector_db.clone() as Arc<dyn VectorDB>,
        embedding_engine.clone() as Arc<dyn EmbeddingEngine>,
        Some(database.clone()),
        Arc::new(NoOpOntologyResolver::new()),
        &config,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Cognify failed (LLM may be unavailable): {e}");
            return;
        }
    };

    assert!(
        !cognify_result.chunks.is_empty(),
        "Cognify should produce chunks"
    );
    assert!(
        !cognify_result.entities.is_empty(),
        "Cognify should produce entities"
    );
    println!(
        "Cognify produced {} chunks, {} entities, {} edges",
        cognify_result.chunks.len(),
        cognify_result.entities.len(),
        cognify_result.edges.len(),
    );

    // ── Step 3: Memify to add Triplet vector points ─────────────────────────
    let memify_config = MemifyConfig::default();
    let memify_result = memify(
        graph_db.as_ref(),
        vector_db.as_ref(),
        embedding_engine.as_ref(),
        Some(dataset.id),
        None,
        None,
        &memify_config,
    )
    .await
    .expect("memify should succeed");

    println!(
        "Memify produced {} triplets, indexed {}",
        memify_result.triplet_count, memify_result.index_result.indexed_count,
    );

    // ── Step 4: Build DeleteService with graph+vector ───────────────────────
    let delete_service = DeleteService::new(
        storage.clone() as Arc<dyn StorageTrait>,
        database.clone() as Arc<dyn DeleteDb>,
    )
    .with_graph_db(graph_db.clone())
    .with_vector_db(vector_db.clone());

    let delete_request = DeleteRequest {
        scope: DeleteScope::Dataset {
            owner_id,
            dataset_name: dataset_name.to_string(),
        },
        mode: DeleteMode::Soft,
    };

    // ── Step 5: Preview ─────────────────────────────────────────────────────
    let preview = delete_service
        .preview(&delete_request)
        .await
        .expect("preview should succeed");

    println!("Delete preview: {preview:?}");

    // Assert preview counts are non-zero
    assert!(
        preview.datasets_to_delete >= 1,
        "preview should find at least 1 dataset to delete, got {}",
        preview.datasets_to_delete,
    );
    assert!(
        preview.data_to_delete >= 2,
        "preview should find at least 2 data items to delete, got {}",
        preview.data_to_delete,
    );
    assert!(
        preview.graph_nodes_to_delete > 0,
        "preview should find graph nodes to delete, got {}",
        preview.graph_nodes_to_delete,
    );
    assert!(
        preview.vector_points_to_delete > 0,
        "preview should find vector points to delete, got {}",
        preview.vector_points_to_delete,
    );

    // ── Step 6: Execute with the SAME request ───────────────────────────────
    let result = delete_service
        .execute(&delete_request)
        .await
        .expect("execute should succeed");

    println!("Delete result: {result:?}");

    // ── Step 7: Compare preview vs execution counts field by field ───────────
    assert_eq!(
        preview.datasets_to_delete, result.deleted_datasets,
        "datasets_to_delete ({}) != deleted_datasets ({})",
        preview.datasets_to_delete, result.deleted_datasets,
    );
    assert_eq!(
        preview.data_to_delete, result.deleted_data,
        "data_to_delete ({}) != deleted_data ({})",
        preview.data_to_delete, result.deleted_data,
    );
    assert_eq!(
        preview.dataset_links_to_delete, result.deleted_dataset_links,
        "dataset_links_to_delete ({}) != deleted_dataset_links ({})",
        preview.dataset_links_to_delete, result.deleted_dataset_links,
    );
    assert_eq!(
        preview.storage_files_to_delete, result.deleted_storage_files,
        "storage_files_to_delete ({}) != deleted_storage_files ({})",
        preview.storage_files_to_delete, result.deleted_storage_files,
    );
    assert_eq!(
        preview.graph_nodes_to_delete, result.deleted_graph_nodes,
        "graph_nodes_to_delete ({}) != deleted_graph_nodes ({})",
        preview.graph_nodes_to_delete, result.deleted_graph_nodes,
    );
    assert_eq!(
        preview.vector_points_to_delete, result.deleted_vector_points,
        "vector_points_to_delete ({}) != deleted_vector_points ({})",
        preview.vector_points_to_delete, result.deleted_vector_points,
    );
    assert_eq!(
        preview.provenance_nodes_to_delete, result.deleted_provenance_nodes,
        "provenance_nodes_to_delete ({}) != deleted_provenance_nodes ({})",
        preview.provenance_nodes_to_delete, result.deleted_provenance_nodes,
    );
    assert_eq!(
        preview.provenance_edges_to_delete, result.deleted_provenance_edges,
        "provenance_edges_to_delete ({}) != deleted_provenance_edges ({})",
        preview.provenance_edges_to_delete, result.deleted_provenance_edges,
    );
    assert_eq!(
        preview.search_queries_to_delete, result.deleted_search_queries,
        "search_queries_to_delete ({}) != deleted_search_queries ({})",
        preview.search_queries_to_delete, result.deleted_search_queries,
    );

    // ── Step 8: In Soft mode, orphan counts should be 0 ────────────────────
    assert_eq!(
        preview.orphaned_edge_types_to_delete, 0,
        "preview orphaned_edge_types_to_delete should be 0 in Soft mode",
    );
    assert_eq!(
        result.deleted_orphan_entities, 0,
        "deleted_orphan_entities should be 0 in Soft mode",
    );
    assert_eq!(
        result.deleted_orphan_entity_types, 0,
        "deleted_orphan_entity_types should be 0 in Soft mode",
    );
    assert_eq!(
        result.deleted_orphan_edge_types, 0,
        "deleted_orphan_edge_types should be 0 in Soft mode",
    );

    println!("All preview vs execution counts match!");
}
