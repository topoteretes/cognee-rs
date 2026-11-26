//! The CODE search type through the orchestrator: `code_query` validation,
//! the per-dataset fan-out, and the per-dataset seed-miss softening of
//! Python's `modules/search/methods/search.py:523-565`.
//!
//! Each dataset has its own mock graph behind a dataset-routed engine, as
//! per-dataset storage (`ENABLE_BACKEND_ACCESS_CONTROL`) wires it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use cognee_graph::{
    DatasetGraphResolver, DatasetRoutedGraphDB, GraphDBResult, GraphDBTrait, MockGraphDB,
};
use cognee_search::{
    CodeRetriever, SearchError, SearchOrchestrator, SearchOutput, SearchRequest, SearchType,
    SearchTypeRegistry,
};
use serde_json::{Value, json};
use uuid::Uuid;

struct Datasets(HashMap<Uuid, Arc<dyn GraphDBTrait>>);

#[async_trait]
impl DatasetGraphResolver for Datasets {
    async fn graph_for_dataset(&self, dataset_id: Uuid) -> GraphDBResult<Arc<dyn GraphDBTrait>> {
        Ok(Arc::clone(&self.0[&dataset_id]))
    }

    fn cached_graph(&self, dataset_id: Uuid) -> Option<Arc<dyn GraphDBTrait>> {
        self.0.get(&dataset_id).cloned()
    }

    fn cached_graphs(&self) -> Vec<Arc<dyn GraphDBTrait>> {
        self.0.values().cloned().collect()
    }
}

async fn graph(nodes: Vec<Value>) -> Arc<dyn GraphDBTrait> {
    let graph = MockGraphDB::new();
    graph.add_nodes_raw(nodes).await.unwrap();
    Arc::new(graph)
}

fn symbol(id: &str, name: &str, repo: &str) -> Value {
    json!({"id": id, "type": "CodeSymbol", "name": name, "repo": repo,
           "file_path": format!("{name}.py"), "symbol_kind": "function"})
}

/// Dataset `a` holds `orders.create_order`; dataset `b` holds two
/// `billing.charge` symbols (an ambiguous name) and no `create_order`.
async fn world() -> (SearchOrchestrator, Uuid, Uuid) {
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let datasets = Datasets(HashMap::from([
        (
            a,
            graph(vec![symbol("create", "orders.create_order", "shop")]).await,
        ),
        (
            b,
            graph(vec![
                symbol("charge-1", "billing.charge", "shop"),
                symbol("charge-2", "billing.charge", "payments"),
            ])
            .await,
        ),
    ]));
    let routed: Arc<dyn GraphDBTrait> = Arc::new(DatasetRoutedGraphDB::new(Arc::new(datasets)));
    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::new(CodeRetriever::new(routed)));
    let orchestrator = SearchOrchestrator::new(registry).with_per_dataset_storage(true);
    (orchestrator, a, b)
}

fn code_request(code_query: Value, query: &str, datasets: &[Uuid]) -> SearchRequest {
    let mut request: SearchRequest = serde_json::from_value(json!({
        "query_text": query,
        "search_type": "CODE",
        "code_query": code_query,
    }))
    .unwrap();
    request.dataset_ids = Some(datasets.to_vec());
    request
}

#[tokio::test]
async fn a_seed_one_dataset_cannot_resolve_is_that_datasets_result() {
    let (orchestrator, a, b) = world().await;
    let request = code_request(
        json!({"operation": "neighbors", "name": "create_order"}),
        "",
        &[a, b],
    );
    let results = orchestrator.search_datasets(&request).await.unwrap();
    assert_eq!(results.len(), 2);

    assert_eq!(results[0].dataset_id, Some(a));
    assert_eq!(results[0].error, None);
    let SearchOutput::Structured(found) = &results[0].response.result else {
        panic!("a structured result: {:?}", results[0].response.result);
    };
    assert_eq!(found["operation"], json!("neighbors"));
    assert_eq!(found["anchor"]["id"], json!("create"));

    let message = "CodeSearchValidationError: CODE could not resolve neighbors anchor name \
                   'create_order'. (Status code: 422)";
    assert_eq!(results[1].dataset_id, Some(b));
    assert_eq!(results[1].error.as_deref(), Some(message));
    let SearchOutput::Structured(miss) = &results[1].response.result else {
        panic!("a structured miss: {:?}", results[1].response.result);
    };
    assert_eq!(
        miss,
        &json!({"seed_not_found": true, "error": message, "code": "not_found", "param": "name"})
    );
    assert_eq!(results[1].response.datasets, Some(vec![b]));

    // The merged answer keeps one structured result per dataset, in order.
    let merged = orchestrator.search(&request).await.unwrap();
    let SearchOutput::Structured(Value::Array(per_dataset)) = merged.result else {
        panic!("one result per dataset");
    };
    assert_eq!(per_dataset.len(), 2);
    assert_eq!(per_dataset[0]["operation"], json!("neighbors"));
    assert_eq!(per_dataset[1]["seed_not_found"], json!(true));
}

