#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Tests for the run-scoped provenance queries — the ledger's answer to
//! "what did this run create, and which of it is safe to delete".
//!
//! The selection predicate is `pipeline_run_id = :run AND dataset_id = :dataset
//! [AND data_id IN (:data…)]`; exclusivity is its negation, expressed as a
//! correlated NOT EXISTS over the predicate rather than over a list of the
//! selected row ids.
//!
//! Runs on in-memory SQLite (NOT EXISTS is standard SQL, identical on Postgres).
#![cfg(feature = "sqlite")]

use chrono::Utc;
use cognee_database::ops::datasets::create_dataset;
use cognee_database::ops::graph_storage::{
    RunScope, delete_edges_for_run, delete_nodes_for_run, get_data_ids_for_run, get_edges_for_run,
    get_nodes_for_run, get_relationship_names_claimed_outside_run, get_unique_edges_for_run,
    get_unique_nodes_for_run, upsert_edges, upsert_nodes,
};
use cognee_database::{DatabaseConnection, GraphEdge, GraphNode, connect, initialize};
use cognee_models::Dataset;
use serde_json::json;
use uuid::Uuid;

/// One user, two datasets, two data items, two runs — everything the queries
/// have to tell apart.
struct Fixture {
    db: DatabaseConnection,
    user: Uuid,
    /// The dataset under test.
    dataset_d: Uuid,
    /// A second dataset, used to prove selection and exclusivity are scoped.
    dataset_e: Uuid,
    data_a: Uuid,
    data_b: Uuid,
    run_1: Uuid,
    run_2: Uuid,
}

impl Fixture {
    async fn new() -> Self {
        let db = connect("sqlite::memory:").await.expect("connect");
        initialize(&db).await.expect("migrate");

        let user = Uuid::new_v4();
        let dataset_d = Uuid::new_v4();
        let dataset_e = Uuid::new_v4();
        for (id, name) in [(dataset_d, "d"), (dataset_e, "e")] {
            create_dataset(&db, Dataset::new(name.into(), user, None, id))
                .await
                .expect("dataset");
        }

        Self {
            db,
            user,
            dataset_d,
            dataset_e,
            data_a: Uuid::new_v4(),
            data_b: Uuid::new_v4(),
            run_1: Uuid::new_v4(),
            run_2: Uuid::new_v4(),
        }
    }

    fn node(&self, dataset: Uuid, data: Uuid, run: Option<Uuid>, slug: Uuid) -> GraphNode {
        GraphNode {
            id: Uuid::new_v4(),
            slug,
            user_id: self.user,
            data_id: data,
            dataset_id: dataset,
            pipeline_run_id: run,
            label: Some("n".into()),
            node_type: "Entity".into(),
            indexed_fields: json!({ "index_fields": ["name"] }),
            attributes: None,
            created_at: Utc::now(),
        }
    }

    fn edge(&self, dataset: Uuid, data: Uuid, run: Option<Uuid>, slug: Uuid) -> GraphEdge {
        GraphEdge {
            id: Uuid::new_v4(),
            slug,
            user_id: self.user,
            data_id: data,
            dataset_id: dataset,
            pipeline_run_id: run,
            source_node_id: Uuid::new_v4(),
            destination_node_id: Uuid::new_v4(),
            relationship_name: "is_a".into(),
            label: None,
            attributes: None,
            created_at: Utc::now(),
        }
    }

    async fn seed_nodes(&self, nodes: &[GraphNode]) {
        upsert_nodes(&self.db, nodes).await.expect("upsert nodes");
    }

