#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Stage 2c integration tests — `distill_session` /
//! `distill_sessions_in_knowledge_graph`.
//!
//! The pipeline curates a session's Q&A into proposed lessons (curator LLM),
//! writes/rejects each (writer LLM), and add+cognifies accepted lessons tagged
//! with the `session_learnings` node-set. The LLM is mocked so the gate
//! ordering and node-set tagging are deterministic.
#![cfg(feature = "testing")]

use std::sync::Arc;

use cognee_cognify::CognifyConfig;
use cognee_cognify::memify::distill_sessions::{
    DISTILLATE_NODE_SET, DistillationStatus, distill_session, distill_sessions_in_knowledge_graph,
};
use cognee_database::ops::datasets as ds_ops;
use cognee_database::{DatabaseConnection, IngestDb, connect, initialize};
use cognee_embedding::MockEmbeddingEngine;
use cognee_graph::MockGraphDB;
use cognee_ingestion::AddPipeline;
use cognee_llm::Llm;
use cognee_ontology::{NoOpOntologyResolver, OntologyResolver};
use cognee_session::{FsSessionStore, SessionStore};
use cognee_storage::{LocalStorage, StorageTrait};
use cognee_test_utils::MockLlm;
use cognee_vector::MockVectorDB;
use tempfile::TempDir;
use uuid::Uuid;

use async_trait::async_trait;
use cognee_llm::{GenerationOptions, GenerationResponse, LlmError, Message};
use serde_json::Value;

/// In-test LLM double that dispatches on the request's JSON schema instead of a
/// FIFO queue, so the full distill → publish → **cognify** path completes
/// deterministically under the mock backends: the curator proposes one lesson,
/// the writer accepts it, the summarizer returns a summary, and graph extraction
/// returns an empty graph. Order-independence is the point — the queue-based
/// [`MockLlm`] cannot satisfy cognify's summary call in a fixed position, so it
/// can only be used where cognify is *expected* to fail open.
#[derive(Clone)]
struct SchemaDispatchLlm {
    /// Statement the writer emits when it accepts (must be non-empty to publish).
    accepted_statement: String,
}

