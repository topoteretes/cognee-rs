#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test support module — every consumer uses a different subset, and panics are acceptable failures"
)]
//! Shared harness for the run-orchestration tests (`rollback_axes`,
//! `completion_markers`).
//!
//! Fully offline: `MockLlm` / `MockStorage` / `MockGraphDB` / `MockVectorDB` /
//! `MockEmbeddingEngine` and a real `SeaOrmPipelineRunRepository` over
//! in-memory SQLite. No network, no LLM key, no skip path.
//!
//! One thing it adds over `failures_are_data.rs`'s harness: the data items are
//! inserted as real `Data` rows and attached to the dataset, because completion
//! markers live on those rows.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use cognee_cognify::{CognifyConfig, CognifyError, CognifyResult, cognify};
use cognee_database::ops::data::{create_data, get_cognify_completed_data_ids};
use cognee_database::ops::datasets::{attach_data_to_dataset, create_dataset};
use cognee_database::ops::graph_storage::{get_edges_by_dataset, get_nodes_by_dataset};
use cognee_database::{
    DatabaseConnection, GraphEdge, GraphNode, PipelineRunRepository, PipelineRunStatus,
    SeaOrmPipelineRunRepository, connect, initialize,
};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::GraphDBTrait;
use cognee_models::{Data, Dataset};
use cognee_ontology::NoOpOntologyResolver;
use cognee_storage::{MockStorage, StorageTrait};
use cognee_test_utils::{MockGraphDB, MockLlm, MockVectorDB};
use cognee_vector::VectorDB;
use serde_json::json;
use uuid::Uuid;

/// The marker `MockLlm::with_failing_markers` keys on. A file whose text
/// carries it fails deterministically, whatever order the chunks are
/// dispatched in.
pub const FAIL_MARKER: &str = "FAILMARKER";

/// Every file produces the same two entities and the same edge, on purpose:
/// that is what makes an artifact *shared* between a failed file and a
/// surviving one, which is the case an item-scoped sweep has to get right.
pub fn canned_graph_response() -> String {
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

/// One chunk per file and one chunk per batch, so the abort boundary falls
/// exactly between files.
///
/// Summarization is off: both stages read the same chunk text, so leaving it on
/// would double every count and say nothing extra.
pub fn extraction_config() -> CognifyConfig {
    CognifyConfig::default()
        .with_chunk_size(1500)
        .with_chunks_per_batch(1)
        .with_summarization(false)
}

/// An LLM that answers graph extraction from the canned response, fails on
/// [`FAIL_MARKER`], and counts how many extraction calls it saw.
pub struct CountingLlm {
    inner: MockLlm,
    pub calls: AtomicUsize,
}

impl CountingLlm {
    pub fn new(response_count: usize) -> Self {
        Self {
            inner: MockLlm::new(vec![canned_graph_response(); response_count])
                .with_failing_markers(vec![FAIL_MARKER.to_string()]),
            calls: AtomicUsize::new(0),
        }
    }

    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl cognee_llm::Llm for CountingLlm {
    async fn generate(
        &self,
        messages: Vec<cognee_llm::Message>,
        options: Option<cognee_llm::GenerationOptions>,
    ) -> cognee_llm::LlmResult<cognee_llm::GenerationResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.generate(messages, options).await
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<cognee_llm::Message>,
        json_schema: &serde_json::Value,
        options: Option<cognee_llm::GenerationOptions>,
    ) -> cognee_llm::LlmResult<serde_json::Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner
            .create_structured_output_with_messages_raw(messages, json_schema, options)
            .await
    }

    fn model(&self) -> &str {
        "mock-counting"
    }
}

/// A dataset, its stores and its database — reused across several `cognify()`
/// calls so a test can assert what the *second* run does.
pub struct Harness {
    pub db: Arc<DatabaseConnection>,
    pub repo: Arc<dyn PipelineRunRepository>,
    pub storage: Arc<dyn StorageTrait>,
    pub graph_db: Arc<MockGraphDB>,
    pub vector_db: Arc<MockVectorDB>,
    pub dataset_id: Uuid,
    pub owner_id: Uuid,
    items: Vec<Data>,
}

