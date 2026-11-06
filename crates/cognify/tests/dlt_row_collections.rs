#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! DLT rows end to end, offline: ingest -> cognify -> hybrid `get_context`.
//!
//! Python gives a DLT row its own graph type (`DltRow`) and vector collection
//! (`DltRow_text`), so chunk search (`DocumentChunk_text`) stays
//! documents-only, and indexes the table, foreign-key and cell-value nodes
//! (`SchemaTable_name`, `SchemaRelationship_name`, `DltColumn_properties`) the
//! hybrid entity lane reads. This runs the real pipeline against real SQLite,
//! a real Ladybug graph and the real in-memory `BruteForceVectorDB`, with a
//! deterministic mock embedding and an LLM-free graph backend, and checks:
//!
//! 1. the rows are in `DltRow_text` and *not* in `DocumentChunk_text`;
//! 2. the schema and cell-value collections exist and are populated;
//! 3. a hybrid `get_context` returns the rows, next to the ordinary chunk of
//!    a mixed dataset, each once and ranked by similarity;
//! 4. the entity lane switches on: a table name finds its `SchemaTable`;
//! 5. `HYBRID_COMPLETION` with `only_context`, run through
//!    `SearchOrchestrator::search`, returns the rows;
//! 6. the temporal-hybrid lane reranks a row it reaches through a `Timestamp`;
//! 7. deleting a DLT data item removes its `DltRow_text` point and its schema
//!    and cell-value vector rows, and leaves the other row's.

use std::sync::Arc;

use cognee_cognify::graph_backend::MockChunkGraphExtractor;
use cognee_cognify::{CognifyConfig, cognify};
use cognee_core::CpuPool;
use cognee_database::ops::data::create_data;
use cognee_database::ops::datasets::{attach_data_to_dataset, create_dataset};
use cognee_database::{DatabaseConnection, SeaOrmPipelineRunRepository, connect, initialize};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_embedding::{EmbeddingEngine, MockEmbeddingEngine};
use cognee_graph::{GraphDBTrait, GraphDBTraitExt, LadybugAdapter};
use cognee_models::{Data, Dataset, Timestamp};
use cognee_ontology::NoOpOntologyResolver;
use cognee_search::types::{SearchRequest, SearchType};
use cognee_search::{
    HybridRetriever, SearchBuilder, SearchParams, TemporalHybridRetriever,
    retrievers::SearchRetriever,
};
use cognee_storage::{MockStorage, StorageTrait};
use cognee_test_utils::MockLlm;
use cognee_vector::{BruteForceVectorDB, VectorDB};
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

const PLAIN_TEXT: &str = "Quarterly planning notes about the garden project.";
const ORDER_ROW: &str = "Table: orders\nRow Data:\n  id: 1\n  status: shipped";
const CUSTOMER_ROW: &str = "Table: customers\nRow Data:\n  id: 7\n  name: Acme";

fn dlt_metadata(table: &str, key: &str, extra: Value) -> String {
    let mut metadata = json!({
        "source": "dlt",
        "table_name": table,
        "primary_key_value": key,
        "dlt_db_name": "shop",
        "schema_info": [{"name": "id"}],
        "foreign_keys": [],
        "column_values": {},
    });
    for (k, v) in extra.as_object().unwrap() {
        metadata[k] = v.clone();
    }
    metadata.to_string()
}

struct Stores {
    db: Arc<DatabaseConnection>,
    storage: Arc<dyn StorageTrait>,
    owner_id: Uuid,
    /// The data item holding [`ORDER_ROW`].
    order_data_id: Uuid,
    graph: Arc<dyn GraphDBTrait>,
    vector: Arc<dyn VectorDB>,
    embedding: Arc<dyn EmbeddingEngine>,
    _dir: TempDir,
}

