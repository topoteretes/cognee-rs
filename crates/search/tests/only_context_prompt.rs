#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `only_context` returns the prompt the completion would have sent
//! (SDK-1036, retrieval-27).
//!
//! Python's `build_only_context_prompt` (`only_context_prompt.py`) hands an
//! `only_context` caller the `(user_prompt, system_prompt)` pair a completion
//! sends — built by the same code, with the session read as it stands — and
//! `SearchResultPayload.result` is that user prompt. These tests drive the real
//! RAG retriever through the orchestrator and compare the preview against the
//! messages a real completion actually sends.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cognee_embedding::EmbeddingResult;
use cognee_embedding::engine::EmbeddingEngine;
use cognee_llm::{GenerationOptions, GenerationResponse, Llm, LlmError, LlmResult, Message};
use cognee_session::{FsSessionStore, SessionManager, SessionStore};
use cognee_vector::{BruteForceVectorDB, VectorDB, VectorPoint};
use serde_json::json;
use uuid::Uuid;

use cognee_search::retrievers::{ChunksRetriever, CompletionRetriever};
use cognee_search::types::{SearchOutput, SearchRequest, SearchResponse, SearchType};
use cognee_search::{SearchOrchestrator, SearchTypeRegistry};

struct FixedEmbedding;

#[async_trait]
impl EmbeddingEngine for FixedEmbedding {
    async fn embed(&self, texts: &[&str]) -> EmbeddingResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }

    fn dimension(&self) -> usize {
        2
    }

    fn batch_size(&self) -> usize {
        16
    }

    fn max_sequence_length(&self) -> usize {
        512
    }
}

/// Records every message list it is sent.
#[derive(Default)]
struct RecordingLlm {
    calls: Mutex<Vec<Vec<Message>>>,
}