    async fn seed_edges(&self, edges: &[GraphEdge]) {
        upsert_edges(&self.db, edges).await.expect("upsert edges");
    }
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_selection_returns_only_this_runs_rows() {
    let f = Fixture::new().await;
    let mine = f.node(f.dataset_d, f.data_a, Some(f.run_1), Uuid::new_v4());
    let theirs = f.node(f.dataset_d, f.data_a, Some(f.run_2), Uuid::new_v4());
    f.seed_nodes(&[mine.clone(), theirs]).await;

    let my_edge = f.edge(f.dataset_d, f.data_a, Some(f.run_1), Uuid::new_v4());
    let their_edge = f.edge(f.dataset_d, f.data_a, Some(f.run_2), Uuid::new_v4());
    f.seed_edges(&[my_edge.clone(), their_edge]).await;

    let scope = RunScope::whole_run(f.run_1, f.dataset_d);

    let nodes = get_nodes_for_run(&f.db, &scope).await.expect("nodes");
    assert_eq!(nodes.len(), 1, "only run 1's node");
    assert_eq!(nodes[0].id, mine.id);
    assert_eq!(nodes[0].pipeline_run_id, Some(f.run_1));

    let edges = get_edges_for_run(&f.db, &scope).await.expect("edges");
    assert_eq!(edges.len(), 1, "only run 1's edge");
    assert_eq!(edges[0].id, my_edge.id);
    assert_eq!(edges[0].pipeline_run_id, Some(f.run_1));
}

/// The permanent-exemption invariant: rows written before run ownership
/// existed carry a NULL run and no sweep may ever select them.
#[tokio::test]
async fn legacy_null_run_rows_are_never_selected() {
    let f = Fixture::new().await;
    f.seed_nodes(&[f.node(f.dataset_d, f.data_a, None, Uuid::new_v4())])
        .await;
    f.seed_edges(&[f.edge(f.dataset_d, f.data_a, None, Uuid::new_v4())])
        .await;

    let only_a_ids = [f.data_a];
    for scope in [
        RunScope::whole_run(f.run_1, f.dataset_d),
        RunScope::for_data(f.run_1, f.dataset_d, &only_a_ids),
    ] {
        assert!(
            get_nodes_for_run(&f.db, &scope)
                .await
                .expect("nodes")
                .is_empty(),
            "a NULL run id must match no run"
        );
        assert!(
            get_edges_for_run(&f.db, &scope)
                .await
                .expect("edges")
                .is_empty(),
            "a NULL run id must match no run"
        );
    }
}

#[tokio::test]
async fn run_selection_is_scoped_to_the_dataset() {
    let f = Fixture::new().await;
    let here = f.node(f.dataset_d, f.data_a, Some(f.run_1), Uuid::new_v4());
    let elsewhere = f.node(f.dataset_e, f.data_a, Some(f.run_1), Uuid::new_v4());
    f.seed_nodes(&[here.clone(), elsewhere]).await;

    let here_edge = f.edge(f.dataset_d, f.data_a, Some(f.run_1), Uuid::new_v4());
    let elsewhere_edge = f.edge(f.dataset_e, f.data_a, Some(f.run_1), Uuid::new_v4());
    f.seed_edges(&[here_edge.clone(), elsewhere_edge]).await;

    let scope = RunScope::whole_run(f.run_1, f.dataset_d);
    let nodes = get_nodes_for_run(&f.db, &scope).await.expect("nodes");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].id, here.id);

    let edges = get_edges_for_run(&f.db, &scope).await.expect("edges");
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].id, here_edge.id);
}

#[tokio::test]
async fn run_selection_narrowed_to_data_ids() {
    let f = Fixture::new().await;
    let for_a = f.node(f.dataset_d, f.data_a, Some(f.run_1), Uuid::new_v4());
    let for_b = f.node(f.dataset_d, f.data_b, Some(f.run_1), Uuid::new_v4());
    f.seed_nodes(&[for_a.clone(), for_b]).await;

    let edge_a = f.edge(f.dataset_d, f.data_a, Some(f.run_1), Uuid::new_v4());
    let edge_b = f.edge(f.dataset_d, f.data_b, Some(f.run_1), Uuid::new_v4());
    f.seed_edges(&[edge_a.clone(), edge_b]).await;

    let only_a_ids = [f.data_a];
    let scope = RunScope::for_data(f.run_1, f.dataset_d, &only_a_ids);
    let nodes = get_nodes_for_run(&f.db, &scope).await.expect("nodes");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].id, for_a.id);

    let edges = get_edges_for_run(&f.db, &scope).await.expect("edges");
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].id, edge_a.id);
}