impl Harness {
    pub async fn new() -> Self {
        let dataset_id = Uuid::new_v4();
        let owner_id = Uuid::new_v4();

        let conn = connect("sqlite::memory:").await.expect("connect sqlite");
        initialize(&conn).await.expect("initialize");
        create_dataset(
            &conn,
            Dataset::new("rollback".into(), owner_id, None, dataset_id),
        )
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
        }
    }

    /// Store `text` as a file, persist its `Data` row and attach it to the
    /// dataset. The row is what carries the completion marker.
    pub async fn add_file(&mut self, text: &str) -> Uuid {
        let data_id = Uuid::new_v4();
        let index = self.items.len();
        let location = self
            .storage
            .store(text.as_bytes(), &format!("rollback-{data_id}"))
            .await
            .expect("MockStorage::store");
        let item = Data::builder(
            data_id,
            format!("rollback-{index}.txt"),
            location,
            format!("rollback-{index}.txt"),
            "txt",
            "text/plain",
            format!("test-hash-{data_id}"),
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
        data_id
    }

    /// Run cognify over every file added so far.
    pub async fn run(&self, config: &CognifyConfig) -> Result<CognifyResult, CognifyError> {
        let llm: Arc<dyn cognee_llm::Llm> = Arc::new(
            MockLlm::new(vec![canned_graph_response(); self.items.len() * 2 + 4])
                .with_failing_markers(vec![FAIL_MARKER.to_string()]),
        );
        self.run_with_llm(config, llm, self.items.clone()).await
    }

    /// Run cognify over a subset of the files — the "second wave" shape.
    pub async fn run_over(
        &self,
        config: &CognifyConfig,
        data_ids: &[Uuid],
        llm: Arc<dyn cognee_llm::Llm>,
    ) -> Result<CognifyResult, CognifyError> {
        let items = self
            .items
            .iter()
            .filter(|item| data_ids.contains(&item.id))
            .cloned()
            .collect();
        self.run_with_llm(config, llm, items).await
    }

    pub async fn run_with_llm(
        &self,
        config: &CognifyConfig,
        llm: Arc<dyn cognee_llm::Llm>,
        items: Vec<Data>,
    ) -> Result<CognifyResult, CognifyError> {
        let thread_pool: Arc<dyn cognee_core::CpuPool> = Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool"),
        );
        cognify(
            items,
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

    /// The status the last `pipeline_runs` row for this dataset carries.
    pub async fn latest_status(&self) -> PipelineRunStatus {
        let runs = self
            .repo
            .list_recent(Some(self.dataset_id), 100)
            .await
            .expect("list pipeline runs");
        assert!(!runs.is_empty(), "the run must have left a status trail");
        runs[0].status.clone()
    }

    /// Whether `data_id` carries this dataset's cognify completion marker.
    pub async fn is_marked(&self, data_id: Uuid) -> bool {
        get_cognify_completed_data_ids(&self.db, self.dataset_id, &[data_id])
            .await
            .expect("read completion markers")
            .contains(&data_id)
    }

    /// The ownership rows the dataset holds for graph nodes.
    pub async fn ledger_nodes(&self) -> Vec<GraphNode> {
        get_nodes_by_dataset(&self.db, self.dataset_id)
            .await
            .expect("read the node ledger")
    }

    /// The ownership rows the dataset holds for graph edges.
    pub async fn ledger_edges(&self) -> Vec<GraphEdge> {
        get_edges_by_dataset(&self.db, self.dataset_id)
            .await
            .expect("read the edge ledger")
    }

    /// The ids of every node currently in the graph store.
    pub async fn graph_node_ids(&self) -> Vec<String> {
        let (nodes, _edges) = self
            .graph_db
            .get_graph_data()
            .await
            .expect("read the graph store");
        nodes.into_iter().map(|(id, _)| id).collect()
    }

    /// The ids of every node currently in the graph store, as a set.
    pub async fn graph_node_id_set(&self) -> std::collections::BTreeSet<String> {
        self.graph_node_ids().await.into_iter().collect()
    }

    /// The total number of points across every vector collection.
    pub async fn vector_point_count(&self) -> usize {
        let collections = self
            .vector_db
            .list_collections()
            .await
            .expect("list collections");
        let mut total = 0;
        for (data_type, field) in collections {
            total += self
                .vector_db
                .collection_size(&data_type, &field)
                .await
                .expect("collection size");
        }
        total
    }
}

/// An LLM that answers graph extraction from the canned response and fails only
/// the *summarization* calls whose chunk text carries [`FAIL_MARKER`] — so a
/// fixture can exercise the summarization policy without also tripping
/// extraction.
///
/// Dispatches on the schema the caller supplies: the summarization call carries
/// `SummarizedContent`'s schema (a `summary` property), extraction carries
/// `KnowledgeGraph`'s.
pub struct SummarizationFailingLlm;

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

        let content: String = messages.iter().map(|m| m.content.as_str()).collect();
        if content.contains(FAIL_MARKER) {
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

/// The narrowest LLM that satisfies the temporal pipeline: one event, one
/// entity attribute. Dispatches on the system prompt the way
/// `temporal_cognify.rs`'s fixture does.
pub struct TemporalFixtureLlm;

#[async_trait::async_trait]
impl cognee_llm::Llm for TemporalFixtureLlm {
    async fn generate(
        &self,
        _messages: Vec<cognee_llm::Message>,
        _options: Option<cognee_llm::GenerationOptions>,
    ) -> cognee_llm::LlmResult<cognee_llm::GenerationResponse> {
        unreachable!("the temporal pipeline only uses structured output")
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<cognee_llm::Message>,
        _json_schema: &serde_json::Value,
        _options: Option<cognee_llm::GenerationOptions>,
    ) -> cognee_llm::LlmResult<serde_json::Value> {
        let system_prompt = messages
            .iter()
            .find(|message| matches!(message.role, cognee_llm::MessageRole::System))
            .map(|message| message.content.as_str())
            .unwrap_or_default();

        if system_prompt.contains("extracting highly granular stream events") {
            return Ok(json!({
                "events": [{
                    "name": "Alice joins Acme",
                    "description": "Alice started at Acme in 2020.",
                    "time_from": { "year": 2020 },
                    "time_to": null,
                    "location": null
                }]
            }));
        }
        if system_prompt.contains("extracting highly granular entities from events") {
            return Ok(json!({
                "events": [{
                    "event_name": "Alice joins Acme",
                    "attributes": [
                        { "entity": "Alice", "entity_type": "person", "relationship": "subject" }
                    ]
                }]
            }));
        }
        Ok(json!({"summary": "Alice and Acme.", "description": "A description."}))
    }

    fn model(&self) -> &str {
        "temporal-fixture"
    }
}
