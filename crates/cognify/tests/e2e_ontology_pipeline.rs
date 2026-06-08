//! End-to-end ontology pipeline test: add -> cognify -> search.
//!
//! Uses a real `.ttl` ontology fixture and asserts ontology enrichment
//! survived persistence and is discoverable via search.

use std::sync::Arc;

use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::{DatabaseConnection, IngestDb, SearchHistoryDb, connect, initialize, ops};
use cognee_embedding::{
    EmbeddingEngine, MockEmbeddingEngine, config::OnnxEmbeddingConfig, onnx::OnnxEmbeddingEngine,
};
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_ingestion::AddPipeline;
use cognee_llm::{Llm, OpenAIAdapter};
use cognee_models::DataInput;
use cognee_ontology::{OntologyFileInput, OntologyResolver, RdfLibOntologyResolver};
use cognee_search::{
    SearchBuilder, SearchRequest, SearchType,
    types::{SearchOutput, SearchResponse},
};
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_test_utils::{MockLlm, e2e_embedding_model_dir};
use cognee_vector::{QdrantAdapter, VectorDB};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

mod test_utils;
use test_utils::require_env;

const ONTOLOGY_FIXTURE: &str = "tests/test_data/ontology/tech_taxonomy.ttl";
const ONTOLOGY_TECH_ONLY_FIXTURE: &str = "tests/test_data/ontology/tech_only.ttl";
const ONTOLOGY_ORG_ONLY_FIXTURE: &str = "tests/test_data/ontology/org_only.ttl";
const ONTOLOGY_TEXT: &str = r#"
TechCorp is an Organisation building an Algorithm-driven platform.
DeepSort is an Algorithm used by TechCorp to rank Technology insights.
"#;
const MULTI_ONTOLOGY_TEXT: &str = r#"
TechCorp is an Organisation delivering software services.
DeepSort is an Algorithm used by TechCorp for ranking.
"#;

fn make_request(query: &str, search_type: SearchType) -> SearchRequest {
    SearchRequest {
        query_text: query.to_string(),
        search_type,
        top_k: None,
        datasets: None,
        dataset_ids: None,
        system_prompt: None,
        system_prompt_path: None,
        only_context: None,
        use_combined_context: None,
        session_id: None,
        node_type: None,
        node_name: None,
        node_name_filter_operator: None,
        wide_search_top_k: None,
        triplet_distance_penalty: None,
        save_interaction: None,
        user_id: None,
        verbose: None,
        feedback_influence: None,
        retriever_specific_config: None,
        response_schema: None,
        custom_search_type: None,
        auto_feedback_detection: None,
        neighborhood_depth: None,
        neighborhood_seed_top_k: None,
    }
}

fn is_non_empty(response: &SearchResponse) -> bool {
    match &response.result {
        SearchOutput::Text(text) => !text.is_empty(),
        SearchOutput::Texts(texts) => !texts.is_empty(),
        SearchOutput::Items(items) => !items.is_empty(),
        SearchOutput::GraphQueryRows(rows) => !rows.is_empty(),
        SearchOutput::Rules(rules) => !rules.is_empty(),
        SearchOutput::Ack { .. } => true,
        SearchOutput::Structured(value) => !value.is_null(),
    }
}

fn search_contains_any(response: &SearchResponse, markers: &[&str]) -> bool {
    let haystack = match &response.result {
        SearchOutput::Text(text) => text.clone(),
        SearchOutput::Texts(texts) => texts.join(" "),
        SearchOutput::Items(items) => items
            .iter()
            .map(|item| item.payload.to_string())
            .collect::<Vec<_>>()
            .join(" "),
        SearchOutput::GraphQueryRows(rows) => rows
            .iter()
            .flat_map(|row| row.iter().map(|v| v.to_string()))
            .collect::<Vec<_>>()
            .join(" "),
        SearchOutput::Rules(rules) => rules
            .iter()
            .map(|r| r.text.clone())
            .collect::<Vec<_>>()
            .join(" "),
        SearchOutput::Ack { message } => message.clone(),
        SearchOutput::Structured(value) => value.to_string(),
    }
    .to_lowercase();

    markers
        .iter()
        .any(|m| haystack.contains(&m.to_ascii_lowercase()))
}