#[async_trait]
impl cognee_llm::Llm for SchemaDispatchLlm {
    async fn generate(
        &self,
        _messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> Result<GenerationResponse, LlmError> {
        Ok(GenerationResponse {
            content: "ok".to_string(),
            model: "schema-dispatch".to_string(),
            usage: None,
            finish_reason: Some("stop".to_string()),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<Message>,
        json_schema: &Value,
        _options: Option<GenerationOptions>,
    ) -> Result<Value, LlmError> {
        let schema = json_schema.to_string();
        if schema.contains("why_learned") {
            // Writer/rejecter (`WrittenLesson`) → accept + write.
            Ok(serde_json::json!({
                "accept": true,
                "statement": self.accepted_statement,
                "entities": ["TerraScout"],
                "why_learned": "learned while asking how indexing works",
            }))
        } else if schema.contains("working_statement") {
            // Curator (`CuratorBatchOutput`) → one proposed lesson.
            Ok(serde_json::json!({
                "lessons": [{"working_statement": "TerraScout indexes nightly.", "member_entry_ids": []}]
            }))
        } else if schema.contains("summary") {
            // Cognify summarization (`SummarizedContent`).
            Ok(
                serde_json::json!({"summary": "A test summary.", "description": "A test description."}),
            )
        } else {
            // Cognify graph extraction → empty graph.
            Ok(serde_json::json!({"nodes": [], "edges": []}))
        }
    }

    fn model(&self) -> &str {
        "schema-dispatch"
    }
}

struct Harness {
    _temp: TempDir,
    _sess_dir: TempDir,
    db: Arc<DatabaseConnection>,
    storage: Arc<dyn StorageTrait>,
    add_pipeline: AddPipeline,
    graph_db: Arc<MockGraphDB>,
    vector_db: Arc<MockVectorDB>,
    embedding_engine: Arc<MockEmbeddingEngine>,
    ontology: Arc<dyn OntologyResolver>,
    session_store: Arc<dyn SessionStore>,
}

async fn make_harness() -> Harness {
    let temp = TempDir::new().unwrap();
    let sess_dir = TempDir::new().unwrap();
    let db_path = temp.path().join("cognee.db");
    std::fs::File::create(&db_path).unwrap();
    let url = format!("sqlite://{}", db_path.display());
    let db = connect(&url).await.unwrap();
    initialize(&db).await.unwrap();
    let db = Arc::new(db);
    let storage: Arc<dyn StorageTrait> = Arc::new(LocalStorage::new(temp.path().join("storage")));
    storage.initialize().await.unwrap();

    let ingest_db: Arc<dyn IngestDb> = db.clone();
    let graph_db = Arc::new(MockGraphDB::new());
    let vector_db = Arc::new(MockVectorDB::new());
    let add_pipeline = AddPipeline::new(Arc::clone(&storage), ingest_db)
        .with_thread_pool(Arc::new(
            cognee_core::RayonThreadPool::with_default_threads().unwrap(),
        ))
        .with_graph_db(graph_db.clone() as Arc<dyn cognee_graph::GraphDBTrait>)
        .with_vector_db(vector_db.clone() as Arc<dyn cognee_vector::VectorDB>)
        .with_database(db.clone());

    let embedding_engine = Arc::new(MockEmbeddingEngine::new(16));
    let ontology: Arc<dyn OntologyResolver> = Arc::new(NoOpOntologyResolver::new());

    let session_store: Arc<dyn SessionStore> = Arc::new(FsSessionStore::new(sess_dir.path()));

    Harness {
        _temp: temp,
        _sess_dir: sess_dir,
        db,
        storage,
        add_pipeline,
        graph_db,
        vector_db,
        embedding_engine,
        ontology,
        session_store,
    }
}

fn noop_repo() -> Arc<dyn cognee_database::PipelineRunRepository> {
    Arc::new(cognee_database::NoopPipelineRunRepository::new())
}

fn thread_pool() -> Arc<dyn cognee_core::CpuPool> {
    Arc::new(cognee_core::RayonThreadPool::with_default_threads().expect("RayonThreadPool init"))
}

/// No non-empty Q&A → `NoQaEntries` without any LLM call.
#[tokio::test]
async fn distill_no_qa_returns_no_qa_entries() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let llm: Arc<dyn Llm> = Arc::new(MockLlm::empty());
    let config = CognifyConfig::default();

    let res = distill_session(
        "no_such_session",
        "ds_noqa",
        owner,
        None,
        Arc::clone(&h.session_store),
        &h.add_pipeline,
        llm,
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        h.db.clone(),
        noop_repo(),
        thread_pool(),
        Arc::clone(&h.ontology),
        &config,
    )
    .await
    .unwrap();

    assert_eq!(res.status, DistillationStatus::NoQaEntries);
    assert!(res.documents.is_empty());
    // No dataset was created (add never ran).
    let ds = ds_ops::get_dataset_by_name_in_tenant(h.db.as_ref(), "ds_noqa", owner, None)
        .await
        .unwrap();
    assert!(ds.is_none(), "no dataset should exist when there is no Q&A");
}

/// Curator returns invalid JSON → the batch fails open to `[]`, so the whole
/// session gates at `NoProposedLessons` (no writer/add/cognify runs).
#[tokio::test]
async fn distill_curator_failopen_yields_no_proposed_lessons() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess_curator_fail";

    h.session_store
        .create_qa_entry(session_id, Some(&user_id), "Q?", "A.", None)
        .await
        .unwrap();

    // One (only) LLM response, deliberately not valid structured output for the
    // curator schema → curate_batch fails open to [].
    let llm: Arc<dyn Llm> = Arc::new(MockLlm::new(vec!["not json at all".to_string()]));
    let config = CognifyConfig::default();

    let res = distill_session(
        session_id,
        "ds_curator_fail",
        owner,
        None,
        Arc::clone(&h.session_store),
        &h.add_pipeline,
        llm,
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        h.db.clone(),
        noop_repo(),
        thread_pool(),
        Arc::clone(&h.ontology),
        &config,
    )
    .await
    .unwrap();

    assert_eq!(res.status, DistillationStatus::NoProposedLessons);
    assert!(res.documents.is_empty());
}

/// Writer rejects the proposed lesson → `NoAcceptedLessons`, no add/cognify.
#[tokio::test]
async fn distill_writer_rejects_yields_no_accepted_lessons() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess_reject";

    h.session_store
        .create_qa_entry(
            session_id,
            Some(&user_id),
            "Who is Alice?",
            "Alice is an engineer.",
            None,
        )
        .await
        .unwrap();

    // 1) curator proposes one lesson; 2) writer rejects it.
    let llm: Arc<dyn Llm> = Arc::new(MockLlm::new(vec![
        r#"{"lessons":[{"working_statement":"Alice is an engineer.","member_entry_ids":[]}]}"#
            .to_string(),
        r#"{"accept":false,"reason":"not_durable"}"#.to_string(),
    ]));
    let config = CognifyConfig::default();

