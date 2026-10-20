#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! A runtime custom graph (SDK-1124) is owned and removed like the default
//! one: every custom node and field edge is claimed per data item, so a node
//! two items share survives the first delete, and data, dataset and
//! forget-all deletes — and the run sweep after a failure — take the item's
//! custom graph nodes, its `{Model}_{field}` points and the `EdgeType` points
//! keyed on its field edges' stamped text with them.
//!
//! The `EdgeType` assertions are the ones that need the ledger's field-edge
//! rows to carry that text: a row with only the bare field name resolves to an
//! `"employees"` point cognify never writes.
//!
//! Offline throughout — see `rollback_harness`.

mod rollback_harness;

use std::collections::BTreeSet;
use std::sync::Arc;

use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::DeleteDb;
use cognee_delete::{DeleteMode, DeleteRequest, DeleteScope, DeleteService};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_models::EdgeType;
use cognee_ontology::NoOpOntologyResolver;
use cognee_test_utils::{MockLlm, MockVectorDB};
use cognee_vector::{SearchResult, VectorDB, VectorDBError, VectorDBResult, VectorPoint};
use rollback_harness::{FAIL_MARKER, Harness, extraction_config};
use sea_orm::ConnectionTrait;
use serde_json::json;
use uuid::Uuid;

const COMPANY_ID_NAME: &[u8] = b"Company:acme";
const PERSON_ID_NAME: &[u8] = b"Person:alice";
/// The text `add_data_points` stamps on the `employees` field edge, which is
/// what its `EdgeType_relationship_name` point is keyed on.
const FIELD_EDGE_TEXT: &str = "Acme employees Alice.";

fn company_id() -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, COMPANY_ID_NAME)
}

fn person_id() -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, PERSON_ID_NAME)
}

fn config() -> CognifyConfig {
    extraction_config().with_graph_schema(json!({
        "$defs": {
            "Person": {
                "title": "Person",
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "metadata": {"type": "object", "default": {
                        "index_fields": ["name"], "identity_fields": ["name"]
                    }}
                },
                "required": ["name"]
            }
        },
        "title": "Company",
        "type": "object",
        "properties": {
            "name": {"type": "string"},
            "employees": {"type": "array", "items": {"$ref": "#/$defs/Person"}},
            "metadata": {"type": "object", "default": {
                "index_fields": ["name"], "identity_fields": ["name"]
            }}
        },
        "required": ["name", "employees"]
    }))
}

/// Every chunk gets the same answer, so files share their custom nodes.
fn llm(calls: usize) -> Arc<dyn cognee_llm::Llm> {
    let answer = json!({"name": "Acme", "employees": [{"name": "Alice"}]}).to_string();
    Arc::new(MockLlm::new(vec![answer; calls]).with_failing_markers(vec![FAIL_MARKER.to_string()]))
}

fn delete_service(harness: &Harness) -> DeleteService {
    DeleteService::new(
        Arc::clone(&harness.storage),
        harness.db.clone() as Arc<dyn DeleteDb>,
    )
    .with_graph_db(Arc::clone(&harness.graph_db) as Arc<dyn GraphDBTrait>)
    .with_vector_db(Arc::clone(&harness.vector_db) as Arc<dyn VectorDB>)
}

async fn delete(harness: &Harness, scope: DeleteScope) {
    delete_service(harness)
        .execute(&DeleteRequest {
            scope,
            mode: DeleteMode::Soft,
            memory_only: false,
        })
        .await
        .expect("delete succeeds");
}

/// Which of the custom graph's artifacts are present, by store.
#[derive(Debug, PartialEq, Eq)]
struct CustomArtifacts {
    company_node: bool,
    person_node: bool,
    company_point: bool,
    person_point: bool,
    field_edge_type_point: bool,
}

impl CustomArtifacts {
    const ALL: Self = Self {
        company_node: true,
        person_node: true,
        company_point: true,
        person_point: true,
        field_edge_type_point: true,
    };
    const NONE: Self = Self {
        company_node: false,
        person_node: false,
        company_point: false,
        person_point: false,
        field_edge_type_point: false,
    };

    async fn of(harness: &Harness) -> Self {
        let nodes = harness.graph_node_id_set().await;
        let vectors = &harness.vector_db;
        Self {
            company_node: nodes.contains(&company_id().to_string()),
            person_node: nodes.contains(&person_id().to_string()),
            company_point: vectors
                .get_payload("Company", "name", company_id())
                .is_some(),
            person_point: vectors.get_payload("Person", "name", person_id()).is_some(),
            field_edge_type_point: vectors
                .get_payload(
                    "EdgeType",
                    "relationship_name",
                    EdgeType::deterministic_id(FIELD_EDGE_TEXT),
                )
                .is_some(),
        }
    }
}

