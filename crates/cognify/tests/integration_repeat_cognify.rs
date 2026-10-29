#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! A dataset must be cognifiable more than once, so data can be loaded in
//! waves (`add` → `cognify` → `add` → `cognify`), and concurrent runs on one
//! dataset are serialized by the per-dataset lock — as in Python, where
//! `run_pipeline_per_dataset` takes `get_dataset_lock(dataset.id)` and has had
//! no run-history gate since `dc0b4ad6a`. A run killed mid-flight is recovered
//! at startup from its ownership marker (`modules/cognify/recovery.py`).
//!
//! **These tests must use a real [`SeaOrmPipelineRunRepository`].** The rest of
//! the crate's tests pass `NoopPipelineRunRepository`, which records nothing,
//! so a test of the `pipeline_runs` trail against it would be vacuous.
//!
//! Runs fully offline (mock LLM / embeddings / graph / vector) — a real CI gate.

use std::sync::Arc;

use cognee_cognify::{CognifyConfig, CognifyError, CognifyResult, cognify};
use cognee_core::pipeline_run_registry::{recovery_lock_of, run_info_for_started};
use cognee_core::{DatasetLocks, RunOwnership};
use cognee_database::ops::graph_storage::{RunScope, get_nodes_for_run, upsert_nodes};
use cognee_database::{
    DatabaseConnection, GraphNode, IngestDb, PipelineRunRepository, PipelineRunStatus,
    SeaOrmPipelineRunRepository, connect, initialize, ops,
};
use cognee_embedding::{EmbeddingEngine, MockEmbeddingEngine};
use cognee_graph::{GraphDBTrait, MockGraphDB};
use cognee_ingestion::AddPipeline;
use cognee_llm::{GenerationOptions, GenerationResponse, Llm, Message};
use cognee_models::{Data, DataInput};
use cognee_ontology::{NoOpOntologyResolver, OntologyResolver};
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_test_utils::MockVectorDB;
use cognee_vector::VectorDB;
use tempfile::TempDir;
use uuid::Uuid;

const WAVE_1_TEXT: &str = "\
Alice is a senior software engineer at TechCorp, a technology company. \
She has worked on the cloud platform for three years.";

const WAVE_2_TEXT: &str = "\
Bob is a data scientist at DataCorp, an analytics company. \
He joined the research team last spring.";

/// The `Pipeline.name` the cognify DAG stamps, and therefore the
/// `pipeline_runs.pipeline_name` of its rows. Mirrors
/// `tasks::COGNIFY_PIPELINE_STAMP_NAME` (not re-exported from the crate root).
const COGNIFY_PIPELINE: &str = "cognify_pipeline";

/// LLM stub that extracts one fixed entity pair regardless of input. The
/// content does not matter here — only whether extraction ran at all.
#[derive(Clone)]
struct FixedGraphLlm;

