#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Failures are data, not control flow.
//!
//! The per-stage tests in `tasks.rs` pin what each collecting stage does with a
//! failure. What they cannot show is that the structured report survives the
//! whole trip — stage output → executor → `cognify()`'s caller — instead of
//! being flattened into an error string somewhere in between, and that the run
//! row still comes out `ERRORED` when the policy says the run failed.
//!
//! Offline: `MockLlm` / `MockStorage` / `MockGraphDB` / `MockVectorDB` /
//! `MockEmbeddingEngine` and a real `SeaOrmPipelineRunRepository` over
//! in-memory SQLite. No network, no LLM key, no skip path.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use cognee_cognify::{
    CognifyConfig, CognifyError, CognifyResult, FailureReport, FailureStage, FailureStop,
    RollbackScope, cognify,
};
use cognee_database::ops::datasets::create_dataset;
use cognee_database::{
    DatabaseConnection, PipelineRunRepository, PipelineRunStatus, SeaOrmPipelineRunRepository,
    connect, initialize,
};
use cognee_embedding::MockEmbeddingEngine;
use cognee_models::{Data, Dataset};
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{MockStorage, StorageTrait};
use cognee_test_utils::{MockGraphDB, MockLlm, MockVectorDB};
use cognee_vector::VectorDB;
use serde_json::json;
use uuid::Uuid;

/// The marker `MockLlm::with_failing_markers` keys on. A file whose text
/// contains it fails deterministically, whatever order the chunks are
/// dispatched in — which a FIFO response queue cannot be once the stage
/// dispatches chunks concurrently.
const FAIL_MARKER: &str = "FAILMARKER";

fn canned_graph_response() -> String {
    json!({
        "nodes": [
            {"id": "alice", "name": "Alice", "type": "PERSON", "description": "A person."},
            {"id": "acme", "name": "Acme", "type": "ORGANIZATION", "description": "A company."}
        ],
        "edges": [{
            "source_node_id": "alice",
            "target_node_id": "acme",
            "relationship_name": "works_at",
            "description": "Alice works at Acme."
        }]
    })
    .to_string()
}

/// What one run of the harness returns: the SDK outcome, the vector store it
/// wrote through, and the repository that recorded the run's status trail.
struct RunOutcome {
    result: Result<CognifyResult, CognifyError>,
    vector_db: Arc<dyn VectorDB>,
    repo: Arc<dyn PipelineRunRepository>,
    dataset_id: Uuid,
}

impl RunOutcome {
    /// The status the executor last wrote for this dataset's run.
    async fn latest_status(&self) -> PipelineRunStatus {
        let runs = self
            .repo
            .list_recent(Some(self.dataset_id), 50)
            .await
            .expect("list pipeline runs");
        assert!(!runs.is_empty(), "the run must have left a status trail");
        runs[0].status.clone()
    }

    /// The failure report, whichever side of the `Result` it came back on.
    fn report(&self) -> &FailureReport {
        match &self.result {
            Err(CognifyError::RunFailed { report }) => report,
            Err(other) => panic!("expected RunFailed, got: {other:?}"),
            Ok(result) => &result.failures,
        }
    }
}