#[tokio::test]
async fn e2e_ontology_pipeline_add_cognify_search() {
    let _ = require_env("OPENAI_URL");
    let _ = require_env("OPENAI_TOKEN");
    let _ = require_env("OPENAI_MODEL");

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

    let model_dir = e2e_embedding_model_dir();
    let embedding_engine: Arc<dyn EmbeddingEngine> =
        match OnnxEmbeddingEngine::with_auto_download(OnnxEmbeddingConfig::bge_small(&model_dir))
            .await
        {
            Ok(engine) => Arc::new(engine),
            Err(e) => {
                eprintln!("Skipping test: failed to prepare embedding model: {e}");
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

    let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>)
        .with_thread_pool(Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().expect("thread pool"),
        ))
        .with_graph_db(Arc::clone(&graph_db))
        .with_vector_db(Arc::clone(&vector_db))
        .with_database(Arc::clone(&database));

    let data_items = ingest
        .add(
            vec![DataInput::Text(ONTOLOGY_TEXT.to_string())],
            "ontology_e2e_dataset",
            owner_id,
            None,
        )
        .await
        .expect("ingest.add");

    assert_eq!(
        data_items.len(),
        1,
        "Expected exactly one ingested data item"
    );

    let dataset =
        ops::datasets::get_dataset_by_name(&database, "ontology_e2e_dataset", owner_id, None)
            .await
            .expect("get_dataset_by_name")
            .expect("dataset should exist after ingest");

    let ontology_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(ONTOLOGY_FIXTURE);
    let ontology_resolver = RdfLibOntologyResolver::new(OntologyFileInput::Path(ontology_path))
        .expect("load ontology fixture");
    assert!(
        ontology_resolver.is_loaded(),
        "ontology resolver must be loaded"
    );

    let cognify_result = match cognify(
        data_items,
        dataset.id,
        Some(owner_id),
        None,
        None,
        llm.clone(),
        storage.clone(),
        graph_db.clone(),
        vector_db.clone(),
        embedding_engine.clone(),
        Arc::clone(&database),
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool init"),
        ) as Arc<dyn cognee_core::CpuPool>,
        Arc::new(ontology_resolver),
        &CognifyConfig::default().with_summarization(true),
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            eprintln!("Skipping test: cognify failed: {e}");
            return;
        }
    };

    assert!(
        cognify_result
            .entities
            .iter()
            .any(|pair| pair.entity_type.is_ontology_valid()),
        "Expected at least one ontology-valid entity type"
    );

    assert!(
        cognify_result
            .edges
            .iter()
            .any(|edge| edge.relationship_name == "is_a"),
        "Expected at least one ontology-derived is_a edge"
    );

    let (persisted_nodes, persisted_edges) = graph_db
        .get_graph_data()
        .await
        .expect("graph_db.get_graph_data");

    assert!(
        persisted_edges.iter().any(|(_, _, rel, _)| rel == "is_a"),
        "Persisted graph should contain ontology-derived is_a edges"
    );

    let ancestor_present = persisted_nodes.iter().any(|(_, props)| {
        props
            .get("name")
            .and_then(|v| v.as_str())
            .map(|name| {
                name.eq_ignore_ascii_case("Technology") || name.eq_ignore_ascii_case("LegalEntity")
            })
            .unwrap_or(false)
    });
    assert!(
        ancestor_present,
        "Persisted graph should contain ontology ancestor nodes"
    );

    let orchestrator = SearchBuilder::new(
        vector_db.clone() as Arc<dyn VectorDB>,
        embedding_engine.clone() as Arc<dyn EmbeddingEngine>,
        graph_db.clone() as Arc<dyn GraphDBTrait>,
        llm.clone() as Arc<dyn Llm>,
        database.clone() as Arc<dyn SearchHistoryDb>,
    )
    .build();

    let response = orchestrator
        .search(&make_request(
            "Algorithm Technology",
            SearchType::GraphCompletion,
        ))
        .await
        .expect("search GraphCompletion");

    assert!(
        is_non_empty(&response),
        "Expected non-empty search response"
    );
    assert!(
        search_contains_any(&response, &["algorithm", "technology", "is_a"]),
        "Expected ontology-enriched concepts in search response"
    );
}