#[async_trait::async_trait]
impl Llm for FixedGraphLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> cognee_llm::LlmResult<GenerationResponse> {
        Ok(GenerationResponse {
            content: String::new(),
            model: self.model().to_string(),
            usage: None,
            finish_reason: Some("stop".to_string()),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<Message>,
        _json_schema: &serde_json::Value,
        _options: Option<GenerationOptions>,
    ) -> cognee_llm::LlmResult<serde_json::Value> {
        Ok(serde_json::json!({
            "nodes": [
                { "id": "alice", "name": "Alice", "type": "Person",
                  "description": "A software engineer." },
                { "id": "techcorp", "name": "TechCorp", "type": "Organization",
                  "description": "A technology company." }
            ],
            "edges": [
                { "source_node_id": "alice", "target_node_id": "techcorp",
                  "relationship_name": "works_at" }
            ]
        }))
    }

    fn model(&self) -> &str {
        "fixed-graph-fixture"
    }
}

/// Everything a cognify run needs, built once per test over one temp dir so the
/// relational DB (and therefore the `pipeline_runs` trail) persists across runs.
struct Harness {
    _temp_dir: TempDir,
    storage: Arc<dyn StorageTrait>,
    database: Arc<DatabaseConnection>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    llm: Arc<dyn Llm>,
    ontology: Arc<dyn OntologyResolver>,
    // The real repository — see the module docs on why a no-op one would make
    // these tests vacuous.
    pipeline_run_repo: Arc<dyn PipelineRunRepository>,
    ingest: AddPipeline,
    owner_id: Uuid,
    /// Where this harness's runs keep their ownership markers.
    ownership_dir: std::path::PathBuf,
}

impl Harness {
    async fn new() -> Self {
        let temp_dir = TempDir::new().expect("temp dir");

        let storage: Arc<dyn StorageTrait> =
            Arc::new(LocalStorage::new(temp_dir.path().join("storage")));
        storage.initialize().await.expect("storage.initialize");

        let db_path = temp_dir.path().join("cognee.db");
        std::fs::File::create(&db_path).expect("create sqlite db file");
        let db_url = format!("sqlite://{}", db_path.display());
        let ownership_dir = temp_dir.path().join("pipeline_run_locks");
        let db = connect(&db_url).await.expect("connect");
        initialize(&db).await.expect("initialize");
        let database: Arc<DatabaseConnection> = Arc::new(db);

        let graph_db: Arc<dyn GraphDBTrait> = Arc::new(MockGraphDB::new());
        graph_db.initialize().await.expect("graph_db.initialize");

        let vector_db: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());
        let embedding_engine: Arc<dyn EmbeddingEngine> = Arc::new(MockEmbeddingEngine::new(8));
        let llm: Arc<dyn Llm> = Arc::new(FixedGraphLlm);
        let ontology: Arc<dyn OntologyResolver> = Arc::new(NoOpOntologyResolver::new());
        let pipeline_run_repo: Arc<dyn PipelineRunRepository> =
            Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&database)));

        let ingest = AddPipeline::new(Arc::clone(&storage), database.clone() as Arc<dyn IngestDb>)
            .with_thread_pool(Arc::new(
                cognee_core::RayonThreadPool::with_default_threads().unwrap(),
            ))
            .with_graph_db(Arc::clone(&graph_db))
            .with_vector_db(Arc::clone(&vector_db))
            .with_database(database.clone());

        Self {
            _temp_dir: temp_dir,
            storage,
            database,
            graph_db,
            vector_db,
            embedding_engine,
            llm,
            ontology,
            pipeline_run_repo,
            ingest,
            owner_id: Uuid::nil(),
            ownership_dir,
        }
    }

    /// Startup recovery, as `CogneeServices::build` runs it: every in-flight
    /// run whose ownership marker can be claimed is rolled back and closed.
    /// `min_age` is the eligibility floor (Python's
    /// `STALE_RUN_RECOVERY_MIN_AGE_SECONDS`).
    async fn recover(&self, min_age: std::time::Duration) -> cognee_delete::RecoveryReport {
        cognee_delete::recover_stale_pipeline_runs(
            self.pipeline_run_repo.as_ref(),
            self.database.clone(),
            Arc::clone(&self.graph_db),
            Arc::clone(&self.vector_db),
            &DatasetLocks::global(),
            &self.ownership_dir,
            min_age,
        )
        .await
    }

    /// Materialise one entity the way a cognify run does: an ownership-ledger
    /// row attributed to `run`, the graph node it names, and its `Entity_name`
    /// vector point. Returns the artifact's slug.
    ///
    /// The ledger row is the part that matters. A run-scoped rollback selects
    /// on `graph_nodes.pipeline_run_id`, so an artifact with no ledger row is
    /// invisible to it — seeding only a graph node would make a rollback test
    /// pass against a rollback that does nothing at all.
    async fn seed_run_artifact(&self, dataset_id: Uuid, data_id: Uuid, run: Uuid) -> Uuid {
        let slug = Uuid::new_v4();
        upsert_nodes(
            &self.database,
            &[GraphNode {
                id: Uuid::new_v4(),
                slug,
                user_id: self.owner_id,
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

        self.graph_db
            .add_node_raw(serde_json::json!({ "id": slug.to_string(), "name": "n" }))
            .await
            .expect("graph node");

        if !self
            .vector_db
            .has_collection("Entity", "name")
            .await
            .expect("has_collection")
        {
            self.vector_db
                .create_collection("Entity", "name", 8)
                .await
                .expect("create collection");
        }
        self.vector_db
            .index_points(
                "Entity",
                "name",
                &[cognee_vector::VectorPoint::new(slug, vec![1.0; 8])],
            )
            .await
            .expect("index point");

        slug
    }

    /// Ownership-ledger rows still attributed to `run` in `dataset_id`.
    async fn ledger_rows_for_run(&self, run: Uuid, dataset_id: Uuid) -> usize {
        get_nodes_for_run(&self.database, &RunScope::whole_run(run, dataset_id))
            .await
            .expect("get_nodes_for_run")
            .len()
    }

    async fn graph_has(&self, slug: Uuid) -> bool {
        self.graph_db
            .has_node(&slug.to_string())
            .await
            .expect("has_node")
    }

    async fn entity_points(&self) -> usize {
        self.vector_db
            .collection_size("Entity", "name")
            .await
            .expect("collection_size")
    }

    async fn add(&self, dataset_name: &str, text: &str) -> Vec<Data> {
        self.ingest
            .add(
                vec![DataInput::Text(text.to_string())],
                dataset_name,
                self.owner_id,
                None,
            )
            .await
            .expect("ingest")
    }

    async fn dataset_id(&self, dataset_name: &str) -> Uuid {
        ops::datasets::get_dataset_by_name_in_tenant(
            &self.database,
            dataset_name,
            self.owner_id,
            None,
        )
        .await
        .expect("get_dataset_by_name_in_tenant")
        .expect("dataset exists")
        .id
    }

    async fn cognify(
        &self,
        dataset_id: Uuid,
        items: Vec<Data>,
        config: &CognifyConfig,
    ) -> CognifyResult {
        cognify(
            items,
            dataset_id,
            Some(self.owner_id),
            None,
            None,
            Arc::clone(&self.llm),
            Arc::clone(&self.storage),
            Arc::clone(&self.graph_db),
            Arc::clone(&self.vector_db),
            Arc::clone(&self.embedding_engine),
            self.database.clone(),
            Arc::clone(&self.pipeline_run_repo),
            Arc::new(cognee_core::RayonThreadPool::with_default_threads().unwrap())
                as Arc<dyn cognee_core::CpuPool>,
            Arc::clone(&self.ontology),
            config,
        )
        .await
        .expect("cognify")
    }

    /// Like [`Self::cognify`] but surfaces the error instead of panicking.
    async fn try_cognify(
        &self,
        dataset_id: Uuid,
        items: Vec<Data>,
        config: &CognifyConfig,
    ) -> Result<CognifyResult, CognifyError> {
        cognify(
            items,
            dataset_id,
            Some(self.owner_id),
            None,
            None,
            Arc::clone(&self.llm),
            Arc::clone(&self.storage),
            Arc::clone(&self.graph_db),
            Arc::clone(&self.vector_db),
            Arc::clone(&self.embedding_engine),
            self.database.clone(),
            Arc::clone(&self.pipeline_run_repo),
            Arc::new(cognee_core::RayonThreadPool::with_default_threads().unwrap())
                as Arc<dyn cognee_core::CpuPool>,
            Arc::clone(&self.ontology),
            config,
        )
        .await
    }

    /// Write a `Started` row for the cognify pipeline carrying `recovery_lock`
    /// the way the executor does, standing in for a run that is in flight or
    /// was killed mid-run. Returns the `pipeline_run_id` it used — the same id
    /// the ownership ledger keys on, so a caller can attribute artifacts to it.
    async fn seed_started_row(&self, dataset_id: Uuid, recovery_lock: Option<Uuid>) -> Uuid {
        let pipeline_run_id = Uuid::new_v4();
        self.pipeline_run_repo
            .log_pipeline_run(
                pipeline_run_id,
                Uuid::new_v4(),
                COGNIFY_PIPELINE,
                Some(dataset_id),
                PipelineRunStatus::Started,
                Some(run_info_for_started(recovery_lock)),
            )
            .await
            .expect("log_pipeline_run");
        pipeline_run_id
    }

    /// Every cognify row of the dataset, oldest first.
    async fn cognify_rows(&self, dataset_id: Uuid) -> Vec<cognee_database::PipelineRun> {
        let mut rows = self
            .pipeline_run_repo
            .list_recent(Some(dataset_id), 100)
            .await
            .expect("list_recent");
        rows.retain(|r| r.pipeline_name == COGNIFY_PIPELINE);
        rows.sort_by_key(|r| r.created_at);
        rows
    }

    /// Latest `pipeline_runs` status for the cognify pipeline on this dataset.
    async fn latest_cognify_status(&self, dataset_id: Uuid) -> Option<PipelineRunStatus> {
        self.pipeline_run_repo
            .get_pipeline_run_by_dataset(dataset_id, COGNIFY_PIPELINE)
            .await
            .expect("get_pipeline_run_by_dataset")
            .map(|run| run.status)
    }
}

impl Harness {
    /// A cheap config — no summaries — that keeps its ownership markers inside
    /// this harness's temp dir, as `Settings::cognify_config` does under the
    /// system root.
    fn config(&self) -> CognifyConfig {
        CognifyConfig::default()
            .with_summarization(false)
            .with_run_ownership_dir(&self.ownership_dir)
    }

    /// Marker directories still on disk.
    fn markers(&self) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(&self.ownership_dir)
            .map(|dir| dir.filter_map(|e| e.ok()).map(|e| e.path()).collect())
            .unwrap_or_default()
    }
}

/// The regression test for this issue: a second wave of data added to an
/// already-cognified dataset must actually be extracted.
#[tokio::test]
async fn second_cognify_wave_is_processed_not_skipped() {
    let h = Harness::new().await;
    let dataset_name = "repeat_cognify_waves";
    let config = h.config();

    // ── Wave 1 ──────────────────────────────────────────────────────────────
    let items_1 = h.add(dataset_name, WAVE_1_TEXT).await;
    let dataset_id = h.dataset_id(dataset_name).await;

    let result_1 = h.cognify(dataset_id, items_1.clone(), &config).await;
    assert!(!result_1.already_completed, "wave 1 must extract, not skip");
    assert!(
        !result_1.entities.is_empty(),
        "wave 1 must produce entities"
    );

    // Guard against a vacuous pass: the trail must really carry a COMPLETED
    // row, i.e. the repository is live.
    assert_eq!(
        h.latest_cognify_status(dataset_id).await,
        Some(PipelineRunStatus::Completed),
        "wave 1 must leave a COMPLETED pipeline_runs row"
    );

    // ── Wave 2: more data into the same dataset ─────────────────────────────
    let items_2 = h.add(dataset_name, WAVE_2_TEXT).await;
    assert_eq!(
        h.dataset_id(dataset_name).await,
        dataset_id,
        "wave 2 must land in the same dataset"
    );

    let all_items: Vec<Data> = items_1.iter().chain(items_2.iter()).cloned().collect();
    assert!(
        all_items.len() > items_1.len(),
        "wave 2 must add a new data item"
    );

    let result_2 = h.cognify(dataset_id, all_items, &config).await;
    assert!(
        !result_2.already_completed,
        "wave 2 must extract: a COMPLETED row from wave 1 must not skip the run"
    );
    assert!(
        !result_2.entities.is_empty(),
        "wave 2 must produce entities"
    );
    assert!(
        !result_2.chunks.is_empty(),
        "wave 2 must produce chunks from the newly added data"
    );
}

/// Python's observable behaviour with no run-history gate (`dc0b4ad6a`): a
/// second cognify of an unchanged dataset is a run of its own — a fresh
/// `STARTED` / `COMPLETED` pair under a new `pipeline_run_id` — that skips
/// every already-completed item (`run_tasks_data_item.py:207-218`), instead of
/// being short-circuited by the dataset's last `COMPLETED` row.
#[tokio::test]
async fn a_second_cognify_on_an_unchanged_dataset_writes_a_fresh_pair_and_skips_every_item() {
    let h = Harness::new().await;
    let dataset_name = "repeat_cognify_unchanged";
    let config = h.config();

    let items = h.add(dataset_name, WAVE_1_TEXT).await;
    let dataset_id = h.dataset_id(dataset_name).await;

    let first = h.cognify(dataset_id, items.clone(), &config).await;
    assert!(!first.already_completed && !first.entities.is_empty());
    let after_first = h.cognify_rows(dataset_id).await;
    let statuses = |rows: &[cognee_database::PipelineRun]| {
        rows.iter().map(|r| r.status.clone()).collect::<Vec<_>>()
    };
    assert_eq!(
        statuses(&after_first),
        vec![PipelineRunStatus::Started, PipelineRunStatus::Completed],
        "the first run writes its pair and no INITIATED row"
    );
    let nodes_after_first = h.graph_db.get_graph_data().await.map(|(n, _)| n.len()).ok();

    let second = h.cognify(dataset_id, items, &config).await;
    assert!(
        second.already_completed,
        "every item was cognified by the first run, so the second skips them all"
    );
    assert!(
        second.entities.is_empty() && second.chunks.is_empty(),
        "a skipped item is never classified, chunked or extracted"
    );
    assert_eq!(
        second.prior_pipeline_run_id,
        Some(after_first[0].pipeline_run_id),
        "the no-op names the run that did the work"
    );

    let after_second = h.cognify_rows(dataset_id).await;
    assert_eq!(
        statuses(&after_second),
        vec![
            PipelineRunStatus::Started,
            PipelineRunStatus::Completed,
            PipelineRunStatus::Started,
            PipelineRunStatus::Completed,
        ],
        "the second run is a fresh STARTED / COMPLETED pair"
    );
    assert_ne!(
        after_second[2].pipeline_run_id, after_second[0].pipeline_run_id,
        "under its own pipeline_run_id"
    );
    assert_eq!(
        second.pipeline_run_id,
        Some(after_second[2].pipeline_run_id)
    );
    assert_eq!(
        h.graph_db.get_graph_data().await.map(|(n, _)| n.len()).ok(),
        nodes_after_first,
        "and the graph is untouched"
    );
}

/// `DATASET_PROCESSING_INITIATED` stays in the stored-row enum for rows an
/// older writer left (Python keeps it too, `PipelineRun.py:13`), and such a row
/// is just history: it gates nothing and is read back as `INITIATED`.
#[tokio::test]
async fn a_stored_initiated_row_neither_blocks_the_run_nor_fails_to_read() {
    let h = Harness::new().await;
    let config = h.config();
    let items = h.add("repeat_cognify_initiated", WAVE_1_TEXT).await;
    let dataset_id = h.dataset_id("repeat_cognify_initiated").await;

    h.pipeline_run_repo
        .log_pipeline_run(
            Uuid::new_v4(),
            Uuid::new_v4(),
            COGNIFY_PIPELINE,
            Some(dataset_id),
            PipelineRunStatus::Initiated,
            None,
        )
        .await
        .expect("an old INITIATED row");
    assert_eq!(
        h.latest_cognify_status(dataset_id).await,
        Some(PipelineRunStatus::Initiated),
        "the stored row round-trips through the conversion layer"
    );

    let result = h.cognify(dataset_id, items, &config).await;
    assert!(
        !result.already_completed && !result.entities.is_empty(),
        "an INITIATED row must not stop the run"
    );
}

/// A stale `STARTED` row — a process killed mid-run, or a run still going in
/// another process — is not a gate any more: Python has none, and the next run
/// proceeds.
#[tokio::test]
async fn an_unrecovered_started_row_does_not_refuse_the_next_run() {
    let h = Harness::new().await;
    let config = h.config();
    let items = h.add("repeat_cognify_stale_row", WAVE_1_TEXT).await;
    let dataset_id = h.dataset_id("repeat_cognify_stale_row").await;

    h.seed_started_row(dataset_id, None).await;
    assert_eq!(
        h.latest_cognify_status(dataset_id).await,
        Some(PipelineRunStatus::Started)
    );

    let result = h.cognify(dataset_id, items, &config).await;
    assert!(
        !result.already_completed && !result.entities.is_empty(),
        "the run proceeds"
    );
}

/// The per-dataset lock serializes: a second run on a dataset whose lock is
/// held **waits** for it and then proceeds, instead of being refused.
///
/// Deterministic: the lock is taken by the test, so "a run is in flight" does
/// not depend on task interleaving.
#[tokio::test]
async fn a_concurrent_second_run_waits_for_the_dataset_lock_instead_of_being_refused() {
    let h = Harness::new().await;
    let config = h.config();
    let items = h.add("repeat_cognify_lock", WAVE_1_TEXT).await;
    let dataset_id = h.dataset_id("repeat_cognify_lock").await;

    let guard = DatasetLocks::global().lock(dataset_id).await;
    let run = h.try_cognify(dataset_id, items, &config);
    tokio::pin!(run);

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(400), &mut run)
            .await
            .is_err(),
        "the run must still be waiting on the lock, not finished or refused"
    );
    assert!(
        h.cognify_rows(dataset_id).await.is_empty(),
        "and it has written nothing"
    );

    drop(guard);
    let result = tokio::time::timeout(std::time::Duration::from_secs(60), &mut run)
        .await
        .expect("the run proceeds once the lock is released")
        .expect("and succeeds");
    assert!(!result.already_completed && !result.entities.is_empty());
}

