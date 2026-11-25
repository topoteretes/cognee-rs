#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The run sweep's handling of nil-`data_id` rows (SDK-1694 O5b).
//!
//! Since O5a an EntityType, a structural edge and an ontology-first entity
//! carry one ledger row per producing document plus a nil-`data_id` row, the
//! guard the data-scoped delete reads. Two sweeps decide about those nil rows:
//!
//! * the item-scoped sweep (`RunScope::for_data`) takes a nil row of the run
//!   when a swept row of the same artifact stands beside it and no non-nil row
//!   survives — nodes by slug, edges by (source, target, relationship);
//! * the sweep that keeps a run's completed items (`retag_run_rows` +
//!   `retag_guarded_nil_rows`, then a whole-run sweep, then the move back)
//!   takes every nil row of the run no surviving non-nil row stands beside,
//!   including the run-level ones no document names (a DLT schema node).
//!
//! The world covers a nil row tagged by an earlier run, a surviving sibling in
//! another run and with a `NULL` run, a sibling only in another dataset, an
//! ontology-first entity, a run-level nil-only row, and `is_a` edges that
//! share one slug. Every case runs on in-memory SQLite and, when
//! `TEST_POSTGRES_URL` is set, on a throwaway Postgres database: the
//! predicates are correlated `EXISTS` subqueries inside `SELECT`, `DELETE`
//! and `UPDATE`.
#![cfg(feature = "sqlite")]

use std::collections::BTreeSet;

use chrono::Utc;
use cognee_database::ops::datasets::create_dataset;
use cognee_database::ops::graph_storage::{
    RunScope, delete_edges_for_run, delete_nodes_for_run, get_data_ids_for_run,
    get_edge_retrieval_texts_claimed_outside_run, get_edges_by_dataset, get_nodes_by_dataset,
    get_unique_edges_for_run, get_unique_nodes_for_run, retag_guarded_nil_rows, retag_run_rows,
    upsert_edges, upsert_nodes,
};
use cognee_database::{DatabaseConnection, GraphEdge, GraphNode, connect, initialize};
use cognee_models::Dataset;
use serde_json::json;
use uuid::Uuid;

const NIL: Uuid = Uuid::nil();

/// Artifacts, by name: their slug is derived from the name.
fn slug(name: &str) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, name.as_bytes())
}

/// The shared slug of every `is_a` row (as `edge_slug("is_a")` is).
fn is_a_slug() -> Uuid {
    slug("relationship:is_a")
}

struct World<'a> {
    db: &'a DatabaseConnection,
    user: Uuid,
    dataset: Uuid,
    other_dataset: Uuid,
    /// This run, and an earlier one.
    run: Uuid,
    earlier: Uuid,
    /// The swept document, the kept one, another run's document.
    swept: Uuid,
    kept: Uuid,
    outsider: Uuid,
}

impl<'a> World<'a> {
    async fn new(db: &'a DatabaseConnection) -> Self {
        let user = Uuid::new_v4();
        let dataset = Uuid::new_v4();
        let other_dataset = Uuid::new_v4();
        for id in [dataset, other_dataset] {
            create_dataset(db, Dataset::new(format!("nil-{id}"), user, None, id))
                .await
                .expect("dataset");
        }
        let world = Self {
            db,
            user,
            dataset,
            other_dataset,
            run: Uuid::new_v4(),
            earlier: Uuid::new_v4(),
            swept: Uuid::new_v4(),
            kept: Uuid::new_v4(),
            outsider: Uuid::new_v4(),
        };
        world.seed().await;
        world
    }

