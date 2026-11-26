//! Recall's `code` lane ([`run_code`]) over the CODE search type: Python's
//! `_run_code` (`api/v1/recall/recall.py:1093-1157`), checked as
//! `cognee/tests/unit/api/recall/test_recall_code_scope.py` checks it.
//!
//! Each dataset has its own mock code graph behind a dataset-routed engine,
//! as per-dataset storage wires it.

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
use cognee_search::recall_scope::{RecallOptions, RecallSource, run_code};
use cognee_search::{CodeRetriever, SearchError, SearchOrchestrator, SearchTypeRegistry};
use serde_json::{Map, Value, json};
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
/// `billing.charge` symbols (an ambiguous name) and no `create_order`;
/// dataset `c` has no code graph at all.
async fn world() -> (SearchOrchestrator, Uuid, Uuid, Uuid) {
    let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
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
        (
            c,
            graph(vec![
                json!({"id": "chunk", "type": "DocumentChunk", "text": "prose"}),
            ])
            .await,
        ),
    ]));
    let routed: Arc<dyn GraphDBTrait> = Arc::new(DatasetRoutedGraphDB::new(Arc::new(datasets)));
    let mut registry = SearchTypeRegistry::new();
    registry.register(Arc::new(CodeRetriever::new(routed)));
    let orchestrator = SearchOrchestrator::new(registry).with_per_dataset_storage(true);
    (orchestrator, a, b, c)
}

fn with_query(code_query: Value) -> RecallOptions {
    let Value::Object(map) = code_query else {
        panic!("a code query is an object");
    };
    RecallOptions {
        code_query: Some(map),
        ..Default::default()
    }
}

const USER: &str = "33333333-3333-4333-8333-333333333333";

/// `test_code_scope_runs_code_lane_and_tags_source`: no `code_query` runs
/// `explore` with the query text as seed, and every item is tagged `code`
/// with Python's `ResponseCodeEntry` shape.
#[tokio::test]
async fn the_code_lane_explores_the_query_text_and_tags_code() {
    let (orchestrator, a, _, _) = world().await;
    let items = run_code(
        "create_order",
        None,
        Some(vec![a]),
        7,
        Some(USER),
        &orchestrator,
        None,
    )
    .await
    .unwrap();

    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert_eq!(item.source, RecallSource::Code);
    assert_eq!(item.content["kind"], json!("code"));
    assert_eq!(item.content["search_type"], json!("CODE"));
    assert_eq!(item.content["raw"]["operation"], json!("explore"));
    assert_eq!(item.content["dataset_id"], json!(a.to_string()));
    assert_eq!(item.content.get("source"), None);
    // The content keys are the `SearchResultItem` fields, in order.
    let keys: Vec<&str> = item
        .content
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "kind",
            "search_type",
            "text",
            "system_prompt",
            "score",
            "dataset_id",
            "dataset_name",
            "metadata",
            "raw",
            "structured"
        ]
    );
    // The serialized item carries the `code` source.
    assert_eq!(serde_json::to_value(item).unwrap()["source"], json!("code"));
}

/// `test_code_query_is_forwarded_as_retriever_config`.
#[tokio::test]
async fn the_code_query_picks_the_operation() {
    let (orchestrator, a, _, _) = world().await;
    let options = with_query(json!({"operation": "neighbors", "name": "create_order"}));
    let items = run_code(
        "unused seed text",
        None,
        Some(vec![a]),
        10,
        Some(USER),
        &orchestrator,
        Some(&options),
    )
    .await
    .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].content["raw"]["operation"], json!("neighbors"));
    assert_eq!(items[0].content["raw"]["anchor"]["id"], json!("create"));
}

/// `test_seed_not_found_contributes_nothing`: one dataset, a seed it cannot
/// resolve (raised, not softened) is no code facts.
#[tokio::test]
async fn a_seed_miss_contributes_nothing() {
    let (orchestrator, a, b, c) = world().await;
    for (dataset, query) in [
        // not_found
        (
            a,
            json!({"operation": "neighbors", "name": "nothing_like_it"}),
        ),
        // ambiguous
        (
            b,
            json!({"operation": "neighbors", "name": "billing.charge"}),
        ),
        // no_code_graph
        (c, json!({"operation": "explore"})),
    ] {
        let items = run_code(
            "Xyz",
            None,
            Some(vec![dataset]),
            10,
            Some(USER),
            &orchestrator,
            Some(&with_query(query.clone())),
        )
        .await
        .unwrap();
        assert!(items.is_empty(), "{query}: {items:?}");
    }
}

/// `test_softened_per_dataset_seed_misses_are_dropped`: over several
/// datasets the misses are softened per dataset and dropped here; the
/// dataset that resolves the seed still contributes.
#[tokio::test]
async fn softened_per_dataset_misses_are_dropped() {
    let (orchestrator, a, b, c) = world().await;
    let options = with_query(json!({"operation": "neighbors", "name": "create_order"}));
    let items = run_code(
        "",
        None,
        Some(vec![b, a, c]),
        10,
        Some(USER),
        &orchestrator,
        Some(&options),
    )
    .await
    .unwrap();
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].source, RecallSource::Code);
    assert_eq!(items[0].content["dataset_id"], json!(a.to_string()));
    assert_eq!(items[0].content["raw"].get("seed_not_found"), None);

    // Every dataset missing: no facts, not an error.
    let options = with_query(json!({"operation": "neighbors", "name": "nothing_like_it"}));
    let items = run_code(
        "",
        None,
        Some(vec![a, b]),
        10,
        Some(USER),
        &orchestrator,
        Some(&options),
    )
    .await
    .unwrap();
    assert!(items.is_empty());
}

/// An invalid operation or argument is the caller's bug and fails the lane
/// (`recall.py:1137-1142`: only `CodeSeedNotFoundError` is swallowed).
#[tokio::test]
async fn an_invalid_code_query_still_fails() {
    let (orchestrator, a, b, _) = world().await;
    for query in [
        json!({"operation": "teleport"}),
        json!({"operation": "neighbors", "limit": "many"}),
    ] {
        let error = run_code(
            "x",
            None,
            Some(vec![a, b]),
            10,
            Some(USER),
            &orchestrator,
            Some(&with_query(query.clone())),
        )
        .await
        .expect_err("a bad request fails");
        assert!(
            matches!(&error, SearchError::Code(error) if !error.is_seed_miss()),
            "{query}: {error:?}"
        );
    }
}

/// Under per-dataset storage a caller that is not a UUID, with no dataset
/// filter, reads nothing, as the graph lane does.
#[tokio::test]
async fn an_unknown_caller_without_datasets_gets_nothing() {
    let (orchestrator, _, _, _) = world().await;
    let items = run_code("create_order", None, None, 10, None, &orchestrator, None)
        .await
        .unwrap();
    assert!(items.is_empty());
    let options = RecallOptions {
        code_query: Some(Map::new()),
        ..Default::default()
    };
    let items = run_code(
        "create_order",
        None,
        None,
        10,
        Some("not-a-uuid"),
        &orchestrator,
        Some(&options),
    )
    .await
    .unwrap();
    assert!(items.is_empty());
}