#[tokio::test]
async fn an_ambiguous_seed_is_softened_with_its_code() {
    let (orchestrator, a, b) = world().await;
    let request = code_request(json!({"operation": "neighbors"}), "billing.charge", &[a, b]);
    let results = orchestrator.search_datasets(&request).await.unwrap();
    let SearchOutput::Structured(miss_a) = &results[0].response.result else {
        panic!("structured");
    };
    assert_eq!(miss_a["code"], json!("not_found"));
    let SearchOutput::Structured(miss_b) = &results[1].response.result else {
        panic!("structured");
    };
    assert_eq!(miss_b["code"], json!("ambiguous"));
    assert_eq!(miss_b["param"], json!("name"));
}

#[tokio::test]
async fn one_dataset_and_bad_requests_still_fail_the_search() {
    let (orchestrator, a, b) = world().await;

    // One dataset: nothing to soften against, the miss is the answer.
    let error = orchestrator
        .search_datasets(&code_request(
            json!({"operation": "neighbors", "name": "create_order"}),
            "",
            &[b],
        ))
        .await
        .unwrap_err();
    let SearchError::Code(error) = error else {
        panic!("a CODE error: {error:?}");
    };
    assert_eq!(error.code.as_str(), "not_found");

    // A bad argument is the caller's mistake in every dataset.
    for code_query in [
        json!({"operation": "neighbors", "name": ["create_order"]}),
        json!({"operation": "bogus"}),
        json!({"operation": "flows"}),
    ] {
        let error = orchestrator
            .search_datasets(&code_request(code_query.clone(), "x", &[a, b]))
            .await
            .unwrap_err();
        assert!(
            matches!(&error, SearchError::Code(error) if !error.is_seed_miss()),
            "{code_query}: {error:?}"
        );
    }
}

#[tokio::test]
async fn delta_and_capabilities_answer_per_dataset() {
    let (orchestrator, a, b) = world().await;
    let request = code_request(json!({"operation": "capabilities"}), "", &[a, b]);
    let results = orchestrator.search_datasets(&request).await.unwrap();
    let counts: Vec<Value> = results
        .iter()
        .map(|result| match &result.response.result {
            SearchOutput::Structured(value) => value["counts"]["symbols"].clone(),
            other => panic!("structured: {other:?}"),
        })
        .collect();
    assert_eq!(counts, vec![json!(1), json!(2)]);
}

#[tokio::test]
async fn only_context_returns_the_compact_json_context() {
    let (orchestrator, a, _) = world().await;
    let mut request = code_request(
        json!({"operation": "query_facts", "names": "orders.create_order"}),
        "",
        &[a],
    );
    request.only_context = Some(true);
    let response = orchestrator.search(&request).await.unwrap();
    let SearchOutput::Text(text) = response.result else {
        panic!("context text");
    };
    assert!(text.starts_with(r#"{"facts":[{"file":"orders.create_order.py","id":"create""#));
    assert!(text.ends_with(r#""operation":"query_facts","total":1}"#));
}

#[tokio::test]
async fn code_query_requires_the_code_search_type() {
    let (orchestrator, a, _) = world().await;
    let mut request = code_request(json!({"operation": "delta"}), "", &[a]);
    request.search_type = SearchType::Chunks;
    let error = orchestrator.search(&request).await.unwrap_err();
    assert!(
        matches!(error, SearchError::QueryValidation(_)),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        "code_query requires query_type=SearchType.CODE."
    );
}

#[tokio::test]
async fn walks_soften_seed_misses_and_draw_per_dataset() {
    let (orchestrator, a, b) = world().await;

    // explore, traverse and impact_analysis seeds miss in b like neighbors.
    for code_query in [
        json!({"operation": "explore", "diagram": "dot"}),
        json!({"operation": "traverse"}),
        json!({"operation": "impact_analysis", "diagram": true}),
    ] {
        let request = code_request(code_query.clone(), "create_order", &[a, b]);
        let results = orchestrator.search_datasets(&request).await.unwrap();
        let SearchOutput::Structured(found) = &results[0].response.result else {
            panic!("{code_query}: structured");
        };
        assert_eq!(found["operation"], code_query["operation"]);
        if code_query.get("diagram").is_some() {
            assert!(
                found["diagram"]["source"]
                    .as_str()
                    .is_some_and(|source| source.contains("orders.create_order")),
                "{code_query}: {found}"
            );
        }
        let SearchOutput::Structured(miss) = &results[1].response.result else {
            panic!("{code_query}: structured miss");
        };
        assert_eq!(miss["seed_not_found"], json!(true), "{code_query}");
        assert_eq!(miss["code"], json!("not_found"), "{code_query}");
    }

    // A path with no target is a usage error in every dataset.
    let error = orchestrator
        .search_datasets(&code_request(
            json!({"operation": "find_path"}),
            "create_order",
            &[a, b],
        ))
        .await
        .unwrap_err();
    assert!(
        matches!(&error, SearchError::Code(error) if !error.is_seed_miss() && error.param.as_deref() == Some("name")),
        "{error:?}"
    );

    // The architecture overview needs no seed and draws itself.
    let results = orchestrator
        .search_datasets(&code_request(
            json!({"operation": "architecture", "node_types": ["symbol"]}),
            "",
            &[a, b],
        ))
        .await
        .unwrap();
    let shown: Vec<Value> = results
        .iter()
        .map(|result| match &result.response.result {
            SearchOutput::Structured(value) => {
                assert_eq!(value["diagram"]["format"], json!("mermaid"));
                value["stats"]["nodes_shown"].clone()
            }
            other => panic!("structured: {other:?}"),
        })
        .collect();
    assert_eq!(shown, vec![json!(1), json!(2)]);
}
