#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The ledger queries behind per-document delete (SDK-1694 O6).
//!
//! * An edge row is the last claim on its edge when no other owner's row names
//!   the same `(source, target, relationship)` triple. Every `is_a` row shares
//!   one slug (Python's `generate_edge_id("is_a")`), so the slug said nothing:
//!   a document's own `is_a` edge was never its own while another document
//!   had any. `get_unique_edges_for_data` (both claimant scopes),
//!   `get_unique_edges_for_datasets` and `get_unique_edges_for_run` count by
//!   triple.
//! * A legacy nil-`data_id` row — what a database written before O6 holds for
//!   every EntityType and structural edge — is still a claim, of the type's
//!   slug and of the edge's triple.
//! * `get_node_slugs_owned_by_data` answers which degree-one types a
//!   document still claims, for the hard delete's sweep: nil rows do not
//!   count, the dataset narrows, and the slug list is batched.
//!
//! Every case runs on in-memory SQLite and, when `TEST_POSTGRES_URL` is set,
//! on a throwaway Postgres database: the predicates are correlated `EXISTS`
//! subqueries over three columns.
#![cfg(feature = "sqlite")]

use std::collections::BTreeSet;

use chrono::Utc;
use cognee_database::ops::datasets::create_dataset;
use cognee_database::ops::graph_storage::{
    RunScope, get_node_slugs_owned_by_data, get_unique_edges_for_data,
    get_unique_edges_for_datasets, get_unique_edges_for_run, get_unique_nodes_for_data,
    upsert_edges, upsert_nodes,
};
use cognee_database::{
    ClaimantScope, DatabaseConnection, GraphEdge, GraphNode, connect, initialize,
};
use cognee_models::Dataset;
use sea_orm::ConnectionTrait;
use serde_json::json;
use uuid::Uuid;

const NIL: Uuid = Uuid::nil();

fn id(name: &str) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes())
}

/// The slug every `is_a` row shares, as `edge_slug("is_a")` derives it.
fn is_a_slug() -> Uuid {
    id("is_a")
}

struct World<'a> {
    db: &'a DatabaseConnection,
    user: Uuid,
    dataset: Uuid,
    other_dataset: Uuid,
    run: Uuid,
}

impl<'a> World<'a> {
    async fn new(db: &'a DatabaseConnection) -> Self {
        let user = Uuid::new_v4();
        let (dataset, other_dataset) = (Uuid::new_v4(), Uuid::new_v4());
        for dataset in [dataset, other_dataset] {
            create_dataset(
                db,
                Dataset::new(format!("o6-{dataset}"), user, None, dataset),
            )
            .await
            .expect("dataset");
        }
        Self {
            db,
            user,
            dataset,
            other_dataset,
            run: Uuid::new_v4(),
        }
    }

    async fn node(&self, name: &str, node_type: &str, dataset: Uuid, data: Uuid) {
        upsert_nodes(
            self.db,
            &[GraphNode {
                id: Uuid::new_v4(),
                slug: id(name),
                user_id: self.user,
                data_id: data,
                dataset_id: dataset,
                pipeline_run_id: Some(self.run),
                label: Some(name.to_string()),
                node_type: node_type.into(),
                indexed_fields: json!(["name"]),
                attributes: None,
                created_at: Utc::now(),
            }],
        )
        .await
        .expect("upsert node");
    }

    /// One `entity -is_a-> type` row for `(dataset, data)`.
    async fn is_a(&self, entity: &str, kind: &str, dataset: Uuid, data: Uuid) {
        upsert_edges(
            self.db,
            &[GraphEdge {
                id: Uuid::new_v4(),
                slug: is_a_slug(),
                user_id: self.user,
                data_id: data,
                dataset_id: dataset,
                pipeline_run_id: Some(self.run),
                source_node_id: id(entity),
                destination_node_id: id(kind),
                relationship_name: "is_a".into(),
                label: None,
                attributes: Some(json!({ "edge_text": format!("{entity} is a {kind}.") })),
                created_at: Utc::now(),
            }],
        )
        .await
        .expect("upsert edge");
    }
}

