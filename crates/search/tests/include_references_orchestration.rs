#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! `include_references` through the orchestrator: evidence collection per
//! dataset, the Evidence text block on string answers only, and the
//! flag-off path staying byte-identical (Python `get_retriever_output.py:
//! 114-125`, `test_get_retriever_output.py`, `test_include_references_wiring.py`).

use std::sync::Arc;

use async_trait::async_trait;
use cognee_llm::Message;
use cognee_session::{FsSessionStore, SessionContext, SessionManager};
use serde_json::json;
use uuid::Uuid;

use cognee_search::retrievers::SearchRetriever;
use cognee_search::types::{
    EvidenceKind, EvidenceReference, SearchContext, SearchError, SearchItem, SearchOutput,
    SearchParams, SearchRequest, SearchType,
};
use cognee_search::utils::evidence::{chunk_context_evidence, graph_context_evidence};
use cognee_search::utils::references::append_chunk_evidence;
use cognee_search::{SearchOrchestrator, SearchTypeRegistry};

const ANSWER: &str = "Revenue grew 12 percent.";

/// A RAG-shaped retriever over one chunk that shares terms with [`ANSWER`].
/// The chunk id names the dataset in scope, so a fan-out's runs differ.
struct ChunkRetriever;

fn scope() -> Option<Uuid> {
    cognee_models::dataset_scope::current_dataset()
}

#[async_trait]
impl SearchRetriever for ChunkRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::RagCompletion
    }

    fn dataset_scoping(&self) -> cognee_search::DatasetScoping {
        cognee_search::DatasetScoping::Membership
    }

    async fn get_context(
        &self,
        _query: &str,
        _params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        Ok(vec![SearchItem {
            id: Some(scope().unwrap_or_else(|| Uuid::from_u128(1))),
            score: Some(0.5),
            payload: json!({
                "document_name": "report.pdf",
                "chunk_index": 0,
                "text": "Revenue grew 12 percent year over year.",
            }),
        }])
    }

    async fn get_completion(
        &self,
        _query: &str,
        _context: Option<SearchContext>,
        _session: &SessionContext,
        params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        Ok(match &params.response_schema {
            Some(_) => SearchOutput::Structured(json!({"answer": ANSWER})),
            None => SearchOutput::Text(ANSWER.to_string()),
        })
    }

    fn context_evidence(
        &self,
        context: &SearchContext,
        dataset_id: Option<Uuid>,
    ) -> Vec<EvidenceReference> {
        chunk_context_evidence(context, dataset_id)
    }

    fn append_references(
        &self,
        output: SearchOutput,
        context: &SearchContext,
        params: &SearchParams,
    ) -> SearchOutput {
        if params.include_references && params.response_schema.is_none() {
            append_chunk_evidence(output, context)
        } else {
            output
        }
    }

    /// A prompt that quotes the chunk, so an Evidence block appended to it
    /// by mistake would ground and show up.
    async fn completion_messages(
        &self,
        query: &str,
        context: &SearchContext,
        _session: &SessionContext,
        _params: &SearchParams,
    ) -> Result<Option<Vec<Message>>, SearchError> {
        let text = context
            .iter()
            .filter_map(|item| item.payload["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        Ok(Some(vec![Message::user(format!("{query}\n{text}"))]))
    }
}

/// A graph-shaped retriever: reports evidence, never rewrites the answer.
struct TripletRetriever;

#[async_trait]
impl SearchRetriever for TripletRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::GraphCompletion
    }

    async fn get_context(
        &self,
        _query: &str,
        _params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        Ok(vec![SearchItem {
            id: None,
            score: Some(0.5),
            payload: json!({
                "source_id": "node-a", "target_id": "node-b",
                "source_name": "Alpha", "target_name": "Beta",
                "relationship": "relates_to",
            }),
        }])
    }

    async fn get_completion(
        &self,
        _query: &str,
        _context: Option<SearchContext>,
        _session: &SessionContext,
        _params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        Ok(SearchOutput::Text(ANSWER.to_string()))
    }

    fn context_evidence(
        &self,
        context: &SearchContext,
        dataset_id: Option<Uuid>,
    ) -> Vec<EvidenceReference> {
        graph_context_evidence(context, dataset_id)
    }
}