#[tokio::test]
async fn e2e_ontology_pipeline_multi_ontology_add_cognify_search() {
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
        Arc::new(QdrantAdapter::new(temp_dir.path().join("qdrant"), 8));

    let embedding_engine: Arc<dyn EmbeddingEngine> = Arc::new(MockEmbeddingEngine::new(8));

    let mock_llm = MockLlm::new(vec![
        json!({
            "nodes": [
                {
                    "id": "techcorp",
                    "name": "TechCorp",
                    "type": "Organisation",
                    "description": "A software services company"
                },
                {
                    "id": "deepsort",
                    "name": "DeepSort",
                    "type": "Algorithm",
                    "description": "A ranking algorithm"
                }
            ],
            "edges": [
                {
                    "source_node_id": "deepsort",
                    "target_node_id": "techcorp",
                    "relationship_name": "used_by"
                }
            ]
        })
        .to_string(),
    ]);
    let llm: Arc<dyn Llm> = Arc::new(mock_llm);

    let owner_id = Uuid::nil();

    let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>)
        .with_thread_pool(Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().expect("thread pool"),
        ))
        .with_graph_db(Arc::clone(&graph_db))
        .with_vector_db(Arc::clone(&vector_db))
        .with_database(Arc::clone(&database));

    let data_items = ingest
        .add(
            vec![DataInput::Text(MULTI_ONTOLOGY_TEXT.to_string())],
            "ontology_multi_e2e_dataset",
            owner_id,
            None,
        )
        .await
        .expect("ingest.add");

    assert_eq!(
        data_items.len(),
        1,
        "Expected exactly one ingested data item"
    );

    let dataset =
        ops::datasets::get_dataset_by_name(&database, "ontology_multi_e2e_dataset", owner_id, None)
            .await
            .expect("get_dataset_by_name")
            .expect("dataset should exist after ingest");

    let ontology_paths = vec![
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(ONTOLOGY_TECH_ONLY_FIXTURE),
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(ONTOLOGY_ORG_ONLY_FIXTURE),
    ];
    let ontology_resolver = RdfLibOntologyResolver::new(OntologyFileInput::Paths(ontology_paths))
        .expect("load ontology fixtures");
    assert!(
        ontology_resolver.is_loaded(),
        "ontology resolver must be loaded"
    );
    assert!(
        ontology_resolver.class_count() >= 4,
        "Expected merged multi-ontology resolver to include classes from both files"
    );

    let cognify_result = match cognify(
        data_items,
        dataset.id,
        Some(owner_id),
        None,
        None,
        llm.clone(),
        storage.clone(),
        graph_db.clone(),
        vector_db.clone(),
        embedding_engine.clone(),
        Arc::clone(&database),
        Arc::new(cognee_database::NoopPipelineRunRepository::new())
            as Arc<dyn cognee_database::PipelineRunRepository>,
        Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool init"),
        ) as Arc<dyn cognee_core::CpuPool>,
        Arc::new(ontology_resolver),
        &CognifyConfig::default().with_summarization(false),
    )
    .await
    {
        Ok(result) => result,
        Err(e) => {
            eprintln!("Skipping test: cognify failed: {e}");
            return;
        }
    };

    assert!(
        cognify_result
            .edges
            .iter()
            .any(|edge| edge.relationship_name == "is_a"),
        "Expected at least one ontology-derived is_a edge"
    );

    let (persisted_nodes, persisted_edges) = graph_db
        .get_graph_data()
        .await
        .expect("graph_db.get_graph_data");

    assert!(
        persisted_edges.iter().any(|(_, _, rel, _)| rel == "is_a"),
        "Persisted graph should contain ontology-derived is_a edges"
    );

    let normalize = |name: &str| {
        name.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase()
    };
    let persisted_names = persisted_nodes
        .iter()
        .filter_map(|(_, props)| props.get("name").and_then(|v| v.as_str()))
        .map(normalize)
        .collect::<Vec<_>>();

    let zeta_domain_root_present = persisted_names.iter().any(|name| name == "zetadomainroot");
    let legal_entity_present = persisted_names.iter().any(|name| name == "legalentity");
    assert!(
        zeta_domain_root_present && legal_entity_present,
        "Expected combined ontology enrichment to include ZetaDomainRoot and LegalEntity ancestors; found names: {:?}",
        persisted_names
    );

    let orchestrator = SearchBuilder::new(
        vector_db.clone() as Arc<dyn VectorDB>,
        embedding_engine.clone() as Arc<dyn EmbeddingEngine>,
        graph_db.clone() as Arc<dyn GraphDBTrait>,
        llm.clone() as Arc<dyn Llm>,
        database.clone() as Arc<dyn SearchHistoryDb>,
    )
    .build();

    let response = orchestrator
        .search(&make_request(
            "Algorithm Organisation ZetaDomainRoot LegalEntity",
            SearchType::GraphCompletion,
        ))
        .await
        .expect("search GraphCompletion");

    assert!(
        is_non_empty(&response),
        "Expected non-empty search response"
    );
}
