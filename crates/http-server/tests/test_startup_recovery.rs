#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! The server's own startup path must recover a run a killed predecessor left
//! in flight — all three things it leaves, not just the two relational ones.
//!
//! `crates/cognify/tests/integration_repeat_cognify.rs` proves the recovery
//! *sequence* is correct by assembling it the way a caller must. This proves
//! the caller actually assembles it: it calls
//! [`AppState::build_with_db_and_backends`], the function
//! `cognee-http-server`'s `main` calls, and asserts through the stores. Before
//! the rollback was wired, startup retired the status row and left the dead
//! run's graph nodes, vector points and ownership rows behind forever —
//! attributed to a run that will never finish, so no later run-scoped sweep
//! would ever select them either.
//!
//! Fully offline: in-memory SQLite, `MockGraphDB`, `MockVectorDB`.

use std::sync::Arc;

use cognee_database::ops::graph_storage::{RunScope, get_nodes_for_run, upsert_nodes};
use cognee_database::{
    DatabaseConnection, GraphNode, PipelineRunRepository, PipelineRunStatus,
    SeaOrmPipelineRunRepository, connect, initialize, ops,
};
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_http_server::config::HttpServerConfig;
use cognee_http_server::state::AppState;
use cognee_models::{Data, Dataset};
use cognee_vector::{MockVectorDB, VectorDB, VectorPoint};
use uuid::Uuid;

/// The `pipeline_runs.pipeline_name` cognify stamps, and therefore what the
/// claim is keyed on.
const COGNIFY_PIPELINE: &str = "cognify_pipeline";

/// A day, matching `cognee_cognify::CLAIM_STALE_AFTER`. Not imported, because
/// `cognee-cognify` is a heavyweight dependency for one constant and the value
/// here only has to be "long enough that nothing ages out mid-test".
const CLAIM_STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// An in-memory SQLite URL. The single-process assertion derives `true` from
/// it with no environment manipulation, because no other process can open such
/// a database — which is exactly the property the recovery is gated on.
const IN_MEMORY_URL: &str = "sqlite::memory:";

struct Fixture {
    db: Arc<DatabaseConnection>,
    graph: Arc<dyn GraphDBTrait>,
    vector: Arc<dyn VectorDB>,
    repo: Arc<dyn PipelineRunRepository>,
    owner: Uuid,
}

impl Fixture {
    async fn new() -> Self {
        let db = connect(IN_MEMORY_URL).await.expect("connect");
        initialize(&db).await.expect("migrate");
        let db = Arc::new(db);

        let graph: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());
        graph.initialize().await.expect("graph initialize");
        let vector: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());
        let repo: Arc<dyn PipelineRunRepository> =
            Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&db)));

        Self {
            db,
            graph,
            vector,
            repo,
            owner: Uuid::new_v4(),
        }
    }

    async fn dataset(&self, name: &str) -> Uuid {
        let dataset = Dataset::new(name.to_string(), self.owner, None, Uuid::new_v4());
        let id = dataset.id;
        ops::datasets::create_dataset(&self.db, dataset)
            .await
            .expect("create dataset");
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
        ops::data::create_data(&self.db, data)
            .await
            .expect("create data");
        ops::datasets::attach_data_to_dataset(&self.db, dataset_id, data_id)
            .await
            .expect("attach");
        data_id
    }

    /// One entity as a cognify run writes it: an ownership-ledger row keyed on
    /// `run`, the graph node it names, and its `Entity_name` vector point.
    ///
    /// The ledger row is load-bearing. A run-scoped rollback selects on
    /// `graph_nodes.pipeline_run_id`, so seeding a graph node alone would make
    /// this test pass against a rollback that does nothing.
    async fn seed_run_artifact(&self, dataset_id: Uuid, data_id: Uuid, run: Uuid) -> Uuid {
        let slug = Uuid::new_v4();
        upsert_nodes(
            &self.db,
            &[GraphNode {
                id: Uuid::new_v4(),
                slug,
                user_id: self.owner,
                data_id,
                dataset_id,
                pipeline_run_id: Some(run),
                label: Some(format!("killed-run-entity-{slug}")),
                node_type: "Entity".into(),
                indexed_fields: serde_json::json!(["name"]),
                attributes: None,
                created_at: chrono::Utc::now(),
            }],
        )
        .await
        .expect("upsert ledger node");

        self.graph
            .add_node_raw(serde_json::json!({ "id": slug.to_string(), "name": "n" }))
            .await
            .expect("graph node");

        if !self
            .vector
            .has_collection("Entity", "name")
            .await
            .expect("has_collection")
        {
            self.vector
                .create_collection("Entity", "name", 3)
                .await
                .expect("create collection");
        }
        self.vector
            .index_points(
                "Entity",
                "name",
                &[VectorPoint::new(slug, vec![1.0, 0.0, 0.0])],
            )
            .await
            .expect("index point");

        slug
    }

    async fn ledger_rows_for_run(&self, run: Uuid, dataset_id: Uuid) -> usize {
        get_nodes_for_run(&self.db, &RunScope::whole_run(run, dataset_id))
            .await
            .expect("get_nodes_for_run")
            .len()
    }

    async fn graph_has(&self, slug: Uuid) -> bool {
        self.graph
            .has_node(&slug.to_string())
            .await
            .expect("has_node")
    }

    fn config(&self) -> HttpServerConfig {
        HttpServerConfig {
            relational_db_url: IN_MEMORY_URL.to_string(),
            ..HttpServerConfig::default()
        }
    }
}