/// A retriever that opted into nothing: the trait defaults.
struct PlainRetriever;

#[async_trait]
impl SearchRetriever for PlainRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::RagCompletion
    }

    async fn get_context(
        &self,
        _query: &str,
        _params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        Ok(vec![SearchItem {
            id: Some(Uuid::from_u128(5)),
            score: None,
            payload: json!({"document_name": "d", "chunk_index": 0, "text": ANSWER}),
        }])
    }

    async fn get_completion(
        &self,
        _query: &str,
        _context: Option<SearchContext>,
        _session: &SessionContext,
        _params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        Ok(SearchOutput::Text(ANSWER.to_string()))
    }
}

fn orchestrator(retriever: impl SearchRetriever + 'static) -> SearchOrchestrator {
    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::new(retriever));
    SearchOrchestrator::new(registry)
}

fn request(search_type: SearchType, include_references: bool) -> SearchRequest {
    let mut request: SearchRequest = serde_json::from_value(json!({"query_text": "revenue?"}))
        .expect("a minimal request deserializes");
    request.search_type = search_type;
    request.include_references = include_references;
    request
}

fn text(output: &SearchOutput) -> &str {
    match output {
        SearchOutput::Text(text) => text,
        other => panic!("expected a text answer, got {other:?}"),
    }
}

#[tokio::test]
async fn flag_off_is_byte_identical_and_carries_no_evidence() {
    let response = orchestrator(ChunkRetriever)
        .search(&request(SearchType::RagCompletion, false))
        .await
        .unwrap();
    assert_eq!(text(&response.result), ANSWER);
    assert!(response.evidence.is_empty());
    // The wire slot is always there, empty.
    assert_eq!(
        serde_json::to_value(&response).unwrap()["evidence"],
        json!([])
    );
}

#[tokio::test]
async fn flag_on_attaches_evidence_and_appends_the_block_to_a_text_answer() {
    let response = orchestrator(ChunkRetriever)
        .search(&request(SearchType::RagCompletion, true))
        .await
        .unwrap();

    let answer = text(&response.result);
    assert!(
        answer.starts_with(&format!("{ANSWER}\n\nEvidence:\n")),
        "{answer}"
    );
    assert!(answer.contains("- chunk 1 of document report.pdf (chunk_id:"));

    assert_eq!(response.evidence.len(), 1);
    let reference = &response.evidence[0];
    assert_eq!(reference.kind, EvidenceKind::Segment);
    assert_eq!(reference.document_name.as_deref(), Some("report.pdf"));

    // snake_case on the wire, every field present.
    let wire = serde_json::to_value(&response).unwrap();
    let entry = &wire["evidence"][0];
    assert_eq!(entry["kind"], "segment");
    assert_eq!(entry["role"], "used_as_context");
    assert_eq!(entry["chunk_index"], 0);
    assert!(entry["source_ref_key"].is_null());
    assert!(entry.get("artifactId").is_none());
}

#[tokio::test]
async fn structured_answers_are_untouched_but_still_get_evidence() {
    let mut structured = request(SearchType::RagCompletion, true);
    structured.response_schema = Some(json!({"type": "object"}));
    let response = orchestrator(ChunkRetriever)
        .search(&structured)
        .await
        .unwrap();
    assert!(matches!(
        response.result,
        SearchOutput::Structured(ref value) if *value == json!({"answer": ANSWER})
    ));
    assert_eq!(response.evidence.len(), 1);
}

#[tokio::test]
async fn graph_retrievers_report_evidence_without_touching_the_text() {
    let response = orchestrator(TripletRetriever)
        .search(&request(SearchType::GraphCompletion, true))
        .await
        .unwrap();
    assert_eq!(text(&response.result), ANSWER);
    let kinds: Vec<_> = response.evidence.iter().map(|r| r.kind).collect();
    assert_eq!(
        kinds,
        [
            EvidenceKind::GraphNode,
            EvidenceKind::GraphNode,
            EvidenceKind::GraphEdge
        ]
    );
}

