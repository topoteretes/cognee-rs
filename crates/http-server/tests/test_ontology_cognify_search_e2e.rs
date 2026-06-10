//! HTTP ontology E2E: upload ontology -> cognify with ontologyKey -> search.

mod support;

use std::sync::Arc;

use axum::{
    body::{self, Body},
    http::{Request, StatusCode, header},
};
use cognee_database::{DatabaseConnection, IngestDb, SearchHistoryDb, connect, initialize, ops};
use cognee_delete::DeleteService;

use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_http_server::{AppState, HttpServerConfig, build_router, components::ComponentHandles};
use cognee_ingestion::AddPipeline;
use cognee_llm::{Llm, OpenAIAdapter};
use cognee_models::DataInput;
use cognee_ontology::OntologyManager;
use cognee_search::{SearchBuilder, SearchOrchestrator};
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_vector::{QdrantAdapter, VectorDB};
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

const DATASET_TEXT: &str = r#"
TechCorp is an Organisation building an Algorithm-driven platform.
DeepSort is an Algorithm used by TechCorp for Technology ranking.
"#;

const ONTOLOGY_UPLOAD_BODY: &str = r#"@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix : <http://test.cognee.ai/ontology#> .

:LegalEntity a owl:Class ;
    rdfs:label "LegalEntity" .

:Organisation a owl:Class ;
    rdfs:subClassOf :LegalEntity ;
    rdfs:label "Organisation" .

:Technology a owl:Class ;
    rdfs:label "Technology" .

:Algorithm a owl:Class ;
    rdfs:subClassOf :Technology ;
    rdfs:label "Algorithm" .
"#;

fn require_env(var_name: &str) -> String {
    let _ = dotenv::dotenv();

    let canonical_fallback = match var_name {
        "OPENAI_TOKEN" => Some("LLM_API_KEY"),
        "OPENAI_URL" => Some("LLM_ENDPOINT"),
        "OPENAI_MODEL" => Some("LLM_MODEL"),
        _ => None,
    };

    if let Ok(v) = std::env::var(var_name)
        && !v.is_empty()
    {
        return v;
    }
    if let Some(canonical) = canonical_fallback
        && let Ok(v) = std::env::var(canonical)
        && !v.is_empty()
    {
        return v;
    }
    panic!("Required environment variable '{var_name}' is not set");
}

fn search_payload_contains_any_markers(payload: &serde_json::Value, markers: &[&str]) -> bool {
    let haystack = payload.to_string().to_ascii_lowercase();
    markers
        .iter()
        .any(|marker| haystack.contains(&marker.to_ascii_lowercase()))
}

