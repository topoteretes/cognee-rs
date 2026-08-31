#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Tests for [`RunSweeper`] — rolling back one pipeline run's contribution to
//! one dataset.
//!
//! The properties under test are the ones the two invariants rest on: only
//! exclusively-owned artifacts are deleted, *every* selected ownership row goes
//! whether or not its artifact did, markers are cleared for every touched item,
//! and the artifacts-before-rows ordering survives a failure so a re-run
//! converges.
//!
//! Real in-memory SQLite for the ledger, mock graph/vector stores, no LLM.

use std::sync::Arc;

use cognee_database::ops::graph_storage::{
    RunScope, get_edges_for_run, get_nodes_for_run, upsert_edges, upsert_nodes,
};
use cognee_database::{DatabaseConnection, GraphEdge, GraphNode, connect, initialize, ops};
use cognee_delete::{DeleteError, RunSweeper, SweepScope};
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_models::{Data, Dataset, EdgeType, Triplet};
use cognee_vector::{MockVectorDB, VectorDB, VectorPoint};
use serde_json::json;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Fixture {
    sweeper: RunSweeper,
    db: Arc<DatabaseConnection>,
    graph: Arc<MockGraphDB>,
    vector: Arc<MockVectorDB>,
    owner: Uuid,
}

impl Fixture {
    async fn new() -> Self {
        let db = connect("sqlite::memory:").await.expect("connect");
        initialize(&db).await.expect("migrate");
        let db = Arc::new(db);
        let graph = Arc::new(MockGraphDB::new());
        let vector = Arc::new(MockVectorDB::new());
        let sweeper = RunSweeper::new(
            db.clone(),
            graph.clone() as Arc<dyn GraphDBTrait>,
            vector.clone() as Arc<dyn VectorDB>,
        );
        Self {
            sweeper,
            db,
            graph,
            vector,
            owner: Uuid::new_v4(),
        }
    }

    async fn dataset(&self, name: &str) -> Uuid {
        let dataset = Dataset::new(name.to_string(), self.owner, None, Uuid::new_v4());
        let id = dataset.id;
        ops::datasets::create_dataset(&self.db, dataset)
            .await
            .expect("dataset");
        id
    }

    async fn data(&self, dataset_id: Uuid, name: &str) -> Uuid {
        let data_id = Uuid::new_v4();
        let data = Data::builder(
            data_id,
            name,
            format!("/tmp/{name}"),
            format!("file://{name}"),
            "txt",
            "text/plain",
            "hash_placeholder",
            self.owner,
        )
        .build();
        ops::data::create_data(&self.db, data).await.expect("data");
        ops::datasets::attach_data_to_dataset(&self.db, dataset_id, data_id)
            .await
            .expect("attach");
        data_id
    }

    fn node(&self, dataset: Uuid, data: Uuid, run: Option<Uuid>, slug: Uuid) -> GraphNode {
        GraphNode {
            id: Uuid::new_v4(),
            slug,
            user_id: self.owner,
            data_id: data,
            dataset_id: dataset,
            pipeline_run_id: run,
            label: Some(format!("node-{slug}")),
            node_type: "Entity".into(),
            // The plain-array encoding `parse_indexed_fields` expects.
            indexed_fields: json!(["name"]),
            attributes: None,
            created_at: chrono::Utc::now(),
        }
    }

    fn edge(&self, dataset: Uuid, data: Uuid, run: Option<Uuid>, slug: Uuid) -> GraphEdge {
        GraphEdge {
            id: Uuid::new_v4(),
            slug,
            user_id: self.owner,
            data_id: data,
            dataset_id: dataset,
            pipeline_run_id: run,
            source_node_id: Uuid::new_v4(),
            destination_node_id: Uuid::new_v4(),
            relationship_name: "is_a".into(),
            label: None,
            attributes: None,
            created_at: chrono::Utc::now(),
        }
    }

    async fn seed_nodes(&self, nodes: &[GraphNode]) {
        upsert_nodes(&self.db, nodes).await.expect("upsert nodes");
    }

    async fn seed_edges(&self, edges: &[GraphEdge]) {
        upsert_edges(&self.db, edges).await.expect("upsert edges");
    }

    /// Put the graph node a provenance row names into the graph store, and its
    /// vector point into the `Entity_name` collection, so a sweep has something
    /// real to delete.
    async fn seed_artifacts(&self, slugs: &[Uuid]) {
        self.vector
            .create_collection("Entity", "name", 3)
            .await
            .expect("collection");
        for slug in slugs {
            self.graph
                .add_node_raw(json!({ "id": slug.to_string(), "name": "n" }))
                .await
                .expect("graph node");
            self.vector
                .index_points(
                    "Entity",
                    "name",
                    &[VectorPoint::new(*slug, vec![1.0, 0.0, 0.0])],
                )
                .await
                .expect("index");
        }
    }

