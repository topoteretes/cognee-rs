#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Regression test: a dataset must be cognifiable more than once, so data can
//! be loaded in waves (`add` → `cognify` → `add` → `cognify`).
//!
//! Before the fix, `cognify()` consulted `check_pipeline_run_qualification`
//! unconditionally, so the COMPLETED `pipeline_runs` row left by the first run
//! made every later run a silent no-op — new data added to the dataset was
//! never extracted. Python only consults that layer behind
//! `if use_pipeline_cache:` (`modules/pipelines/operations/pipeline.py`), and
//! both public entry points pass `use_pipeline_cache=False`, so upstream a
//! repeat cognify always re-runs.
//!
//! **These tests must use a real [`SeaOrmPipelineRunRepository`].** The rest of
//! the crate's tests pass `NoopPipelineRunRepository`, whose
//! `get_pipeline_run_by_dataset` returns `Ok(None)` → `Qualification::Proceed`,
//! which is precisely why the bug shipped with a green suite. The first test
//! asserts the COMPLETED row actually exists before re-running, so it cannot
//! silently degrade into a vacuous pass.
//!
//! Runs fully offline (mock LLM / embeddings / graph / vector) — a real CI gate.

use std::sync::Arc;

use cognee_cognify::tasks::CLAIM_STALE_AFTER;
use cognee_cognify::{CognifyConfig, CognifyError, CognifyResult, cognify};
use cognee_database::{
    DatabaseConnection, IngestDb, PipelineRunRepository, PipelineRunStatus,
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
/// `pipeline_runs.pipeline_name` the gate reads. Mirrors
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
            .with_database(Arc::clone(&database));

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
        }
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
        ops::datasets::get_dataset_by_name(&self.database, dataset_name, self.owner_id, None)
            .await
            .expect("get_dataset_by_name")
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
            Arc::clone(&self.database),
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
            Arc::clone(&self.database),
            Arc::clone(&self.pipeline_run_repo),
            Arc::new(cognee_core::RayonThreadPool::with_default_threads().unwrap())
                as Arc<dyn cognee_core::CpuPool>,
            Arc::clone(&self.ontology),
            config,
        )
        .await
    }

    /// Write a bare `Started` row for the cognify pipeline, standing in for a
    /// run that is still in flight (or was killed mid-run).
    async fn seed_started_row(&self, dataset_id: Uuid) {
        self.pipeline_run_repo
            .log_pipeline_run(
                Uuid::new_v4(),
                Uuid::new_v4(),
                COGNIFY_PIPELINE,
                Some(dataset_id),
                PipelineRunStatus::Started,
                None,
            )
            .await
            .expect("log_pipeline_run");
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

/// Keep the graph focused and the run cheap — no summaries or triplet embeddings.
fn base_config() -> CognifyConfig {
    CognifyConfig::default()
        .with_summarization(false)
        .with_triplet_embeddings(false)
}

/// The regression test for this issue: a second wave of data added to an
/// already-cognified dataset must actually be extracted.
#[tokio::test]
async fn second_cognify_wave_is_processed_not_skipped() {
    let h = Harness::new().await;
    let dataset_name = "repeat_cognify_waves";
    let config = base_config();

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
    // row, i.e. the repository is live and the gate has something to find.
    assert_eq!(
        h.latest_cognify_status(dataset_id).await,
        Some(PipelineRunStatus::Completed),
        "wave 1 must leave a COMPLETED pipeline_runs row — without it this test \
         would pass even with the bug present"
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
        "wave 2 must extract: a COMPLETED row from wave 1 must not skip the run \
         when the pipeline cache is off (the default)"
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

/// The cache still works when a caller explicitly opts in — the flag is what
/// selects the behaviour, matching Python's `use_pipeline_cache` parameter.
#[tokio::test]
async fn pipeline_cache_opt_in_short_circuits_the_second_run() {
    let h = Harness::new().await;
    let dataset_name = "repeat_cognify_cache_on";
    let config = base_config().with_pipeline_cache(true);

    let items = h.add(dataset_name, WAVE_1_TEXT).await;
    let dataset_id = h.dataset_id(dataset_name).await;

    let result_1 = h.cognify(dataset_id, items.clone(), &config).await;
    assert!(
        !result_1.already_completed,
        "the first run has no prior row to hit"
    );
    assert_eq!(
        h.latest_cognify_status(dataset_id).await,
        Some(PipelineRunStatus::Completed),
    );

    let result_2 = h.cognify(dataset_id, items, &config).await;
    assert!(
        result_2.already_completed,
        "with the cache on, a COMPLETED dataset must short-circuit"
    );
    assert!(
        result_2.prior_pipeline_run_id.is_some(),
        "the short-circuit must report the prior run id"
    );
    assert!(
        result_2.entities.is_empty(),
        "a short-circuited run does no extraction"
    );
}

/// The concurrency guard must survive the fix: a run still in flight (a
/// `Started` row) rejects a second run *regardless* of the cache flag.
///
/// Python can gate this verdict on `use_pipeline_cache` because
/// `run_pipeline_per_dataset` serializes on `get_dataset_lock(dataset.id)`
/// ("concurrent runs are kept safe by the per-dataset lock, not by this
/// check"). Rust has no such lock, so this row check is the only thing keeping
/// two concurrent cognify runs off one dataset — gating it on the cache flag
/// would have silently removed that protection.
#[tokio::test]
async fn a_started_run_still_rejects_a_concurrent_run_with_the_cache_off() {
    let h = Harness::new().await;
    let dataset_name = "repeat_cognify_concurrent";
    let config = base_config();
    assert!(
        !config.use_pipeline_cache,
        "this test is about the cache being OFF"
    );

    let items = h.add(dataset_name, WAVE_1_TEXT).await;
    let dataset_id = h.dataset_id(dataset_name).await;

    h.seed_started_row(dataset_id).await;
    assert_eq!(
        h.latest_cognify_status(dataset_id).await,
        Some(PipelineRunStatus::Started),
    );

    let err = h
        .try_cognify(dataset_id, items, &config)
        .await
        .expect_err("a run already in flight must be rejected");
    assert!(
        matches!(err, CognifyError::PipelineAlreadyRunning { .. }),
        "expected PipelineAlreadyRunning, got {err:?}"
    );
}

/// A claim held by someone else blocks a run, and the run succeeds again once
/// that claim is released.
///
/// This is the "two simultaneous starts" case in deterministic form. The state
/// that matters is *a claim being held while a second caller enters the run*,
/// and pre-taking the claim reproduces exactly that without depending on task
/// interleaving — a timing-based race would be flaky in both directions (both
/// callers could serialize in time and legitimately succeed). The atomicity of
/// the claim itself is covered by `concurrent_claims_grant_exactly_one` in
/// `cognee-database`, which contends on the real primary key.
#[tokio::test]
async fn a_claim_held_elsewhere_blocks_the_run() {
    let h = Harness::new().await;
    let dataset_name = "repeat_cognify_claimed";
    let config = base_config();

    let items = h.add(dataset_name, WAVE_1_TEXT).await;
    let dataset_id = h.dataset_id(dataset_name).await;

    // Stand in for a run already in flight elsewhere — in another process, or
    // in the window before it has written its `Started` row.
    let elsewhere = Uuid::new_v4();
    assert!(
        h.pipeline_run_repo
            .try_claim_pipeline_run(dataset_id, COGNIFY_PIPELINE, elsewhere, CLAIM_STALE_AFTER)
            .await
            .expect("foreign claim"),
        "the foreign claim must be granted first"
    );

    let err = h
        .try_cognify(dataset_id, items.clone(), &config)
        .await
        .expect_err("a claimed dataset must not be cognified concurrently");
    assert!(
        matches!(err, CognifyError::PipelineAlreadyRunning { .. }),
        "expected PipelineAlreadyRunning, got {err:?}"
    );

    h.pipeline_run_repo
        .release_pipeline_run_claim(dataset_id, COGNIFY_PIPELINE, elsewhere)
        .await
        .expect("release foreign claim");

    let result = h.cognify(dataset_id, items, &config).await;
    assert!(
        !result.already_completed && !result.entities.is_empty(),
        "the run must proceed once the claim is free"
    );
}

/// A completed run must not leave its claim behind — otherwise the very next
/// wave would be rejected instead of processed.
#[tokio::test]
async fn a_finished_run_releases_its_claim() {
    let h = Harness::new().await;
    let dataset_name = "repeat_cognify_release";
    let config = base_config();

    let items = h.add(dataset_name, WAVE_1_TEXT).await;
    let dataset_id = h.dataset_id(dataset_name).await;
    h.cognify(dataset_id, items, &config).await;

    // If the run leaked its claim, this could not be granted.
    assert!(
        h.pipeline_run_repo
            .try_claim_pipeline_run(
                dataset_id,
                COGNIFY_PIPELINE,
                Uuid::new_v4(),
                CLAIM_STALE_AFTER
            )
            .await
            .expect("claim after run"),
        "the claim must be free once the run has finished"
    );
}