/// One custom-cognified file, ready to be deleted.
async fn cognified_single_file() -> (Harness, Uuid) {
    let mut harness = Harness::new().await;
    let data_id = harness.add_file("Alice works at Acme.").await;
    harness
        .run_over(&config(), &[data_id], llm(4))
        .await
        .expect("custom cognify succeeds");
    assert_eq!(
        CustomArtifacts::of(&harness).await,
        CustomArtifacts::ALL,
        "cognify wrote every custom artifact"
    );
    (harness, data_id)
}

#[tokio::test]
async fn soft_deleting_a_custom_cognified_item_removes_its_custom_artifacts() {
    let (harness, data_id) = cognified_single_file().await;

    delete(
        &harness,
        DeleteScope::DataInDataset {
            dataset_id: harness.dataset_id,
            data_id,
            delete_dataset_if_empty: false,
        },
    )
    .await;

    assert_eq!(CustomArtifacts::of(&harness).await, CustomArtifacts::NONE);
    assert!(harness.ledger_nodes().await.is_empty());
}

#[tokio::test]
async fn deleting_a_custom_cognified_dataset_removes_its_custom_artifacts() {
    let (harness, _) = cognified_single_file().await;

    delete(
        &harness,
        DeleteScope::DatasetById {
            dataset_id: harness.dataset_id,
        },
    )
    .await;

    assert_eq!(CustomArtifacts::of(&harness).await, CustomArtifacts::NONE);
}

/// `forget(everything=True)` is `DeleteScope::User`: Python empties every
/// dataset point by point (`datasets.delete_all` → `empty_dataset`) and
/// leaves the collections themselves in place, so the custom collections
/// lose their points rather than disappear.
#[tokio::test]
async fn forget_all_removes_the_custom_points() {
    let (harness, _) = cognified_single_file().await;

    delete(
        &harness,
        DeleteScope::User {
            owner_id: harness.owner_id,
            tenant_id: None,
        },
    )
    .await;

    assert_eq!(CustomArtifacts::of(&harness).await, CustomArtifacts::NONE);
    for (data_type, field) in [("Company", "name"), ("Person", "name")] {
        assert_eq!(
            harness
                .vector_db
                .collection_size(data_type, field)
                .await
                .unwrap_or(0),
            0,
            "{data_type}_{field} is empty"
        );
    }
}

/// The store-wide wipe drops the custom `{Model}_{field}` collections along
/// with the built-in ones.
#[tokio::test]
async fn the_store_wide_wipe_drops_the_custom_collections() {
    let (harness, _) = cognified_single_file().await;

    delete(&harness, DeleteScope::All).await;

    for (data_type, field) in [("Company", "name"), ("Person", "name")] {
        assert!(
            !harness
                .vector_db
                .has_collection(data_type, field)
                .await
                .expect("collection lookup"),
            "{data_type}_{field} is dropped"
        );
    }
    assert!(harness.graph_node_ids().await.is_empty());
}

/// A custom node two data items produce is owned by both, and survives until
/// the last of them is deleted.
#[tokio::test]
async fn a_custom_node_two_items_share_keeps_both_owners() {
    let mut harness = Harness::new().await;
    let first = harness.add_file("Alice works at Acme.").await;
    let second = harness.add_file("Acme employs Alice.").await;
    harness
        .run_over(&config(), &[first, second], llm(8))
        .await
        .expect("custom cognify succeeds");

    let owners = |slug: Uuid| {
        let harness = &harness;
        async move {
            harness
                .ledger_nodes()
                .await
                .into_iter()
                .filter(|row| row.slug == slug)
                .map(|row| row.data_id)
                .collect::<BTreeSet<_>>()
        }
    };
    let both = BTreeSet::from([first, second]);
    assert_eq!(owners(company_id()).await, both);
    assert_eq!(owners(person_id()).await, both);
    let field_edge_owners: BTreeSet<Uuid> = harness
        .ledger_edges()
        .await
        .into_iter()
        .filter(|row| row.source_node_id == company_id() && row.destination_node_id == person_id())
        .map(|row| row.data_id)
        .collect();
    assert_eq!(field_edge_owners, both);

    let in_dataset = |data_id| DeleteScope::DataInDataset {
        dataset_id: harness.dataset_id,
        data_id,
        delete_dataset_if_empty: false,
    };
    delete(&harness, in_dataset(first)).await;
    assert_eq!(
        CustomArtifacts::of(&harness).await,
        CustomArtifacts::ALL,
        "the second item still claims the shared custom graph"
    );
    assert_eq!(owners(company_id()).await, BTreeSet::from([second]));

    delete(&harness, in_dataset(second)).await;
    assert_eq!(CustomArtifacts::of(&harness).await, CustomArtifacts::NONE);
}

