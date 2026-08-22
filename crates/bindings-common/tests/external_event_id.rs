#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use std::collections::{HashMap, HashSet};
use std::path::Path;

use cognee::cognify::tasks::{ExtractedChunks, extract_graph_from_data};
use cognee::cognify::{
    CognifyConfig, Edge, KnowledgeGraph, Node, expand_with_nodes_and_edges_for_external_events,
};
use cognee::config::Settings;
use cognee::database::ops;
use cognee::models::{Data, DataPoint, Dataset, Document, DocumentChunk};
use cognee::ontology::NoOpOntologyResolver;
use cognee_bindings_common::HandleState;
use cognee_bindings_common::ops::memory::{
    run_contains_external_event, run_remember, run_remember_entry,
};
use serde_json::json;

fn handle_under(dir: &Path) -> HandleState {
    HandleState::from_settings(Settings {
        llm_api_key: "sk-test".to_owned(),
        embedding_provider: "mock".to_owned(),
        graph_database_provider: "mock".to_owned(),
        vector_db_provider: "mock".to_owned(),
        data_root_directory: dir.join("data").to_string_lossy().into_owned(),
        system_root_directory: dir.join("sys").to_string_lossy().into_owned(),
        relational_db_url: format!("sqlite://{}?mode=rwc", dir.join("cognee.db").display()),
        ..Settings::default()
    })
}

fn qa(answer: &str) -> serde_json::Value {
    json!({
        "type": "qa",
        "question": "what changed?",
        "answer": answer,
        "context": "Task 6"
    })
}

struct ExternalEventPromptGuardLlm;

fn prompt_guard_graph() -> serde_json::Value {
    json!({
        "nodes": [
            {"id":"alice", "name":"Alice", "type":"Person", "description":"Engineer"},
            {"id":"apex", "name":"APEX", "type":"Project", "description":"Agent harness"}
        ],
        "edges": [{
            "source_node_id":"alice",
            "target_node_id":"apex",
            "relationship_name":"works_on",
            "description":"Alice works on APEX"
        }]
    })
}

fn assert_event_absent_from_prompt(messages: &[cognee::llm::types::Message]) {
    assert!(
        messages
            .iter()
            .all(|message| !message.content.contains("evt-prompt-guard")),
        "external event metadata must not enter LLM prompt text"
    );
}

#[async_trait::async_trait]
impl cognee::llm::Llm for ExternalEventPromptGuardLlm {
    async fn generate(
        &self,
        messages: Vec<cognee::llm::types::Message>,
        _options: Option<cognee::llm::types::GenerationOptions>,
    ) -> cognee::llm::LlmResult<cognee::llm::types::GenerationResponse> {
        assert_event_absent_from_prompt(&messages);
        Ok(cognee::llm::types::GenerationResponse {
            content: prompt_guard_graph().to_string(),
            model: "prompt-guard".to_string(),
            usage: None,
            finish_reason: Some("stop".to_string()),
        })
    }

    async fn create_structured_output_with_messages_raw(
        &self,
        messages: Vec<cognee::llm::types::Message>,
        _json_schema: &serde_json::Value,
        _options: Option<cognee::llm::types::GenerationOptions>,
    ) -> cognee::llm::LlmResult<serde_json::Value> {
        assert_event_absent_from_prompt(&messages);
        Ok(prompt_guard_graph())
    }

