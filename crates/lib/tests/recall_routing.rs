#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration test: how `recall()` picks the graph search type, and when it
//! retries a routed type as the router's fallback.

use std::sync::Arc;

use async_trait::async_trait;
use cognee::api::{RecallResult, recall};
use cognee_search::orchestration::SearchTypeRegistry;
use cognee_search::retrievers::SearchRetriever;
use cognee_search::types::{SearchContext, SearchError, SearchOutput, SearchParams};
use cognee_search::{RoutingConstraints, SearchOrchestrator, SearchType};
use cognee_session::SessionContext;

/// Answers its search type with a fixed output, without real backends.
struct StubRetriever {
    search_type: SearchType,
    output: SearchOutput,
}

#[async_trait]
impl SearchRetriever for StubRetriever {
    fn search_type(&self) -> SearchType {
        self.search_type
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
        Ok(self.output.clone())
    }
}

fn orchestrator(stubs: Vec<(SearchType, SearchOutput)>) -> SearchOrchestrator {
    let mut registry = SearchTypeRegistry::new();
    for (search_type, output) in stubs {
        registry.register(Arc::new(StubRetriever {
            search_type,
            output,
        }));
    }
    SearchOrchestrator::new(registry)
}

async fn recall_graph(
    orchestrator: &SearchOrchestrator,
    query: &str,
    query_type: Option<SearchType>,
    auto_route: bool,
) -> RecallResult {
    recall(
        query,
        query_type,
        None,
        None,
        10,
        auto_route,
        None,
        None,
        orchestrator,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("recall should succeed against the stub retrievers")
}

fn answer() -> SearchOutput {
    SearchOutput::Text("answer".to_string())
}

fn without_an_llm(orchestrator: SearchOrchestrator) -> SearchOrchestrator {
    orchestrator.with_routing_constraints(RoutingConstraints {
        llm_available: false,
        cypher_allowed: true,
    })
}

fn ran_as_only_context(result: &RecallResult) -> bool {
    result
        .search_response
        .as_ref()
        .expect("recall should return the graph search's response")
        .only_context
}

#[tokio::test]
async fn with_no_llm_an_unmatched_query_runs_the_fallback_as_only_context() {
    let orchestrator = without_an_llm(orchestrator(vec![(SearchType::HybridCompletion, answer())]));
    let query = "Where was Marie Curie born?";
    let result = recall_graph(&orchestrator, query, None, true).await;
    assert_eq!(result.search_type_used, Some(SearchType::HybridCompletion));
    assert!(ran_as_only_context(&result));
}

#[tokio::test]
async fn with_no_llm_an_llm_free_route_runs_as_it_is() {
    let orchestrator = without_an_llm(orchestrator(vec![(SearchType::ChunksLexical, answer())]));
    let query = "\"polonium and radium\"";
    let result = recall_graph(&orchestrator, query, None, true).await;
    assert_eq!(result.search_type_used, Some(SearchType::ChunksLexical));
    assert!(!ran_as_only_context(&result));
}

#[tokio::test]
async fn with_no_llm_a_dated_question_runs_the_fallback_as_only_context() {
    let orchestrator = without_an_llm(orchestrator(vec![(SearchType::HybridCompletion, answer())]));
    let query = "What happened in 2019?";
    let result = recall_graph(&orchestrator, query, None, true).await;
    assert_eq!(result.search_type_used, Some(SearchType::HybridCompletion));
    assert!(ran_as_only_context(&result));
}

#[tokio::test]
async fn with_no_llm_a_pinned_completion_type_runs_as_only_context() {
    let orchestrator = without_an_llm(orchestrator(vec![(SearchType::GraphCompletion, answer())]));
    let pinned = Some(SearchType::GraphCompletion);
    let result = recall_graph(&orchestrator, "Who won?", pinned, true).await;
    assert_eq!(result.search_type_used, Some(SearchType::GraphCompletion));
    assert!(ran_as_only_context(&result));
}

#[tokio::test]
async fn with_no_llm_a_pinned_triplet_completion_runs_as_only_context() {
    let stub = (SearchType::TripletCompletion, answer());
    let orchestrator = without_an_llm(orchestrator(vec![stub]));
    let pinned = Some(SearchType::TripletCompletion);
    let result = recall_graph(&orchestrator, "Who won?", pinned, true).await;
    assert_eq!(result.search_type_used, Some(SearchType::TripletCompletion));
    assert!(ran_as_only_context(&result));
}

#[tokio::test]
async fn with_no_llm_a_retry_runs_the_fallback_as_only_context() {
    let orchestrator = without_an_llm(orchestrator(vec![
        (SearchType::CodingRules, SearchOutput::Rules(Vec::new())),
        (SearchType::HybridCompletion, answer()),
    ]));
    let query = "What are the coding rules mentioned?";
    let result = recall_graph(&orchestrator, query, None, true).await;
    assert_eq!(result.search_type_used, Some(SearchType::HybridCompletion));
    assert!(ran_as_only_context(&result));
}

#[tokio::test]
async fn a_query_no_rule_matches_runs_the_fallback() {
    let orchestrator = orchestrator(vec![(SearchType::HybridCompletion, answer())]);
    let result = recall_graph(&orchestrator, "Who won Nobel Prizes?", None, true).await;
    assert_eq!(result.search_type_used, Some(SearchType::HybridCompletion));
    assert!(result.auto_routed);
}

#[tokio::test]
async fn an_explicit_type_is_not_routed() {
    let orchestrator = orchestrator(vec![(SearchType::GraphCompletion, answer())]);
    let query = "What happened in 2019?";
    let result = recall_graph(
        &orchestrator,
        query,
        Some(SearchType::GraphCompletion),
        true,
    )
    .await;
    assert_eq!(result.search_type_used, Some(SearchType::GraphCompletion));
    assert!(!result.auto_routed);
}

#[tokio::test]
async fn without_auto_routing_recall_runs_the_fallback() {
    let orchestrator = orchestrator(vec![(SearchType::HybridCompletion, answer())]);
    let result = recall_graph(&orchestrator, "What happened in 2019?", None, false).await;
    assert_eq!(result.search_type_used, Some(SearchType::HybridCompletion));
    assert!(!result.auto_routed);
}

#[tokio::test]
async fn a_routed_coding_rules_search_that_finds_nothing_runs_the_fallback() {
    let orchestrator = orchestrator(vec![
        (SearchType::CodingRules, SearchOutput::Rules(Vec::new())),
        (SearchType::HybridCompletion, answer()),
    ]);
    let query = "What are the coding rules mentioned?";
    let result = recall_graph(&orchestrator, query, None, true).await;
    assert_eq!(result.search_type_used, Some(SearchType::HybridCompletion));
}

#[tokio::test]
async fn a_routed_type_the_backend_does_not_support_runs_the_fallback() {
    let orchestrator = orchestrator(vec![(SearchType::HybridCompletion, answer())]);
    let query = "What are the coding rules mentioned?";
    let result = recall_graph(&orchestrator, query, None, true).await;
    assert_eq!(result.search_type_used, Some(SearchType::HybridCompletion));
}

#[tokio::test]
async fn an_explicit_type_that_finds_nothing_is_not_retried() {
    let orchestrator = orchestrator(vec![
        (SearchType::CodingRules, SearchOutput::Rules(Vec::new())),
        (SearchType::HybridCompletion, answer()),
    ]);
    let query = "What are the coding rules mentioned?";
    let result = recall_graph(&orchestrator, query, Some(SearchType::CodingRules), true).await;
    assert_eq!(result.search_type_used, Some(SearchType::CodingRules));
}
