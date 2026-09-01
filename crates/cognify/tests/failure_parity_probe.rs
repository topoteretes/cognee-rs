#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Rust half of the cognify-failure differential harness
//! (`e2e-cross-sdk/failure-parity/`).
//!
//! Runs the same (scenario, config) matrix the Python probe runs and emits the
//! SAME JSON observation shape, one object per line, so the two can be diffed
//! field by field. The scenarios are:
//!
//! * `clean`                  — nothing fails; the control.
//! * `unreadable_file`        — one file's bytes are gone before the run.
//! * `extraction_failure`     — graph extraction fails for one file's chunk.
//! * `summarization_failure`  — summarization fails for one file's chunk.
//! * `second_run_after_success` — a run that fails AFTER an earlier run
//!   completed: the question is what the failure does to the earlier run's
//!   markers and artifacts.
//!
//! and the configs are the two Python exposes through
//! `RAISE_INCREMENTAL_LOADING_ERRORS`, mapped onto Rust's axis-1 the way
//! `FailureStop::from_env` maps them (asserted below rather than assumed).
//!
//! Emitting rather than asserting is deliberate: the comparison lives in
//! `compare.py`, which sees both SDKs' observations at once. This file is the
//! Rust *instrument*, and it is run by `run_rust.sh`.

// Deliberately ungated: a `#![cfg(feature = "...")]` here would compile the
// probe away under `cargo test -p cognee-cognify` and report `running 0 tests
// … ok`, and everything it needs (the mocks) comes from this crate's
// `[dev-dependencies]`, which are on for every test build regardless.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use cognee_cognify::failure::{FailureStop, RollbackScope};
use cognee_cognify::{CognifyConfig, CognifyError, cognify};
use cognee_database::ops::data::{create_data, get_cognify_completed_data_ids};
use cognee_database::ops::datasets::{attach_data_to_dataset, create_dataset};
use cognee_database::ops::graph_storage::{get_edges_by_dataset, get_nodes_by_dataset};
use cognee_database::{
    DatabaseConnection, PipelineRunRepository, PipelineRunStatus, SeaOrmPipelineRunRepository,
    connect, initialize,
};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_llm::{GenerationOptions, GenerationResponse, Llm, LlmError, LlmResult, Message};
use cognee_models::{Data, Dataset};
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{MockStorage, StorageTrait};
use cognee_test_utils::{MockGraphDB, MockLlm, MockVectorDB};
use cognee_vector::VectorDB;
use serde_json::{Value, json};
use uuid::Uuid;

const FAIL_MARKER: &str = "FAILMARKER";
const ROLES: [&str; 3] = ["good_a", "poison", "good_b"];

/// The same canned extraction every file gets on both sides of the harness:
/// two entities and one edge, identical for every file, so an artifact is
/// genuinely shared between the failing file and the surviving ones.
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

fn canned_summary_response() -> String {
    json!({"summary": "Alice and Acme.", "description": "A description."}).to_string()
}

/// Fails exactly one stage for the chunk carrying [`FAIL_MARKER`], dispatching
/// on the schema the caller supplies — the mirror image of the Python probe's
/// `install_llm_mock`, which dispatches on `response_model`.
///
/// `MockLlm::with_failing_markers` cannot be used for this: it checks the
/// marker *before* the schema, so it would fail extraction and summarization
/// alike and the two scenarios would stop being distinguishable.
struct StageFailingLlm {
    /// `"graph"`, `"summary"`, or `""` for a mock that never fails.
    fail_stage: &'static str,
}