    fn model(&self) -> &str {
        "prompt-guard"
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn remember_entry_replay_is_exact_once_and_queryable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = handle_under(dir.path());
    let opts = json!({"externalEventId":"evt-entry-123"});

    let first = run_remember_entry(
        &state,
        qa("external event idempotency"),
        "agent_sessions",
        "session-entry",
        &opts,
    )
    .await
    .expect("first remember entry");
    let replay = run_remember_entry(
        &state,
        qa("external event idempotency"),
        "agent_sessions",
        "session-entry",
        &opts,
    )
    .await
    .expect("identical replay");

    assert_eq!(first["entry_id"], replay["entry_id"]);
    assert!(
        run_contains_external_event(
            &state,
            "agent_sessions",
            Some("session-entry"),
            "evt-entry-123",
        )
        .await
        .expect("contains event")
    );

    let owner = state.owner_id().await.expect("owner").to_string();
    let services = state.services().await.expect("services");
    let entries = services
        .session_store
        .get_all_qa_entries("session-entry", Some(&owner))
        .await
        .expect("entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].external_event_id.as_deref(),
        Some("evt-entry-123")
    );
    drop(services);

    let conflict = run_remember_entry(
        &state,
        qa("different content"),
        "agent_sessions",
        "session-entry",
        &opts,
    )
    .await
    .expect_err("same external event with different content must conflict");
    assert!(conflict.to_string().contains("external event conflict"));

    state.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn remember_session_replay_is_exact_once_while_legacy_calls_still_append() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = handle_under(dir.path());
    let input = json!({"type":"text", "text":"Charlie Mike"});
    let opts = json!({
        "sessionId":"session-remember",
        "externalEventId":"evt-remember-123"
    });

    run_remember(&state, input.clone(), "agent_sessions", &opts)
        .await
        .expect("first remember");
    run_remember(&state, input, "agent_sessions", &opts)
        .await
        .expect("replayed remember");

    assert!(
        run_contains_external_event(
            &state,
            "agent_sessions",
            Some("session-remember"),
            "evt-remember-123",
        )
        .await
        .expect("contains event")
    );

    let legacy_opts = json!({"sessionId":"session-legacy"});
    for _ in 0..2 {
        run_remember(
            &state,
            json!({"type":"text", "text":"same legacy content"}),
            "agent_sessions",
            &legacy_opts,
        )
        .await
        .expect("legacy remember");
    }

    let owner = state.owner_id().await.expect("owner").to_string();
    let services = state.services().await.expect("services");
    assert_eq!(
        services
            .session_store
            .get_all_qa_entries("session-remember", Some(&owner))
            .await
            .expect("idempotent entries")
            .len(),
        1
    );
    assert_eq!(
        services
            .session_store
            .get_all_qa_entries("session-legacy", Some(&owner))
            .await
            .expect("legacy entries")
            .len(),
        2
    );
    drop(services);

    state.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn contains_external_event_queries_dataset_membership_without_a_session() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = handle_under(dir.path());
    let owner_id = state.owner_id().await.expect("owner");
    let services = state.services().await.expect("services");
    let dataset_id = uuid::Uuid::new_v4();
    let data_id = uuid::Uuid::new_v4();

    ops::datasets::create_dataset(
        &services.database,
        Dataset::new("agent_sessions".into(), owner_id, None, dataset_id),
    )
    .await
    .expect("dataset");
    ops::data::create_data(
        &services.database,
        Data::builder(
            data_id,
            "dataset event",
            "file:///dataset-event.txt",
            "file:///dataset-event.txt",
            "txt",
            "text/plain",
            "dataset-event-hash",
            owner_id,
        )
        .build(),
    )
    .await
    .expect("data");
    ops::datasets::attach_data_to_dataset_with_external_event(
        &services.database,
        dataset_id,
        data_id,
        Some("evt-dataset-123"),
    )
    .await
    .expect("attach event");
    drop(services);

    assert!(
        run_contains_external_event(&state, "agent_sessions", None, "evt-dataset-123")
            .await
            .expect("contains dataset event")
    );
    assert!(
        !run_contains_external_event(&state, "agent_sessions", None, "evt-missing")
            .await
            .expect("missing dataset event")
    );

    state.close().await;
}

#[tokio::test]
async fn graph_expansion_stamps_external_event_metadata_without_an_llm() {
    let chunk_id = uuid::Uuid::new_v4();
    let graph = KnowledgeGraph {
        nodes: vec![
            Node {
                id: "alice".into(),
                name: "Alice".into(),
                node_type: "Person".into(),
                description: "Engineer".into(),
            },
            Node {
                id: "apex".into(),
                name: "APEX".into(),
                node_type: "Project".into(),
                description: "Agent harness".into(),
            },
        ],
        edges: vec![Edge {
            source_node_id: "alice".into(),
            target_node_id: "apex".into(),
            relationship_name: "works_on".into(),
            description: None,
        }],
    };
    let events = HashMap::from([(chunk_id, "evt-graph-123".to_string())]);

    let (nodes, edges) = expand_with_nodes_and_edges_for_external_events(
        vec![(chunk_id, graph)],
        uuid::Uuid::new_v4(),
        &HashMap::new(),
        &HashMap::new(),
        &events,
        &HashSet::new(),
        &NoOpOntologyResolver::new(),
        None,
        None,
    )
    .await;

    assert!(!nodes.is_empty());
    assert!(!edges.is_empty());
    for node in nodes {
        assert_eq!(
            node.entity.base.get_metadata("cognee_external_event_id"),
            Some(&json!("evt-graph-123"))
        );
        assert_eq!(
            node.entity_type
                .base
                .get_metadata("cognee_external_event_id"),
            Some(&json!("evt-graph-123"))
        );
    }
    for edge in edges {
        assert_eq!(
            edge.properties.get("cognee_external_event_id"),
            Some(&"evt-graph-123".to_string())
        );
    }
}

#[tokio::test]
async fn graph_pipeline_stamps_external_event_only_after_llm_extraction() {
    let document_id = uuid::Uuid::new_v4();
    let chunk_id = uuid::Uuid::new_v4();
    let mut document_base = DataPoint::new("TextDocument", None);
    document_base.id = document_id;
    let document = Document {
        base: document_base,
        document_type: "text".to_string(),
        name: "test.txt".to_string(),
        raw_data_location: "file:///tmp/test.txt".to_string(),
        mime_type: "text/plain".to_string(),
        extension: "txt".to_string(),
        data_id: document_id,
        external_metadata: Some(json!({"cognee_external_event_id":"evt-prompt-guard"}).to_string()),
    };
    let chunk = DocumentChunk::new(
        chunk_id,
        "Alice works on APEX".to_string(),
        4,
        0,
        "paragraph_end".to_string(),
        document_id,
    );
    let input = ExtractedChunks {
        chunks: vec![chunk],
        documents: vec![document],
        dataset_id: uuid::Uuid::new_v4(),
        user_id: None,
        tenant_id: None,
    };

    let result = extract_graph_from_data(
        &input,
        std::sync::Arc::new(ExternalEventPromptGuardLlm),
        std::sync::Arc::new(cognee::graph::MockGraphDB::new()),
        std::sync::Arc::new(NoOpOntologyResolver::new()),
        &CognifyConfig::default(),
        None,
        None,
    )
    .await
    .expect("graph extraction");

    assert!(!result.entities.is_empty());
    assert!(!result.edges.is_empty());
    for node in result.entities {
        assert_eq!(
            node.entity.base.get_metadata("cognee_external_event_id"),
            Some(&json!("evt-prompt-guard"))
        );
        assert_eq!(
            node.entity_type
                .base
                .get_metadata("cognee_external_event_id"),
            Some(&json!("evt-prompt-guard"))
        );
    }
    for edge in result.edges {
        assert_eq!(
            edge.properties.get("cognee_external_event_id"),
            Some(&"evt-prompt-guard".to_string())
        );
    }
}