#[tokio::test]
async fn server_startup_rolls_back_a_killed_runs_artifacts_and_clears_both_gates() {
    // The gate the whole recovery hangs on. An explicit `COGNEE_SINGLE_PROCESS=0`
    // in the environment would silently turn this test into an assertion about
    // nothing, so say so rather than passing vacuously.
    assert!(
        cognee_database::single_process_from_env(IN_MEMORY_URL),
        "an in-memory SQLite URL must derive single-process; unset \
         COGNEE_SINGLE_PROCESS to run this test"
    );

    let f = Fixture::new().await;
    let dataset_id = f.dataset("startup_recovery").await;
    let data_id = f.data(dataset_id, "a.txt").await;

    // The killed run: a `Started` row nothing will succeed, a claim nothing
    // can release, and two entities it had already written.
    let dead_run = Uuid::new_v4();
    f.repo
        .log_pipeline_run(
            dead_run,
            Uuid::new_v4(),
            COGNIFY_PIPELINE,
            Some(dataset_id),
            PipelineRunStatus::Started,
            None,
        )
        .await
        .expect("seed Started row");
    assert!(
        f.repo
            .try_claim_pipeline_run(
                dataset_id,
                COGNIFY_PIPELINE,
                Uuid::new_v4(),
                CLAIM_STALE_AFTER
            )
            .await
            .expect("seed claim"),
        "the killed run's claim must be granted first"
    );
    let dead_a = f.seed_run_artifact(dataset_id, data_id, dead_run).await;
    let dead_b = f.seed_run_artifact(dataset_id, data_id, dead_run).await;

    // A second run's artifact in the same dataset, which must survive: the
    // rollback is scoped to one run and is never a blanket delete. Without
    // this the test would pass against one.
    let other_run = Uuid::new_v4();
    let other = f.seed_run_artifact(dataset_id, data_id, other_run).await;

    // Premises, asserted rather than assumed.
    assert_eq!(f.ledger_rows_for_run(dead_run, dataset_id).await, 2);
    assert!(f.graph_has(dead_a).await && f.graph_has(dead_b).await);

    // The server starting up, through the function `main` calls.
    let _state = AppState::build_with_db_and_backends(
        f.config(),
        Arc::clone(&f.db),
        Some(Arc::clone(&f.graph)),
        Some(Arc::clone(&f.vector)),
    )
    .await
    .expect("build_with_db_and_backends");

    assert_eq!(
        f.ledger_rows_for_run(dead_run, dataset_id).await,
        0,
        "startup must roll back the dead run's ownership rows"
    );
    assert!(
        !f.graph_has(dead_a).await && !f.graph_has(dead_b).await,
        "startup must roll back the dead run's graph nodes"
    );
    assert_eq!(
        f.vector
            .collection_size("Entity", "name")
            .await
            .expect("collection_size"),
        1,
        "startup must roll back the dead run's vector points, and only those"
    );

    assert_eq!(
        f.ledger_rows_for_run(other_run, dataset_id).await,
        1,
        "the other run's ownership row must survive"
    );
    assert!(
        f.graph_has(other).await,
        "the other run's graph node must survive"
    );

    // And the two relational gates, which the rollback must not have replaced.
    assert_eq!(
        f.repo
            .get_pipeline_run_by_dataset(dataset_id, COGNIFY_PIPELINE)
            .await
            .expect("latest run")
            .map(|run| run.status),
        Some(PipelineRunStatus::Errored),
        "the orphaned Started row must have been retired"
    );
    assert!(
        f.repo
            .get_pipeline_run_claim(dataset_id, COGNIFY_PIPELINE)
            .await
            .expect("get claim")
            .is_none(),
        "the dead holder's claim must have been released"
    );
}

/// The safety half, at the same seam: a shared database must be left entirely
/// alone, artifacts included.
#[tokio::test]
async fn server_startup_leaves_a_shared_databases_artifacts_alone() {
    let f = Fixture::new().await;
    let dataset_id = f.dataset("startup_recovery_shared").await;
    let data_id = f.data(dataset_id, "a.txt").await;

    let peer_run = Uuid::new_v4();
    f.repo
        .log_pipeline_run(
            peer_run,
            Uuid::new_v4(),
            COGNIFY_PIPELINE,
            Some(dataset_id),
            PipelineRunStatus::Started,
            None,
        )
        .await
        .expect("seed Started row");
    let peer_slug = f.seed_run_artifact(dataset_id, data_id, peer_run).await;

    // A Postgres URL: every replica shares it, so "in flight" says nothing
    // about "dead". The `db` handle stays the in-memory SQLite one — only the
    // URL in the config drives the assertion.
    let config = HttpServerConfig {
        relational_db_url: "postgres://user:pw@shared-host:5432/cognee".to_string(),
        ..HttpServerConfig::default()
    };
    assert!(
        !cognee_database::single_process_from_env(&config.relational_db_url),
        "a shared Postgres must never derive single-process"
    );

    let _state = AppState::build_with_db_and_backends(
        config,
        Arc::clone(&f.db),
        Some(Arc::clone(&f.graph)),
        Some(Arc::clone(&f.vector)),
    )
    .await
    .expect("build_with_db_and_backends");

    assert_eq!(
        f.ledger_rows_for_run(peer_run, dataset_id).await,
        1,
        "a live peer's ownership rows must survive a restart of one replica"
    );
    assert!(
        f.graph_has(peer_slug).await,
        "a live peer's graph node must survive a restart of one replica"
    );
}