/// `Some(&[])` narrows to no data items at all — distinct from `None`, which
/// selects the whole run.
#[tokio::test]
async fn empty_data_id_narrowing_selects_nothing() {
    let f = Fixture::new().await;
    f.seed_nodes(&[f.node(f.dataset_d, f.data_a, Some(f.run_1), Uuid::new_v4())])
        .await;
    f.seed_edges(&[f.edge(f.dataset_d, f.data_a, Some(f.run_1), Uuid::new_v4())])
        .await;

    let no_ids: [Uuid; 0] = [];
    let scope = RunScope::for_data(f.run_1, f.dataset_d, &no_ids);
    assert!(
        get_nodes_for_run(&f.db, &scope)
            .await
            .expect("n")
            .is_empty()
    );
    assert!(
        get_edges_for_run(&f.db, &scope)
            .await
            .expect("e")
            .is_empty()
    );
    assert!(
        get_unique_nodes_for_run(&f.db, &scope)
            .await
            .expect("un")
            .is_empty()
    );
    assert!(
        get_unique_edges_for_run(&f.db, &scope)
            .await
            .expect("ue")
            .is_empty()
    );
    assert_eq!(delete_nodes_for_run(&f.db, &scope).await.expect("dn"), 0);
    assert_eq!(delete_edges_for_run(&f.db, &scope).await.expect("de"), 0);

    // …and nothing was actually removed.
    let whole = RunScope::whole_run(f.run_1, f.dataset_d);
    assert_eq!(get_nodes_for_run(&f.db, &whole).await.expect("n").len(), 1);
    assert_eq!(get_edges_for_run(&f.db, &whole).await.expect("e").len(), 1);
}

// ---------------------------------------------------------------------------
// Exclusivity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn exclusive_rows_are_returned_when_nothing_else_claims_the_slug() {
    let f = Fixture::new().await;
    let slug = Uuid::new_v4();
    let node = f.node(f.dataset_d, f.data_a, Some(f.run_1), slug);
    f.seed_nodes(std::slice::from_ref(&node)).await;
    let edge = f.edge(f.dataset_d, f.data_a, Some(f.run_1), slug);
    f.seed_edges(std::slice::from_ref(&edge)).await;

    let scope = RunScope::whole_run(f.run_1, f.dataset_d);
    let nodes = get_unique_nodes_for_run(&f.db, &scope).await.expect("n");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].id, node.id);

    let edges = get_unique_edges_for_run(&f.db, &scope).await.expect("e");
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].id, edge.id);
}

/// A slug also claimed by a pre-ownership row is NOT exclusive. This is the
/// test that fails if the `pipeline_run_id IS NULL` disjunct is "simplified"
/// away: `NULL <> :run` evaluates to NULL, not TRUE, so without it the sweep
/// would delete an artifact that predates every run.
/// The second exclusivity axis. An `EdgeType` vector point is keyed on the
/// edge's retrieval text, not its slug, so a sweep also has to ask which
/// relation names rows *outside* the scope still claim. Every kind of outsider
/// counts — another run, a legacy NULL-run row, another dataset — and, once the
/// scope is narrowed, a surviving file of the sweep's own run.
#[tokio::test]
async fn claimed_relationship_names_cover_every_kind_of_outsider() {
    let f = Fixture::new().await;
    let named = |dataset: Uuid, data: Uuid, run: Option<Uuid>, name: &str| GraphEdge {
        relationship_name: name.to_string(),
        ..f.edge(dataset, data, run, Uuid::new_v4())
    };
    f.seed_edges(&[
        named(f.dataset_d, f.data_a, Some(f.run_1), "mine"),
        named(f.dataset_d, f.data_b, Some(f.run_1), "sibling_file"),
        named(f.dataset_d, f.data_b, Some(f.run_2), "another_run"),
        named(f.dataset_d, f.data_b, None, "legacy"),
        named(f.dataset_e, f.data_a, Some(f.run_1), "another_dataset"),
    ])
    .await;

    let mut whole_run = get_relationship_names_claimed_outside_run(
        &f.db,
        &RunScope::whole_run(f.run_1, f.dataset_d),
    )
    .await
    .expect("names");
    whole_run.sort();
    assert_eq!(
        whole_run,
        ["another_dataset", "another_run", "legacy"],
        "the run's own rows — both files' — are inside the scope",
    );

    let data_ids = [f.data_a];
    let mut narrowed = get_relationship_names_claimed_outside_run(
        &f.db,
        &RunScope::for_data(f.run_1, f.dataset_d, &data_ids),
    )
    .await
    .expect("names");
    narrowed.sort();
    assert_eq!(
        narrowed,
        ["another_dataset", "another_run", "legacy", "sibling_file"],
        "a surviving file of the same run claims its names too",
    );
}

