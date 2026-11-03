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
//! 4. the entity lane switches on: a table name finds its `SchemaTable`.

use std::sync::Arc;

use cognee_cognify::graph_backend::MockChunkGraphExtractor;
use cognee_cognify::{CognifyConfig, cognify};
use cognee_core::CpuPool;
use cognee_database::ops::data::create_data;
use cognee_database::ops::datasets::{attach_data_to_dataset, create_dataset};
use cognee_database::{DatabaseConnection, SeaOrmPipelineRunRepository, connect, initialize};
use cognee_embedding::{EmbeddingEngine, MockEmbeddingEngine};
use cognee_graph::{GraphDBTrait, LadybugAdapter};
use cognee_models::{Data, Dataset};
use cognee_ontology::NoOpOntologyResolver;
use cognee_search::{HybridRetriever, SearchParams, retrievers::SearchRetriever};
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
    graph: Arc<dyn GraphDBTrait>,
    vector: Arc<dyn VectorDB>,
    embedding: Arc<dyn EmbeddingEngine>,
    _dir: TempDir,
}

async fn ingest_and_cognify() -> Stores {
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
    let storage: Arc<dyn StorageTrait> = Arc::new(MockStorage::new());
    // (data id, content, external metadata): one prose document and two DLT
    // rows linked by a foreign key.
    let items = [
        (Uuid::new_v4(), PLAIN_TEXT, None),
        (
            customer_data_id,
            CUSTOMER_ROW,
            Some(dlt_metadata(
                "customers",
                "7",
                json!({"column_values": {"name": "Acme"}}),
            )),
        ),
        (
            Uuid::new_v4(),
            ORDER_ROW,
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
        storage,
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
    let stores = ingest_and_cognify().await;

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
