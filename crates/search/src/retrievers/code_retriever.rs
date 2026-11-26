//! The `CODE` search type: one deterministic operation over the dataset's
//! code graph, with no LLM, embedding or vector search.
//!
//! Port of Python's `CodeRetriever` (`cognee/modules/retrieval/
//! code_retriever.py:1309-1417`). The operation and its arguments come from
//! the request's `code_query` merged over `retriever_specific_config`
//! ([`SearchParams::code_config`]); the query text is the human-readable
//! seed. The engine itself — snapshot, cache and operations — is
//! [`cognee_code_graph::query`].
//!
//! The context is one item whose payload is the operation's result; the
//! completion is that result as [`SearchOutput::Structured`], and an
//! `only_context` search returns it as Python's context text: the result as
//! compact JSON with sorted keys ([`json_key`]).

use std::sync::Arc;

use async_trait::async_trait;
use cognee_code_graph::query::{
    CodeQueryError, CodeRequest, code_graph_snapshot_cache, json_key, load_code_graph_snapshot,
    run_code_operation,
};
use cognee_graph::GraphDBTrait;
use cognee_session::SessionContext;
use serde_json::Value;

use crate::types::{
    SearchContext, SearchError, SearchItem, SearchOutput, SearchParams, SearchType,
};

use super::SearchRetriever;

/// Runs one `CODE` operation (Python's `CodeRetriever`).
pub struct CodeRetriever {
    graph_db: Arc<dyn GraphDBTrait>,
}

impl CodeRetriever {
    /// A retriever over `graph_db`. Under per-dataset storage it is the
    /// routing engine, and each search reads the code graph of the dataset
    /// it runs in.
    pub fn new(graph_db: Arc<dyn GraphDBTrait>) -> Self {
        Self { graph_db }
    }

    /// Validate the request, read the code graph (through the process-wide
    /// snapshot cache, keyed by the current dataset scope and this graph
    /// engine) and run the operation (Python's `get_retrieved_objects`).
    async fn run(&self, query: &str, params: &SearchParams) -> Result<Value, SearchError> {
        // The operation and its arguments are checked before the graph is
        // read, as Python checks them when the retriever is built.
        let request = CodeRequest::parse(params.code_config.clone().unwrap_or_default())?;
        let graph = Arc::clone(&self.graph_db);
        let snapshot = code_graph_snapshot_cache()
            .get_or_load(
                cognee_models::dataset_scope::current_dataset(),
                &self.graph_db,
                || {
                    let graph = Arc::clone(&graph);
                    async move { load_code_graph_snapshot(graph.as_ref()).await }
                },
            )
            .await
            .map_err(|error| SearchError::GraphError(error.to_string()))?;
        Ok(run_code_operation(&snapshot, &request, query)?)
    }
}

/// Python's `CodeSearchValidationError("SearchType.CODE does not support
/// batched queries.", param="query_batch")`.
fn batch_error() -> SearchError {
    SearchError::Code(CodeQueryError::invalid(
        "SearchType.CODE does not support batched queries.",
        "query_batch",
    ))
}

#[async_trait]
impl SearchRetriever for CodeRetriever {
    fn search_type(&self) -> SearchType {
        SearchType::Code
    }

    /// Python bypasses session interpretation for CODE, which may use LLMs
    /// or vector search (`prepare_session_turn_for_retrieval`).
    fn supports_session_turn_preparation(&self) -> bool {
        false
    }

    async fn get_context(
        &self,
        query: &str,
        params: &SearchParams,
    ) -> Result<SearchContext, SearchError> {
        let result = self.run(query, params).await?;
        Ok(vec![SearchItem {
            id: None,
            score: None,
            payload: result,
        }])
    }

    async fn get_completion(
        &self,
        query: &str,
        context: Option<SearchContext>,
        _session: &SessionContext,
        params: &SearchParams,
    ) -> Result<SearchOutput, SearchError> {
        // Python's `get_completion_from_context` returns the retrieved
        // objects unchanged.
        let result = match context.and_then(|items| items.into_iter().next()) {
            Some(item) => item.payload,
            None => self.run(query, params).await?,
        };
        Ok(SearchOutput::Structured(result))
    }

    /// Python's context of a CODE search is `_json_key(result)`.
    async fn only_context_output(
        &self,
        _query: &str,
        context: &SearchContext,
        _params: &SearchParams,
    ) -> Result<Option<SearchOutput>, SearchError> {
        Ok(context
            .first()
            .map(|item| SearchOutput::Text(json_key(&item.payload))))
    }