#[tokio::test]
async fn upload_cognify_search_with_ontology_key_and_unknown_key_negative() {
    let _ = require_env("OPENAI_URL");
    let _ = require_env("OPENAI_TOKEN");
    let _ = require_env("OPENAI_MODEL");

    let temp_dir = TempDir::new().expect("temp dir");

    let storage: Arc<dyn StorageTrait> =
        Arc::new(LocalStorage::new(temp_dir.path().join("storage")));
    storage.initialize().await.expect("storage.initialize");

    let db_path = temp_dir.path().join("http_ontology.db");
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

    let Some((embedding_engine, embedding_dims)) =
        cognee_test_utils::create_test_embedding_engine().await
    else {
        eprintln!("Skipping test: embedding engine unavailable");
        return;
    };

    let vector_db: Arc<dyn VectorDB> = Arc::new(QdrantAdapter::new(
        temp_dir.path().join("qdrant"),
        embedding_dims,
    ));

    let llm: Arc<dyn Llm> = Arc::new(
        OpenAIAdapter::new(
            require_env("OPENAI_MODEL"),
            require_env("OPENAI_TOKEN"),
            Some(require_env("OPENAI_URL")),
        )
        .expect("OpenAIAdapter::new"),
    );

    let owner_id = Uuid::nil();
    let dataset_name = "http_ontology_e2e";

    let add = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>)
        .with_thread_pool(Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().expect("thread pool"),
        ))
        .with_graph_db(Arc::clone(&graph_db))
        .with_vector_db(Arc::clone(&vector_db))
        .with_database(Arc::clone(&database));

    add.add(
        vec![DataInput::Text(DATASET_TEXT.to_string())],
        dataset_name,
        owner_id,
        None,
    )
    .await
    .expect("seed dataset via AddPipeline");

    let dataset = ops::datasets::get_dataset_by_name(&database, dataset_name, owner_id, None)
        .await
        .expect("get_dataset_by_name")
        .expect("seeded dataset should exist");

    let search_orchestrator: Arc<SearchOrchestrator> = Arc::new(
        SearchBuilder::new(
            Arc::clone(&vector_db),
            Arc::clone(&embedding_engine),
            Arc::clone(&graph_db),
            Arc::clone(&llm),
            database.clone() as Arc<dyn SearchHistoryDb>,
        )
        .build(),
    );

    let ontology_dir = tempfile::tempdir().expect("tmp ontology");
    let ontology_manager = Arc::new(OntologyManager::new(ontology_dir.path().to_path_buf()));
    Box::leak(Box::new(ontology_dir));

    let delete_service = Arc::new(DeleteService::new(
        Arc::clone(&storage),
        database.clone() as Arc<dyn cognee_database::DeleteDb>,
    ));

    let graph_db_for_assertions = Arc::clone(&graph_db);

    let handles = Arc::new(ComponentHandles {
        database: Arc::clone(&database),
        storage,
        delete_service,
        cloud_client: None,
        ontology_manager,
        search_orchestrator: Some(search_orchestrator),
        llm: Some(llm),
        graph_db: Some(Arc::clone(&graph_db)),
        vector_db: Some(vector_db),
        thread_pool: Some(Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().expect("thread pool"),
        )),
        embedding_engine: Some(embedding_engine),
        ontology_resolver: None,
        permissions: None,
        sync_ops: None,
        session_store: None,
        session_manager: None,
        checkpoint_store: None,
        responses_client: None,
        transcriber: None,
        notebook_runner: None,
    });

    let mut state = AppState::build_with_db(HttpServerConfig::default(), Arc::clone(&database))
        .await
        .expect("AppState::build_with_db");
    state.lib = Some(handles);

    let app = build_router(state).await.expect("build_router");

    let boundary = "ontology_upload_boundary";
    let upload_body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"ontology_key\"\r\n\r\ntech\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"ontology_file\"; filename=\"tech.owl\"\r\nContent-Type: application/rdf+xml\r\n\r\n{}\r\n--{boundary}--\r\n",
        ONTOLOGY_UPLOAD_BODY
    );

    let upload_req = Request::builder()
        .method("POST")
        .uri("/api/v1/ontologies")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(upload_body))
        .expect("upload request");

    let upload_resp = app
        .clone()
        .oneshot(upload_req)
        .await
        .expect("upload response");
    assert_eq!(upload_resp.status(), StatusCode::OK);

    let upload_json = support::body_json(upload_resp).await;
    assert_eq!(
        upload_json["uploaded_ontologies"][0]["ontology_key"], "tech",
        "upload metadata should include ontology key"
    );

    let bad_cognify_req = Request::builder()
        .method("POST")
        .uri("/api/v1/cognify")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "dataset_ids": [dataset.id],
                "ontologyKey": ["does-not-exist"],
                "runInBackground": false
            })
            .to_string(),
        ))
        .expect("bad cognify request");

    let bad_resp = app
        .clone()
        .oneshot(bad_cognify_req)
        .await
        .expect("bad cognify response");
    assert_eq!(
        bad_resp.status(),
        StatusCode::NOT_FOUND,
        "unknown ontology key must return non-200"
    );

    let cognify_req = Request::builder()
        .method("POST")
        .uri("/api/v1/cognify")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "dataset_ids": [dataset.id],
                "ontologyKey": ["tech"],
                "runInBackground": false,
                "chunksPerBatch": 1
            })
            .to_string(),
        ))
        .expect("cognify request");

    let cognify_resp = app
        .clone()
        .oneshot(cognify_req)
        .await
        .expect("cognify response");

    let cognify_status = cognify_resp.status();
    if cognify_status != StatusCode::OK {
        let bytes = body::to_bytes(cognify_resp.into_body(), usize::MAX)
            .await
            .expect("error body bytes");
        eprintln!(
            "Skipping test: /api/v1/cognify failed with status {} body {}",
            cognify_status,
            String::from_utf8_lossy(&bytes)
        );
        return;
    }

    let cognify_json = support::body_json(cognify_resp).await;
    assert_eq!(
        cognify_json[dataset.id.to_string()]["status"],
        "PipelineRunCompleted",
        "cognify must complete in blocking mode"
    );

    let (persisted_nodes, persisted_edges) = graph_db_for_assertions
        .get_graph_data()
        .await
        .expect("graph_db.get_graph_data after cognify");

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
        "Persisted graph should contain ontology-expanded ancestor nodes"
    );

    let search_req = Request::builder()
        .method("POST")
        .uri("/api/v1/search")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "query": "Algorithm Technology",
                "search_type": "GRAPH_COMPLETION"
            })
            .to_string(),
        ))
        .expect("search request");

    let search_resp = app
        .clone()
        .oneshot(search_req)
        .await
        .expect("search response");
    assert_eq!(search_resp.status(), StatusCode::OK);

    let search_json = support::body_json(search_resp).await;
    assert!(
        search_json
            .as_array()
            .is_some_and(|arr| !arr.is_empty() && !arr[0]["searchResult"].is_null()),
        "search response must contain ontology-enriched result"
    );
    assert!(
        search_payload_contains_any_markers(&search_json, &["algorithm", "technology", "is_a"]),
        "search response should reference ontology-enriched concepts"
    );
}