/// Two runs started together on one dataset both complete, one after the
/// other: their `STARTED` / `COMPLETED` rows never interleave.
#[tokio::test]
async fn two_simultaneous_runs_on_one_dataset_both_complete_in_sequence() {
    let h = Harness::new().await;
    let config = h.config();
    let items = h.add("repeat_cognify_two", WAVE_1_TEXT).await;
    let dataset_id = h.dataset_id("repeat_cognify_two").await;

    let (a, b) = tokio::join!(
        h.try_cognify(dataset_id, items.clone(), &config),
        h.try_cognify(dataset_id, items.clone(), &config),
    );
    a.expect("the first run completes");
    b.expect("the second is not refused");

    let statuses: Vec<_> = h
        .cognify_rows(dataset_id)
        .await
        .into_iter()
        .map(|r| r.status)
        .collect();
    assert_eq!(
        statuses,
        vec![
            PipelineRunStatus::Started,
            PipelineRunStatus::Completed,
            PipelineRunStatus::Started,
            PipelineRunStatus::Completed,
        ],
        "serialized: the second run starts only after the first completed"
    );
}

/// A real run records its ownership marker in the `STARTED` row (Python's
/// `log_pipeline_run_start(recovery_token=…)`) and removes the marker once the
/// run reached a terminal status.
#[tokio::test]
async fn a_run_records_its_recovery_lock_and_removes_the_marker_when_it_finishes() {
    let h = Harness::new().await;
    let config = h.config();
    let items = h.add("repeat_cognify_marker", WAVE_1_TEXT).await;
    let dataset_id = h.dataset_id("repeat_cognify_marker").await;

    h.cognify(dataset_id, items, &config).await;

    let rows = h.cognify_rows(dataset_id).await;
    let started = rows
        .iter()
        .find(|r| r.status == PipelineRunStatus::Started)
        .expect("a STARTED row");
    let token = recovery_lock_of(started.run_info.as_ref())
        .expect("the STARTED row carries {\"recovery_lock\": …}");
    assert_eq!(
        started
            .run_info
            .as_ref()
            .map(|i| i.as_object().map(|o| o.len())),
        Some(Some(1)),
        "and nothing else"
    );
    let completed = rows
        .iter()
        .find(|r| r.status == PipelineRunStatus::Completed)
        .expect("a COMPLETED row");
    assert_eq!(
        completed.run_info.as_ref().map(|i| i.to_string()),
        Some("{}".to_string()),
        "COMPLETED stays an empty object"
    );
    assert!(
        !h.ownership_dir.join(token.to_string()).exists() && h.markers().is_empty(),
        "a finished run leaves no marker behind"
    );
}