    async fn ensure_collection(&self, data_type: &str, field: &str) {
        if !self
            .vector
            .has_collection(data_type, field)
            .await
            .expect("has_collection")
        {
            self.vector
                .create_collection(data_type, field, 3)
                .await
                .expect("collection");
        }
    }

    /// Index the `EdgeType` and `Triplet` points cognify would have written for
    /// `edge`. Both are content-addressed, so seeding two edges that share a
    /// retrieval text leaves one `EdgeType` point — which is the point.
    async fn seed_edge_artifacts(&self, edge: &GraphEdge) {
        let (edge_type_id, triplet_id) = edge_vector_ids(edge);
        for (data_type, field, id) in [
            ("EdgeType", "relationship_name", edge_type_id),
            ("Triplet", "text", triplet_id),
        ] {
            self.ensure_collection(data_type, field).await;
            self.vector
                .index_points(
                    data_type,
                    field,
                    &[VectorPoint::new(id, vec![1.0, 0.0, 0.0])],
                )
                .await
                .expect("index");
        }
    }

    async fn mark_complete(&self, data_id: Uuid, dataset_id: Uuid) {
        let key = ops::data::pipeline_status_dataset_key(dataset_id);
        let status = json!({ "cognify_pipeline": { key: "DATA_ITEM_PROCESSING_COMPLETED" } });
        let data = ops::data::get_data(&self.db, data_id)
            .await
            .expect("get")
            .expect("data exists");
        ops::data::update_data(
            &self.db,
            Data {
                pipeline_status: Some(status.to_string()),
                ..data
            },
        )
        .await
        .expect("mark");
    }

    async fn is_marked(&self, data_id: Uuid, dataset_id: Uuid) -> bool {
        let key = ops::data::pipeline_status_dataset_key(dataset_id);
        let Some(data) = ops::data::get_data(&self.db, data_id).await.expect("get") else {
            return false;
        };
        let Some(raw) = data.pipeline_status else {
            return false;
        };
        serde_json::from_str::<serde_json::Value>(&raw)
            .expect("status json")
            .get("cognify_pipeline")
            .and_then(|v| v.get(&key))
            .is_some()
    }

    async fn prov_counts(&self, run: Uuid, dataset: Uuid) -> (usize, usize) {
        let scope = RunScope::whole_run(run, dataset);
        (
            get_nodes_for_run(&self.db, &scope).await.expect("n").len(),
            get_edges_for_run(&self.db, &scope).await.expect("e").len(),
        )
    }

    async fn graph_has(&self, slug: Uuid) -> bool {
        self.graph.has_node(&slug.to_string()).await.expect("has")
    }

    async fn entity_points(&self) -> usize {
        self.vector
            .collection_size("Entity", "name")
            .await
            .expect("size")
    }
}

/// The `EdgeType` / `Triplet` vector ids cognify writes for an edge, recomputed
/// the way the delete path recomputes them.
fn edge_vector_ids(edge: &GraphEdge) -> (Uuid, Uuid) {
    let edge_text = edge
        .attributes
        .as_ref()
        .and_then(|a| a.get("edge_text"))
        .and_then(serde_json::Value::as_str);
    let text = EdgeType::retrieval_text(edge_text, &edge.relationship_name);
    let triplet = Triplet::new(
        edge.source_node_id,
        edge.destination_node_id,
        edge.relationship_name.clone(),
        String::new(),
    );
    (EdgeType::deterministic_id(&text), triplet.id)
}

// ---------------------------------------------------------------------------
// Whole-run sweeps
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_whole_run_sweep_removes_its_nodes_vectors_rows_and_markers() {
    let f = Fixture::new().await;
    let dataset = f.dataset("ds").await;
    let data = f.data(dataset, "a.txt").await;
    let run = Uuid::new_v4();

    let (s1, s2) = (Uuid::new_v4(), Uuid::new_v4());
    f.seed_nodes(&[
        f.node(dataset, data, Some(run), s1),
        f.node(dataset, data, Some(run), s2),
    ])
    .await;
    f.seed_artifacts(&[s1, s2]).await;
    f.mark_complete(data, dataset).await;

    let outcome = f
        .sweeper
        .sweep(&SweepScope::whole_run(run, dataset))
        .await
        .expect("sweep");

    assert_eq!(outcome.graph_nodes_deleted, 2);
    assert_eq!(outcome.vector_points_deleted, 2);
    assert_eq!(outcome.provenance_nodes_deleted, 2);
    assert_eq!(outcome.data_items_unmarked, 1);
    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);

    assert_eq!(f.graph.node_count(), 0);
    assert_eq!(f.entity_points().await, 0);
    assert_eq!(f.prov_counts(run, dataset).await, (0, 0));
    assert!(!f.is_marked(data, dataset).await, "marker must be cleared");
}

