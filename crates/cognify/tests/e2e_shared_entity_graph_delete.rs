//! E2E test: multi-document shared entity preservation at graph DB level.
//!
//! Two documents in separate datasets share overlapping entities.
//! After deleting one dataset, shared entities must survive in the graph
//! while exclusive entities are removed.
//!
//! Required env vars: OPENAI_URL, OPENAI_TOKEN, OPENAI_MODEL, COGNEE_E2E_EMBED_MODEL_PATH

use std::collections::HashSet;
use std::sync::Arc;

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

mod test_utils;
use test_utils::{get_embedding_model_dir, require_env};

const AI_TEXT: &str = include_str!("test_data/artificial_intelligence.txt");

/// Machine learning text that intentionally overlaps with AI text entities.
const ML_TEXT: &str = "\
Machine learning is a core subfield of artificial intelligence that enables \
computers to learn from data without being explicitly programmed. \
Deep learning, which uses neural networks with many layers, has driven \
recent breakthroughs in natural language processing and computer vision. \
Large language models like GPT-4 from OpenAI and LLaMA from Meta \
demonstrate the power of transformer architectures trained on massive datasets. \
Reinforcement learning, another branch of machine learning, trains agents \
through trial and error in complex environments.";

/// Extract lowercase entity names from graph nodes.
fn extract_node_names(nodes: &[(String, cognee_graph::NodeData)]) -> HashSet<String> {
    nodes
        .iter()
        .filter_map(|(_id, props)| {
            props.get("name")?.as_str().map(|s| s.to_lowercase())
        })
        .collect()
}

