#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! A run's ownership marker is closed only once its terminal `pipeline_runs`
//! row is written. Python sets `ownership.closed` after
//! `log_pipeline_run_complete` / `_error` returned (`run_tasks.py:342,403`).
//! A failed terminal write leaves the run `STARTED` in the database, and
//! startup recovery can only retire it while the marker exists.
//!
//! A failed `COMPLETED` write is itself a failed run, as in Python
//! (`run_tasks.py`: `log_pipeline_run_complete` sits inside the `try`; its
//! failure goes to the `except` branch, which rolls back, writes `ERRORED` and
//! yields `PipelineRunErrored`): `cognify` returns an error, rolls the run
//! back and writes `ERRORED`; only when that write fails too does the run stay
//! `STARTED` with its marker.
//!
//! Offline: mock stores, in-memory SQLite.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use cognee_cognify::{CognifyConfig, cognify};
use cognee_database::ops::datasets::create_dataset;
use cognee_database::pipelines::repository::PipelineRunRow;
use cognee_database::{
    DatabaseConnection, DatabaseError as DbError, PipelineRun, PipelineRunRepository,
    PipelineRunStatus, SeaOrmPipelineRunRepository, connect, initialize,
};
use cognee_embedding::MockEmbeddingEngine;
use cognee_models::{Data, Dataset};
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{MockStorage, StorageTrait};
use cognee_test_utils::{MockGraphDB, MockLlm, MockVectorDB};
use cognee_vector::VectorDB;
use serde_json::json;
use uuid::Uuid;

/// Which terminal row writes the repository rejects.
#[derive(Clone, Copy, PartialEq)]
enum Fail {
    Nothing,
    /// Only `COMPLETED`: the run's `ERRORED` row can still be written.
    Completed,
    /// Every non-`STARTED` row: the database stays at `STARTED`.
    Terminal,
}

/// Delegates to a real repository, failing the writes `fail` names.
struct FailingTerminalRepo {
    inner: Arc<dyn PipelineRunRepository>,
    fail: Fail,
}

#[async_trait]
impl PipelineRunRepository for FailingTerminalRepo {
    async fn log_pipeline_run(
        &self,
        pipeline_run_id: Uuid,
        pipeline_id: Uuid,
        pipeline_name: &str,
        dataset_id: Option<Uuid>,
        status: PipelineRunStatus,
        run_info: Option<serde_json::Value>,
    ) -> Result<Uuid, DbError> {
        let rejected = match self.fail {
            Fail::Nothing => false,
            Fail::Completed => status == PipelineRunStatus::Completed,
            Fail::Terminal => status != PipelineRunStatus::Started,
        };
        if rejected {
            return Err(DbError::QueryError("synthetic terminal failure".into()));
        }
        self.inner
            .log_pipeline_run(
                pipeline_run_id,
                pipeline_id,
                pipeline_name,
                dataset_id,
                status,
                run_info,
            )
            .await
    }

    async fn latest_status(
        &self,
        dataset_ids: &[Uuid],
        pipeline_name: &str,
    ) -> Result<HashMap<Uuid, PipelineRunStatus>, DbError> {
        self.inner.latest_status(dataset_ids, pipeline_name).await
    }

    async fn list_recent(
        &self,
        dataset_id: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<PipelineRunRow>, DbError> {
        self.inner.list_recent(dataset_id, limit).await
    }

    async fn set_payload_field(
        &self,
        run_id: Uuid,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), DbError> {
        self.inner.set_payload_field(run_id, key, value).await
    }

    async fn get_payload(
        &self,
        run_id: Uuid,
    ) -> Result<serde_json::Map<String, serde_json::Value>, DbError> {
        self.inner.get_payload(run_id).await
    }

    async fn get_pipeline_run(&self, id: Uuid) -> Result<Option<PipelineRun>, DbError> {
        self.inner.get_pipeline_run(id).await
    }

    async fn get_pipeline_run_by_dataset(
        &self,
        dataset_id: Uuid,
        pipeline_name: &str,
    ) -> Result<Option<PipelineRun>, DbError> {
        self.inner
            .get_pipeline_run_by_dataset(dataset_id, pipeline_name)
            .await
    }

    async fn get_pipeline_runs_by_dataset(
        &self,
        dataset_id: Uuid,
    ) -> Result<Vec<PipelineRun>, DbError> {
        self.inner.get_pipeline_runs_by_dataset(dataset_id).await
    }
}

/// What a run left behind.
struct Outcome {
    result: Result<cognee_cognify::CognifyResult, cognee_cognify::CognifyError>,
    /// The statuses of the dataset's persisted rows, oldest first.
    statuses: Vec<PipelineRunStatus>,
    /// Graph nodes still in the store after the run.
    graph_nodes: usize,
}

/// Run one cognify with `ownership_dir` as the marker root, the repository
/// failing the writes `fail` names.
async fn run_cognify(ownership_dir: &Path, fail: Fail) -> Outcome {
    let owner_id = Uuid::new_v4();
    let dataset_id = Uuid::new_v4();
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(
        MockLlm::new(vec![
            json!({
                "nodes": [{"id": "a", "name": "A", "type": "PERSON", "description": "d"}],
                "edges": []
            })
            .to_string(),
        ])
        .with_summary_response(json!({"summary": "s", "description": "d"}).to_string()),
    );
    let storage: Arc<dyn StorageTrait> = Arc::new(MockStorage::new());
    let graph_db: Arc<dyn cognee_graph::GraphDBTrait> = Arc::new(MockGraphDB::new());
    let graph_probe = Arc::clone(&graph_db);
    let vector_db: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());
    let embedding: Arc<dyn cognee_embedding::EmbeddingEngine> =
        Arc::new(MockEmbeddingEngine::new(8));