/// A slug another run still claims keeps its artifact — but this run's
/// ownership row goes regardless. The surplus artifact still has an owner, so
/// the no-orphan invariant holds.
#[tokio::test]
async fn a_slug_another_run_still_claims_survives_but_its_own_row_goes() {
    let f = Fixture::new().await;
    let dataset = f.dataset("ds").await;
    let data_a = f.data(dataset, "a.txt").await;
    let data_b = f.data(dataset, "b.txt").await;
    let (run_a, run_b) = (Uuid::new_v4(), Uuid::new_v4());

    let shared = Uuid::new_v4();
    f.seed_nodes(&[
        f.node(dataset, data_a, Some(run_a), shared),
        f.node(dataset, data_b, Some(run_b), shared),
    ])
    .await;
    f.seed_artifacts(&[shared]).await;

    let outcome = f
        .sweeper
        .sweep(&SweepScope::whole_run(run_a, dataset))
        .await
        .expect("sweep");

    assert_eq!(outcome.graph_nodes_deleted, 0);
    assert_eq!(outcome.provenance_nodes_deleted, 1);
    assert!(f.graph_has(shared).await, "run B still claims the slug");
    assert_eq!(f.entity_points().await, 1);
    assert_eq!(f.prov_counts(run_a, dataset).await, (0, 0));
    assert_eq!(f.prov_counts(run_b, dataset).await, (1, 0));
}

/// A row predating run ownership carries a NULL run: it protects its artifact
/// and no sweep may ever select it.
#[tokio::test]
async fn a_legacy_null_run_row_protects_its_artifact_and_survives_the_sweep() {
    let f = Fixture::new().await;
    let dataset = f.dataset("ds").await;
    let data = f.data(dataset, "a.txt").await;
    let run = Uuid::new_v4();

    let slug = Uuid::new_v4();
    let legacy = f.node(dataset, data, None, slug);
    f.seed_nodes(&[f.node(dataset, data, Some(run), slug), legacy.clone()])
        .await;
    f.seed_artifacts(&[slug]).await;

    f.sweeper
        .sweep(&SweepScope::whole_run(run, dataset))
        .await
        .expect("sweep");

    assert!(f.graph_has(slug).await, "the legacy row protects the node");
    assert_eq!(f.prov_counts(run, dataset).await, (0, 0));
    let all = ops::graph_storage::get_nodes_by_dataset(&f.db, dataset)
        .await
        .expect("rows");
    assert_eq!(all.len(), 1, "the legacy row itself must survive");
    assert_eq!(all[0].id, legacy.id);
}

/// The same run id used against two datasets: a sweep touches only its own.
#[tokio::test]
async fn a_sweep_is_scoped_to_its_dataset() {
    let f = Fixture::new().await;
    let ds_a = f.dataset("a").await;
    let ds_b = f.dataset("b").await;
    let data_a = f.data(ds_a, "a.txt").await;
    let data_b = f.data(ds_b, "b.txt").await;
    let run = Uuid::new_v4();

    let (slug_a, slug_b) = (Uuid::new_v4(), Uuid::new_v4());
    f.seed_nodes(&[
        f.node(ds_a, data_a, Some(run), slug_a),
        f.node(ds_b, data_b, Some(run), slug_b),
    ])
    .await;
    f.seed_artifacts(&[slug_a, slug_b]).await;
    f.mark_complete(data_b, ds_b).await;

    f.sweeper
        .sweep(&SweepScope::whole_run(run, ds_a))
        .await
        .expect("sweep");

    assert!(!f.graph_has(slug_a).await);
    assert!(f.graph_has(slug_b).await, "the other dataset is untouched");
    assert_eq!(f.prov_counts(run, ds_b).await, (1, 0));
    assert!(f.is_marked(data_b, ds_b).await);
}