#[tokio::test]
async fn exclusivity_is_defeated_by_a_null_run_row() {
    let f = Fixture::new().await;
    let slug = Uuid::new_v4();
    f.seed_nodes(&[
        f.node(f.dataset_d, f.data_a, Some(f.run_1), slug),
        f.node(f.dataset_d, f.data_b, None, slug),
    ])
    .await;
    f.seed_edges(&[
        f.edge(f.dataset_d, f.data_a, Some(f.run_1), slug),
        f.edge(f.dataset_d, f.data_b, None, slug),
    ])
    .await;

    let scope = RunScope::whole_run(f.run_1, f.dataset_d);
    assert!(
        get_unique_nodes_for_run(&f.db, &scope)
            .await
            .expect("n")
            .is_empty(),
        "a legacy NULL-run row claiming the slug must protect it"
    );
    assert!(
        get_unique_edges_for_run(&f.db, &scope)
            .await
            .expect("e")
            .is_empty(),
        "a legacy NULL-run row claiming the slug must protect it"
    );
}

#[tokio::test]
async fn exclusivity_is_defeated_by_another_run() {
    let f = Fixture::new().await;
    let slug = Uuid::new_v4();
    f.seed_nodes(&[
        f.node(f.dataset_d, f.data_a, Some(f.run_1), slug),
        f.node(f.dataset_d, f.data_b, Some(f.run_2), slug),
    ])
    .await;
    f.seed_edges(&[
        f.edge(f.dataset_d, f.data_a, Some(f.run_1), slug),
        f.edge(f.dataset_d, f.data_b, Some(f.run_2), slug),
    ])
    .await;

    let scope = RunScope::whole_run(f.run_1, f.dataset_d);
    assert!(
        get_unique_nodes_for_run(&f.db, &scope)
            .await
            .expect("n")
            .is_empty()
    );
    assert!(
        get_unique_edges_for_run(&f.db, &scope)
            .await
            .expect("e")
            .is_empty()
    );
}

/// Rows in other datasets count as outside the selection, so a slug claimed
/// there protects the artifact.
///
/// This is deliberate and is the opposite of `get_unique_*_for_data`: OSS Rust
/// has a single graph store shared by every dataset, and `slug` is the
/// content-addressed graph node id, so two datasets mentioning the same entity
/// share one physical node. Deleting it while another dataset's ownership row
/// still names it would corrupt that dataset.
#[tokio::test]
async fn exclusivity_is_defeated_by_another_dataset() {
    let f = Fixture::new().await;
    let slug = Uuid::new_v4();
    f.seed_nodes(&[
        f.node(f.dataset_d, f.data_a, Some(f.run_1), slug),
        f.node(f.dataset_e, f.data_a, Some(f.run_1), slug),
    ])
    .await;
    f.seed_edges(&[
        f.edge(f.dataset_d, f.data_a, Some(f.run_1), slug),
        f.edge(f.dataset_e, f.data_a, Some(f.run_1), slug),
    ])
    .await;

    let scope = RunScope::whole_run(f.run_1, f.dataset_d);
    assert!(
        get_unique_nodes_for_run(&f.db, &scope)
            .await
            .expect("n")
            .is_empty(),
        "the same slug in another dataset must protect the node"
    );
    assert!(
        get_unique_edges_for_run(&f.db, &scope)
            .await
            .expect("e")
            .is_empty(),
        "the same slug in another dataset must protect the edge"
    );
}