    async fn get_context_batch(
        &self,
        _queries: &[String],
        _params: &SearchParams,
    ) -> Result<Vec<SearchContext>, SearchError> {
        Err(batch_error())
    }

    async fn get_completion_batch(
        &self,
        _queries: &[String],
        _contexts: Option<Vec<SearchContext>>,
        _session: &SessionContext,
        _params: &SearchParams,
    ) -> Result<Vec<SearchOutput>, SearchError> {
        Err(batch_error())
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
mod tests {
    use cognee_code_graph::query::CodeErrorCode;
    use cognee_graph::MockGraphDB;
    use serde_json::json;

    use super::*;

    async fn shop_graph() -> Arc<dyn GraphDBTrait> {
        let graph = MockGraphDB::new();
        graph
            .add_nodes_raw(vec![
                json!({"id": "repo", "type": "CodeRepository", "name": "shop",
                       "last_snapshot_id": "s1", "last_delta": {"nodes_added": 2}}),
                json!({"id": "create", "type": "CodeSymbol", "name": "orders/api.create_order",
                       "file_path": "orders/api.py", "line": 8, "repo": "shop",
                       "symbol_kind": "function", "doc": "Creates an order — fast."}),
                json!({"id": "place", "type": "CodeSymbol", "name": "orders/service.place_order",
                       "file_path": "orders/service.py", "line": 20, "repo": "shop"}),
                json!({"id": "chunk", "type": "DocumentChunk", "text": "create_order"}),
            ])
            .await
            .unwrap();
        graph
            .add_edges(&[(
                "create".to_string(),
                "place".to_string(),
                "calls".to_string(),
                Default::default(),
            )])
            .await
            .unwrap();
        Arc::new(graph)
    }

    fn params(config: Value) -> SearchParams {
        SearchParams {
            code_config: Some(config.as_object().unwrap().clone()),
            ..SearchParams::default()
        }
    }

    #[tokio::test]
    async fn runs_an_operation_into_a_structured_completion() {
        let retriever = CodeRetriever::new(shop_graph().await);
        assert_eq!(retriever.search_type(), SearchType::Code);
        assert!(!retriever.supports_session_turn_preparation());
        let params = params(json!({"operation": "neighbors"}));
        let context = retriever
            .get_context("orders/api.create_order", &params)
            .await
            .unwrap();
        assert_eq!(context.len(), 1);
        assert_eq!(context[0].payload["operation"], json!("neighbors"));
        assert_eq!(
            context[0].payload["neighbors"][0]["node"]["name"],
            json!("orders/service.place_order")
        );

        let output = retriever
            .get_completion(
                "orders/api.create_order",
                Some(context.clone()),
                &SessionContext::default(),
                &params,
            )
            .await
            .unwrap();
        let SearchOutput::Structured(result) = output else {
            panic!("a structured result");
        };
        assert_eq!(result, context[0].payload);

        // Without a context the completion runs the operation itself.
        let output = retriever
            .get_completion(
                "orders/api.create_order",
                None,
                &SessionContext::default(),
                &params,
            )
            .await
            .unwrap();
        assert!(matches!(output, SearchOutput::Structured(ref value) if value == &result));
    }

    #[tokio::test]
    async fn the_context_text_is_compact_sorted_json() {
        let retriever = CodeRetriever::new(shop_graph().await);
        let params =
            params(json!({"operation": "query_facts", "names": "orders/api.create_order"}));
        let context = retriever.get_context("", &params).await.unwrap();
        let Some(SearchOutput::Text(text)) = retriever
            .only_context_output("", &context, &params)
            .await
            .unwrap()
        else {
            panic!("context text");
        };
        assert!(text.starts_with(
            r#"{"facts":[{"doc":"Creates an order — fast.","file":"orders/api.py","id":"create""#
        ));
        assert!(text.ends_with(
            r#""has_more":false,"limit":100,"offset":0,"operation":"query_facts","total":1}"#
        ));
    }

    #[tokio::test]
    async fn errors_carry_their_code_and_param() {
        let retriever = CodeRetriever::new(shop_graph().await);
        for (config, code, param) in [
            (
                json!({"operation": "bogus"}),
                CodeErrorCode::UnknownOperation,
                "operation",
            ),
            (
                json!({"operation": "neighbors", "name": "nowhere"}),
                CodeErrorCode::NotFound,
                "name",
            ),
            (
                json!({"operation": "query_facts", "limit": "x"}),
                CodeErrorCode::InvalidArgument,
                "limit",
            ),
            (
                json!({"operation": "flows"}),
                CodeErrorCode::FeatureUnavailable,
                "operation",
            ),
        ] {
            let error = retriever
                .get_context("", &params(config))
                .await
                .unwrap_err();
            let SearchError::Code(error) = error else {
                panic!("a CODE error: {error:?}");
            };
            assert_eq!((error.code, error.param.as_deref()), (code, Some(param)));
        }
    }

    #[tokio::test]
    async fn no_configuration_explores_the_query() {
        // Python's default operation is explore, seeded by the query text.
        let retriever = CodeRetriever::new(shop_graph().await);
        let context = retriever
            .get_context("create_order", &SearchParams::default())
            .await
            .unwrap();
        let result = &context[0].payload;
        assert_eq!(result["operation"], json!("explore"));
        assert_eq!(result["focus"]["id"], json!("create"));
        assert_eq!(
            result["nodes"][1]["name"],
            json!("orders/service.place_order")
        );
        assert!(result.get("diagram").is_none());
    }

    #[tokio::test]
    async fn a_diagram_rides_in_the_result_and_its_context_text() {
        let retriever = CodeRetriever::new(shop_graph().await);
        let params = params(json!({
            "operation": "find_path",
            "target": "place_order",
            "diagram": "mermaid",
        }));
        let context = retriever
            .get_context("create_order", &params)
            .await
            .unwrap();
        let diagram = &context[0].payload["diagram"];
        // The YAML title swaps `>` for its look-alike `›`, as Python does.
        assert_eq!(diagram["format"], json!("mermaid"));
        assert_eq!(
            diagram["source"],
            json!(
                "---\ntitle: \"path: orders/api.create_order -› orders/service.place_order\"\n---\n\
                 flowchart LR\n    n0([\"orders/api.create_order\"])\n    \
                 n1([\"orders/service.place_order\"])\n    n0 -- \"calls\" --> n1\n    \
                 classDef highlight stroke:#d64545,stroke-width:3px\n    class n0,n1 highlight\n"
            )
        );
        let Some(SearchOutput::Text(text)) = retriever
            .only_context_output("create_order", &context, &params)
            .await
            .unwrap()
        else {
            panic!("context text");
        };
        // Keys sorted: the diagram block sits between "edges" and "found".
        assert!(text.contains(
            r#""diagram":{"edges":1,"format":"mermaid","nodes":2,"source":"---\ntitle:"#
        ));
    }

    #[tokio::test]
    async fn batched_queries_are_refused() {
        let retriever = CodeRetriever::new(shop_graph().await);
        let error = retriever
            .get_context_batch(&["a".to_string()], &SearchParams::default())
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "SearchType.CODE does not support batched queries."
        );
        assert!(
            matches!(error, SearchError::Code(ref error) if error.param.as_deref() == Some("query_batch"))
        );
        assert!(
            retriever
                .get_completion_batch(
                    &[],
                    None,
                    &SessionContext::default(),
                    &SearchParams::default()
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_snapshot_is_cached_until_invalidated() {
        let graph = MockGraphDB::new();
        graph
            .add_node_raw(json!({"id": "a", "type": "CodeSymbol", "name": "first"}))
            .await
            .unwrap();
        let engine: Arc<dyn GraphDBTrait> = Arc::new(graph.clone());
        let retriever = CodeRetriever::new(Arc::clone(&engine));
        let dataset = uuid::Uuid::new_v4();
        let params = params(json!({"operation": "capabilities"}));
        let count = |context: SearchContext| context[0].payload["counts"]["nodes"].clone();
        let first =
            cognee_models::dataset_scope::with_dataset(dataset, retriever.get_context("", &params))
                .await
                .unwrap();
        assert_eq!(count(first), json!(1));
        graph
            .add_node_raw(json!({"id": "b", "type": "CodeSymbol", "name": "second"}))
            .await
            .unwrap();
        // A write the cache was not told about is not seen within the TTL.
        let cached =
            cognee_models::dataset_scope::with_dataset(dataset, retriever.get_context("", &params))
                .await
                .unwrap();
        assert_eq!(count(cached), json!(1));
        cognee_code_graph::query::invalidate_code_graph_snapshots(Some(dataset));
        let fresh =
            cognee_models::dataset_scope::with_dataset(dataset, retriever.get_context("", &params))
                .await
                .unwrap();
        assert_eq!(count(fresh), json!(2));
    }
}