/// Graph edges are never deleted directly — they cascade with their endpoints,
/// matching Python and the dataset delete path. Their *vectors* do go.
#[tokio::test]
async fn edges_lose_their_vectors_but_are_never_deleted_from_the_graph_directly() {
    let f = Fixture::new().await;
    let dataset = f.dataset("ds").await;
    let data = f.data(dataset, "a.txt").await;
    let run = Uuid::new_v4();

    let mut edge = f.edge(dataset, data, Some(run), Uuid::new_v4());
    edge.attributes = Some(json!({ "edge_text": "Alice knows Bob" }));
    f.seed_edges(&[edge.clone()]).await;

    let (edge_type_id, triplet_id) = edge_vector_ids(&edge);
    for (data_type, field, id) in [
        ("EdgeType", "relationship_name", edge_type_id),
        ("Triplet", "text", triplet_id),
    ] {
        f.vector
            .create_collection(data_type, field, 3)
            .await
            .expect("collection");
        f.vector
            .index_points(
                data_type,
                field,
                &[VectorPoint::new(id, vec![1.0, 0.0, 0.0])],
            )
            .await
            .expect("index");
    }
    f.graph
        .add_edge(
            &edge.source_node_id.to_string(),
            &edge.destination_node_id.to_string(),
            &edge.relationship_name,
            None,
        )
        .await
        .expect("graph edge");

    let outcome = f
        .sweeper
        .sweep(&SweepScope::whole_run(run, dataset))
        .await
        .expect("sweep");

    assert_eq!(outcome.provenance_edges_deleted, 1);
    assert_eq!(outcome.vector_points_deleted, 2);
    assert_eq!(
        f.vector
            .collection_size("EdgeType", "relationship_name")
            .await
            .unwrap(),
        0,
    );
    assert_eq!(
        f.vector.collection_size("Triplet", "text").await.unwrap(),
        0
    );
    assert_eq!(f.graph.edge_count(), 1, "graph edges are left to cascade");
    assert!(
        !f.graph.get_call_log().contains(&"delete_nodes".to_string()),
        "an edge-only sweep must not issue a node delete",
    );
}

/// The other half of that claim: cascading is how a swept node's edges *do*
/// leave the graph. A backend deletes a node with `DETACH DELETE`
/// (Ladybug) or `ON DELETE CASCADE` (Postgres), so nothing incident to a
/// deleted node can outlive it — while an edge between two survivors is
/// nobody's collateral.
#[tokio::test]
async fn a_swept_node_takes_its_incident_graph_edges_with_it() {
    let f = Fixture::new().await;
    let dataset = f.dataset("ds").await;
    let data = f.data(dataset, "a.txt").await;
    let (run, other_run) = (Uuid::new_v4(), Uuid::new_v4());

    // `mine` is this run's alone and goes; `shared` is claimed by another run
    // and stays.
    let (mine, shared) = (Uuid::new_v4(), Uuid::new_v4());
    f.seed_nodes(&[
        f.node(dataset, data, Some(run), mine),
        f.node(dataset, data, Some(run), shared),
        f.node(dataset, data, Some(other_run), shared),
    ])
    .await;
    f.seed_artifacts(&[mine, shared]).await;

    // A third node no provenance row names — an artifact of some earlier run,
    // there to hold an edge that must not be swept.
    let bystander = Uuid::new_v4();
    f.graph
        .add_node_raw(json!({ "id": bystander.to_string(), "name": "n" }))
        .await
        .expect("graph node");
    f.graph
        .add_edge(&mine.to_string(), &shared.to_string(), "is_a", None)
        .await
        .expect("edge");
    f.graph
        .add_edge(&shared.to_string(), &bystander.to_string(), "is_a", None)
        .await
        .expect("edge");

    let outcome = f
        .sweeper
        .sweep(&SweepScope::whole_run(run, dataset))
        .await
        .expect("sweep");

    assert_eq!(outcome.graph_nodes_deleted, 1);
    assert!(!f.graph_has(mine).await);
    assert!(f.graph_has(shared).await, "the other run still claims it");
    assert!(
        !f.graph
            .has_edge(&mine.to_string(), &shared.to_string(), "is_a")
            .await
            .expect("has_edge"),
        "an edge incident to a swept node cannot outlive it",
    );
    assert!(
        f.graph
            .has_edge(&shared.to_string(), &bystander.to_string(), "is_a")
            .await
            .expect("has_edge"),
        "an edge between two survivors is not collateral",
    );
    assert_eq!(f.graph.edge_count(), 1);
}

// ---------------------------------------------------------------------------
// Item-scoped sweeps
// ---------------------------------------------------------------------------

/// §2.4's load-bearing clause: a surviving file of the *same run* is outside
/// the selection, so what it also produced is kept.
#[tokio::test]
async fn an_item_scoped_sweep_keeps_what_a_surviving_file_also_produced() {
    let f = Fixture::new().await;
    let dataset = f.dataset("ds").await;
    let failed = f.data(dataset, "failed.txt").await;
    let survivor = f.data(dataset, "ok.txt").await;
    let run = Uuid::new_v4();

    let shared = Uuid::new_v4();
    let only_failed = Uuid::new_v4();
    f.seed_nodes(&[
        f.node(dataset, failed, Some(run), shared),
        f.node(dataset, failed, Some(run), only_failed),
        f.node(dataset, survivor, Some(run), shared),
    ])
    .await;
    f.seed_artifacts(&[shared, only_failed]).await;
    f.mark_complete(failed, dataset).await;
    f.mark_complete(survivor, dataset).await;

    let outcome = f
        .sweeper
        .sweep(&SweepScope::for_data(run, dataset, vec![failed]))
        .await
        .expect("sweep");

    assert_eq!(outcome.graph_nodes_deleted, 1);
    assert_eq!(outcome.provenance_nodes_deleted, 2);
    assert_eq!(outcome.data_items_unmarked, 1);

    assert!(
        !f.graph_has(only_failed).await,
        "the failed file's own node"
    );
    assert!(
        f.graph_has(shared).await,
        "a node the surviving file also produced must stay",
    );
    assert_eq!(f.entity_points().await, 1);
    assert!(!f.is_marked(failed, dataset).await);
    assert!(f.is_marked(survivor, dataset).await, "the survivor is kept");
}

