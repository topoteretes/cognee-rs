#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Integration test: `recall()` records router overrides.
//!
//! When the caller passes an explicit `query_type` and `auto_route=true`,
//! the router still runs so we can compare its choice against the user's
//! override. The mismatch is logged via `record_override()` into the
//! process-global counter.

use std::sync::Arc;

use async_trait::async_trait;
use cognee_lib::api::recall;
use cognee_search::orchestration::SearchTypeRegistry;
use cognee_search::retrievers::SearchRetriever;
use cognee_search::types::{SearchContext, SearchError, SearchOutput, SearchParams};
use cognee_search::{
    SearchOrchestrator, SearchType, clear_override_counts, override_counts_snapshot,
};
use cognee_session::SessionContext;

/// Minimal retriever that responds to whichever search type it was registered
/// for. Returns an empty context and a static text completion so that the
/// orchestrator pipeline runs end-to-end without needing real backends.
struct StubRetriever(SearchType);

#[async_trait]
impl SearchRetriever for StubRetriever {
    fn search_type(&self) -> SearchType {
        self.0
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
        Ok(SearchOutput::Text("stub".to_string()))
    }
}

#[tokio::test]
#[serial_test::serial]
async fn recall_records_router_override() {
    clear_override_counts();

    // Build an orchestrator with a Temporal retriever so that recall()
    // can run end-to-end when query_type=Temporal is passed explicitly.
    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::new(StubRetriever(SearchType::Temporal)));
    let orchestrator = SearchOrchestrator::new(registry);

    // "Give me a summary of the project" routes to GraphSummaryCompletion.
    // We explicitly override with Temporal + auto_route=true so the
    // router still runs and record_override() is called.
    let result = recall(
        "Give me a summary of the project",
        Some(SearchType::Temporal),
        None,
        10,
        true,
        None,
        None,
        &orchestrator,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("recall should succeed against the stub retriever");

    // The executed search_type is the user's choice (Temporal), not the
    // router's (GraphSummaryCompletion).
    assert_eq!(result.search_type_used, Some(SearchType::Temporal));
    assert!(
        !result.auto_routed,
        "explicit query_type => auto_routed=false"
    );

    // The override counter should record (routed=GraphSummaryCompletion,
    // override=Temporal) exactly once.
    let snap = override_counts_snapshot();
    let count = snap
        .get(&(SearchType::GraphSummaryCompletion, SearchType::Temporal))
        .copied()
        .unwrap_or(0);
    assert_eq!(
        count, 1,
        "expected exactly one (GraphSummaryCompletion -> Temporal) override; snapshot={snap:?}"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn recall_auto_route_does_not_record_override() {
    clear_override_counts();

    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::new(StubRetriever(SearchType::GraphSummaryCompletion)));
    let orchestrator = SearchOrchestrator::new(registry);

    let result = recall(
        "Give me a summary of the project",
        None,
        None,
        10,
        true,
        None,
        None,
        &orchestrator,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("recall should succeed");

    assert_eq!(
        result.search_type_used,
        Some(SearchType::GraphSummaryCompletion)
    );
    assert!(result.auto_routed, "no query_type => auto_routed=true");

    // With no explicit user override, nothing should be recorded.
    assert!(
        override_counts_snapshot().is_empty(),
        "no override should be recorded when query_type is None"
    );
}

#[tokio::test]
#[serial_test::serial]
async fn recall_explicit_without_auto_route_skips_router() {
    clear_override_counts();

    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::new(StubRetriever(SearchType::Temporal)));
    let orchestrator = SearchOrchestrator::new(registry);

    let result = recall(
        "Give me a summary of the project",
        Some(SearchType::Temporal),
        None,
        10,
        false, // auto_route=false
        None,
        None,
        &orchestrator,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("recall should succeed");

    assert_eq!(result.search_type_used, Some(SearchType::Temporal));
    assert!(!result.auto_routed);

    // With auto_route=false, the router must not run -> no override recorded.
    assert!(
        override_counts_snapshot().is_empty(),
        "auto_route=false => router must not run"
    );
}