/// Run one cognify over `texts` — one file each — against mock backends and the
/// caller-supplied LLM.
async fn run_cognify_with_llm(
    config: CognifyConfig,
    llm: Arc<dyn cognee_llm::Llm>,
    texts: &[&str],
) -> RunOutcome {
    let dataset_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();

    let storage: Arc<dyn StorageTrait> = Arc::new(MockStorage::new());
    let graph_db: Arc<dyn cognee_graph::GraphDBTrait> = Arc::new(MockGraphDB::new());
    let vector_db: Arc<dyn VectorDB> = Arc::new(MockVectorDB::new());
    let embedding_engine: Arc<dyn cognee_embedding::EmbeddingEngine> =
        Arc::new(MockEmbeddingEngine::new(8));

    let mut data_items = Vec::new();
    for (index, text) in texts.iter().enumerate() {
        let data_id = Uuid::new_v4();
        let location = storage
            .store(text.as_bytes(), &format!("failure-{data_id}"))
            .await
            .expect("MockStorage::store");
        data_items.push(
            Data::builder(
                data_id,
                format!("failure-{index}.txt"),
                location,
                format!("failure-{index}.txt"),
                "txt",
                "text/plain",
                format!("test-hash-{data_id}"),
                owner_id,
            )
            .build(),
        );
    }

    let db: Arc<DatabaseConnection> = {
        let conn = connect("sqlite::memory:").await.expect("connect sqlite");
        initialize(&conn).await.expect("initialize");
        create_dataset(
            &conn,
            Dataset::new("failures".into(), owner_id, None, dataset_id),
        )
        .await
        .expect("seed dataset");
        Arc::new(conn)
    };

    let repo: Arc<dyn PipelineRunRepository> =
        Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&db)));
    let thread_pool: Arc<dyn cognee_core::CpuPool> =
        Arc::new(cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool"));

    let result = cognify(
        data_items,
        dataset_id,
        Some(owner_id),
        None,
        None,
        llm,
        storage,
        graph_db,
        Arc::clone(&vector_db),
        embedding_engine,
        Arc::clone(&db),
        Arc::clone(&repo),
        thread_pool,
        Arc::new(NoOpOntologyResolver::new()),
        &config,
    )
    .await;

    RunOutcome {
        result,
        vector_db,
        repo,
        dataset_id,
    }
}

/// A run whose graph extraction fails on the marker.
async fn run_cognify(config: CognifyConfig, texts: &[&str]) -> RunOutcome {
    let llm: Arc<dyn cognee_llm::Llm> = Arc::new(
        MockLlm::new(vec![canned_graph_response(); texts.len() + 4])
            .with_failing_markers(vec![FAIL_MARKER.to_string()]),
    );
    run_cognify_with_llm(config, llm, texts).await
}

/// One chunk per file and one chunk per batch, so the abort boundary falls
/// exactly between files.
///
/// Summarization is off here so a marker in the fixture text is purely a
/// graph-extraction failure — both stages read the same chunk text, so leaving
/// it on would double every count and say nothing extra. The summarization
/// tests below drive it with an LLM that fails only on the summarization
/// schema.
fn extraction_config() -> CognifyConfig {
    CognifyConfig::default()
        .with_chunk_size(1500)
        .with_chunks_per_batch(1)
        .with_summarization(false)
}

/// The default pair still aborts at the first failure — but the caller now gets
/// the structured report instead of a flattened error string, and the run is
/// still written `ERRORED`.
#[tokio::test]
async fn default_policy_errors_with_a_report_and_marks_the_run_errored() {
    let outcome = run_cognify(
        extraction_config(),
        &[
            "Alice works at Acme.",
            "Bob FAILMARKER breaks here.",
            "Carol also works at Acme.",
        ],
    )
    .await;

    match &outcome.result {
        Err(CognifyError::RunFailed { report }) => {
            assert_eq!(report.entries().len(), 1);
            assert_eq!(report.entries()[0].stage, FailureStage::GraphExtraction);
            assert_eq!(report.failed_items().len(), 1);
            assert_eq!(
                report.unreached_items().len(),
                1,
                "the third file was never reached"
            );
        }
        // The whole point of the downcast in `unwrap_execution_error`: without
        // it this arrives as `Execute(String)` and the report is gone.
        other => panic!("expected RunFailed with a report, got: {other:?}"),
    }

    assert_eq!(outcome.latest_status().await, PipelineRunStatus::Errored);
}