/// The clause that stops an item-scoped sweep from deleting an artifact a
/// surviving item in the same run also produced.
#[tokio::test]
async fn exclusivity_is_defeated_by_a_surviving_data_id_in_the_same_run() {
    let f = Fixture::new().await;
    let shared = Uuid::new_v4();
    let only_a = Uuid::new_v4();
    f.seed_nodes(&[
        f.node(f.dataset_d, f.data_a, Some(f.run_1), shared),
        f.node(f.dataset_d, f.data_a, Some(f.run_1), only_a),
        f.node(f.dataset_d, f.data_b, Some(f.run_1), shared),
    ])
    .await;
    f.seed_edges(&[
        f.edge(f.dataset_d, f.data_a, Some(f.run_1), shared),
        f.edge(f.dataset_d, f.data_a, Some(f.run_1), only_a),
        f.edge(f.dataset_d, f.data_b, Some(f.run_1), shared),
    ])
    .await;

    let only_a_ids = [f.data_a];
    let scope = RunScope::for_data(f.run_1, f.dataset_d, &only_a_ids);
    let nodes = get_unique_nodes_for_run(&f.db, &scope).await.expect("n");
    assert_eq!(nodes.len(), 1, "only the slug data_b does not also claim");
    assert_eq!(nodes[0].slug, only_a);

    let edges = get_unique_edges_for_run(&f.db, &scope).await.expect("e");
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].slug, only_a);

    // Widen the scope to both items and the shared slug is exclusive again.
    let whole = RunScope::whole_run(f.run_1, f.dataset_d);
    assert_eq!(
        get_unique_nodes_for_run(&f.db, &whole)
            .await
            .expect("n")
            .len(),
        3,
    );
    assert_eq!(
        get_unique_edges_for_run(&f.db, &whole)
            .await
            .expect("e")
            .len(),
        3,
    );
}

// ---------------------------------------------------------------------------
// Deletion
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_for_run_removes_only_the_selected_rows() {
    let f = Fixture::new().await;
    let mine = f.node(f.dataset_d, f.data_a, Some(f.run_1), Uuid::new_v4());
    let other_run = f.node(f.dataset_d, f.data_a, Some(f.run_2), Uuid::new_v4());
    let legacy = f.node(f.dataset_d, f.data_a, None, Uuid::new_v4());
    let other_dataset = f.node(f.dataset_e, f.data_a, Some(f.run_1), Uuid::new_v4());
    f.seed_nodes(&[
        mine,
        other_run.clone(),
        legacy.clone(),
        other_dataset.clone(),
    ])
    .await;

    let my_edge = f.edge(f.dataset_d, f.data_a, Some(f.run_1), Uuid::new_v4());
    let other_run_edge = f.edge(f.dataset_d, f.data_a, Some(f.run_2), Uuid::new_v4());
    let legacy_edge = f.edge(f.dataset_d, f.data_a, None, Uuid::new_v4());
    let other_dataset_edge = f.edge(f.dataset_e, f.data_a, Some(f.run_1), Uuid::new_v4());
    f.seed_edges(&[
        my_edge,
        other_run_edge.clone(),
        legacy_edge.clone(),
        other_dataset_edge.clone(),
    ])
    .await;

    let scope = RunScope::whole_run(f.run_1, f.dataset_d);
    assert_eq!(delete_nodes_for_run(&f.db, &scope).await.expect("dn"), 1);
    assert_eq!(delete_edges_for_run(&f.db, &scope).await.expect("de"), 1);

    assert!(
        get_nodes_for_run(&f.db, &scope)
            .await
            .expect("n")
            .is_empty(),
        "the run owns nothing after its sweep"
    );

    // Everything outside the selection survives.
    let survivors_run_2 = RunScope::whole_run(f.run_2, f.dataset_d);
    assert_eq!(
        get_nodes_for_run(&f.db, &survivors_run_2)
            .await
            .expect("n")
            .len(),
        1
    );
    assert_eq!(
        get_edges_for_run(&f.db, &survivors_run_2)
            .await
            .expect("e")
            .len(),
        1
    );

    let survivors_dataset_e = RunScope::whole_run(f.run_1, f.dataset_e);
    assert_eq!(
        get_nodes_for_run(&f.db, &survivors_dataset_e)
            .await
            .expect("n")
            .len(),
        1
    );
    assert_eq!(
        get_edges_for_run(&f.db, &survivors_dataset_e)
            .await
            .expect("e")
            .len(),
        1
    );

    // The legacy rows are unreachable through any run scope, so read them back
    // directly to prove they were not swept.
    assert!(
        node_exists(&f.db, legacy.id).await,
        "a NULL-run node must survive every sweep"
    );
    assert!(
        edge_exists(&f.db, legacy_edge.id).await,
        "a NULL-run edge must survive every sweep"
    );
}

