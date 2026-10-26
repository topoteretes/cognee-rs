#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Cognify, memify, rollback and delete under per-dataset storage
//! (`ENABLE_BACKEND_ACCESS_CONTROL`, `docs/design/per-dataset-storage.md`).
//!
//! The graph and vector engines are the routing ones over one mock store per
//! dataset (`cognee_test_utils::PerDatasetMocks`), so every graph and vector
//! call the pipelines make outside a dataset scope fails — exactly as in
//! production — and every call inside one lands in that dataset's own store.
//!
//! Fully offline: `MockLlm`, `MockStorage`, `MockEmbeddingEngine` and in-memory
//! SQLite.

use std::collections::BTreeSet;
use std::sync::Arc;

use cognee_cognify::memify::{MemifyConfig, memify};
use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::ops::data::create_data;
use cognee_database::ops::datasets::{attach_data_to_dataset, create_dataset};
use cognee_database::{
    DatabaseConnection, DeleteDb, PipelineRunRepository, SeaOrmPipelineRunRepository, connect,
    initialize,
};
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_models::{Data, Dataset};
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{MockStorage, StorageTrait};
use cognee_test_utils::{MockLlm, PerDatasetMocks};
use cognee_vector::VectorDB;
use serde_json::json;
use uuid::Uuid;

/// A canned extraction naming `person` and `company`, so each dataset's graph
/// is recognisably its own.
fn graph_response(person: &str, company: &str) -> String {
    json!({
        "nodes": [
            {"id": person.to_lowercase(), "name": person, "type": "PERSON", "description": "A person."},
            {"id": company.to_lowercase(), "name": company, "type": "ORGANIZATION", "description": "A company."}
        ],
        "edges": [{
            "source_node_id": person.to_lowercase(),
            "target_node_id": company.to_lowercase(),
            "relationship_name": "works_at",
            "description": format!("{person} works at {company}."),
        }]
    })
    .to_string()
}

fn config() -> CognifyConfig {
    CognifyConfig::default()
        .with_chunk_size(1500)
        .with_chunks_per_batch(1)
        .with_summarization(false)
}

struct Fixture {
    db: Arc<DatabaseConnection>,
    repo: Arc<dyn PipelineRunRepository>,
    storage: Arc<dyn StorageTrait>,
    stores: Arc<PerDatasetMocks>,
    owner_id: Uuid,
}

impl Fixture {
    async fn new() -> Self {
        let conn = connect("sqlite::memory:").await.expect("connect sqlite");
        initialize(&conn).await.expect("initialize");
        let db = Arc::new(conn);
        Self {
            repo: Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&db))),
            db,
            storage: Arc::new(MockStorage::new()),
            stores: PerDatasetMocks::new(),
            owner_id: Uuid::new_v4(),
        }
    }

    async fn dataset(&self, name: &str) -> Uuid {
        let id = Uuid::new_v4();
        create_dataset(&self.db, Dataset::new(name.into(), self.owner_id, None, id))
            .await
            .expect("seed dataset");
        id
    }

    async fn add_file(&self, dataset_id: Uuid, text: &str) -> Data {
        let data_id = Uuid::new_v4();
        let location = self
            .storage
            .store(text.as_bytes(), &format!("per-dataset-{data_id}"))
            .await
            .expect("store");
        let item = Data::builder(
            data_id,
            format!("{data_id}.txt"),
            location,
            format!("{data_id}.txt"),
            "txt",
            "text/plain",
            format!("hash-{data_id}"),
            self.owner_id,
        )
        .build();
        create_data(&self.db, item.clone()).await.expect("data row");
        attach_data_to_dataset(&self.db, dataset_id, data_id)
            .await
            .expect("attach");
        item
    }

    async fn cognify(&self, dataset_id: Uuid, items: Vec<Data>, response: String) {
        let llm: Arc<dyn cognee_llm::Llm> = Arc::new(MockLlm::new(vec![response; 8]));
        cognify(
            items,
            dataset_id,
            Some(self.owner_id),
            None,
            None,
            llm,
            Arc::clone(&self.storage),
            self.stores.graph_db(),
            self.stores.vector_db(),
            Arc::new(MockEmbeddingEngine::new(8)),
            self.db.clone(),
            Arc::clone(&self.repo),
            Arc::new(cognee_core::RayonThreadPool::with_default_threads().unwrap()),
            Arc::new(NoOpOntologyResolver::new()),
            &config(),
        )
        .await
        .expect("cognify under per-dataset storage");
    }

    /// The `name` property of every node in the dataset's own graph,
    /// lowercased (entity names are normalised on write).
    async fn names(&self, dataset_id: Uuid) -> BTreeSet<String> {
        let (nodes, _) = self
            .stores
            .graph(dataset_id)
            .get_graph_data()
            .await
            .unwrap();
        nodes
            .into_iter()
            .filter_map(|(_, props)| props.get("name")?.as_str().map(str::to_lowercase))
            .collect()
    }

    fn delete_service(&self) -> DeleteService {
        DeleteService::new(
            Arc::clone(&self.storage),
            self.db.clone() as Arc<dyn DeleteDb>,
        )
        .with_graph_db(self.stores.graph_db())
        .with_vector_db(self.stores.vector_db())
        .with_dataset_storage(Arc::clone(&self.stores) as _)
    }
}