/// `filler_rows` extra rows of a `notes` table, for tests that need more DLT
/// rows than a lane's candidate budget.
async fn ingest_and_cognify(filler_rows: usize) -> Stores {
    let dir = TempDir::new().unwrap();
    let dataset_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let conn = connect("sqlite::memory:").await.unwrap();
    initialize(&conn).await.unwrap();
    create_dataset(
        &conn,
        Dataset::new("dlt".into(), owner_id, None, dataset_id),
    )
    .await
    .unwrap();
    let db: Arc<DatabaseConnection> = Arc::new(conn);

    let customer_data_id = Uuid::new_v4();
    let order_data_id = Uuid::new_v4();
    let storage: Arc<dyn StorageTrait> = Arc::new(MockStorage::new());
    // (data id, content, external metadata): one prose document and two DLT
    // rows linked by a foreign key.
    let mut items = vec![
        (Uuid::new_v4(), PLAIN_TEXT.to_string(), None),
        (
            customer_data_id,
            CUSTOMER_ROW.to_string(),
            Some(dlt_metadata(
                "customers",
                "7",
                json!({"column_values": {"name": "Acme"}}),
            )),
        ),
        (
            order_data_id,
            ORDER_ROW.to_string(),
            Some(dlt_metadata(
                "orders",
                "1",
                json!({
                    "foreign_keys": [
                        {"column": "customer_id", "ref_table": "customers", "ref_column": "id"}
                    ],
                    "fk_references": [{
                        "column": "customer_id",
                        "target_table": "customers",
                        "target_data_id": customer_data_id.to_string(),
                        "relationship_name": "customer_id_references_customers",
                    }],
                    "column_values": {"status": "shipped"},
                }),
            )),
        ),
    ];
    for n in 0..filler_rows {
        items.push((
            Uuid::new_v4(),
            format!("Table: notes\nRow Data:\n  id: {n}\n  body: filler {n}"),
            Some(dlt_metadata("notes", &n.to_string(), json!({}))),
        ));
    }
    let mut data_items = Vec::new();
    for (index, (data_id, content, metadata)) in items.into_iter().enumerate() {
        let location = storage
            .store(content.as_bytes(), &format!("dlt-{data_id}"))
            .await
            .unwrap();
        let mut builder = Data::builder(
            data_id,
            format!("dlt-{index}.txt"),
            location,
            format!("dlt-{index}.txt"),
            "txt",
            "text/plain",
            format!("hash-{data_id}"),
            owner_id,
        );
        if let Some(metadata) = metadata {
            builder = builder.external_metadata(metadata);
        }
        let data = builder.build();
        create_data(&db, data.clone()).await.unwrap();
        attach_data_to_dataset(&db, dataset_id, data_id)
            .await
            .unwrap();
        data_items.push(data);
    }

    let graph: Arc<dyn GraphDBTrait> = Arc::new(
        LadybugAdapter::new(&dir.path().join("graph").to_string_lossy())
            .await
            .unwrap(),
    );
    graph.initialize().await.unwrap();
    let vector: Arc<dyn VectorDB> = Arc::new(BruteForceVectorDB::new());
    let embedding: Arc<dyn EmbeddingEngine> = Arc::new(MockEmbeddingEngine::new(32));
    let pool: Arc<dyn CpuPool> =
        Arc::new(cognee_core::RayonThreadPool::with_default_threads().unwrap());
    let config = CognifyConfig::default()
        .with_summarization(false)
        .with_graph_backend(Arc::new(MockChunkGraphExtractor::new()));

    cognify(
        data_items,
        dataset_id,
        Some(owner_id),
        None,
        None,
        Arc::new(MockLlm::empty()),
        Arc::clone(&storage),
        Arc::clone(&graph),
        Arc::clone(&vector),
        Arc::clone(&embedding),
        db.clone(),
        Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&db))),
        pool,
        Arc::new(NoOpOntologyResolver::new()),
        &config,
    )
    .await
    .expect("cognify");

    Stores {
        db,
        storage,
        owner_id,
        order_data_id,
        graph,
        vector,
        embedding,
        _dir: dir,
    }
}