#[async_trait::async_trait]
impl Llm for StageFailingLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        unreachable!("the cognify pipeline only uses structured output")
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<Message>,
        json_schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<Value> {
        let is_summary = json_schema
            .get("properties")
            .and_then(|props| props.get("summary"))
            .is_some();
        let content: String = messages.iter().map(|m| m.content.as_str()).collect();
        let poisoned = content.contains(FAIL_MARKER);

        if poisoned && self.fail_stage == "graph" && !is_summary {
            return Err(LlmError::ApiError(
                "simulated LLM failure during graph extraction".to_string(),
            ));
        }
        if poisoned && self.fail_stage == "summary" && is_summary {
            return Err(LlmError::ApiError(
                "simulated LLM failure during summarization".to_string(),
            ));
        }

        let raw = if is_summary {
            canned_summary_response()
        } else {
            canned_graph_response()
        };
        serde_json::from_str(&raw).map_err(|e| LlmError::DeserializationError(e.to_string()))
    }

    fn model(&self) -> &str {
        "mock-stage-failing"
    }
}

/// One cell's world: a dataset, its stores, its database.
struct Cell {
    db: Arc<DatabaseConnection>,
    repo: Arc<dyn PipelineRunRepository>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<MockGraphDB>,
    vector_db: Arc<MockVectorDB>,
    dataset_id: Uuid,
    owner_id: Uuid,
    items: Vec<Data>,
    by_role: BTreeMap<String, Uuid>,
}