/// `Some(vec![])` means "sweep nothing", and must not be confused with `None`.
#[tokio::test]
async fn an_empty_narrowing_sweeps_nothing() {
    let f = Fixture::new().await;
    let dataset = f.dataset("ds").await;
    let data = f.data(dataset, "a.txt").await;
    let run = Uuid::new_v4();

    let slug = Uuid::new_v4();
    f.seed_nodes(&[f.node(dataset, data, Some(run), slug)])
        .await;
    f.seed_artifacts(&[slug]).await;
    f.mark_complete(data, dataset).await;

    let outcome = f
        .sweeper
        .sweep(&SweepScope::for_data(run, dataset, vec![]))
        .await
        .expect("sweep");

    assert_eq!(outcome, Default::default());
    assert!(f.graph_has(slug).await);
    assert_eq!(f.entity_points().await, 1);
    assert_eq!(f.prov_counts(run, dataset).await, (1, 0));
    assert!(f.is_marked(data, dataset).await);
}

/// An `EdgeType_relationship_name` point is keyed on the edge's *retrieval
/// text*, which is many-to-one over edges: a failed file's `is_a` edge and a
/// surviving file's `is_a` edge resolve to one point. Slug exclusivity says the
/// failed edge is exclusively owned, and it is — but the point must stay.
#[tokio::test]
async fn an_edge_type_point_a_surviving_file_still_needs_is_kept() {
    let f = Fixture::new().await;
    let dataset = f.dataset("ds").await;
    let failed = f.data(dataset, "failed.txt").await;
    let survivor = f.data(dataset, "ok.txt").await;
    let run = Uuid::new_v4();

    // Different endpoints, so different slugs: each edge is exclusively owned
    // by its own file. Both are `is_a`, so they share one EdgeType point.
    let failed_edge = f.edge(dataset, failed, Some(run), Uuid::new_v4());
    let survivor_edge = f.edge(dataset, survivor, Some(run), Uuid::new_v4());
    f.seed_edges(&[failed_edge.clone(), survivor_edge.clone()])
        .await;
    f.seed_edge_artifacts(&failed_edge).await;
    f.seed_edge_artifacts(&survivor_edge).await;
    assert_eq!(
        edge_vector_ids(&failed_edge).0,
        edge_vector_ids(&survivor_edge).0,
        "the fixture only tests anything if the two edges share an EdgeType id",
    );

    let outcome = f
        .sweeper
        .sweep(&SweepScope::for_data(run, dataset, vec![failed]))
        .await
        .expect("sweep");

    assert_eq!(outcome.provenance_edges_deleted, 1);
    assert_eq!(outcome.vector_points_deleted, 1, "the Triplet point only");
    assert_eq!(
        f.vector
            .collection_size("EdgeType", "relationship_name")
            .await
            .unwrap(),
        1,
        "the surviving file's `is_a` edge still needs the EdgeType point",
    );
    assert_eq!(
        f.vector.collection_size("Triplet", "text").await.unwrap(),
        1,
        "only the failed edge's own Triplet point goes",
    );
}

/// The same protection across runs: a whole-run sweep must not strip an
/// EdgeType point an *earlier* run's surviving edges still need.
#[tokio::test]
async fn a_whole_run_sweep_keeps_an_edge_type_point_an_earlier_run_still_needs() {
    let f = Fixture::new().await;
    let dataset = f.dataset("ds").await;
    let mine = f.data(dataset, "mine.txt").await;
    let earlier = f.data(dataset, "earlier.txt").await;
    let (run, earlier_run) = (Uuid::new_v4(), Uuid::new_v4());

    let my_edge = f.edge(dataset, mine, Some(run), Uuid::new_v4());
    let earlier_edge = f.edge(dataset, earlier, Some(earlier_run), Uuid::new_v4());
    f.seed_edges(&[my_edge.clone(), earlier_edge.clone()]).await;
    f.seed_edge_artifacts(&my_edge).await;
    f.seed_edge_artifacts(&earlier_edge).await;

    f.sweeper
        .sweep(&SweepScope::whole_run(run, dataset))
        .await
        .expect("sweep");

    assert_eq!(
        f.vector
            .collection_size("EdgeType", "relationship_name")
            .await
            .unwrap(),
        1,
        "the earlier run's edges still need it",
    );
    assert_eq!(
        f.vector.collection_size("Triplet", "text").await.unwrap(),
        1,
        "but this run's own Triplet point goes",
    );
}