fn retriever(stores: &Stores) -> HybridRetriever {
    HybridRetriever::new(
        Arc::clone(&stores.vector),
        Arc::clone(&stores.embedding),
        Arc::clone(&stores.graph),
        Arc::new(MockLlm::empty()),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

/// The indexed text of every point in `data_type_field`, or none if the
/// collection does not exist.
async fn texts(stores: &Stores, data_type: &str, field: &str) -> Vec<String> {
    if !stores
        .vector
        .has_collection(data_type, field)
        .await
        .unwrap()
    {
        return vec![];
    }
    let query = stores
        .embedding
        .embed(&["anything"])
        .await
        .unwrap()
        .remove(0);
    stores
        .vector
        .search_similar(data_type, field, &query, 50)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|hit| {
            hit.metadata
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

#[tokio::test]
async fn dlt_rows_index_into_their_own_collections_and_hybrid_returns_them() {
    let stores = ingest_and_cognify(0).await;

    // 1. Rows are in DltRow_text and never in DocumentChunk_text.
    let mut rows = texts(&stores, "DltRow", "text").await;
    rows.sort();
    let mut expected_rows = vec![CUSTOMER_ROW.to_string(), ORDER_ROW.to_string()];
    expected_rows.sort();
    assert_eq!(rows, expected_rows);
    assert_eq!(
        texts(&stores, "DocumentChunk", "text").await,
        vec![PLAIN_TEXT.to_string()],
        "DocumentChunk_text holds documents only"
    );

    // 2. The schema and cell-value collections exist.
    for (data_type, field, size) in [
        ("SchemaTable", "name", 2),
        ("SchemaTable", "description", 2),
        ("SchemaRelationship", "name", 1),
        ("DltColumn", "properties", 2),
    ] {
        assert_eq!(
            stores
                .vector
                .collection_size(data_type, field)
                .await
                .unwrap(),
            size,
            "{data_type}_{field}"
        );
    }

    // The graph carries Python's model types: DltRow nodes (not DocumentChunk)
    // whose `is_row_of` and foreign-key edges leave the row node itself.
    let (nodes, edges) = stores.graph.get_graph_data().await.unwrap();
    let type_of = |id: &str| {
        nodes
            .iter()
            .find(|(node_id, _)| node_id == id)
            .and_then(|(_, data)| data.get("type").and_then(Value::as_str))
            .unwrap_or("")
            .to_string()
    };
    let row_ids: Vec<&String> = nodes
        .iter()
        .filter(|(_, data)| data.get("type").and_then(Value::as_str) == Some("DltRow"))
        .map(|(id, _)| id)
        .collect();
    assert_eq!(row_ids.len(), 2, "one DltRow node per row");
    let is_row_of: Vec<_> = edges.iter().filter(|e| e.2 == "is_row_of").collect();
    assert_eq!(is_row_of.len(), 2);
    for edge in &is_row_of {
        assert_eq!(type_of(&edge.0), "DltRow");
        assert_eq!(type_of(&edge.1), "SchemaTable");
    }
    let fk = edges
        .iter()
        .find(|e| e.2 == "customer_id_references_customers")
        .expect("row-to-row foreign key edge");
    assert_eq!(
        (type_of(&fk.0).as_str(), type_of(&fk.1).as_str()),
        ("DltRow", "DltRow")
    );
    assert_eq!(
        edges
            .iter()
            .filter(|e| type_of(&e.1) == "DltColumn")
            .count(),
        2,
        "each row links to its shared cell-value node"
    );

    // 3. Hybrid context: the query is a row's exact text, so the mock
    // embedding makes it the nearest passage. Each passage appears once.
    let context = retriever(&stores)
        .get_context(ORDER_ROW, &SearchParams::default())
        .await
        .expect("hybrid get_context");
    let chunk_texts: Vec<&str> = context
        .iter()
        .filter(|item| item.payload["kind"] == "chunk")
        .filter_map(|item| item.payload["text"].as_str())
        .collect();
    assert_eq!(
        chunk_texts.first().copied(),
        Some(ORDER_ROW),
        "the row ranks first: {chunk_texts:?}"
    );
    for expected in [ORDER_ROW, CUSTOMER_ROW, PLAIN_TEXT] {
        assert_eq!(
            chunk_texts.iter().filter(|t| **t == expected).count(),
            1,
            "{expected:?} must appear exactly once in {chunk_texts:?}"
        );
    }

    // 4. The entity lane reads the DLT collections now that rows exist: a
    // table's name finds its SchemaTable node.
    let context = retriever(&stores)
        .get_context("orders", &SearchParams::default())
        .await
        .expect("hybrid get_context");
    assert!(
        context
            .iter()
            .any(|item| item.payload["kind"] == "entity" && item.payload["name"] == "orders"),
        "SchemaTable `orders` must surface through the entity lane: {context:?}"
    );
}

/// An `only_context` request of `search_type` through the orchestrator.
fn request(search_type: SearchType, query: &str) -> SearchRequest {
    SearchRequest {
        query_text: query.to_string(),
        search_type,
        top_k: Some(5),
        datasets: None,
        dataset_ids: None,
        system_prompt: None,
        system_prompt_path: None,
        only_context: Some(true),
        use_combined_context: Some(false),
        session_id: None,
        node_type: None,
        node_name: None,
        node_name_filter_operator: None,
        wide_search_top_k: None,
        triplet_distance_penalty: None,
        user_id: None,
        tenant_id: None,
        verbose: None,
        feedback_influence: None,
        retriever_specific_config: None,
        response_schema: None,
        custom_search_type: None,
        auto_feedback_detection: None,
        neighborhood_depth: None,
        neighborhood_seed_top_k: None,
        include_references: false,
    }
}

/// `HYBRID_COMPLETION` + `only_context` through `SearchOrchestrator::search`,
/// the entry point every binding calls, returns the DLT rows the producer
/// wrote, each once, the queried row first.
#[tokio::test]
async fn hybrid_completion_only_context_through_the_orchestrator_returns_dlt_rows() {
    let stores = ingest_and_cognify(0).await;
    let orchestrator = SearchBuilder::new(
        Arc::clone(&stores.vector),
        Arc::clone(&stores.embedding),
        Arc::clone(&stores.graph),
        Arc::new(MockLlm::empty()),
        Arc::clone(&stores.db) as Arc<dyn cognee_database::SearchHistoryDb>,
    )
    .build();

    let response = orchestrator
        .search(&request(SearchType::HybridCompletion, ORDER_ROW))
        .await
        .expect("orchestrated search");
    assert!(response.only_context);
    let context = response.context.expect("the response carries its context");
    assert_eq!(context.len(), 1, "an unscoped search has one context");
    let chunk_texts: Vec<&str> = context
        .values()
        .next()
        .unwrap()
        .iter()
        .filter(|item| item.payload["kind"] == "chunk")
        .filter_map(|item| item.payload["text"].as_str())
        .collect();
    assert_eq!(chunk_texts.first().copied(), Some(ORDER_ROW));
    for expected in [ORDER_ROW, CUSTOMER_ROW, PLAIN_TEXT] {
        assert_eq!(
            chunk_texts.iter().filter(|t| **t == expected).count(),
            1,
            "{expected:?} must appear exactly once in {chunk_texts:?}"
        );
    }
}

/// The temporal-hybrid lane over the real producer's `DltRow_text` rows. Rust
/// writes no `Timestamp` edges for DLT cells (a documented divergence), so the
/// test attaches one to the order row's graph node, as Python's
/// `dlt_temporal` does: the window then promotes that row, which plain
/// similarity leaves out of the candidates, to the front.
#[tokio::test]
async fn temporal_hybrid_promotes_a_dlt_row_attached_to_the_window() {
    let stores = ingest_and_cognify(20).await;

    let (nodes, _) = stores.graph.get_graph_data().await.unwrap();
    let order_node = nodes
        .iter()
        .find(|(_, data)| data.get("name").and_then(Value::as_str) == Some("orders:1"))
        .map(|(id, _)| id.clone())
        .expect("the order row's DltRow node");
    let timestamp = Timestamp::from_text("5 March 2024").unwrap();
    stores.graph.add_nodes(&[&timestamp]).await.unwrap();
    stores
        .graph
        .add_edge(
            &order_node,
            &timestamp.base.id.to_string(),
            "ordered_at",
            None,
        )
        .await
        .unwrap();

    let window = json!({
        "starts_at": {"year": 2024, "month": 3, "day": 5},
        "ends_at": {"year": 2024, "month": 3, "day": 6},
    });
    let retriever = TemporalHybridRetriever::new(
        Arc::clone(&stores.vector),
        Arc::clone(&stores.embedding),
        Arc::clone(&stores.graph),
        Arc::new(MockLlm::new(vec![window.to_string()])),
        Some(1),
        Some(10),
        None,
        None,
        None,
        None,
    );
    // The query is the customer row's exact text. With one candidate per
    // chunk collection, plain hybrid keeps that row and the document and
    // leaves the dated order row among the filler rows, so only the window's
    // expansion reaches it.
    let retrieval = retriever
        .retrieve(CUSTOMER_ROW, &SearchParams::default())
        .await
        .expect("temporal retrieve");

    assert!(
        retrieval.diagnostics.reason.is_none(),
        "the rerank must have applied: {:?}",
        retrieval.diagnostics.reason
    );
    assert!(
        retrieval.diagnostics.expansion.contains(&order_node),
        "the window's expansion reads the DltRow_text point: {:?}",
        retrieval.diagnostics.expansion
    );
    let first_chunk = retrieval
        .context
        .iter()
        .find(|item| item.payload["kind"] == "chunk")
        .and_then(|item| item.payload["text"].as_str())
        .expect("a chunk");
    assert_eq!(first_chunk, ORDER_ROW, "the dated DLT row leads");
}

/// Deleting a DLT data item removes its `DltRow` graph node, its `DltRow_text`
/// point and its cell-value vector rows (Python's
/// `test_dlt_orphan_graph_vector_purge`), and keeps the other row's.
#[tokio::test]
async fn deleting_a_dlt_data_item_purges_its_vector_rows() {
    let stores = ingest_and_cognify(0).await;
    for (data_type, field, size) in [
        ("DltRow", "text", 2),
        ("SchemaTable", "name", 2),
        ("SchemaTable", "description", 2),
        ("DltColumn", "properties", 2),
    ] {
        assert_eq!(
            stores
                .vector
                .collection_size(data_type, field)
                .await
                .unwrap(),
            size,
            "{data_type}_{field} before the delete"
        );
    }

    DeleteService::new(
        Arc::clone(&stores.storage),
        Arc::clone(&stores.db) as Arc<dyn cognee_database::DeleteDb>,
    )
    .with_graph_db(Arc::clone(&stores.graph))
    .with_vector_db(Arc::clone(&stores.vector))
    .execute(&DeleteRequest {
        scope: DeleteScope::Data {
            owner_id: stores.owner_id,
            data_id: stores.order_data_id,
            dataset_name: Some("dlt".to_string()),
            delete_dataset_if_empty: false,
            tenant_id: None,
        },
        mode: DeleteMode::Soft,
        memory_only: false,
    })
    .await
    .expect("delete the order row");

    assert_eq!(
        texts(&stores, "DltRow", "text").await,
        vec![CUSTOMER_ROW.to_string()],
        "only the other row's point is left"
    );
    // Cell-value nodes belong to the rows that carry them: the deleted row's
    // value is gone, the other row's stays.
    assert_eq!(
        stores
            .vector
            .collection_size("DltColumn", "properties")
            .await
            .unwrap(),
        1,
        "DltColumn_properties keeps only the customers row's cell value"
    );
    // Schema nodes span data items (ledger owner `Uuid::nil()`, as for
    // Python's schema-level nodes), so they outlive any one row.
    for field in ["name", "description"] {
        assert_eq!(
            stores
                .vector
                .collection_size("SchemaTable", field)
                .await
                .unwrap(),
            2,
            "SchemaTable_{field} is schema-level"
        );
    }
    // The row's graph node is gone too.
    let (nodes, _) = stores.graph.get_graph_data().await.unwrap();
    let row_names: Vec<&str> = nodes
        .iter()
        .filter(|(_, data)| data.get("type").and_then(Value::as_str) == Some("DltRow"))
        .filter_map(|(_, data)| data.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(row_names, ["customers:7"]);
}