impl Cell {
    async fn new() -> Self {
        let dataset_id = Uuid::new_v4();
        let owner_id = Uuid::new_v4();
        let conn = connect("sqlite::memory:").await.expect("connect sqlite");
        initialize(&conn).await.expect("initialize");
        create_dataset(&conn, Dataset::new("fp".into(), owner_id, None, dataset_id))
            .await
            .expect("seed dataset");
        let db = Arc::new(conn);
        Self {
            repo: Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&db))),
            db,
            storage: Arc::new(MockStorage::new()),
            graph_db: Arc::new(MockGraphDB::new()),
            vector_db: Arc::new(MockVectorDB::new()),
            dataset_id,
            owner_id,
            items: Vec::new(),
            by_role: BTreeMap::new(),
        }
    }

    /// Persist one file. `readable = false` gives the `Data` row a location
    /// nothing was ever stored under — the Rust analogue of the Python probe
    /// unlinking the ingested copy: the read fails on the way into the chunker.
    async fn add_file(&mut self, role: &str, text: &str, readable: bool) {
        let data_id = Uuid::new_v4();
        let location = if readable {
            self.storage
                .store(text.as_bytes(), &format!("fp-{data_id}"))
                .await
                .expect("MockStorage::store")
        } else {
            format!("mock://never-stored-{data_id}")
        };
        let item = Data::builder(
            data_id,
            format!("{role}.txt"),
            location.clone(),
            location,
            "txt",
            "text/plain",
            format!("hash-{data_id}"),
            self.owner_id,
        )
        .build();
        create_data(&self.db, item.clone())
            .await
            .expect("persist the Data row");
        attach_data_to_dataset(&self.db, self.dataset_id, data_id)
            .await
            .expect("attach to the dataset");
        self.items.push(item);
        self.by_role.insert(role.to_string(), data_id);
    }

    async fn run(
        &self,
        config: &CognifyConfig,
        llm: Arc<dyn Llm>,
    ) -> Result<cognee_cognify::CognifyResult, CognifyError> {
        let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool"),
        );
        cognify(
            self.items.clone(),
            self.dataset_id,
            Some(self.owner_id),
            None,
            None,
            llm,
            Arc::clone(&self.storage),
            Arc::clone(&self.graph_db) as Arc<dyn GraphDBTrait>,
            Arc::clone(&self.vector_db) as Arc<dyn VectorDB>,
            Arc::new(MockEmbeddingEngine::new(8)),
            Arc::clone(&self.db),
            Arc::clone(&self.repo),
            thread_pool,
            Arc::new(NoOpOntologyResolver::new()),
            config,
        )
        .await
    }

    async fn observe(&self, scenario: &str, config: &str, caller: Value) -> Value {
        let (nodes, edges) = self
            .graph_db
            .get_graph_data()
            .await
            .expect("read the graph store");
        let node_ids: BTreeSet<String> = nodes
            .iter()
            .map(|(id, _)| id.replace('-', "").to_lowercase())
            .collect();

        // `MockGraphDB::delete_nodes` removes nodes only; the real adapters
        // issue `DETACH DELETE` (see `LadybugAdapter::delete_node`), so on a
        // real backend an edge cannot outlive its endpoints. Counting only the
        // edges whose endpoints are still present is what makes this number
        // comparable with a Python run on ladybug, and the raw count is kept
        // beside it so the mock's shortfall stays visible rather than hidden.
        let live_edges = edges
            .iter()
            .filter(|(source, target, _, _)| {
                let norm = |id: &String| id.replace('-', "").to_lowercase();
                node_ids.contains(&norm(source)) && node_ids.contains(&norm(target))
            })
            .count();

        let mut doc_node_present = serde_json::Map::new();
        for (role, data_id) in &self.by_role {
            doc_node_present.insert(
                role.clone(),
                Value::Bool(node_ids.contains(&data_id.simple().to_string())),
            );
        }

        let mut vector_points = 0usize;
        for (data_type, field) in self
            .vector_db
            .list_collections()
            .await
            .expect("list collections")
        {
            vector_points += self
                .vector_db
                .collection_size(&data_type, &field)
                .await
                .expect("collection size");
        }

        let ledger_nodes = get_nodes_by_dataset(&self.db, self.dataset_id)
            .await
            .expect("read the node ledger");
        let ledger_edges = get_edges_by_dataset(&self.db, self.dataset_id)
            .await
            .expect("read the edge ledger");
        let mut node_rows_by_role = serde_json::Map::new();
        for (role, data_id) in &self.by_role {
            node_rows_by_role.insert(
                role.clone(),
                Value::from(
                    ledger_nodes
                        .iter()
                        .filter(|n| n.data_id == *data_id)
                        .count(),
                ),
            );
        }

        // Completion markers, read the way a cross-SDK reader would: straight
        // out of the JSON column, so the KEY FORMAT is observed and not merely
        // assumed to be whatever the helper writes.
        let mut markers = serde_json::Map::new();
        let mut key_format_ok = true;
        for (role, data_id) in &self.by_role {
            let raw = read_pipeline_status(&self.db, *data_id).await;
            let cognify_entry = raw
                .as_ref()
                .and_then(|v| v.get("cognify_pipeline"))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let keys: Vec<String> = cognify_entry.keys().cloned().collect();
            if !keys.is_empty() && keys != vec![self.dataset_id.to_string()] {
                key_format_ok = false;
            }
            markers.insert(role.clone(), Value::Object(cognify_entry));
        }
        // Cross-check against the SDK's own reader, so a marker written under a
        // key the reader does not recognise cannot pass as "marked".
        let all_ids: Vec<Uuid> = self.by_role.values().copied().collect();
        let completed = get_cognify_completed_data_ids(&self.db, self.dataset_id, &all_ids)
            .await
            .expect("read completion markers");
        let mut marked_per_sdk_reader = serde_json::Map::new();
        for (role, data_id) in &self.by_role {
            marked_per_sdk_reader.insert(role.clone(), Value::Bool(completed.contains(data_id)));
        }

        let mut runs = self
            .repo
            .list_recent(Some(self.dataset_id), 100)
            .await
            .expect("list pipeline runs");
        runs.sort_by_key(|r| r.created_at);
        let run_rows: Vec<Value> = runs
            .iter()
            .map(|r| {
                let mut keys: Vec<String> = r
                    .run_info
                    .as_ref()
                    .and_then(Value::as_object)
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default();
                keys.sort();
                json!({
                    "status": pipeline_status_label(&r.status),
                    "run_info_keys": keys,
                    "run_info_error_present": r
                        .run_info
                        .as_ref()
                        .and_then(|i| i.get("error"))
                        .is_some(),
                })
            })
            .collect();
        let distinct_run_ids: BTreeSet<String> =
            runs.iter().map(|r| r.pipeline_run_id.to_string()).collect();

        json!({
            "sdk": "rust",
            "dataset_id": self.dataset_id.to_string(),
            "scenario": scenario,
            "config": config,
            "caller": caller,
            "graph": {
                "node_count": nodes.len(),
                "edge_count": live_edges,
                "edge_count_raw_including_dangling": edges.len(),
            },
            "graph_document_node_present": Value::Object(doc_node_present),
            "vector": {"point_count": vector_points},
            "ownership": {
                "node_rows": ledger_nodes.len(),
                "edge_rows": ledger_edges.len(),
                "node_rows_by_role": Value::Object(node_rows_by_role),
            },
            "markers": Value::Object(markers),
            "marked_per_sdk_reader": Value::Object(marked_per_sdk_reader),
            "marker_dataset_key_matches_dashed_uuid": key_format_ok,
            "pipeline_runs": run_rows,
            "distinct_run_ids": distinct_run_ids.len(),
        })
    }
}