    async fn node(
        &self,
        name: &str,
        node_type: &str,
        dataset: Uuid,
        data: Uuid,
        run: Option<Uuid>,
    ) {
        upsert_nodes(
            self.db,
            &[GraphNode {
                id: Uuid::new_v4(),
                slug: slug(name),
                user_id: self.user,
                data_id: data,
                dataset_id: dataset,
                pipeline_run_id: run,
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

    async fn is_a(&self, source: &str, target: &str, data: Uuid, run: Option<Uuid>) {
        upsert_edges(
            self.db,
            &[GraphEdge {
                id: Uuid::new_v4(),
                slug: is_a_slug(),
                user_id: self.user,
                data_id: data,
                dataset_id: self.dataset,
                pipeline_run_id: run,
                source_node_id: slug(source),
                destination_node_id: slug(target),
                relationship_name: "is_a".into(),
                label: None,
                attributes: None,
                created_at: Utc::now(),
            }],
        )
        .await
        .expect("upsert edge");
    }

    async fn seed(&self) {
        let (d, run, s, k) = (self.dataset, Some(self.run), self.swept, self.kept);
        // A type only the swept document declared.
        self.node("only_swept", "EntityType", d, s, run).await;
        self.node("only_swept", "EntityType", d, NIL, run).await;
        // A type the kept document declared too.
        self.node("shared_with_kept", "EntityType", d, s, run).await;
        self.node("shared_with_kept", "EntityType", d, k, run).await;
        self.node("shared_with_kept", "EntityType", d, NIL, run)
            .await;
        // A type whose nil row an earlier run wrote (the row kept its id).
        self.node("earlier_nil", "EntityType", d, s, run).await;
        self.node("earlier_nil", "EntityType", d, NIL, Some(self.earlier))
            .await;
        // Surviving siblings in another run and with a NULL run.
        self.node("other_run_sibling", "EntityType", d, s, run)
            .await;
        self.node("other_run_sibling", "EntityType", d, NIL, run)
            .await;
        self.node(
            "other_run_sibling",
            "EntityType",
            d,
            self.outsider,
            Some(self.earlier),
        )
        .await;
        self.node("null_run_sibling", "EntityType", d, s, run).await;
        self.node("null_run_sibling", "EntityType", d, NIL, run)
            .await;
        self.node("null_run_sibling", "EntityType", d, self.outsider, None)
            .await;
        // A sibling only in another dataset: no guard here, but a claimant.
        self.node("other_dataset_sibling", "EntityType", d, s, run)
            .await;
        self.node("other_dataset_sibling", "EntityType", d, NIL, run)
            .await;
        self.node(
            "other_dataset_sibling",
            "EntityType",
            self.other_dataset,
            self.outsider,
            run,
        )
        .await;
        // Ontology-first entities: one the swept document alone looked up,
        // one the kept document reached too.
        self.node("onto_only_swept", "Entity", d, s, run).await;
        self.node("onto_only_swept", "Entity", d, NIL, run).await;
        self.node("onto_shared", "Entity", d, s, run).await;
        self.node("onto_shared", "Entity", d, k, run).await;
        self.node("onto_shared", "Entity", d, NIL, run).await;
        // A run-level row no document names (a DLT schema node).
        self.node("schema_table", "SchemaTable", d, NIL, run).await;
        // `is_a` edges sharing one slug, matched on their triple.
        self.is_a("x", "only_swept", s, run).await;
        self.is_a("x", "only_swept", NIL, run).await;
        self.is_a("y", "shared_with_kept", s, run).await;
        self.is_a("y", "shared_with_kept", k, run).await;
        self.is_a("y", "shared_with_kept", NIL, run).await;
    }

    /// (name, data id) of every node row left in the dataset.
    async fn node_rows(&self) -> BTreeSet<(String, Uuid)> {
        get_nodes_by_dataset(self.db, self.dataset)
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.label.unwrap_or_default(), row.data_id))
            .collect()
    }

    /// (target slug, data id) of every `is_a` row left in the dataset.
    async fn edge_rows(&self) -> BTreeSet<(Uuid, Uuid)> {
        get_edges_by_dataset(self.db, self.dataset)
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.destination_node_id, row.data_id))
            .collect()
    }

    fn named(&self, rows: &[(&str, Uuid)]) -> BTreeSet<(String, Uuid)> {
        rows.iter().map(|(n, d)| (n.to_string(), *d)).collect()
    }
}

fn names(rows: Vec<GraphNode>) -> BTreeSet<String> {
    rows.into_iter()
        .map(|row| row.label.unwrap_or_default())
        .collect()
}

/// The item-scoped sweep of the swept document.
async fn item_sweep(db: &DatabaseConnection) {
    let w = World::new(db).await;
    let (s, k, o) = (w.swept, w.kept, w.outsider);
    let items = [s];
    let scope = RunScope::for_data(w.run, w.dataset, &items);

    // Only artifacts nothing outside the scope claims may go.
    assert_eq!(
        names(get_unique_nodes_for_run(db, &scope).await.unwrap()),
        ["only_swept", "onto_only_swept"]
            .into_iter()
            .map(String::from)
            .collect(),
    );
    // The `is_a` slug is shared, and the kept document's row claims it.
    assert!(
        get_unique_edges_for_run(db, &scope)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        get_edge_retrieval_texts_claimed_outside_run(db, &scope)
            .await
            .unwrap()
            .contains(&"is_a".to_string())
    );
    assert_eq!(
        get_data_ids_for_run(db, &scope).await.unwrap(),
        vec![s],
        "a swept nil row names no data item"
    );

    delete_edges_for_run(db, &scope).await.unwrap();
    delete_nodes_for_run(db, &scope).await.unwrap();

    assert_eq!(
        w.node_rows().await,
        w.named(&[
            ("shared_with_kept", k),
            ("shared_with_kept", NIL),
            ("earlier_nil", NIL),
            ("other_run_sibling", NIL),
            ("other_run_sibling", o),
            ("null_run_sibling", NIL),
            ("null_run_sibling", o),
            ("onto_shared", k),
            ("onto_shared", NIL),
            // No swept row stands beside it: the item scope says nothing.
            ("schema_table", NIL),
        ]),
        "the nil rows of `only_swept`, `onto_only_swept` and `other_dataset_sibling` \
         (its sibling is in another dataset) went with the swept item"
    );
    assert_eq!(
        w.edge_rows().await,
        [
            (slug("shared_with_kept"), k),
            (slug("shared_with_kept"), NIL)
        ]
        .into_iter()
        .collect(),
        "the `is_a` nil row of the swept item's triple went; the kept one's stayed"
    );
}