/// The edges' *own* identity, the one the `EdgeType` check says nothing about:
/// a `Triplet_text` point is keyed on the edge itself, so it may only go when
/// no row outside the scope claims that edge's slug. Two runs that both
/// produced the same edge leave two ownership rows over one slug; sweeping one
/// of them takes its row and nothing else.
#[tokio::test]
async fn an_edge_slug_another_run_still_claims_keeps_its_triplet_point() {
    let f = Fixture::new().await;
    let dataset = f.dataset("ds").await;
    let data_a = f.data(dataset, "a.txt").await;
    let data_b = f.data(dataset, "b.txt").await;
    let (run_a, run_b) = (Uuid::new_v4(), Uuid::new_v4());

    // One edge, two ownership rows: same slug and same endpoints — and so one
    // physical graph edge and one Triplet point — written by two runs.
    let mine = f.edge(dataset, data_a, Some(run_a), Uuid::new_v4());
    let theirs = GraphEdge {
        id: Uuid::new_v4(),
        data_id: data_b,
        pipeline_run_id: Some(run_b),
        ..mine.clone()
    };
    f.seed_edges(&[mine.clone(), theirs.clone()]).await;
    f.seed_edge_artifacts(&mine).await;
    assert_eq!(
        edge_vector_ids(&mine),
        edge_vector_ids(&theirs),
        "the fixture only tests anything if the two rows name one edge",
    );

    let outcome = f
        .sweeper
        .sweep(&SweepScope::whole_run(run_a, dataset))
        .await
        .expect("sweep");

    assert_eq!(
        f.vector.collection_size("Triplet", "text").await.unwrap(),
        1,
        "run B still claims the edge, so its Triplet point must stay",
    );
    assert_eq!(
        outcome.vector_points_deleted, 0,
        "no artifact was deletable"
    );
    assert_eq!(outcome.provenance_edges_deleted, 1, "only run A's row goes");
    assert_eq!(f.prov_counts(run_a, dataset).await, (0, 0));
    assert_eq!(f.prov_counts(run_b, dataset).await, (0, 1), "run B's row");
}

/// The same exclusivity across datasets. One graph store is shared by every
/// dataset, so an edge two datasets both produced is one physical edge with one
/// Triplet point; sweeping the run out of one dataset must not strip the point
/// the other still needs.
#[tokio::test]
async fn an_edge_slug_another_dataset_still_claims_keeps_its_triplet_point() {
    let f = Fixture::new().await;
    let ds_a = f.dataset("a").await;
    let ds_b = f.dataset("b").await;
    let data_a = f.data(ds_a, "a.txt").await;
    let data_b = f.data(ds_b, "b.txt").await;
    let run = Uuid::new_v4();

    let mine = f.edge(ds_a, data_a, Some(run), Uuid::new_v4());
    let theirs = GraphEdge {
        id: Uuid::new_v4(),
        data_id: data_b,
        dataset_id: ds_b,
        ..mine.clone()
    };
    f.seed_edges(&[mine.clone(), theirs.clone()]).await;
    f.seed_edge_artifacts(&mine).await;

    let outcome = f
        .sweeper
        .sweep(&SweepScope::whole_run(run, ds_a))
        .await
        .expect("sweep");

    assert_eq!(
        f.vector.collection_size("Triplet", "text").await.unwrap(),
        1,
        "dataset B still claims the edge, so its Triplet point must stay",
    );
    assert_eq!(
        outcome.vector_points_deleted, 0,
        "no artifact was deletable"
    );
    assert_eq!(outcome.provenance_edges_deleted, 1, "only dataset A's row");
    assert_eq!(f.prov_counts(run, ds_a).await, (0, 0));
    assert_eq!(f.prov_counts(run, ds_b).await, (0, 1), "dataset B's row");
}

// ---------------------------------------------------------------------------
// Markers
// ---------------------------------------------------------------------------

/// The affected items come from the *selection*, not from the exclusive subset:
/// an item whose every artifact is shared deletes nothing from the stores, yet
/// its work was still rolled back and its marker must go.
#[tokio::test]
async fn markers_clear_for_every_touched_item_even_when_no_artifact_is_deletable() {
    let f = Fixture::new().await;
    let dataset = f.dataset("ds").await;
    let mine = f.data(dataset, "a.txt").await;
    let theirs = f.data(dataset, "b.txt").await;
    let (run, other_run) = (Uuid::new_v4(), Uuid::new_v4());

    let shared = Uuid::new_v4();
    f.seed_nodes(&[
        f.node(dataset, mine, Some(run), shared),
        f.node(dataset, theirs, Some(other_run), shared),
    ])
    .await;
    f.seed_artifacts(&[shared]).await;
    f.mark_complete(mine, dataset).await;

    let outcome = f
        .sweeper
        .sweep(&SweepScope::whole_run(run, dataset))
        .await
        .expect("sweep");

    assert_eq!(outcome.graph_nodes_deleted, 0);
    assert_eq!(outcome.data_items_unmarked, 1);
    assert!(f.graph_has(shared).await);
    assert!(!f.is_marked(mine, dataset).await);
}