/// `test_get_retriever_output_treats_none_evidence_as_empty` and the default
/// hook: a retriever that reports nothing yields no evidence and an unchanged
/// answer even with the flag on.
#[tokio::test]
async fn a_retriever_without_the_hook_yields_no_evidence() {
    let response = orchestrator(PlainRetriever)
        .search(&request(SearchType::RagCompletion, true))
        .await
        .unwrap();
    assert_eq!(text(&response.result), ANSWER);
    assert!(response.evidence.is_empty());
}

#[tokio::test]
async fn only_context_collects_evidence_but_appends_no_text() {
    let mut preview = request(SearchType::RagCompletion, true);
    preview.only_context = Some(true);
    let response = orchestrator(ChunkRetriever).search(&preview).await.unwrap();
    assert!(response.only_context);
    assert_eq!(response.evidence.len(), 1);
    // No completion ran, so there is no answer to cite under.
    assert!(!matches!(&response.result, SearchOutput::Text(t) if t.contains("Evidence:")));
}

#[tokio::test]
async fn a_single_named_dataset_stamps_its_id_on_the_evidence() {
    let dataset = Uuid::from_u128(42);
    let mut scoped = request(SearchType::RagCompletion, true);
    scoped.dataset_ids = Some(vec![dataset]);
    let response = orchestrator(ChunkRetriever).search(&scoped).await.unwrap();
    assert_eq!(response.evidence[0].dataset_id, Some(dataset.to_string()));
}

#[tokio::test]
async fn per_dataset_fanout_collects_and_cites_each_datasets_own_chunks() {
    let (a, b) = (Uuid::from_u128(0xa), Uuid::from_u128(0xb));
    let orchestrator = orchestrator(ChunkRetriever).with_per_dataset_storage(true);
    let mut fanout = request(SearchType::RagCompletion, true);
    fanout.dataset_ids = Some(vec![a, b]);

    let results = orchestrator.search_datasets(&fanout).await.unwrap();
    assert_eq!(results.len(), 2);
    for (result, dataset) in results.iter().zip([a, b]) {
        let response = &result.response;
        assert_eq!(response.evidence.len(), 1);
        assert_eq!(response.evidence[0].artifact_id, dataset.to_string());
        assert_eq!(response.evidence[0].dataset_id, Some(dataset.to_string()));
        assert!(text(&response.result).contains(&format!("chunk_id: {dataset}")));
    }

    let merged = orchestrator.search(&fanout).await.unwrap();
    let ids: Vec<_> = merged
        .evidence
        .iter()
        .map(|r| r.artifact_id.clone())
        .collect();
    assert_eq!(ids, [a.to_string(), b.to_string()]);
}

/// An `only_context` fan-out returns each dataset's prompt: evidence is
/// collected, but no Evidence block is appended to a prompt (Python's
/// `only_context` run never calls `append_references`).
#[tokio::test]
async fn only_context_fanout_appends_no_text_to_the_prompts() {
    let (a, b) = (Uuid::from_u128(0xa), Uuid::from_u128(0xb));
    let orchestrator = orchestrator(ChunkRetriever).with_per_dataset_storage(true);
    let mut preview = request(SearchType::RagCompletion, true);
    preview.only_context = Some(true);
    preview.dataset_ids = Some(vec![a, b]);

    let results = orchestrator.search_datasets(&preview).await.unwrap();
    assert_eq!(results.len(), 2);
    for (result, dataset) in results.iter().zip([a, b]) {
        let response = &result.response;
        let prompt = text(&response.result);
        assert!(prompt.contains("Revenue grew 12 percent"), "{prompt}");
        assert!(!prompt.contains("Evidence:"), "{prompt}");
        assert_eq!(response.evidence.len(), 1);
        assert_eq!(response.evidence[0].dataset_id, Some(dataset.to_string()));
    }
}

