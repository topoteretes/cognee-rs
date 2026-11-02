#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration test: `recall()` returns a note instead of searching a graph
//! that is not built, and searches it once it is.

use std::sync::Arc;

use async_trait::async_trait;
use cognee::api::{RecallItem, RecallOptions, RecallResult, RecallScope, RecallSource, recall};
use cognee_database::{
    DatabaseConnection, IngestDb, PipelineRunRepository, PipelineRunStatus,
    SeaOrmPipelineRunRepository, connect, initialize, ops,
};
use cognee_models::Dataset;
use cognee_search::graph_warmup::{GraphWarmup, WarmupConfig};
use cognee_search::orchestration::SearchTypeRegistry;
use cognee_search::retrievers::SearchRetriever;
use cognee_search::types::{SearchContext, SearchError, SearchOutput, SearchParams};
use cognee_search::{SearchOrchestrator, SearchType};
use cognee_session::SessionContext;
use serde_json::json;
use uuid::Uuid;

/// Answers HYBRID_COMPLETION with "answer", so a search that ran shows up.
struct AnswerRetriever;

#[async_trait]
impl SearchRetriever for AnswerRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::HybridCompletion
    }

    async fn get_context(
        &self,
        _query: &str,
        _params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        Ok(vec![])
    }

    async fn get_completion(
        &self,
        _query: &str,
        _context: Option<SearchContext>,
        _session: &SessionContext,
        _params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        Ok(SearchOutput::Text("answer".to_string()))
    }
}

/// One dataset, owned by one user, in an in-memory database.
struct Fixture {
    db: Arc<DatabaseConnection>,
    owner: Uuid,
    dataset_id: Uuid,
}

impl Fixture {
    async fn new() -> Self {
        let db = connect("sqlite::memory:").await.expect("connect");
        initialize(&db).await.expect("init");
        let (owner, dataset_id) = (Uuid::new_v4(), Uuid::new_v4());
        let dataset = Dataset::new("docs".to_string(), owner, None, dataset_id);
        ops::datasets::create_dataset(&db, dataset)
            .await
            .expect("create_dataset");
        Self {
            db: Arc::new(db),
            owner,
            dataset_id,
        }
    }

    async fn log_cognify(&self, status: PipelineRunStatus, run_info: Option<serde_json::Value>) {
        SeaOrmPipelineRunRepository::new(Arc::clone(&self.db))
            .log_pipeline_run(
                Uuid::new_v4(),
                Uuid::new_v4(),
                "cognify_pipeline",
                Some(self.dataset_id),
                status,
                run_info,
            )
            .await
            .expect("log_pipeline_run");
    }

    fn orchestrator(&self, config: WarmupConfig) -> SearchOrchestrator {
        let mut registry = SearchTypeRegistry::new();
        registry.register(Arc::new(AnswerRetriever));
        let pipeline_runs = Arc::new(SeaOrmPipelineRunRepository::new(Arc::clone(&self.db)));
        SearchOrchestrator::new(registry)
            .with_dataset_resolver(Arc::clone(&self.db) as Arc<dyn IngestDb>)
            .with_graph_warmup(Arc::new(GraphWarmup::new(pipeline_runs, config)))
    }

    async fn recall(
        &self,
        orchestrator: &SearchOrchestrator,
        scope: Vec<RecallScope>,
        options: Option<RecallOptions>,
    ) -> RecallResult {
        let owner = self.owner.to_string();
        recall(
            "Who won?",
            Some(SearchType::HybridCompletion),
            None,
            Some(vec![self.dataset_id]),
            10,
            false,
            None,
            Some(&owner),
            orchestrator,
            None,
            None,
            Some(scope),
            options,
        )
        .await
        .expect("recall should succeed")
    }
}

fn statuses(items: &[RecallItem]) -> Vec<&str> {
    items
        .iter()
        .filter(|item| item.source == RecallSource::System)
        .map(|item| item.content["status"].as_str().unwrap())
        .collect()
}

fn answered(items: &[RecallItem]) -> bool {
    items.iter().any(|item| item.content == json!("answer"))
}

#[tokio::test]
async fn a_graph_that_is_not_built_gets_a_note_until_it_is() {
    let fixture = Fixture::new().await;
    let orchestrator = fixture.orchestrator(WarmupConfig::default());

    let cold = fixture
        .recall(&orchestrator, vec![RecallScope::Graph], None)
        .await;
    assert_eq!(statuses(&cold.items), ["memory_warming_up"]);
    assert!(!answered(&cold.items));

    fixture
        .log_cognify(PipelineRunStatus::Completed, None)
        .await;
    let warm = fixture
        .recall(&orchestrator, vec![RecallScope::Graph], None)
        .await;
    assert!(statuses(&warm.items).is_empty());
    assert!(answered(&warm.items));
}

#[tokio::test]
async fn a_failed_build_gets_a_note_naming_its_error() {
    let fixture = Fixture::new().await;
    let error = json!({"error": "LLM unreachable"});
    fixture
        .log_cognify(PipelineRunStatus::Errored, Some(error))
        .await;
    let orchestrator = fixture.orchestrator(WarmupConfig::default());

    let result = fixture
        .recall(&orchestrator, vec![RecallScope::Graph], None)
        .await;
    assert_eq!(statuses(&result.items), ["build_failed"]);
    let note = &result.items[0].content;
    assert_eq!(note["error_message"], "LLM unreachable");
    assert!(note["text"].as_str().unwrap().contains("(LLM unreachable)"));
}

#[tokio::test]
async fn next_to_another_source_a_cold_graph_contributes_nothing() {
    let fixture = Fixture::new().await;
    let orchestrator = fixture.orchestrator(WarmupConfig::default());
    let scope = vec![RecallScope::Graph, RecallScope::Trace];
    let result = fixture.recall(&orchestrator, scope, None).await;
    assert!(result.items.is_empty(), "{:?}", result.items);
}

#[tokio::test]
async fn only_context_or_a_disabled_short_circuit_searches_a_cold_graph() {
    let fixture = Fixture::new().await;
    let graph = || vec![RecallScope::Graph];

    let orchestrator = fixture.orchestrator(WarmupConfig::default());
    let only_context = Some(RecallOptions {
        only_context: Some(true),
        ..Default::default()
    });
    let result = fixture.recall(&orchestrator, graph(), only_context).await;
    assert!(statuses(&result.items).is_empty());

    let disabled = WarmupConfig {
        shortcircuit: false,
        ..WarmupConfig::default()
    };
    let orchestrator = fixture.orchestrator(disabled);
    let result = fixture.recall(&orchestrator, graph(), None).await;
    assert!(statuses(&result.items).is_empty());
    assert!(answered(&result.items));
}

#[tokio::test]
async fn a_dataset_the_caller_cannot_read_is_refused_not_noted() {
    let fixture = Fixture::new().await;
    let orchestrator = fixture.orchestrator(WarmupConfig::default());
    let stranger = Uuid::new_v4().to_string();
    let result = recall(
        "Who won?",
        Some(SearchType::HybridCompletion),
        None,
        Some(vec![fixture.dataset_id]),
        10,
        false,
        None,
        Some(&stranger),
        &orchestrator,
        None,
        None,
        Some(vec![RecallScope::Graph]),
        None,
    )
    .await;
    let error = result.expect_err("a dataset the caller cannot read should be refused");
    assert!(error.to_string().contains("permission"), "{error}");
}