/// Python writes `PipelineRunStatus.DATASET_PROCESSING_*`; Rust's enum is the
/// same four states under shorter names. Rendered in Python's vocabulary so the
/// two observation streams are directly comparable.
fn pipeline_status_label(status: &PipelineRunStatus) -> &'static str {
    match status {
        PipelineRunStatus::Initiated => "DATASET_PROCESSING_INITIATED",
        PipelineRunStatus::Started => "DATASET_PROCESSING_STARTED",
        PipelineRunStatus::Completed => "DATASET_PROCESSING_COMPLETED",
        PipelineRunStatus::Errored => "DATASET_PROCESSING_ERRORED",
    }
}

/// The raw `data.pipeline_status` JSON for one row, parsed.
///
/// Read as the stored string and parsed here rather than through
/// `get_cognify_completed_data_ids`, so the observation records the KEY FORMAT
/// on disk. A reader that tolerates several encodings (Rust's does) would
/// otherwise hide a format divergence from a shared-database Python reader,
/// which tolerates only its own.
async fn read_pipeline_status(db: &DatabaseConnection, data_id: Uuid) -> Option<Value> {
    let row = cognee_database::ops::data::get_data(db, data_id)
        .await
        .expect("find the Data row")?;
    row.pipeline_status
        .as_ref()
        .and_then(|s| serde_json::from_str(s).ok())
}

/// The Rust config each Python `RAISE_INCREMENTAL_LOADING_ERRORS` value maps
/// onto.
///
/// The value is set in the environment and read back through the SDK's own
/// `FailureStop::from_env`, not hard-coded: the claim under test includes "one
/// `.env` configures both SDKs identically", so the mapping the comparison
/// rests on must come from the shipped parser. `set_var` is sound here because
/// this binary holds exactly one test and nothing else reads the environment
/// concurrently.
fn config_for(name: &str) -> CognifyConfig {
    let raw = match name {
        "raise_true" => "true",
        "raise_false" => "false",
        other => panic!("unknown config {other}"),
    };
    unsafe {
        std::env::set_var("RAISE_INCREMENTAL_LOADING_ERRORS", raw);
    }
    let stop = FailureStop::from_env().expect("the SDK parses both Python values");
    CognifyConfig::default()
        .with_chunk_size(1500)
        .with_chunks_per_batch(1)
        .with_summarization(true)
        .with_failure_stop(stop)
        // Python has no second axis: it always sweeps the whole run.
        .with_rollback_scope(RollbackScope::WholeRun)
}

fn body_for(scenario: &str, role: &str) -> String {
    let text = format!("Document {role}. Alice works at Acme corporation. ").repeat(8);
    if role == "poison"
        && matches!(
            scenario,
            "extraction_failure" | "summarization_failure" | "second_run_after_success"
        )
    {
        format!("{FAIL_MARKER} {text}")
    } else {
        text
    }
}