/// Delegates to the harness's store but refuses to index the `Person`
/// collection — which `add_data_points` writes after the built-in collections,
/// `EdgeType_relationship_name` included, so the run fails with the field
/// edge's `EdgeType` point already in the store.
struct PersonIndexFails(Arc<MockVectorDB>);

#[async_trait::async_trait]
impl VectorDB for PersonIndexFails {
    async fn create_collection(
        &self,
        data_type: &str,
        field_name: &str,
        dimension: usize,
    ) -> VectorDBResult<()> {
        self.0
            .create_collection(data_type, field_name, dimension)
            .await
    }
    async fn has_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<bool> {
        self.0.has_collection(data_type, field_name).await
    }
    async fn index_points(
        &self,
        data_type: &str,
        field_name: &str,
        points: &[VectorPoint],
    ) -> VectorDBResult<()> {
        if data_type == "Person" {
            return Err(VectorDBError::StorageError(
                "simulated Person index outage".to_string(),
            ));
        }
        self.0.index_points(data_type, field_name, points).await
    }
    async fn search_similar(
        &self,
        data_type: &str,
        field_name: &str,
        query: &[f32],
        limit: usize,
    ) -> VectorDBResult<Vec<SearchResult>> {
        self.0
            .search_similar(data_type, field_name, query, limit)
            .await
    }
    async fn delete_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<()> {
        self.0.delete_collection(data_type, field_name).await
    }
    async fn delete_points(
        &self,
        data_type: &str,
        field_name: &str,
        point_ids: &[Uuid],
    ) -> VectorDBResult<()> {
        self.0.delete_points(data_type, field_name, point_ids).await
    }
    async fn retrieve(
        &self,
        data_type: &str,
        field_name: &str,
        ids: &[Uuid],
    ) -> VectorDBResult<Vec<SearchResult>> {
        self.0.retrieve(data_type, field_name, ids).await
    }
    async fn collection_size(&self, data_type: &str, field_name: &str) -> VectorDBResult<usize> {
        self.0.collection_size(data_type, field_name).await
    }
    async fn list_collections(&self) -> VectorDBResult<Vec<(String, String)>> {
        self.0.list_collections().await
    }
}

/// A failed run's sweep takes the custom artifacts it wrote with it, the
/// field edge's `EdgeType` point included.
#[tokio::test]
async fn a_failed_custom_run_is_swept_clean() {
    let mut harness = Harness::new().await;
    let data_id = harness.add_file("Alice works at Acme.").await;
    let vector_db = Arc::new(PersonIndexFails(Arc::clone(&harness.vector_db)));

    let result = cognify(
        vec![harness.data(data_id)],
        harness.dataset_id,
        Some(harness.owner_id),
        None,
        None,
        llm(4),
        Arc::clone(&harness.storage),
        Arc::clone(&harness.graph_db) as Arc<dyn GraphDBTrait>,
        vector_db as Arc<dyn VectorDB>,
        Arc::new(MockEmbeddingEngine::new(8)),
        harness.db.clone(),
        Arc::clone(&harness.repo),
        Arc::new(cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool")),
        Arc::new(NoOpOntologyResolver::new()),
        &config(),
    )
    .await;

    assert!(result.is_err(), "the Person index failure fails the run");
    assert!(
        harness
            .vector_db
            .was_create_collection_called("EdgeType", "relationship_name")
            && harness
                .vector_db
                .was_create_collection_called("Company", "name"),
        "the run wrote its EdgeType and Company points before it failed"
    );
    assert_eq!(CustomArtifacts::of(&harness).await, CustomArtifacts::NONE);
    assert!(harness.ledger_nodes().await.is_empty());
    assert!(harness.ledger_edges().await.is_empty());
}

/// A graph-store failure after the ownership ledger was written — on the node
/// write, or on the edge write after the custom nodes already landed — is
/// swept like the vector-stage one: no custom node, point or ledger row
/// survives the run.
#[tokio::test]
async fn a_custom_run_whose_graph_write_fails_is_swept_clean() {
    for failing_write in ["add_nodes", "add_edges"] {
        let mut harness = Harness::new().await;
        let data_id = harness.add_file("Alice works at Acme.").await;
        match failing_write {
            "add_nodes" => harness
                .graph_db
                .set_add_nodes_error("simulated node outage"),
            _ => harness
                .graph_db
                .set_add_edges_error("simulated edge outage"),
        }

        let result = harness.run_over(&config(), &[data_id], llm(4)).await;

        let error = format!("{:?}", result.expect_err("the graph failure fails the run"));
        assert!(error.contains("simulated"), "{failing_write}: {error}");
        assert_eq!(
            CustomArtifacts::of(&harness).await,
            CustomArtifacts::NONE,
            "{failing_write}"
        );
        assert!(harness.ledger_nodes().await.is_empty(), "{failing_write}");
        assert!(harness.ledger_edges().await.is_empty(), "{failing_write}");
    }
}

/// An ownership-ledger write that fails part-way — the custom field edge's
/// row is refused after the node rows went in — fails the run before any
/// custom artifact is written, and the sweep removes the rows that did land.
#[tokio::test]
async fn a_custom_run_whose_ledger_write_fails_is_swept_clean() {
    let mut harness = Harness::new().await;
    let data_id = harness.add_file("Alice works at Acme.").await;
    harness
        .db
        .execute_unprepared(
            "CREATE TRIGGER refuse_custom_field_edge BEFORE INSERT ON edges \
             WHEN NEW.relationship_name = 'employees' \
             BEGIN SELECT RAISE(ABORT, 'simulated ledger outage'); END",
        )
        .await
        .expect("install the failing-ledger trigger");

    let result = harness.run_over(&config(), &[data_id], llm(4)).await;

    let error = format!(
        "{:?}",
        result.expect_err("the ledger failure fails the run")
    );
    assert!(error.contains("simulated ledger outage"), "{error}");
    assert_eq!(CustomArtifacts::of(&harness).await, CustomArtifacts::NONE);
    assert!(harness.ledger_nodes().await.is_empty());
    assert!(harness.ledger_edges().await.is_empty());
}

/// The custom schema with no `identity_fields`: every node then gets a fresh
/// uuid4 per run, so two runs over the same text produce field edges with
/// different slugs but one shared `EdgeType` retrieval text.
fn config_without_identity() -> CognifyConfig {
    extraction_config().with_graph_schema(json!({
        "$defs": {
            "Person": {
                "title": "Person",
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "metadata": {"type": "object", "default": {"index_fields": ["name"]}}
                },
                "required": ["name"]
            }
        },
        "title": "Company",
        "type": "object",
        "properties": {
            "name": {"type": "string"},
            "employees": {"type": "array", "items": {"$ref": "#/$defs/Person"}},
            "metadata": {"type": "object", "default": {"index_fields": ["name"]}}
        },
        "required": ["name", "employees"]
    }))
}