/// "Keep everything earlier runs completed": an item this run never touched
/// keeps its marker.
#[tokio::test]
async fn a_data_item_completed_by_an_earlier_run_keeps_its_marker() {
    let f = Fixture::new().await;
    let dataset = f.dataset("ds").await;
    let earlier = f.data(dataset, "earlier.txt").await;
    let mine = f.data(dataset, "mine.txt").await;
    let (run_a, run_b) = (Uuid::new_v4(), Uuid::new_v4());

    f.seed_nodes(&[
        f.node(dataset, earlier, Some(run_b), Uuid::new_v4()),
        f.node(dataset, mine, Some(run_a), Uuid::new_v4()),
    ])
    .await;
    f.mark_complete(earlier, dataset).await;
    f.mark_complete(mine, dataset).await;

    f.sweeper
        .sweep(&SweepScope::whole_run(run_a, dataset))
        .await
        .expect("sweep");

    assert!(f.is_marked(earlier, dataset).await, "run B's work stands");
    assert!(!f.is_marked(mine, dataset).await);
}

/// The same rule seen from the item-scoped side: the affected set comes from
/// the ledger, never from the caller's narrowing list. A file that failed
/// before it persisted anything owns no rows, so any marker it carries was
/// written by an earlier run that *did* complete it — and that is kept.
#[tokio::test]
async fn a_scoped_data_item_with_no_ownership_rows_keeps_its_earlier_marker() {
    let f = Fixture::new().await;
    let dataset = f.dataset("ds").await;
    let failed_early = f.data(dataset, "failed-early.txt").await;
    let run = Uuid::new_v4();

    // The run persisted nothing for this item; an earlier run had completed it.
    f.mark_complete(failed_early, dataset).await;

    let outcome = f
        .sweeper
        .sweep(&SweepScope::for_data(run, dataset, vec![failed_early]))
        .await
        .expect("sweep");

    assert_eq!(outcome, Default::default());
    assert!(
        f.is_marked(failed_early, dataset).await,
        "an earlier run completed it; the sweep rolled back nothing of its own",
    );
}

// ---------------------------------------------------------------------------
// Ordering, failure handling, idempotency
// ---------------------------------------------------------------------------

/// The reason artifacts are deleted before ownership rows: when artifact
/// deletion fails, the rows survive as the record of what still needs sweeping,
/// and re-running converges.
#[tokio::test]
async fn ownership_rows_and_markers_survive_when_artifact_deletion_fails() {
    let f = Fixture::new().await;
    let dataset = f.dataset("ds").await;
    let data = f.data(dataset, "a.txt").await;
    let run = Uuid::new_v4();

    let slug = Uuid::new_v4();
    f.seed_nodes(&[f.node(dataset, data, Some(run), slug)])
        .await;
    f.seed_artifacts(&[slug]).await;
    f.mark_complete(data, dataset).await;

    f.graph.set_delete_nodes_error("boom");
    let err = f
        .sweeper
        .sweep(&SweepScope::whole_run(run, dataset))
        .await
        .expect_err("graph deletion failed");
    assert!(matches!(err, DeleteError::GraphCleanup(_)), "{err:?}");

    assert_eq!(f.prov_counts(run, dataset).await, (1, 0), "rows survive");
    assert!(f.is_marked(data, dataset).await, "marker survives");
    assert_eq!(f.entity_points().await, 1, "vectors were never reached");

    // Re-running converges once the store recovers.
    f.graph.clear_delete_nodes_error();
    let outcome = f
        .sweeper
        .sweep(&SweepScope::whole_run(run, dataset))
        .await
        .expect("second sweep");
    assert_eq!(outcome.graph_nodes_deleted, 1);
    assert_eq!(f.prov_counts(run, dataset).await, (0, 0));
    assert_eq!(f.entity_points().await, 0);
    assert!(!f.is_marked(data, dataset).await);
}