/// `FailFast` + `FailedItems` keeps and indexes the file that fully completed,
/// and nothing of the failed or the unreached one. The run genuinely completed.
///
/// The threshold is raised because this fixture is three chunks wide: one
/// failed chunk out of three is a 33 % failure ratio, which the 5 % default
/// rightly calls a failed run. The ratio exists to catch systemic mid-run LLM
/// failure, and on a three-file dataset one bad file *is* systemic.
#[tokio::test]
async fn fail_fast_failed_items_keeps_and_indexes_the_complete_files() {
    let outcome = run_cognify(
        extraction_config()
            .with_rollback_scope(RollbackScope::FailedItems)
            .with_chunk_failure_ratio_threshold(0.5),
        &[
            "Alice works at Acme.",
            "Bob FAILMARKER breaks here.",
            "Carol also works at Acme.",
        ],
    )
    .await;

    let result = outcome
        .result
        .as_ref()
        .expect("a tolerated failure below the ratio completes the run");
    assert_eq!(
        result.chunks.len(),
        1,
        "only the file that fully completed survives"
    );
    assert_eq!(result.failures.failed_items().len(), 1);
    assert_eq!(result.failures.unreached_items().len(), 1);

    // The complete file's chunk reached the vector store; the other two did not.
    assert_eq!(
        outcome
            .vector_db
            .collection_size("DocumentChunk", "text")
            .await
            .expect("collection size"),
        1
    );

    assert_eq!(outcome.latest_status().await, PipelineRunStatus::Completed);
}

/// `RunToEnd` pays for the whole run and reports every failure, rather than
/// surfacing one bad file per run the way the default does.
#[tokio::test]
async fn run_to_end_collects_every_failure_before_failing() {
    let outcome = run_cognify(
        extraction_config().with_failure_stop(FailureStop::RunToEnd),
        &[
            "Alice works at Acme.",
            "Bob FAILMARKER one.",
            "Carol at Acme.",
            "Dan FAILMARKER two.",
            "Erin at Acme.",
            "Frank FAILMARKER three.",
        ],
    )
    .await;

    let report = outcome.report();
    assert_eq!(report.entries().len(), 3, "every failing file is listed");
    assert_eq!(report.failed_items().len(), 3);
    assert!(
        report.unreached_items().is_empty(),
        "RunToEnd attempts every file"
    );
    assert_eq!(outcome.latest_status().await, PipelineRunStatus::Errored);
}

/// Summarization is fatal by default — Python parity — but it never abandons
/// its stream on the way there: the LLM saw one summarization call per chunk,
/// so the surviving summaries were produced and the reported list is complete.
#[tokio::test]
async fn summarization_failure_is_fatal_by_default_but_never_abandons_the_stream() {
    let llm = Arc::new(SummarizationFailingLlm::new(vec![FAIL_MARKER.to_string()]));
    let outcome = run_cognify_with_llm(
        CognifyConfig::default()
            .with_chunk_size(1500)
            .with_chunks_per_batch(1),
        llm.clone(),
        &["Alice works at Acme.", "Bob FAILMARKER.", "Carol at Acme."],
    )
    .await;

    let report = outcome.report();
    assert_eq!(report.summarization_failures(), 1);
    assert_eq!(
        report.failed_items().len(),
        1,
        "an untolerated summarization failure fails its item"
    );
    assert_eq!(
        llm.summary_calls.load(Ordering::SeqCst),
        3,
        "every chunk was summarized — nothing in flight was cancelled"
    );
    assert_eq!(outcome.latest_status().await, PipelineRunStatus::Errored);
}

/// With tolerance on, a summarization failure is recorded, reported, and fatal
/// to nothing: the run completes, the item is not failed, and the failure
/// counts toward neither the ratio nor any other denominator.
#[tokio::test]
async fn summarization_failure_is_tolerated_when_the_flag_is_set() {
    let llm = Arc::new(SummarizationFailingLlm::new(vec![FAIL_MARKER.to_string()]));
    let outcome = run_cognify_with_llm(
        CognifyConfig::default()
            .with_chunk_size(1500)
            .with_chunks_per_batch(1)
            .with_summarization_failure_tolerance(true),
        llm.clone(),
        &["Alice works at Acme.", "Bob FAILMARKER.", "Carol at Acme."],
    )
    .await;

    let result = outcome
        .result
        .as_ref()
        .expect("a tolerated summarization failure never fails the run");
    assert_eq!(result.failures.total(), 1, "it is still reported");
    assert_eq!(result.failures.summarization_failures(), 1);
    assert!(
        result.failures.failed_items().is_empty(),
        "…and fails no item"
    );
    assert_eq!(result.failures.chunk_failure_ratio(), 0.0);
    assert_eq!(
        result.chunks.len(),
        3,
        "every file is still fully cognified"
    );
    assert_eq!(llm.summary_calls.load(Ordering::SeqCst), 3);

    // The two surviving summaries reached the vector store.
    assert_eq!(
        outcome
            .vector_db
            .collection_size("TextSummary", "text")
            .await
            .expect("collection size"),
        2
    );
    assert_eq!(outcome.latest_status().await, PipelineRunStatus::Completed);
}