// ---------------------------------------------------------------------------
// Startup recovery (Python `modules/cognify/recovery.py`)
// ---------------------------------------------------------------------------

/// A run killed mid-flight leaves a `STARTED` row carrying its marker (whose OS
/// lock died with the process) and the artifacts it had already written.
/// Recovery rolls those back and closes the run with the abandoned-run error;
/// the next cognify proceeds.
///
/// Non-vacuity: the artifacts are asserted present first and the ledger row is
/// seeded explicitly, because a rollback selects on
/// `graph_nodes.pipeline_run_id` — a do-nothing rollback must fail here.
#[tokio::test]
async fn a_stale_started_row_left_by_a_killed_process_is_recovered_and_the_next_cognify_proceeds() {
    let h = Harness::new().await;
    let dataset_name = "repeat_cognify_recovered";
    let config = h.config();

    let items = h.add(dataset_name, WAVE_1_TEXT).await;
    let dataset_id = h.dataset_id(dataset_name).await;
    let data_id = items[0].id;

    // The killed run: its marker (lock released, as the OS does on exit), its
    // `STARTED` row, and two entities it had already written.
    let marker = RunOwnership::acquire(&h.ownership_dir);
    let token = marker.token().expect("marker created");
    drop(marker);
    let dead_run = h.seed_started_row(dataset_id, Some(token)).await;
    let slug_a = h.seed_run_artifact(dataset_id, data_id, dead_run).await;
    let slug_b = h.seed_run_artifact(dataset_id, data_id, dead_run).await;

    // Another run's artifact in the same dataset, which must survive: the
    // rollback is scoped to one run, never to the dataset.
    let other_run = Uuid::new_v4();
    let slug_other = h.seed_run_artifact(dataset_id, data_id, other_run).await;

    assert_eq!(h.ledger_rows_for_run(dead_run, dataset_id).await, 2);
    assert!(h.graph_has(slug_a).await && h.graph_has(slug_b).await);
    assert_eq!(h.entity_points().await, 3);

    let report = h.recover(std::time::Duration::ZERO).await;
    assert_eq!(report.recovered, 1, "{report:?}");
    assert_eq!(report.failed, 0, "{report:?}");

    assert_eq!(
        h.ledger_rows_for_run(dead_run, dataset_id).await,
        0,
        "the dead run's ownership rows must be gone"
    );
    assert!(
        !h.graph_has(slug_a).await && !h.graph_has(slug_b).await,
        "the dead run's graph nodes must be gone"
    );
    assert_eq!(
        h.entity_points().await,
        1,
        "the dead run's vector points must be gone, and only those"
    );
    assert_eq!(h.ledger_rows_for_run(other_run, dataset_id).await, 1);
    assert!(h.graph_has(slug_other).await);

    let latest = h
        .pipeline_run_repo
        .get_pipeline_run_by_dataset(dataset_id, COGNIFY_PIPELINE)
        .await
        .unwrap()
        .expect("a row");
    assert_eq!(latest.status, PipelineRunStatus::Errored);
    assert_eq!(latest.pipeline_run_id, dead_run);
    let error = latest
        .run_info
        .as_ref()
        .and_then(|i| i.get("error"))
        .and_then(|e| e.as_str())
        .expect("{\"error\": …}");
    assert_eq!(error, cognee_delete::ABANDONED_RUN_ERROR);
    assert!(
        error.starts_with("AbandonedPipelineRunError: ") && error.contains("(Status code: 500)"),
        "Python's `str(AbandonedPipelineRunError())`"
    );
    assert!(
        !h.ownership_dir.join(token.to_string()).exists(),
        "the closed run's marker is removed"
    );

    // A second recovery finds nothing left to do.
    assert_eq!(h.recover(std::time::Duration::ZERO).await.candidates, 0);

    let result = h.cognify(dataset_id, items, &config).await;
    assert!(
        !result.already_completed && !result.entities.is_empty(),
        "the dataset must be cognifiable again once the corpse is cleared"
    );
}