/// The sweep of the whole run except the kept document.
async fn keeping_sweep(db: &DatabaseConnection) {
    let w = World::new(db).await;
    let (k, o) = (w.kept, w.outsider);
    let holding = Uuid::new_v4();

    retag_run_rows(db, w.run, w.dataset, Some(&[k]), holding)
        .await
        .unwrap();
    retag_guarded_nil_rows(db, w.run, w.dataset, holding)
        .await
        .unwrap();
    let whole = RunScope::whole_run(w.run, w.dataset);
    assert_eq!(
        names(get_unique_nodes_for_run(db, &whole).await.unwrap()),
        ["only_swept", "onto_only_swept", "schema_table"]
            .into_iter()
            .map(String::from)
            .collect(),
        "the kept and outside rows claim the rest"
    );
    assert_eq!(
        get_data_ids_for_run(db, &whole).await.unwrap(),
        vec![w.swept],
        "the kept item's marker is not cleared, and no nil id is reported"
    );
    delete_edges_for_run(db, &whole).await.unwrap();
    delete_nodes_for_run(db, &whole).await.unwrap();
    let back = retag_run_rows(db, holding, w.dataset, None, w.run)
        .await
        .unwrap();
    assert!(back > 0);

    assert_eq!(
        w.node_rows().await,
        w.named(&[
            ("shared_with_kept", k),
            ("shared_with_kept", NIL),
            ("earlier_nil", NIL),
            ("other_run_sibling", NIL),
            ("other_run_sibling", o),
            ("null_run_sibling", NIL),
            ("null_run_sibling", o),
            ("onto_shared", k),
            ("onto_shared", NIL),
        ]),
        "the run-level schema row goes too; every guard a surviving row needs stays"
    );
    // Moved back: the kept rows and their guards name this run again.
    let rows = get_nodes_by_dataset(db, w.dataset).await.unwrap();
    for row in rows.iter().filter(|row| row.data_id == k) {
        assert_eq!(row.pipeline_run_id, Some(w.run), "{row:?}");
    }
    assert!(
        rows.iter().all(|row| row.pipeline_run_id != Some(holding)),
        "nothing is left under the holding id"
    );
    assert_eq!(
        w.edge_rows().await,
        [
            (slug("shared_with_kept"), k),
            (slug("shared_with_kept"), NIL)
        ]
        .into_iter()
        .collect(),
    );
}

// Each scenario gets a database of its own: the artifacts' slugs are derived
// from their names, so a second world in one database would claim the first
// one's artifacts from another dataset.

#[tokio::test]
async fn run_sweep_nil_rows_sqlite() {
    for scenario in 0..2 {
        let db = connect("sqlite::memory:").await.expect("connect");
        initialize(&db).await.expect("migrate");
        match scenario {
            0 => item_sweep(&db).await,
            _ => keeping_sweep(&db).await,
        }
    }
}

/// The same suite on Postgres. Skipped when `TEST_POSTGRES_URL` is unset;
/// each scenario gets its own throwaway database.
#[tokio::test]
async fn run_sweep_nil_rows_postgres() {
    let Some(base_url) = cognee_test_utils::test_postgres_url() else {
        eprintln!("TEST_POSTGRES_URL not set — skipping run_sweep_nil_rows_postgres");
        return;
    };
    for scenario in 0..2 {
        let tmp = cognee_test_utils::create_temp_postgres_db(&base_url)
            .await
            .expect("create temp Postgres database");
        let db = connect(tmp.url()).await.expect("connect to temp Postgres");
        initialize(&db).await.expect("migrate");
        match scenario {
            0 => item_sweep(&db).await,
            _ => keeping_sweep(&db).await,
        }
        drop(db);
        tmp.cleanup().await;
    }
}