/// Python appends references after the session-aware completion saved the
/// turn, so the stored answer is the bare text.
#[tokio::test]
async fn the_session_keeps_the_bare_answer() {
    let dir = tempfile::tempdir().unwrap();
    let manager = Arc::new(SessionManager::new(Arc::new(FsSessionStore::new(
        dir.path(),
    ))));
    let user = Uuid::from_u128(1);

    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::new(ChunkRetriever));
    let orchestrator = SearchOrchestrator::new(registry).with_session_manager(manager.clone());

    let mut turn = request(SearchType::RagCompletion, true);
    turn.session_id = Some("s".into());
    turn.user_id = Some(user);
    turn.auto_feedback_detection = Some(false);
    let response = orchestrator.search(&turn).await.unwrap();
    assert!(text(&response.result).contains("Evidence:"));

    let entries = manager
        .get_session(Some("s"), Some(&user.to_string()))
        .await
        .unwrap();
    assert_eq!(entries.last().unwrap().answer, ANSWER);
}

// ---------------------------------------------------------------------------
// Graph edges resolved to their source chunks (provenance_edge_evidence)
// ---------------------------------------------------------------------------

mod source_evidence {
    use super::*;
    use cognee_database::ops::data::create_data;
    use cognee_database::ops::datasets::create_dataset;
    use cognee_database::ops::edge_evidence::insert_edge_evidence;
    use cognee_database::{DatabaseConnection, EdgeEvidence, SearchHistoryDb, connect, initialize};
    use cognee_search::types::EvidenceRole;
    use cognee_utils::id_generation::generate_edge_object_id;

    /// A graph retriever whose single triplet carries a real edge identity.
    struct KnownEdgeRetriever {
        source: Uuid,
        target: Uuid,
    }

    #[async_trait]
    impl SearchRetriever for KnownEdgeRetriever {
        fn search_type(&self) -> SearchType {
            SearchType::GraphCompletion
        }

        async fn get_context(
            &self,
            _query: &str,
            _params: &SearchParams,
        ) -> Result<SearchContext, SearchError> {
            Ok(vec![SearchItem {
                id: None,
                score: Some(0.5),
                payload: json!({
                    "source_id": self.source.to_string(),
                    "target_id": self.target.to_string(),
                    "source_name": "Alice", "target_name": "Bob",
                    "relationship": "works_with",
                }),
            }])
        }

        async fn get_completion(
            &self,
            _query: &str,
            _context: Option<SearchContext>,
            _session: &SessionContext,
            _params: &SearchParams,
        ) -> Result<SearchOutput, SearchError> {
            Ok(SearchOutput::Text(ANSWER.to_string()))
        }

        fn context_evidence(
            &self,
            context: &SearchContext,
            dataset_id: Option<Uuid>,
        ) -> Vec<EvidenceReference> {
            graph_context_evidence(context, dataset_id)
        }
    }

    struct World {
        db: Arc<DatabaseConnection>,
        dataset: Uuid,
        data_id: Uuid,
        chunk_id: Uuid,
        retriever: KnownEdgeRetriever,
    }

    async fn world() -> World {
        let db = connect("sqlite::memory:").await.unwrap();
        initialize(&db).await.unwrap();
        let user = Uuid::new_v4();
        let dataset = Uuid::new_v4();
        create_dataset(
            &db,
            cognee_models::Dataset::new("reports".into(), user, None, dataset),
        )
        .await
        .unwrap();
        let data_id = Uuid::new_v4();
        create_data(
            &db,
            cognee_models::Data::builder(
                data_id,
                "report.pdf",
                "file:///tmp/r",
                "file:///tmp/r",
                "pdf",
                "application/pdf",
                "hash",
                user,
            )
            .dataset_id(dataset)
            .build(),
        )
        .await
        .unwrap();

        let retriever = KnownEdgeRetriever {
            source: Uuid::new_v4(),
            target: Uuid::new_v4(),
        };
        let chunk_id = Uuid::new_v4();
        insert_edge_evidence(
            &db,
            &[EdgeEvidence {
                id: Uuid::new_v4(),
                tenant_id: None,
                user_id: user,
                dataset_id: dataset,
                data_id,
                pipeline_run_id: None,
                chunk_id,
                chunk_index: Some(2),
                edge_id: generate_edge_object_id(
                    &retriever.source.to_string(),
                    &retriever.target.to_string(),
                    "works_with",
                ),
                source_node_id: retriever.source,
                destination_node_id: retriever.target,
                relationship_name: "works_with".into(),
                evidence_kind: "extracted".into(),
                source_task: None,
                confidence: None,
                created_at: chrono::Utc::now(),
            }],
        )
        .await
        .unwrap();
        World {
            db: Arc::new(db),
            dataset,
            data_id,
            chunk_id,
            retriever,
        }
    }