#[tokio::test]
async fn delete_for_run_narrowed_to_data_ids() {
    let f = Fixture::new().await;
    f.seed_nodes(&[
        f.node(f.dataset_d, f.data_a, Some(f.run_1), Uuid::new_v4()),
        f.node(f.dataset_d, f.data_b, Some(f.run_1), Uuid::new_v4()),
    ])
    .await;
    f.seed_edges(&[
        f.edge(f.dataset_d, f.data_a, Some(f.run_1), Uuid::new_v4()),
        f.edge(f.dataset_d, f.data_b, Some(f.run_1), Uuid::new_v4()),
    ])
    .await;

    let only_a_ids = [f.data_a];
    let scope = RunScope::for_data(f.run_1, f.dataset_d, &only_a_ids);
    assert_eq!(delete_nodes_for_run(&f.db, &scope).await.expect("dn"), 1);
    assert_eq!(delete_edges_for_run(&f.db, &scope).await.expect("de"), 1);

    let remaining = RunScope::whole_run(f.run_1, f.dataset_d);
    let nodes = get_nodes_for_run(&f.db, &remaining).await.expect("n");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].data_id, f.data_b);
    let edges = get_edges_for_run(&f.db, &remaining).await.expect("e");
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].data_id, f.data_b);
}

// ---------------------------------------------------------------------------
// Upsert ownership
// ---------------------------------------------------------------------------

/// The row id is keyed by logical identity, not by run, so a re-upsert records
/// the run that *created* the artifact. Letting a later run steal the tag would
/// make its rollback delete something an earlier, successful run created.
#[tokio::test]
async fn re_upsert_preserves_the_first_runs_ownership() {
    let f = Fixture::new().await;

    let node = f.node(f.dataset_d, f.data_a, Some(f.run_1), Uuid::new_v4());
    f.seed_nodes(std::slice::from_ref(&node)).await;
    let mut node_again = node.clone();
    node_again.pipeline_run_id = Some(f.run_2);
    node_again.label = Some("relabelled".into());
    f.seed_nodes(&[node_again]).await;

    let edge = f.edge(f.dataset_d, f.data_a, Some(f.run_1), Uuid::new_v4());
    f.seed_edges(std::slice::from_ref(&edge)).await;
    let mut edge_again = edge.clone();
    edge_again.pipeline_run_id = Some(f.run_2);
    edge_again.relationship_name = "relabelled".into();
    f.seed_edges(&[edge_again]).await;

    let scope = RunScope::whole_run(f.run_1, f.dataset_d);
    let nodes = get_nodes_for_run(&f.db, &scope).await.expect("n");
    assert_eq!(nodes.len(), 1, "run 1 must still own the node");
    assert_eq!(nodes[0].label.as_deref(), Some("relabelled"));
    assert_eq!(nodes[0].pipeline_run_id, Some(f.run_1));

    let edges = get_edges_for_run(&f.db, &scope).await.expect("e");
    assert_eq!(edges.len(), 1, "run 1 must still own the edge");
    assert_eq!(edges[0].relationship_name, "relabelled");
    assert_eq!(edges[0].pipeline_run_id, Some(f.run_1));

    // …and run 2 owns nothing.
    let scope_2 = RunScope::whole_run(f.run_2, f.dataset_d);
    assert!(
        get_nodes_for_run(&f.db, &scope_2)
            .await
            .expect("n")
            .is_empty()
    );
    assert!(
        get_edges_for_run(&f.db, &scope_2)
            .await
            .expect("e")
            .is_empty()
    );
}

async fn node_exists(db: &DatabaseConnection, id: Uuid) -> bool {
    use sea_orm::EntityTrait;
    cognee_database::entities::node::Entity::find_by_id(cognee_database::uuid_hex::to_hex(id))
        .one(db)
        .await
        .expect("find node")
        .is_some()
}

async fn edge_exists(db: &DatabaseConnection, id: Uuid) -> bool {
    use sea_orm::EntityTrait;
    cognee_database::entities::edge::Entity::find_by_id(cognee_database::uuid_hex::to_hex(id))
        .one(db)
        .await
        .expect("find edge")
        .is_some()
}