/// The report cap bounds the error message without bounding what a sweep will
/// later need: every failed file is still named.
#[tokio::test]
async fn report_cap_bounds_the_error_message() {
    let outcome = run_cognify(
        extraction_config()
            .with_failure_stop(FailureStop::RunToEnd)
            .with_failure_report_cap(2),
        &[
            "Alice FAILMARKER one.",
            "Bob FAILMARKER two.",
            "Carol FAILMARKER three.",
            "Dan FAILMARKER four.",
            "Erin FAILMARKER five.",
        ],
    )
    .await;

    let report = outcome.report();
    assert_eq!(report.entries().len(), 2, "the listing honours the cap");
    assert_eq!(report.total(), 5);
    assert_eq!(report.truncated(), 3);
    assert_eq!(
        report.failed_items().len(),
        5,
        "but every failed file is still named — a sweep selects by this set"
    );

    let rendered = outcome.result.as_ref().unwrap_err().to_string();
    assert!(
        rendered.len() < 1000,
        "the error message must stay bounded, got {} chars",
        rendered.len()
    );
    assert!(rendered.contains("5 failure(s) (2 shown, 3 omitted)"));
}

/// A mock LLM that answers graph extraction from a canned response and fails
/// only the *summarization* calls whose chunk text carries a marker — so a
/// fixture can exercise the summarization policy without also tripping
/// extraction. Dispatches on the schema the caller supplies: the summarization
/// call carries `SummarizedContent`'s schema (a `summary` property), extraction
/// carries `KnowledgeGraph`'s.
struct SummarizationFailingLlm {
    markers: Vec<String>,
    summary_calls: AtomicUsize,
}

impl SummarizationFailingLlm {
    fn new(markers: Vec<String>) -> Self {
        Self {
            markers,
            summary_calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl cognee_llm::Llm for SummarizationFailingLlm {
    async fn generate(
        &self,
        _messages: Vec<cognee_llm::Message>,
        _options: Option<cognee_llm::GenerationOptions>,
    ) -> cognee_llm::LlmResult<cognee_llm::GenerationResponse> {
        unreachable!("the cognify pipeline only uses structured output")
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<cognee_llm::Message>,
        json_schema: &serde_json::Value,
        _options: Option<cognee_llm::GenerationOptions>,
    ) -> cognee_llm::LlmResult<serde_json::Value> {
        let is_summary = json_schema
            .get("properties")
            .and_then(|props| props.get("summary"))
            .is_some();
        if !is_summary {
            return serde_json::from_str(&canned_graph_response())
                .map_err(|e| cognee_llm::LlmError::DeserializationError(e.to_string()));
        }

        self.summary_calls.fetch_add(1, Ordering::SeqCst);
        // Hold the "request" open so a cancelled sibling would show up as a
        // missing call rather than as a race the assertion could win anyway.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let content: String = messages.iter().map(|m| m.content.as_str()).collect();
        if self
            .markers
            .iter()
            .any(|marker| content.contains(marker.as_str()))
        {
            return Err(cognee_llm::LlmError::RateLimitExceeded(
                "simulated 429 on summarization".to_string(),
            ));
        }
        Ok(json!({"summary": "Alice and Acme.", "description": "A description."}))
    }

    fn model(&self) -> &str {
        "mock-summarization-failing"
    }
}