    let data_id = Uuid::new_v4();
    let location = storage
        .store(b"A is a person.", &format!("terminal-{data_id}"))
        .await
        .expect("store");
    let item = Data::builder(
        data_id,
        "terminal.txt",
        location,
        "terminal.txt",
        "txt",
        "text/plain",
        "terminal-hash",
        owner_id,
    )
    .build();

    let db: Arc<DatabaseConnection> = {
        let conn = connect("sqlite::memory:").await.expect("connect");
        initialize(&conn).await.expect("initialize");
        create_dataset(
            &conn,
            Dataset::new("terminal".into(), owner_id, None, dataset_id),
        )
        .await
        .expect("seed dataset");
        Arc::new(conn)
    };
    let real_repo: Arc<dyn PipelineRunRepository> =
        Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&db)));
    let repo: Arc<dyn PipelineRunRepository> = Arc::new(FailingTerminalRepo {
        inner: Arc::clone(&real_repo),
        fail,
    });
    let thread_pool: Arc<dyn cognee_core::CpuPool> =
        Arc::new(cognee_core::RayonThreadPool::with_default_threads().expect("pool"));

    let result = cognify(
        vec![item],
        dataset_id,
        Some(owner_id),
        None,
        None,
        llm,
        storage,
        graph_db,
        vector_db,
        embedding,
        db,
        repo,
        thread_pool,
        Arc::new(NoOpOntologyResolver::new()),
        &CognifyConfig::default().with_run_ownership_dir(ownership_dir),
    )
    .await;
    let mut rows = real_repo
        .list_recent(Some(dataset_id), 50)
        .await
        .expect("list pipeline runs");
    rows.reverse();
    let (nodes, _edges) = graph_probe.get_graph_data().await.expect("read the graph");
    Outcome {
        result,
        statuses: rows.into_iter().map(|r| r.status).collect(),
        graph_nodes: nodes.len(),
    }
}

fn marker_count(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, Iterator::count)
}

/// Control: with the terminal row written, the marker is removed.
#[tokio::test]
async fn the_marker_is_removed_after_the_terminal_row_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_cognify(dir.path(), Fail::Nothing).await;
    out.result.as_ref().expect("cognify succeeds");
    assert_eq!(
        out.statuses,
        vec![PipelineRunStatus::Started, PipelineRunStatus::Completed]
    );
    assert!(out.graph_nodes > 0, "a completed run keeps its graph");
    assert_eq!(
        marker_count(dir.path()),
        0,
        "a finished run leaves no marker"
    );
}

/// Python: a failed `COMPLETED` write is a run failure. The run errors, rolls
/// back its writes and records `ERRORED`; that row is terminal, so the marker
/// goes.
#[tokio::test]
async fn a_failed_completed_write_fails_the_run_and_writes_errored() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_cognify(dir.path(), Fail::Completed).await;
    let err = out
        .result
        .expect_err("a run whose COMPLETED row was not written has failed");
    assert!(
        err.to_string().contains("COMPLETED record"),
        "the error names the failed record write: {err}"
    );
    assert_eq!(
        out.statuses,
        vec![PipelineRunStatus::Started, PipelineRunStatus::Errored],
        "STARTED, then ERRORED in place of the COMPLETED that could not be written"
    );
    assert_eq!(out.graph_nodes, 0, "the failed run was rolled back");
    assert_eq!(
        marker_count(dir.path()),
        0,
        "the ERRORED row is the run's terminal state, so the marker is closed"
    );
}

/// When the `ERRORED` write fails too the run stays `STARTED` (Python's
/// `log_pipeline_run_error` raises, `ownership.closed` is never set): the
/// error is still returned, and the marker stays so startup recovery can
/// retire the run.
#[tokio::test]
async fn the_marker_stays_when_no_terminal_row_could_be_written() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_cognify(dir.path(), Fail::Terminal).await;
    out.result.expect_err("the run failed");
    assert_eq!(out.statuses, vec![PipelineRunStatus::Started]);
    assert_eq!(
        marker_count(dir.path()),
        1,
        "the marker of a run whose terminal row was not written must survive"
    );
}