/// The sweep runs on an error path: its own failure is reported, never raised
/// in place of the pipeline error that triggered it.
#[tokio::test]
async fn a_sweep_failure_is_reported_not_propagated() {
    let f = Fixture::new().await;
    let dataset = f.dataset("ds").await;
    let data = f.data(dataset, "a.txt").await;
    let run = Uuid::new_v4();

    let slug = Uuid::new_v4();
    f.seed_nodes(&[f.node(dataset, data, Some(run), slug)])
        .await;
    f.seed_artifacts(&[slug]).await;

    f.graph.set_delete_nodes_error("boom");
    let outcome = f
        .sweeper
        .sweep_logging_failure(&SweepScope::whole_run(run, dataset))
        .await;

    assert_eq!(outcome.provenance_nodes_deleted, 0);
    assert_eq!(outcome.graph_nodes_deleted, 0);
    assert_eq!(outcome.warnings.len(), 1, "{:?}", outcome.warnings);
    assert!(
        outcome.warnings[0].contains("Run sweep failed") && outcome.warnings[0].contains("boom"),
        "{:?}",
        outcome.warnings,
    );
    assert_eq!(f.prov_counts(run, dataset).await, (1, 0));
}

#[tokio::test]
async fn sweeping_twice_is_a_no_op() {
    let f = Fixture::new().await;
    let dataset = f.dataset("ds").await;
    let data = f.data(dataset, "a.txt").await;
    let run = Uuid::new_v4();

    let slug = Uuid::new_v4();
    f.seed_nodes(&[f.node(dataset, data, Some(run), slug)])
        .await;
    f.seed_edges(&[f.edge(dataset, data, Some(run), Uuid::new_v4())])
        .await;
    f.seed_artifacts(&[slug]).await;
    f.mark_complete(data, dataset).await;

    let scope = SweepScope::whole_run(run, dataset);
    f.sweeper.sweep(&scope).await.expect("first sweep");
    let second = f.sweeper.sweep(&scope).await.expect("second sweep");

    assert_eq!(second, Default::default(), "a second sweep does nothing");
}

/// And the item-scoped case, which is only a no-op because the affected set is
/// read from the ledger rather than from the caller's narrowing list.
#[tokio::test]
async fn sweeping_an_item_scope_twice_is_a_no_op() {
    let f = Fixture::new().await;
    let dataset = f.dataset("ds").await;
    let data = f.data(dataset, "a.txt").await;
    let run = Uuid::new_v4();

    let slug = Uuid::new_v4();
    f.seed_nodes(&[f.node(dataset, data, Some(run), slug)])
        .await;
    f.seed_artifacts(&[slug]).await;
    f.mark_complete(data, dataset).await;

    let scope = SweepScope::for_data(run, dataset, vec![data]);
    let first = f.sweeper.sweep(&scope).await.expect("first sweep");
    assert_eq!(first.data_items_unmarked, 1);

    let second = f.sweeper.sweep(&scope).await.expect("second sweep");
    assert_eq!(second, Default::default(), "a second sweep does nothing");
}

/// Marker clearing is the one phase a re-run cannot redo — the ownership rows
/// that name the items are already gone — so one failure must not abandon the
/// rest. Every item is attempted, each failure is reported, and the sweep still
/// fails loudly.
#[tokio::test]
async fn a_failing_marker_clear_does_not_abandon_the_remaining_items() {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    let f = Fixture::new().await;
    let dataset = f.dataset("ds").await;
    let first = f.data(dataset, "a.txt").await;
    let second = f.data(dataset, "b.txt").await;
    let (run, other_run) = (Uuid::new_v4(), Uuid::new_v4());

    f.seed_nodes(&[
        f.node(dataset, first, Some(run), Uuid::new_v4()),
        f.node(dataset, second, Some(run), Uuid::new_v4()),
        f.node(dataset, first, Some(other_run), Uuid::new_v4()),
    ])
    .await;

    // Break every read of `data`, and only that: the sweep's first four phases
    // touch the provenance tables alone, so it gets all the way to phase 5.
    f.db.execute(Statement::from_string(
        DatabaseBackend::Sqlite,
        "ALTER TABLE data RENAME COLUMN pipeline_status TO pipeline_status_gone",
    ))
    .await
    .expect("break the data table");

    let outcome = f
        .sweeper
        .sweep_logging_failure(&SweepScope::whole_run(run, dataset))
        .await;

    assert_eq!(outcome.provenance_nodes_deleted, 2, "phase 4 ran");
    assert_eq!(outcome.data_items_unmarked, 0, "neither clear succeeded");
    assert_eq!(
        outcome
            .warnings
            .iter()
            .filter(|w| w.contains("completion marker for data"))
            .count(),
        2,
        "both items were attempted: {:?}",
        outcome.warnings,
    );
    assert!(
        outcome
            .warnings
            .iter()
            .any(|w| w.contains("Run sweep failed")),
        "the sweep still reports failure: {:?}",
        outcome.warnings,
    );

    // The same failure, raised rather than reported, for callers of `sweep`.
    let err = f
        .sweeper
        .sweep(&SweepScope::whole_run(other_run, dataset))
        .await
        .expect_err("still an error");
    assert!(matches!(err, DeleteError::Runtime(_)), "{err:?}");
}