fn llm_for(scenario: &str) -> Arc<dyn Llm> {
    match scenario {
        "extraction_failure" | "second_run_after_success" => Arc::new(StageFailingLlm {
            fail_stage: "graph",
        }),
        "summarization_failure" => Arc::new(StageFailingLlm {
            fail_stage: "summary",
        }),
        _ => Arc::new(
            MockLlm::new(vec![canned_graph_response(); 64])
                .with_summary_response(canned_summary_response()),
        ),
    }
}

async fn run_cell(scenario: &str, config_name: &str) -> Value {
    let mut cell = Cell::new().await;
    let config = config_for(config_name);

    if scenario == "second_run_after_success" {
        // Run A: the two good files alone, and it succeeds — so both carry a
        // completion marker and their artifacts are in the stores. Run B then
        // fails on a newly added third file. What run B does to run A's
        // markers and run A's artifacts is the question.
        for role in ["good_a", "good_b"] {
            cell.add_file(role, &body_for(scenario, role), true).await;
        }
        let clean: Arc<dyn Llm> = Arc::new(
            MockLlm::new(vec![canned_graph_response(); 64])
                .with_summary_response(canned_summary_response()),
        );
        cell.run(&config, clean)
            .await
            .expect("run A must succeed, or the scenario tests nothing");
        cell.add_file("poison", &body_for(scenario, "poison"), true)
            .await;
    } else {
        for role in ROLES {
            let readable = !(role == "poison" && scenario == "unreadable_file");
            cell.add_file(role, &body_for(scenario, role), readable)
                .await;
        }
    }

    let llm = llm_for(scenario);

    let caller = match cell.run(&config, llm).await {
        Ok(result) => json!({
            "kind": "value",
            "run_info_statuses": ["PipelineRunCompleted"],
            "already_completed": result.already_completed,
            "failure_total": result.failures.total(),
        }),
        Err(e) => json!({
            "kind": "exception",
            "type": error_variant(&e),
            "message": e.to_string().chars().take(200).collect::<String>(),
        }),
    };

    cell.observe(scenario, config_name, caller).await
}

fn error_variant(e: &CognifyError) -> &'static str {
    match e {
        CognifyError::RunFailed { .. } => "RunFailed",
        CognifyError::ChunkingError(_) => "ChunkingError",
        CognifyError::Execute(_) => "Execute",
        _ => "Other",
    }
}

/// Emit the whole matrix as JSON lines. Not an assertion: the comparison is
/// `compare.py`'s job, and a probe that asserted its own expectations would be
/// asserting the very thing under test.
#[tokio::test(flavor = "multi_thread")]
async fn emit_failure_parity_observations() {
    let mut lines = Vec::new();
    for scenario in [
        "clean",
        "unreadable_file",
        "extraction_failure",
        "summarization_failure",
        "second_run_after_success",
    ] {
        for config in ["raise_true", "raise_false"] {
            let obs = run_cell(scenario, config).await;
            println!("@@OBS@@{obs}");
            lines.push(obs.to_string());
        }
    }
    // Written only when a path is named. Defaulting to a relative one dropped
    // `observations_rust.jsonl` into the crate root on every `cargo test
    // --workspace`, which is not this test's business: `run_rust.sh` passes
    // `COGNEE_FAILURE_PARITY_OUT`, and without it the observations are still on
    // stdout, one `@@OBS@@` line each.
    match std::env::var("COGNEE_FAILURE_PARITY_OUT") {
        Ok(out) => {
            std::fs::write(&out, lines.join("\n") + "\n").expect("write the observation file")
        }
        Err(_) => println!(
            "COGNEE_FAILURE_PARITY_OUT unset — {} observations left on stdout only",
            lines.len()
        ),
    }
}