/// Each dataset's cognify writes only its own store, and a dataset never sees
/// another's entities — the leak post-filter scoping could not close.
#[tokio::test]
async fn each_dataset_is_cognified_into_its_own_store() {
    let fx = Fixture::new().await;
    let (a, b) = (fx.dataset("a").await, fx.dataset("b").await);
    let file_a = fx.add_file(a, "Alice works at Acme.").await;
    let file_b = fx.add_file(b, "Bob works at Globex.").await;

    fx.cognify(a, vec![file_a], graph_response("Alice", "Acme"))
        .await;
    fx.cognify(b, vec![file_b], graph_response("Bob", "Globex"))
        .await;

    let (in_a, in_b) = (fx.names(a).await, fx.names(b).await);
    assert!(in_a.contains("alice") && in_a.contains("acme"), "{in_a:?}");
    assert!(in_b.contains("bob") && in_b.contains("globex"), "{in_b:?}");
    assert!(
        !in_a.contains("bob") && !in_a.contains("globex"),
        "{in_a:?}"
    );
    assert!(
        !in_b.contains("alice") && !in_b.contains("acme"),
        "{in_b:?}"
    );
    assert_eq!(fx.stores.graph_datasets(), {
        let mut both = vec![a, b];
        both.sort_unstable();
        both
    });
    assert!(
        !fx.stores
            .vector(a)
            .list_collections()
            .await
            .unwrap()
            .is_empty(),
        "the dataset's vectors are in its own store"
    );
}

/// Deleting a dataset drops its store and leaves the other's untouched.
#[tokio::test]
async fn deleting_a_dataset_drops_only_its_store() {
    let fx = Fixture::new().await;
    let (a, b) = (fx.dataset("a").await, fx.dataset("b").await);
    let file_a = fx.add_file(a, "Alice works at Acme.").await;
    let file_b = fx.add_file(b, "Bob works at Globex.").await;
    fx.cognify(a, vec![file_a], graph_response("Alice", "Acme"))
        .await;
    fx.cognify(b, vec![file_b], graph_response("Bob", "Globex"))
        .await;

    let result = fx
        .delete_service()
        .execute(&DeleteRequest {
            scope: DeleteScope::DatasetById { dataset_id: a },
            mode: DeleteMode::Hard,
            memory_only: false,
        })
        .await
        .expect("delete dataset a");
    assert_eq!(result.deleted_datasets, 1);
    assert!(
        result.deleted_graph_nodes > 0,
        "the ledger sizes the report"
    );

    assert_eq!(fx.stores.graph_datasets(), vec![b], "a's store is gone");
    assert!(fx.names(b).await.contains("bob"));
}