fn triples(rows: Vec<GraphEdge>) -> BTreeSet<(Uuid, Uuid)> {
    rows.into_iter()
        .map(|row| (row.source_node_id, row.destination_node_id))
        .collect()
}

fn pair(entity: &str, kind: &str) -> (Uuid, Uuid) {
    (id(entity), id(kind))
}

/// Doc A types Apple as a company; doc B types Apple and Pear as fruit; doc
/// C types Pear as fruit too, and a document of the other dataset types
/// Apple as a company. Every row shares the `is_a` slug.
async fn is_a_rows_are_counted_by_triple(db: &DatabaseConnection) {
    let w = World::new(db).await;
    let (a, b, c, other) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    w.is_a("apple", "company", w.dataset, a).await;
    w.is_a("apple", "fruit", w.dataset, b).await;
    w.is_a("pear", "fruit", w.dataset, b).await;
    w.is_a("pear", "fruit", w.dataset, c).await;
    w.is_a("apple", "company", w.other_dataset, other).await;

    let unique = |data, scope| get_unique_edges_for_data(db, data, w.dataset, scope);
    for scope in [ClaimantScope::SameDataset, ClaimantScope::AnyDataset] {
        assert_eq!(
            triples(unique(b, scope).await.unwrap()),
            BTreeSet::from([pair("apple", "fruit")]),
            "{scope:?}: B alone declared Apple a fruit; C shares Pear's"
        );
        assert!(
            triples(unique(c, scope).await.unwrap()).is_empty(),
            "{scope:?}"
        );
    }
    // Apple the company: A's alone in the dataset, not in the shared store.
    assert_eq!(
        triples(unique(a, ClaimantScope::SameDataset).await.unwrap()),
        BTreeSet::from([pair("apple", "company")])
    );
    assert!(
        triples(unique(a, ClaimantScope::AnyDataset).await.unwrap()).is_empty(),
        "the other dataset declares the same pair"
    );

    // A dataset delete: the dataset's triples the other dataset has no row of.
    assert_eq!(
        triples(
            get_unique_edges_for_datasets(db, &[w.dataset])
                .await
                .unwrap()
        ),
        BTreeSet::from([pair("apple", "fruit"), pair("pear", "fruit")])
    );
    assert_eq!(
        triples(
            get_unique_edges_for_datasets(db, &[w.other_dataset])
                .await
                .unwrap()
        ),
        BTreeSet::new()
    );

    // A run sweep of B alone, and of the whole run in the dataset.
    let items = [b];
    assert_eq!(
        triples(
            get_unique_edges_for_run(db, &RunScope::for_data(w.run, w.dataset, &items))
                .await
                .unwrap()
        ),
        BTreeSet::from([pair("apple", "fruit")])
    );
    assert_eq!(
        triples(
            get_unique_edges_for_run(db, &RunScope::whole_run(w.run, w.dataset))
                .await
                .unwrap()
        ),
        BTreeSet::from([pair("apple", "fruit"), pair("pear", "fruit")])
    );
}

/// A legacy ledger: the fruit type and Apple's `is_a` edge to it are claimed
/// by a nil row, beside doc B's own rows. Neither is B's alone, while a type
/// and a triple only B's rows name are.
async fn legacy_nil_rows_are_still_claims(db: &DatabaseConnection) {
    let w = World::new(db).await;
    let b = Uuid::new_v4();
    w.node("fruit", "EntityType", w.dataset, NIL).await;
    w.node("fruit", "EntityType", w.dataset, b).await;
    w.node("gadget", "EntityType", w.dataset, b).await;
    w.is_a("apple", "fruit", w.dataset, NIL).await;
    w.is_a("apple", "fruit", w.dataset, b).await;
    w.is_a("apple", "gadget", w.dataset, b).await;

    for scope in [ClaimantScope::SameDataset, ClaimantScope::AnyDataset] {
        let nodes: BTreeSet<Uuid> = get_unique_nodes_for_data(db, b, w.dataset, scope)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.slug)
            .collect();
        assert_eq!(nodes, BTreeSet::from([id("gadget")]), "{scope:?}");
        assert_eq!(
            triples(
                get_unique_edges_for_data(db, b, w.dataset, scope)
                    .await
                    .unwrap()
            ),
            BTreeSet::from([pair("apple", "gadget")]),
            "{scope:?}"
        );
    }
}