/// The safety half: a run whose writer is alive holds its marker's OS lock, so
/// recovery cannot claim it and must leave the row and its artifacts alone —
/// however old the row is.
#[tokio::test]
async fn a_run_whose_writer_is_alive_is_not_recovered() {
    let h = Harness::new().await;
    let items = h.add("repeat_cognify_alive", WAVE_1_TEXT).await;
    let dataset_id = h.dataset_id("repeat_cognify_alive").await;

    let live_marker = RunOwnership::acquire(&h.ownership_dir);
    let run = h.seed_started_row(dataset_id, live_marker.token()).await;
    let slug = h.seed_run_artifact(dataset_id, items[0].id, run).await;

    let report = h.recover(std::time::Duration::ZERO).await;
    assert_eq!(report.recovered, 0, "{report:?}");
    assert_eq!(report.skipped_unverifiable, 1, "{report:?}");
    assert_eq!(
        h.latest_cognify_status(dataset_id).await,
        Some(PipelineRunStatus::Started),
        "the live run's row must not be retired underneath it"
    );
    assert!(h.graph_has(slug).await, "nor its artifacts rolled back");

    // Once the writer is gone, the same row is recoverable.
    drop(live_marker);
    assert_eq!(h.recover(std::time::Duration::ZERO).await.recovered, 1);
    assert!(!h.graph_has(slug).await);
}