/// Deleting one item removes its artifacts inside its dataset's own store.
#[tokio::test]
async fn deleting_an_item_cleans_its_dataset_store() {
    let fx = Fixture::new().await;
    let a = fx.dataset("a").await;
    let file = fx.add_file(a, "Alice works at Acme.").await;
    fx.cognify(a, vec![file.clone()], graph_response("Alice", "Acme"))
        .await;
    assert!(fx.names(a).await.contains("alice"));

    fx.delete_service()
        .execute(&DeleteRequest {
            scope: DeleteScope::DataInDataset {
                data_id: file.id,
                dataset_id: a,
                delete_dataset_if_empty: false,
            },
            mode: DeleteMode::Hard,
            memory_only: false,
        })
        .await
        .expect("delete the item");
    let left = fx.names(a).await;
    assert!(
        !left.contains("alice") && !left.contains("acme"),
        "{left:?}"
    );
}

/// The store-wide wipe drops every dataset's store.
#[tokio::test]
async fn delete_all_drops_every_store() {
    let fx = Fixture::new().await;
    let a = fx.dataset("a").await;
    let file = fx.add_file(a, "Alice works at Acme.").await;
    fx.cognify(a, vec![file], graph_response("Alice", "Acme"))
        .await;

    fx.delete_service()
        .execute(&DeleteRequest {
            scope: DeleteScope::All,
            mode: DeleteMode::Hard,
            memory_only: false,
        })
        .await
        .expect("delete all");
    assert!(fx.stores.graph_datasets().is_empty());
}

/// Memify reads and indexes the dataset's own graph.
#[tokio::test]
async fn memify_runs_in_its_dataset() {
    let fx = Fixture::new().await;
    let (a, b) = (fx.dataset("a").await, fx.dataset("b").await);
    let file = fx.add_file(a, "Alice works at Acme.").await;
    fx.cognify(a, vec![file], graph_response("Alice", "Acme"))
        .await;

    let config = MemifyConfig::default();
    let run = |dataset_id: Uuid| {
        memify(
            fx.stores.graph_db(),
            fx.stores.vector_db(),
            Arc::new(MockEmbeddingEngine::new(8)),
            Arc::new(cognee_core::RayonThreadPool::with_default_threads().unwrap()),
            fx.db.clone(),
            Arc::clone(&fx.repo),
            Some(dataset_id),
            Some(fx.owner_id),
            None,
            &config,
        )
    };
    let in_a = run(a).await.expect("memify a");
    assert!(in_a.triplet_count > 0, "a's graph has a triplet to index");
    let in_b = run(b).await.expect("memify b");
    assert_eq!(
        in_b.triplet_count, 0,
        "b's graph is empty: it never saw a's"
    );
}

/// Without a dataset, the routing engines refuse instead of reaching a store.
#[tokio::test]
async fn an_unscoped_call_fails_closed() {
    let fx = Fixture::new().await;
    let error = fx.stores.graph_db().get_graph_data().await.unwrap_err();
    assert!(error.to_string().contains("no dataset scope"), "{error}");
}

/// A failed run is rolled back inside its dataset's store: the sweep reaches
/// the dataset's own graph, not a shared one (and does not fail closed).
#[tokio::test]
async fn a_failed_run_is_rolled_back_in_its_dataset() {
    let fx = Fixture::new().await;
    let a = fx.dataset("a").await;
    let good = fx.add_file(a, "Alice works at Acme.").await;
    let bad = fx.add_file(a, "Bob FAILMARKER breaks here.").await;
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(
        MockLlm::new(vec![graph_response("Alice", "Acme"); 8])
            .with_failing_markers(vec!["FAILMARKER".to_string()]),
    );

    let result = cognify(
        vec![good, bad],
        a,
        Some(fx.owner_id),
        None,
        None,
        llm,
        Arc::clone(&fx.storage),
        fx.stores.graph_db(),
        fx.stores.vector_db(),
        Arc::new(MockEmbeddingEngine::new(8)),
        fx.db.clone(),
        Arc::clone(&fx.repo),
        Arc::new(cognee_core::RayonThreadPool::with_default_threads().unwrap()),
        Arc::new(NoOpOntologyResolver::new()),
        &config(),
    )
    .await;

    assert!(
        matches!(result, Err(cognee_cognify::CognifyError::RunFailed { .. })),
        "{result:?}"
    );
    assert!(
        fx.names(a).await.is_empty(),
        "the rollback emptied the dataset's own graph: {:?}",
        fx.names(a).await
    );
}