    fn scoped(world: &World, include_references: bool) -> SearchRequest {
        let mut request = request(SearchType::GraphCompletion, include_references);
        request.dataset_ids = Some(vec![world.dataset]);
        request
    }

    #[tokio::test]
    async fn graph_edges_gain_supporting_chunks_and_the_text_block() {
        let world = world().await;
        let orchestrator = orchestrator(KnownEdgeRetriever {
            source: world.retriever.source,
            target: world.retriever.target,
        })
        .with_database(world.db.clone() as Arc<dyn SearchHistoryDb>);

        let response = orchestrator.search(&scoped(&world, true)).await.unwrap();

        let kinds: Vec<_> = response.evidence.iter().map(|r| (r.kind, r.role)).collect();
        assert_eq!(
            kinds,
            [
                (EvidenceKind::GraphNode, EvidenceRole::UsedAsContext),
                (EvidenceKind::GraphNode, EvidenceRole::UsedAsContext),
                (EvidenceKind::GraphEdge, EvidenceRole::UsedAsContext),
                (EvidenceKind::Segment, EvidenceRole::SupportsAssertion),
            ]
        );
        let source = &response.evidence[3];
        assert_eq!(source.chunk_id, Some(world.chunk_id.to_string()));
        assert_eq!(source.data_id, Some(world.data_id.to_string()));
        assert_eq!(source.document_name.as_deref(), Some("report.pdf"));
        assert_eq!(source.chunk_index, Some(2));
        assert_eq!(
            source.assertion_id,
            response.evidence[2].artifact_id.clone().into()
        );
        assert_eq!(source.rank, Some(3));
        assert_eq!(
            source.source_ref_key,
            Some(format!("source_ref:v1:{}:{}", world.dataset, world.data_id))
        );
        assert_eq!(
            text(&response.result),
            format!(
                "{ANSWER}\n\nEvidence:\n- chunk 3 of document report.pdf \
                 (data_id: {}, chunk_id: {})",
                world.data_id, world.chunk_id
            )
        );
    }

    #[tokio::test]
    async fn without_the_flag_a_dataset_or_a_database_nothing_is_looked_up() {
        let world = world().await;
        let build = || {
            orchestrator(KnownEdgeRetriever {
                source: world.retriever.source,
                target: world.retriever.target,
            })
        };
        let with_db = |orchestrator: SearchOrchestrator| {
            orchestrator.with_database(world.db.clone() as Arc<dyn SearchHistoryDb>)
        };

        // Flag off.
        let response = with_db(build())
            .search(&scoped(&world, false))
            .await
            .unwrap();
        assert!(response.evidence.is_empty());
        assert_eq!(text(&response.result), ANSWER);

        // No dataset in scope: Python's `dataset is not None` guard.
        let unscoped = request(SearchType::GraphCompletion, true);
        let response = with_db(build()).search(&unscoped).await.unwrap();
        assert_eq!(response.evidence.len(), 3);
        assert_eq!(text(&response.result), ANSWER);

        // No database wired.
        let response = build().search(&scoped(&world, true)).await.unwrap();
        assert_eq!(response.evidence.len(), 3);
        assert_eq!(text(&response.result), ANSWER);
    }

    #[tokio::test]
    async fn a_missing_sidecar_never_fails_the_search() {
        use sea_orm::{ConnectionTrait, Statement};
        let world = world().await;
        world
            .db
            .execute(Statement::from_string(
                world.db.get_database_backend(),
                "DROP TABLE provenance_edge_evidence",
            ))
            .await
            .unwrap();
        let orchestrator = orchestrator(KnownEdgeRetriever {
            source: world.retriever.source,
            target: world.retriever.target,
        })
        .with_database(world.db.clone() as Arc<dyn SearchHistoryDb>);

        let response = orchestrator.search(&scoped(&world, true)).await.unwrap();

        assert_eq!(response.evidence.len(), 3);
        assert_eq!(text(&response.result), ANSWER);
    }
}