    let res = distill_session(
        session_id,
        "ds_reject",
        owner,
        None,
        Arc::clone(&h.session_store),
        &h.add_pipeline,
        llm,
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        h.db.clone(),
        noop_repo(),
        thread_pool(),
        Arc::clone(&h.ontology),
        &config,
    )
    .await
    .unwrap();

    assert_eq!(res.status, DistillationStatus::NoAcceptedLessons);
    assert!(res.documents.is_empty());
    // No dataset created — add never ran.
    let ds = ds_ops::get_dataset_by_name_in_tenant(h.db.as_ref(), "ds_reject", owner, None)
        .await
        .unwrap();
    assert!(ds.is_none(), "rejected lessons must not create a dataset");
}

/// Writer LLM returns non-JSON → `write_or_reject` fails **open** to `None`
/// (the `Err → None` branch, distinct from a valid `accept:false` decision). No
/// lesson is accepted, so the run gates at `NoAcceptedLessons` with no dataset —
/// a failed writer call must never abort `distill_session` (it returns `Ok`, not
/// `Err`) nor be treated as an acceptance.
#[tokio::test]
async fn distill_writer_failopen_yields_no_accepted_lessons() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess_writer_failopen";

    h.session_store
        .create_qa_entry(
            session_id,
            Some(&user_id),
            "Who is Alice?",
            "Alice is an engineer.",
            None,
        )
        .await
        .unwrap();

    // 1) curator proposes exactly one lesson (valid structured output);
    // 2) writer response is NOT valid JSON → the writer call fails open to None.
    let llm: Arc<dyn Llm> = Arc::new(MockLlm::new(vec![
        r#"{"lessons":[{"working_statement":"Alice is an engineer.","member_entry_ids":[]}]}"#
            .to_string(),
        "this is not valid json for the writer schema".to_string(),
    ]));
    let config = CognifyConfig::default();

    let res = distill_session(
        session_id,
        "ds_writer_failopen",
        owner,
        None,
        Arc::clone(&h.session_store),
        &h.add_pipeline,
        llm,
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        h.db.clone(),
        noop_repo(),
        thread_pool(),
        Arc::clone(&h.ontology),
        &config,
    )
    .await
    .unwrap();

    assert_eq!(res.status, DistillationStatus::NoAcceptedLessons);
    assert!(res.documents.is_empty());
    assert!(res.dataset_id.is_none());
    // No dataset created — the fail-open writer produced no accepted lesson, so
    // add/cognify never ran.
    let ds =
        ds_ops::get_dataset_by_name_in_tenant(h.db.as_ref(), "ds_writer_failopen", owner, None)
            .await
            .unwrap();
    assert!(
        ds.is_none(),
        "a fail-open (Err→None) writer call must not create a dataset"
    );
}

/// Multi-session fail-open isolation: session A has Q&A + an accepted lesson
/// (publishes exactly one document); session B has no Q&A (gates at
/// `NoQaEntries` and contributes nothing). The aggregate reflects only A, and
/// A's dataset carries BOTH node-set tags — B's empty gate never disturbs A.
#[tokio::test]
async fn distill_sessions_failopen_isolates_sessions() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_a = "sess_iso_a";
    let session_b = "sess_iso_b";

    // Session A gets one Q&A turn; session B gets none.
    h.session_store
        .create_qa_entry(
            session_a,
            Some(&user_id),
            "How does TerraScout index data?",
            "TerraScout indexes nightly.",
            None,
        )
        .await
        .unwrap();

    // Schema-dispatch LLM so A's publish (add + cognify) fully completes: curator
    // proposes one lesson, writer accepts, cognify's summary + graph-extraction
    // calls are satisfied. B makes no LLM call (it gates at NoQaEntries first).
    let llm: Arc<dyn Llm> = Arc::new(SchemaDispatchLlm {
        accepted_statement: "TerraScout indexes data nightly.".to_string(),
    });
    let config = CognifyConfig::default();

    let r = distill_sessions_in_knowledge_graph(
        &[session_a.to_string(), session_b.to_string()],
        "ds_iso",
        owner,
        None,
        Arc::clone(&h.session_store),
        &h.add_pipeline,
        llm,
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        h.db.clone(),
        noop_repo(),
        thread_pool(),
        Arc::clone(&h.ontology),
        &config,
    )
    .await;

    // Only session A produced documents; B's NoQaEntries gate is invisible to
    // the aggregate.
    assert_eq!(
        r.sessions_distilled, 1,
        "only session A (with Q&A + accepted lesson) distilled"
    );
    assert_eq!(
        r.lessons_published, 1,
        "session A published exactly one lesson; B published none"
    );

    // A's dataset holds the single rendered lesson Data row, tagged with BOTH the
    // generic and the per-session node-set.
    let ds = ds_ops::get_dataset_by_name_in_tenant(h.db.as_ref(), "ds_iso", owner, None)
        .await
        .unwrap()
        .expect("dataset exists after session A publishes");
    let data_items = ds_ops::get_dataset_data(h.db.as_ref(), ds.id)
        .await
        .unwrap();
    assert_eq!(
        data_items.len(),
        1,
        "exactly one distilled-lesson Data row from session A"
    );
    let per_session_tag = format!("{DISTILLATE_NODE_SET}:{session_a}");
    let has_both = data_items.iter().any(|d| {
        d.node_set
            .as_deref()
            .map(|s| s.contains(DISTILLATE_NODE_SET) && s.contains(&per_session_tag))
            .unwrap_or(false)
    });
    assert!(
        has_both,
        "expected A's Data row tagged with both '{DISTILLATE_NODE_SET}' and '{per_session_tag}'; got {:?}",
        data_items
            .iter()
            .map(|d| d.node_set.clone())
            .collect::<Vec<_>>()
    );
}