#[async_trait]
impl Llm for RecordingLlm {
    async fn generate(
        &self,
        messages: Vec<Message>,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<GenerationResponse> {
        self.calls.lock().unwrap().push(messages);
        Ok(GenerationResponse {
            content: "the answer".to_string(),
            model: "recording".to_string(),
            usage: None,
            finish_reason: Some("stop".to_string()),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        _messages: Vec<Message>,
        _json_schema: &serde_json::Value,
        _options: Option<GenerationOptions>,
    ) -> LlmResult<serde_json::Value> {
        Err(LlmError::ConfigError("not used".to_string()))
    }

    fn model(&self) -> &str {
        "recording"
    }
}

struct Fixture {
    orchestrator: SearchOrchestrator,
    llm: Arc<RecordingLlm>,
    session_store: Arc<FsSessionStore>,
    _dir: tempfile::TempDir,
}

/// A RAG + CHUNKS orchestrator over `chunks` (an empty slice leaves the
/// collection existing but empty), with a filesystem session store.
async fn fixture(chunks: &[&str]) -> Fixture {
    let vector_db = BruteForceVectorDB::new();
    vector_db
        .create_collection("DocumentChunk", "text", 2)
        .await
        .unwrap();
    let points: Vec<VectorPoint> = chunks
        .iter()
        .map(|text| VectorPoint {
            id: Uuid::new_v4(),
            vector: vec![1.0, 0.0],
            metadata: HashMap::from([("text".to_string(), json!(text))]),
        })
        .collect();
    vector_db
        .index_points("DocumentChunk", "text", &points)
        .await
        .unwrap();
    let vector_db: Arc<dyn VectorDB> = Arc::new(vector_db);

    let llm = Arc::new(RecordingLlm::default());
    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::new(CompletionRetriever::new(
        Arc::clone(&vector_db),
        Arc::new(FixedEmbedding),
        Arc::clone(&llm) as Arc<dyn Llm>,
        Some(5),
        None,
        None,
        None,
        None,
    )));
    registry.register(Arc::new(ChunksRetriever::new(
        vector_db,
        Arc::new(FixedEmbedding),
        Some(5),
    )));

    let dir = tempfile::tempdir().unwrap();
    let session_store = Arc::new(FsSessionStore::new(dir.path().join("sessions")));
    let orchestrator = SearchOrchestrator::new(registry)
        .with_session_manager(Arc::new(SessionManager::new(session_store.clone())));

    Fixture {
        orchestrator,
        llm,
        session_store,
        _dir: dir,
    }
}

fn request(search_type: SearchType, only_context: bool, session_id: Option<&str>) -> SearchRequest {
    let mut request: SearchRequest =
        serde_json::from_value(json!({ "query_text": "What does Alice do?" })).unwrap();
    request.search_type = search_type;
    request.only_context = Some(only_context);
    request.session_id = session_id.map(String::from);
    request
}

/// The (user, system) prompt pair an `only_context` response carries.
fn prompts(response: &SearchResponse) -> (String, String) {
    (
        response.user_prompt.clone().expect("user prompt is built"),
        response
            .system_prompt
            .clone()
            .expect("system prompt is built"),
    )
}

#[tokio::test]
async fn only_context_returns_the_prompt_the_completion_sends() {
    let f = fixture(&["Alice is an engineer at Acme."]).await;

    let preview = f
        .orchestrator
        .search(&request(SearchType::RagCompletion, true, None))
        .await
        .unwrap();
    assert!(
        f.llm.calls.lock().unwrap().is_empty(),
        "only_context must not call the LLM"
    );

    let (user_prompt, system_prompt) = prompts(&preview);
    // Python's `SearchResultPayload.result`: the user prompt.
    assert!(
        matches!(preview.result, SearchOutput::Text(ref text) if *text == user_prompt),
        "result must be the user prompt, got {:?}",
        preview.result
    );
    assert!(user_prompt.contains("What does Alice do?"));
    assert!(user_prompt.contains("Alice is an engineer at Acme."));
    // The context still travels alongside.
    assert!(preview.context.is_some());

    // Byte-identical to what a real completion sends.
    f.orchestrator
        .search(&request(SearchType::RagCompletion, false, None))
        .await
        .unwrap();
    let sent = f.llm.calls.lock().unwrap()[0].clone();
    assert_eq!(sent[0].content, system_prompt);
    assert_eq!(sent[1].content, user_prompt);
}

/// With a session, the preview carries the prior turns exactly where the real
/// completion puts them.
#[tokio::test]
async fn only_context_includes_the_session_history() {
    let f = fixture(&["Alice is an engineer at Acme."]).await;
    let session_id = "only-context-session";
    f.session_store
        .create_qa_entry(
            session_id,
            None,
            "Who is Bob?",
            "Bob is Alice's manager.",
            None,
        )
        .await
        .unwrap();

    let preview = f
        .orchestrator
        .search(&request(SearchType::RagCompletion, true, Some(session_id)))
        .await
        .unwrap();
    let (user_prompt, system_prompt) = prompts(&preview);
    assert!(
        format!("{system_prompt}{user_prompt}").contains("Who is Bob?"),
        "the prior turn must be in the prompt: {system_prompt:?} / {user_prompt:?}"
    );

    f.orchestrator
        .search(&request(SearchType::RagCompletion, false, Some(session_id)))
        .await
        .unwrap();
    let sent = f.llm.calls.lock().unwrap()[0].clone();
    assert_eq!(sent[0].content, system_prompt);
    assert_eq!(sent[1].content, user_prompt);
}

/// Nothing retrieved: no prompt is wrapped around nothing, so the miss stays
/// detectable — the bare (empty) context comes back.
#[tokio::test]
async fn only_context_with_no_context_builds_no_prompt() {
    let f = fixture(&[]).await;

    let response = f
        .orchestrator
        .search(&request(SearchType::RagCompletion, true, None))
        .await
        .unwrap();
    assert!(matches!(response.result, SearchOutput::Items(ref items) if items.is_empty()));
    assert!(response.user_prompt.is_none());
    assert!(response.system_prompt.is_none());
}

/// A non-generative retriever sends no prompt: `only_context` returns its bare
/// context, as before.
#[tokio::test]
async fn only_context_for_chunks_returns_the_bare_context() {
    let f = fixture(&["Alice is an engineer at Acme."]).await;

    let response = f
        .orchestrator
        .search(&request(SearchType::Chunks, true, None))
        .await
        .unwrap();
    assert!(matches!(response.result, SearchOutput::Items(ref items) if items.len() == 1));
    assert!(response.user_prompt.is_none());
    assert!(response.system_prompt.is_none());
}