/// A failed run's sweep keeps an `EdgeType` point a successful run's field
/// edge still claims. The slug check cannot protect it — without identity
/// fields the two runs' field edges have different slugs — so this rests on
/// the sweep reading the surviving row's stamped `edge_text`, not the
/// `"employees"` name column every field-edge row shares.
#[tokio::test]
async fn a_failed_custom_run_keeps_a_field_edge_point_a_successful_run_claims() {
    let mut harness = Harness::new().await;
    let edge_type_point = |harness: &Harness| {
        harness
            .vector_db
            .get_payload(
                "EdgeType",
                "relationship_name",
                EdgeType::deterministic_id(FIELD_EDGE_TEXT),
            )
            .is_some()
    };

    let first = harness.add_file("Alice works at Acme.").await;
    harness
        .run_over(&config_without_identity(), &[first], llm(4))
        .await
        .expect("the first custom cognify succeeds");
    assert!(edge_type_point(&harness), "the first run wrote the point");

    let second = harness.add_file("Acme employs Alice.").await;
    let vector_db = Arc::new(PersonIndexFails(Arc::clone(&harness.vector_db)));
    let result = cognify(
        vec![harness.data(second)],
        harness.dataset_id,
        Some(harness.owner_id),
        None,
        None,
        llm(4),
        Arc::clone(&harness.storage),
        Arc::clone(&harness.graph_db) as Arc<dyn GraphDBTrait>,
        vector_db as Arc<dyn VectorDB>,
        Arc::new(MockEmbeddingEngine::new(8)),
        harness.db.clone(),
        Arc::clone(&harness.repo),
        Arc::new(cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool")),
        Arc::new(NoOpOntologyResolver::new()),
        &config_without_identity(),
    )
    .await;
    assert!(result.is_err(), "the Person index failure fails the run");

    let first_field_edges = harness
        .ledger_edges()
        .await
        .into_iter()
        .filter(|row| row.data_id == first && row.relationship_name == "employees")
        .count();
    assert_eq!(
        first_field_edges, 1,
        "the first run's field-edge row survives"
    );
    let leftover: Vec<_> = harness
        .ledger_edges()
        .await
        .into_iter()
        .filter(|row| row.data_id == second)
        .map(|row| row.relationship_name)
        .collect();
    assert!(
        leftover.is_empty(),
        "the failed run's rows are swept: {leftover:?}"
    );
    assert!(
        edge_type_point(&harness),
        "the sweep kept the EdgeType point the first run's field edge still claims"
    );
}