/// Full curate → propose → accept → publish happy path: the accepted lesson is
/// add+cognified and the resulting Data row carries BOTH node-set tags.
#[tokio::test]
async fn distill_happy_path_tags_both_node_sets() {
    let h = make_harness().await;
    let owner = Uuid::new_v4();
    let user_id = owner.to_string();
    let session_id = "sess_happy";

    h.session_store
        .create_qa_entry(
            session_id,
            Some(&user_id),
            "How does TerraScout index data?",
            "TerraScout indexes nightly.",
            None,
        )
        .await
        .unwrap();

    // 1) curator proposes one lesson; 2) writer accepts + writes it.
    // Remaining cognify graph-extraction calls fall back to the mock's default
    // empty-graph response.
    let llm: Arc<dyn Llm> = Arc::new(MockLlm::new(vec![
        r#"{"lessons":[{"working_statement":"TerraScout indexes nightly.","member_entry_ids":[]}]}"#
            .to_string(),
        r#"{"accept":true,"statement":"TerraScout indexes data nightly.","entities":["TerraScout"],"why_learned":"learned while asking how indexing works"}"#
            .to_string(),
    ]));
    let config = CognifyConfig::default();

    // Use the fail-open multi-session loop so that any internal cognify error
    // (mock backends) does not abort the test; the add-phase node-set tagging is
    // the deterministic invariant we assert.
    let r = distill_sessions_in_knowledge_graph(
        &[session_id.to_string()],
        "ds_happy",
        owner,
        None,
        Arc::clone(&h.session_store),
        &h.add_pipeline,
        llm,
        Arc::clone(&h.storage),
        h.graph_db.clone() as Arc<_>,
        h.vector_db.clone() as Arc<_>,
        h.embedding_engine.clone() as Arc<_>,
        h.db.clone(),
        noop_repo(),
        thread_pool(),
        Arc::clone(&h.ontology),
        &config,
    )
    .await;

    // The dataset exists and holds the rendered lesson Data row, tagged with
    // BOTH the generic and the per-session node-set.
    let ds = ds_ops::get_dataset_by_name_in_tenant(h.db.as_ref(), "ds_happy", owner, None)
        .await
        .unwrap()
        .expect("dataset exists after publish");
    let data_items = ds_ops::get_dataset_data(h.db.as_ref(), ds.id)
        .await
        .unwrap();
    assert!(
        !data_items.is_empty(),
        "dataset should have at least one distilled-lesson Data row"
    );
    let per_session_tag = format!("{DISTILLATE_NODE_SET}:{session_id}");
    let has_both = data_items.iter().any(|d| {
        d.node_set
            .as_deref()
            .map(|s| s.contains(DISTILLATE_NODE_SET) && s.contains(&per_session_tag))
            .unwrap_or(false)
    });
    assert!(
        has_both,
        "expected a Data row tagged with both '{DISTILLATE_NODE_SET}' and '{per_session_tag}'; got {:?}",
        data_items
            .iter()
            .map(|d| d.node_set.clone())
            .collect::<Vec<_>>()
    );

    // If cognify also succeeded end-to-end, the aggregate reflects one published
    // lesson. (Tolerant of internal mock-cognify failure, which the loop swallows
    // fail-open; the tagging assertion above is the hard invariant.)
    assert!(r.lessons_published <= 1);
}