/// A `STARTED` row with no marker (an older writer, a run started without an
/// ownership directory, another machine) cannot be proven abandoned and is
/// never recovered.
#[tokio::test]
async fn a_started_row_without_a_marker_is_never_recovered() {
    let h = Harness::new().await;
    let items = h.add("repeat_cognify_no_marker", WAVE_1_TEXT).await;
    let dataset_id = h.dataset_id("repeat_cognify_no_marker").await;

    let run = h.seed_started_row(dataset_id, None).await;
    let slug = h.seed_run_artifact(dataset_id, items[0].id, run).await;
    // A token whose marker is not on this filesystem is just as unverifiable.
    h.seed_started_row(dataset_id, Some(Uuid::new_v4())).await;

    let report = h.recover(std::time::Duration::ZERO).await;
    assert_eq!(report.recovered, 0, "{report:?}");
    assert_eq!(report.skipped_unverifiable, 2, "{report:?}");
    assert!(h.graph_has(slug).await);
}

/// Age is only an eligibility floor: a run younger than it is treated as live
/// even when its marker could be claimed.
#[tokio::test]
async fn a_run_younger_than_the_age_floor_is_not_recovered() {
    let h = Harness::new().await;
    h.add("repeat_cognify_young", WAVE_1_TEXT).await;
    let dataset_id = h.dataset_id("repeat_cognify_young").await;

    let marker = RunOwnership::acquire(&h.ownership_dir);
    h.seed_started_row(dataset_id, marker.token()).await;
    drop(marker);

    let report = h.recover(cognee_delete::DEFAULT_STALE_RUN_MIN_AGE).await;
    assert_eq!(report.skipped_young, 1, "{report:?}");
    assert_eq!(report.recovered, 0, "{report:?}");
    assert_eq!(
        h.latest_cognify_status(dataset_id).await,
        Some(PipelineRunStatus::Started)
    );
}

/// A dataset whose lock this process holds has a run of ours in flight:
/// recovery leaves it for later (Python's `lock.locked()` check).
#[tokio::test]
async fn a_dataset_locked_in_this_process_is_skipped_by_recovery() {
    let h = Harness::new().await;
    h.add("repeat_cognify_locked_recovery", WAVE_1_TEXT).await;
    let dataset_id = h.dataset_id("repeat_cognify_locked_recovery").await;

    let marker = RunOwnership::acquire(&h.ownership_dir);
    h.seed_started_row(dataset_id, marker.token()).await;
    drop(marker);

    let guard = DatasetLocks::global().lock(dataset_id).await;
    let report = h.recover(std::time::Duration::ZERO).await;
    assert_eq!(report.skipped_dataset_locked, 1, "{report:?}");
    assert_eq!(report.recovered, 0, "{report:?}");
    drop(guard);

    assert_eq!(h.recover(std::time::Duration::ZERO).await.recovered, 1);
}