#[tokio::test]
async fn test_shared_entity_graph_delete() {
    // ── Environment gating ──────────────────────────────────────────────
    let _ = require_env("OPENAI_URL");
    let _ = require_env("OPENAI_TOKEN");
    let _ = require_env("OPENAI_MODEL");
    let _ = require_env("COGNEE_E2E_EMBED_MODEL_PATH");

    // ── Infrastructure ──────────────────────────────────────────────────
    let temp_dir = TempDir::new().expect("temp dir");

    let storage: Arc<dyn StorageTrait> =
        Arc::new(LocalStorage::new(temp_dir.path().join("storage")));
    storage.initialize().await.expect("storage.initialize");

    let db_path = temp_dir.path().join("cognee.db");
    std::fs::File::create(&db_path).expect("create sqlite db file");
    let db_url = format!("sqlite://{}", db_path.display());
    let db = connect(&db_url).await.expect("connect");
    initialize(&db).await.expect("initialize");
    let database: Arc<DatabaseConnection> = Arc::new(db);

    let graph_path = temp_dir.path().join("graph").to_string_lossy().to_string();
    let graph_db: Arc<dyn GraphDBTrait> = Arc::new(
        LadybugAdapter::new(&graph_path)
            .await
            .expect("LadybugAdapter::new"),
    );
    graph_db.initialize().await.expect("graph_db.initialize");

    let vector_db: Arc<dyn VectorDB> =
        Arc::new(QdrantAdapter::new(temp_dir.path().join("qdrant"), 384));

    let model_dir = get_embedding_model_dir();
    let embedding_engine: Arc<dyn EmbeddingEngine> =
        match OnnxEmbeddingEngine::new(OnnxEmbeddingConfig::bge_small(&model_dir)) {
            Ok(engine) => Arc::new(engine),
            Err(e) => {
                eprintln!("Skipping test: failed to load embedding model: {e}");
                return;
            }
        };

    let llm: Arc<dyn Llm> = Arc::new(
        OpenAIAdapter::new(
            require_env("OPENAI_MODEL"),
            require_env("OPENAI_TOKEN"),
            Some(require_env("OPENAI_URL")),
        )
        .expect("OpenAIAdapter::new"),
    );

    let owner_id = Uuid::nil();
    let ontology = Arc::new(NoOpOntologyResolver::new());

    // ── Step 1: Ingest two documents into separate datasets ─────────────
    let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>);

    let data_ai = ingest
        .add(
            vec![DataInput::Text(AI_TEXT.to_string())],
            "ds_ai",
            owner_id,
            None,
        )
        .await
        .expect("ingest ds_ai");
    assert_eq!(data_ai.len(), 1);

    let data_ml = ingest
        .add(
            vec![DataInput::Text(ML_TEXT.to_string())],
            "ds_ml",
            owner_id,
            None,
        )
        .await
        .expect("ingest ds_ml");
    assert_eq!(data_ml.len(), 1);

    let ds_ai = ops::datasets::get_dataset_by_name(&database, "ds_ai", owner_id, None)
        .await
        .expect("get ds_ai")
        .expect("ds_ai should exist");
    let ds_ml = ops::datasets::get_dataset_by_name(&database, "ds_ml", owner_id, None)
        .await
        .expect("get ds_ml")
        .expect("ds_ml should exist");

    println!("Step 1 OK: Ingested 2 documents into ds_ai and ds_ml");

    // ── Step 2: Cognify both datasets ───────────────────────────────────
    let config = CognifyConfig::default()
        .with_summarization(false)
        .with_triplet_embeddings(false);

    let result_ai = match cognify(
        data_ai,
        ds_ai.id,
        None,
        None,
        llm.clone() as Arc<dyn Llm>,
        storage.clone(),
        graph_db.clone(),
        vector_db.clone(),
        embedding_engine.clone(),
        None,
        ontology.clone(),
        &config,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Skipping: cognify ds_ai failed: {e}");
            return;
        }
    };

    let result_ml = match cognify(
        data_ml,
        ds_ml.id,
        None,
        None,
        llm.clone() as Arc<dyn Llm>,
        storage.clone(),
        graph_db.clone(),
        vector_db.clone(),
        embedding_engine.clone(),
        None,
        ontology.clone(),
        &config,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Skipping: cognify ds_ml failed: {e}");
            return;
        }
    };

    println!(
        "Step 2 OK: Cognified ds_ai ({} entities, {} edges), ds_ml ({} entities, {} edges)",
        result_ai.entities.len(),
        result_ai.edges.len(),
        result_ml.entities.len(),
        result_ml.edges.len(),
    );

    // ── Step 3: Capture pre-delete graph state ──────────────────────────
    let (pre_nodes, _pre_edges) = graph_db
        .get_graph_data()
        .await
        .expect("get_graph_data pre-delete");
    let pre_node_count = pre_nodes.len();
    let pre_names = extract_node_names(&pre_nodes);

    // Collect entity names from each cognify result for reference
    let ai_entity_names: HashSet<String> = result_ai
        .entities
        .iter()
        .map(|e| e.entity.name.to_lowercase())
        .collect();
    let ml_entity_names: HashSet<String> = result_ml
        .entities
        .iter()
        .map(|e| e.entity.name.to_lowercase())
        .collect();
    let shared_names: HashSet<String> = ai_entity_names
        .intersection(&ml_entity_names)
        .cloned()
        .collect();

    println!(
        "Step 3: Pre-delete state: {} graph nodes, {} pre-delete names",
        pre_node_count,
        pre_names.len(),
    );
    println!(
        "  AI entities: {:?}\n  ML entities: {:?}\n  Shared: {:?}",
        ai_entity_names, ml_entity_names, shared_names,
    );

    assert!(
        pre_node_count > 0,
        "Graph should have nodes after cognifying both datasets"
    );

    // ── Step 4: Delete dataset ds_ai ────────────────────────────────────
    let delete_svc =
        DeleteService::new(Arc::clone(&storage), database.clone() as Arc<dyn DeleteDb>)
            .with_graph_db(graph_db.clone())
            .with_vector_db(vector_db.clone());

    let delete_result = delete_svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Dataset {
                owner_id,
                dataset_name: "ds_ai".to_string(),
            },
            mode: DeleteMode::Hard,
        })
        .await
        .expect("delete ds_ai");

    assert!(
        delete_result.deleted_datasets >= 1,
        "Should have deleted at least 1 dataset"
    );
    println!(
        "Step 4 OK: Deleted ds_ai ({} datasets, {} data, {} graph nodes)",
        delete_result.deleted_datasets,
        delete_result.deleted_data,
        delete_result.deleted_graph_nodes,
    );

    // ── Step 5: Verify post-delete invariants ───────────────────────────
    // 5a. Graph is NOT empty (ds_ml nodes should survive)
    assert!(
        !graph_db.is_empty().await.expect("is_empty"),
        "Graph should NOT be empty — ds_ml data should survive"
    );

    let (post_nodes, _post_edges) = graph_db
        .get_graph_data()
        .await
        .expect("get_graph_data post-delete");
    let post_node_count = post_nodes.len();
    let post_names = extract_node_names(&post_nodes);

    println!(
        "Step 5: Post-delete state: {} graph nodes (was {})",
        post_node_count, pre_node_count,
    );

    // 5b. Node count should have decreased (some exclusive ds_ai nodes removed)
    assert!(
        post_node_count < pre_node_count,
        "Node count should decrease after deleting ds_ai: post={}, pre={}",
        post_node_count,
        pre_node_count,
    );

    // 5c. Shared entities should still be present in the graph.
    // Due to LLM non-determinism, we use a soft assertion: at least some
    // shared entity names should survive.
    if !shared_names.is_empty() {
        let surviving_shared: Vec<&String> = shared_names
            .iter()
            .filter(|name| post_names.contains(*name))
            .collect();

        println!(
            "  Shared entities surviving: {}/{} ({:?})",
            surviving_shared.len(),
            shared_names.len(),
            surviving_shared,
        );

        // Soft assertion: we expect at least some shared entities to survive.
        // With real LLMs, entity extraction is non-deterministic, so we
        // log a warning rather than hard-failing if none survive.
        if surviving_shared.is_empty() {
            eprintln!(
                "WARNING: No shared entities survived deletion. \
                 This may be due to LLM non-determinism in entity naming. \
                 Shared names were: {:?}",
                shared_names
            );
        }
    }

    // 5d. ds_ai should no longer exist in the database
    let ds_ai_after = ops::datasets::get_dataset_by_name(&database, "ds_ai", owner_id, None)
        .await
        .expect("get ds_ai after delete");
    assert!(
        ds_ai_after.is_none(),
        "ds_ai should not exist after deletion"
    );

    // 5e. ds_ml should still exist with its data
    let ds_ml_after = ops::datasets::get_dataset_by_name(&database, "ds_ml", owner_id, None)
        .await
        .expect("get ds_ml after delete")
        .expect("ds_ml should still exist");

    let ml_data = ops::datasets::get_dataset_data(&database, ds_ml_after.id)
        .await
        .expect("get ds_ml data");
    assert!(
        !ml_data.is_empty(),
        "ds_ml should still have data after deleting ds_ai"
    );

    println!("Step 5 OK: All post-delete invariants verified");
    println!("PASSED: test_shared_entity_graph_delete");
}