// ---------------------------------------------------------------------------
// Affected data items — the set whose completion markers a sweep must clear
// ---------------------------------------------------------------------------

/// Exclusivity must not leak into this query. An item whose every artifact is
/// shared still had its work rolled back, so it still has to be unmarked.
#[tokio::test]
async fn data_ids_for_run_reports_every_touched_item_not_just_the_exclusive_ones() {
    let f = Fixture::new().await;
    let shared = Uuid::new_v4();
    f.seed_nodes(&[
        // data_a's only slug is claimed by another run, so it is not exclusive.
        f.node(f.dataset_d, f.data_a, Some(f.run_1), shared),
        f.node(f.dataset_d, f.data_a, Some(f.run_2), shared),
        f.node(f.dataset_d, f.data_b, Some(f.run_1), Uuid::new_v4()),
    ])
    .await;

    let scope = RunScope::whole_run(f.run_1, f.dataset_d);
    assert_eq!(
        get_unique_nodes_for_run(&f.db, &scope)
            .await
            .expect("n")
            .len(),
        1,
        "only data_b's slug is exclusive",
    );

    let mut ids = get_data_ids_for_run(&f.db, &scope).await.expect("ids");
    ids.sort();
    let mut expected = vec![f.data_a, f.data_b];
    expected.sort();
    assert_eq!(ids, expected, "both items were touched by run 1");
}

/// An item that owns only an *edge* row is still one the run touched.
#[tokio::test]
async fn data_ids_for_run_unions_nodes_and_edges() {
    let f = Fixture::new().await;
    f.seed_nodes(&[f.node(f.dataset_d, f.data_a, Some(f.run_1), Uuid::new_v4())])
        .await;
    f.seed_edges(&[f.edge(f.dataset_d, f.data_b, Some(f.run_1), Uuid::new_v4())])
        .await;

    let mut ids = get_data_ids_for_run(&f.db, &RunScope::whole_run(f.run_1, f.dataset_d))
        .await
        .expect("ids");
    ids.sort();
    let mut expected = vec![f.data_a, f.data_b];
    expected.sort();
    assert_eq!(ids, expected);
}

/// The selection predicate is the same one the deletes use: another run,
/// another dataset and a pre-ownership NULL-run row contribute nothing.
#[tokio::test]
async fn data_ids_for_run_excludes_other_runs_datasets_and_legacy_rows() {
    let f = Fixture::new().await;
    f.seed_nodes(&[
        f.node(f.dataset_d, f.data_a, Some(f.run_1), Uuid::new_v4()),
        f.node(f.dataset_d, f.data_b, Some(f.run_2), Uuid::new_v4()),
        f.node(f.dataset_e, f.data_b, Some(f.run_1), Uuid::new_v4()),
        f.node(f.dataset_d, f.data_b, None, Uuid::new_v4()),
    ])
    .await;
    f.seed_edges(&[
        f.edge(f.dataset_d, f.data_b, Some(f.run_2), Uuid::new_v4()),
        f.edge(f.dataset_d, f.data_b, None, Uuid::new_v4()),
    ])
    .await;

    let ids = get_data_ids_for_run(&f.db, &RunScope::whole_run(f.run_1, f.dataset_d))
        .await
        .expect("ids");
    assert_eq!(ids, vec![f.data_a]);
}

/// Narrowing reaches this query too, and `Some(&[])` still means "nothing".
#[tokio::test]
async fn data_ids_for_run_honours_the_narrowing_and_the_empty_set() {
    let f = Fixture::new().await;
    f.seed_nodes(&[
        f.node(f.dataset_d, f.data_a, Some(f.run_1), Uuid::new_v4()),
        f.node(f.dataset_d, f.data_b, Some(f.run_1), Uuid::new_v4()),
    ])
    .await;

    let only_a = [f.data_a];
    assert_eq!(
        get_data_ids_for_run(&f.db, &RunScope::for_data(f.run_1, f.dataset_d, &only_a))
            .await
            .expect("ids"),
        vec![f.data_a],
    );

    assert!(
        get_data_ids_for_run(&f.db, &RunScope::for_data(f.run_1, f.dataset_d, &[]))
            .await
            .expect("ids")
            .is_empty(),
        "an empty narrowing selects nothing, unlike `None`",
    );
}
