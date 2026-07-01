#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Deterministic regression test (no LLM, no embedding models): a HARD delete
//! of a data item must also remove the Document graph node (whose id == the
//! source Data item's id).
//!
//! This guards the fix for the leak introduced by graph-extraction parity
//! (release task 16): cognify stores a Document node per ingested Data item,
//! and `upsert_provenance` must register a provenance row for it (keyed with
//! `data_id == document.id`) so the existing provenance-based delete cleanup
//! removes the Document node. Without that row the Document node survived a
//! hard delete (the orphan sweep only targets Entity/EntityType/EdgeType).
//!
//! The test seeds the relational + graph state directly (mirroring what the
//! cognify pipeline writes, including the new Document provenance row) so it
//! runs offline with `MockGraphDB`.

use std::sync::Arc;

use cognee_database::{
    DatabaseConnection, DeleteDb, GraphNode, connect, initialize,
    ops::{self, graph_storage},
};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_models::{Data, Dataset};
use cognee_storage::{MockStorage, StorageTrait};
use serde_json::json;
use uuid::Uuid;

/// Build a graph-node JSON blob suitable for `MockGraphDB::add_nodes_raw`.
fn graph_node(id: Uuid, node_type: &str) -> serde_json::Value {
    json!({
        "id": id.to_string(),
        "type": node_type,
    })
}

/// Build a relational provenance node row for `(data_id, dataset_id)`.
fn prov_node(
    user_id: Uuid,
    dataset_id: Uuid,
    data_id: Uuid,
    node_id: Uuid,
    node_type: &str,
    indexed_fields: serde_json::Value,
) -> GraphNode {
    GraphNode {
        // The provenance row `id` is content-addressed in production, but for
        // this test any distinct deterministic value works.
        id: Uuid::new_v4(),
        slug: node_id,
        user_id,
        data_id,
        dataset_id,
        label: Some(node_type.to_string()),
        node_type: node_type.to_string(),
        indexed_fields,
        attributes: None,
        created_at: chrono::Utc::now(),
    }
}

#[tokio::test]
async fn hard_delete_removes_document_graph_node() {
    // ── Infrastructure (all in-memory / mock; no LLM, no embeddings) ─────────
    let db = connect("sqlite::memory:").await.unwrap();
    initialize(&db).await.unwrap();
    let database: Arc<DatabaseConnection> = Arc::new(db);
    let storage: Arc<dyn StorageTrait> = Arc::new(MockStorage::new());
    let graph_db: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());

    let owner_id = Uuid::new_v4();

    // ── Seed relational dataset + data + link ───────────────────────────────
    let dataset = ops::datasets::create_dataset(
        &database,
        Dataset::new("doc_delete_ds".to_string(), owner_id, None, Uuid::new_v4()),
    )
    .await
    .unwrap();

    // The Document graph node id equals the Data item's id (content-addressed,
    // Python-identical via classify_documents). We model that directly.
    let data_id = Uuid::new_v4();
    let data = Data::builder(
        data_id,
        "doc.txt",
        "/storage/doc.txt",
        "file:///storage/doc.txt",
        "txt",
        "text/plain",
        "deadbeef",
        owner_id,
    )
    .build();
    ops::data::create_data(&database, data).await.unwrap();
    ops::datasets::attach_data_to_dataset(&database, dataset.id, data_id)
        .await
        .unwrap();

    // ── Seed graph nodes (mirror cognify's add_data_points output) ──────────
    // Document node id == data_id, plus one chunk and one entity.
    //
    // Note: we intentionally omit an EntityType node. In production EntityType
    // rows use `data_id == nil` (shared across data items) and are reclaimed by
    // the hard-mode degree-one orphan sweep only once their connecting edges
    // disappear. Seeding a disconnected (degree-0) EntityType here would model
    // neither path and would leak for reasons unrelated to this regression
    // (the Document-node leak), so we keep the fixture focused.
    let chunk_id = Uuid::new_v4();
    let entity_id = Uuid::new_v4();
    graph_db
        .add_nodes_raw(vec![
            graph_node(data_id, "TextDocument"),
            graph_node(chunk_id, "DocumentChunk"),
            graph_node(entity_id, "Entity"),
        ])
        .await
        .unwrap();

    // ── Seed relational provenance rows (mirror upsert_provenance) ──────────
    // Crucially: the Document provenance row is keyed `data_id == data_id` with
    // `slug == data_id`. This is the row added by the fix; without it the
    // Document graph node would not be matched by the delete cleanup.
    let prov_nodes = vec![
        prov_node(
            owner_id,
            dataset.id,
            data_id,
            data_id,
            "TextDocument",
            json!(["name"]),
        ),
        prov_node(
            owner_id,
            dataset.id,
            data_id,
            chunk_id,
            "DocumentChunk",
            json!(["text"]),
        ),
        prov_node(
            owner_id,
            dataset.id,
            data_id,
            entity_id,
            "Entity",
            json!(["name"]),
        ),
    ];
    graph_storage::upsert_nodes(database.as_ref(), &prov_nodes)
        .await
        .unwrap();

    // Sanity: 3 graph nodes before delete.
    let (pre_nodes, _) = graph_db.get_graph_data().await.unwrap();
    assert_eq!(pre_nodes.len(), 3, "expected 3 seeded graph nodes");

    // ── HARD delete the data item ───────────────────────────────────────────
    let delete_svc =
        DeleteService::new(Arc::clone(&storage), database.clone() as Arc<dyn DeleteDb>)
            .with_graph_db(Arc::clone(&graph_db));

    let result = delete_svc
        .execute(&DeleteRequest {
            scope: DeleteScope::Data {
                owner_id,
                data_id,
                dataset_name: Some("doc_delete_ds".to_string()),
                delete_dataset_if_empty: false,
            },
            mode: DeleteMode::Hard,
            memory_only: false,
        })
        .await
        .expect("hard delete should succeed");

    assert_eq!(result.deleted_data, 1, "the data item should be deleted");

    // ── Assert: graph is EMPTY (the Document node was removed too) ───────────
    let (post_nodes, _) = graph_db.get_graph_data().await.unwrap();
    assert_eq!(
        post_nodes.len(),
        0,
        "graph should be empty after hard delete; the Document node (id == data_id) leaked: {:?}",
        post_nodes.iter().map(|(id, _)| id).collect::<Vec<_>>(),
    );

    // Explicitly confirm the Document node is gone.
    assert!(
        !graph_db.has_node(&data_id.to_string()).await.unwrap(),
        "Document graph node (id == data_id) must be removed by hard delete"
    );
}