/// The degree-one sweep's question: of these slugs, which does a document's
/// row still claim? Not one with nil rows only, not one claimed only in
/// another dataset when a dataset is named, and every one of a list longer
/// than a batch.
async fn owned_slugs_ignore_nil_rows_and_narrow_by_dataset(db: &DatabaseConnection) {
    let w = World::new(db).await;
    let (a, other) = (Uuid::new_v4(), Uuid::new_v4());
    w.node("company", "EntityType", w.dataset, a).await;
    w.node("company", "EntityType", w.dataset, NIL).await;
    w.node("legacy", "EntityType", w.dataset, NIL).await;
    w.node("elsewhere", "EntityType", w.other_dataset, other)
        .await;
    let asked = [id("company"), id("legacy"), id("elsewhere"), id("absent")];

    let owned = |dataset| get_node_slugs_owned_by_data(db, &asked, dataset);
    assert_eq!(
        owned(None)
            .await
            .unwrap()
            .into_iter()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([id("company"), id("elsewhere")])
    );
    assert_eq!(owned(Some(w.dataset)).await.unwrap(), vec![id("company")]);
    assert!(
        get_node_slugs_owned_by_data(db, &[], None)
            .await
            .unwrap()
            .is_empty()
    );

    // More slugs than one `IN` list holds.
    let names: Vec<String> = (0..1_200).map(|n| format!("type-{n}")).collect();
    for name in names.iter().step_by(100) {
        w.node(name, "EntityType", w.dataset, a).await;
    }
    let slugs: Vec<Uuid> = names.iter().map(|name| id(name)).collect();
    let owned: BTreeSet<Uuid> = get_node_slugs_owned_by_data(db, &slugs, Some(w.dataset))
        .await
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(
        owned,
        names.iter().step_by(100).map(|name| id(name)).collect(),
        "every batch is read"
    );
}

async fn clear_ledger(db: &DatabaseConnection) {
    for table in ["nodes", "edges"] {
        db.execute_unprepared(&format!("DELETE FROM {table}"))
            .await
            .expect("clear ledger");
    }
}

async fn run_all(db: &DatabaseConnection) {
    is_a_rows_are_counted_by_triple(db).await;
    clear_ledger(db).await;
    legacy_nil_rows_are_still_claims(db).await;
    clear_ledger(db).await;
    owned_slugs_ignore_nil_rows_and_narrow_by_dataset(db).await;
}

#[tokio::test]
async fn per_document_delete_sqlite() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("migrate");
    run_all(&db).await;
}

/// The same suite on Postgres. Skipped when `TEST_POSTGRES_URL` is unset; the
/// run gets its own throwaway database.
#[tokio::test]
async fn per_document_delete_postgres() {
    let Some(base_url) = cognee_test_utils::test_postgres_url() else {
        eprintln!("TEST_POSTGRES_URL not set — skipping per_document_delete_postgres");
        return;
    };
    let tmp = cognee_test_utils::create_temp_postgres_db(&base_url)
        .await
        .expect("create temp Postgres database");
    let db = connect(tmp.url()).await.expect("connect to temp Postgres");
    initialize(&db).await.expect("migrate");
    run_all(&db).await;
    drop(db);
    tmp.cleanup().await;
}
