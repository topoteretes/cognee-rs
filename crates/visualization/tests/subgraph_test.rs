#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
//! Bounded-subgraph seed resolution, truncation and fetch — port of Python's
//! `cognee/tests/unit/modules/visualization/test_subgraph_data.py`.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;
use cognee_graph::{EdgeData, GraphDBTrait, GraphNode, MockGraphDB};
use cognee_visualization::VisualizationError;
use cognee_visualization::preprocessor::SEMANTIC_TYPE_KEY;
use cognee_visualization::subgraph::{
    BoundsError, DEFAULT_MAX_NODES, DEFAULT_NEIGHBORHOOD_DEPTH, DEFAULT_SEED_TOP_K,
    DEFAULT_WIDE_SEARCH_TOP_K, JSON_MAX_NODES, SEED_VECTOR_COLLECTIONS, STREAM_MAX_NODES,
    SeedSearchError, SeedSearcher, SeedSource, VisualizationBounds, apply_entity_types,
    fetch_visualization_graph_data, rank_seed_hits, resolve_entity_types, resolve_seed_node_ids,
    resolve_seeds_from_recall, truncate_subgraph,
};
use serde_json::{Value, json};

fn props(value: Value) -> HashMap<Cow<'static, str>, Value> {
    value
        .as_object()
        .expect("object")
        .iter()
        .map(|(k, v)| (Cow::Owned(k.clone()), v.clone()))
        .collect()
}

/// A simple 0-1-2-...-N chain.
fn chain_graph(count: usize) -> (Vec<GraphNode>, Vec<EdgeData>) {
    let nodes = (0..count)
        .map(|i| {
            (
                i.to_string(),
                props(json!({"type": "Entity", "name": format!("N{i}")})),
            )
        })
        .collect();
    let edges = (0..count.saturating_sub(1))
        .map(|i| {
            (
                i.to_string(),
                (i + 1).to_string(),
                "related_to".to_string(),
                HashMap::new(),
            )
        })
        .collect();
    (nodes, edges)
}

async fn chain_db(count: usize) -> MockGraphDB {
    let db = MockGraphDB::new();
    for i in 0..count {
        db.add_node_raw(json!({"id": i.to_string(), "type": "Entity", "name": format!("N{i}")}))
            .await
            .unwrap();
    }
    for i in 0..count.saturating_sub(1) {
        db.add_edge(&i.to_string(), &(i + 1).to_string(), "related_to", None)
            .await
            .unwrap();
    }
    db
}

/// alice -knows- bob, each `is_a` Person, a Person type node and a chunk
/// containing alice (Python `_TypedStore`).
async fn typed_store() -> MockGraphDB {
    let db = MockGraphDB::new();
    for node in [
        json!({"id": "alice", "type": "Entity", "name": "Alice"}),
        json!({"id": "bob", "type": "Entity", "name": "Bob"}),
        json!({"id": "person", "type": "EntityType", "name": "Person"}),
        json!({"id": "chunk", "type": "DocumentChunk", "text": "Alice knows Bob"}),
    ] {
        db.add_node_raw(node).await.unwrap();
    }
    for (s, t, r) in [
        ("alice", "bob", "knows"),
        ("alice", "person", "is_a"),
        ("bob", "person", "is_a"),
        ("chunk", "alice", "contains"),
    ] {
        db.add_edge(s, t, r, None).await.unwrap();
    }
    db
}

fn ids(nodes: &[GraphNode]) -> Vec<&str> {
    nodes.iter().map(|(id, _)| id.as_str()).collect()
}

/// A searcher returning fixed hits and recording what it was asked.
#[derive(Default)]
struct FakeSearcher {
    hits: Vec<String>,
    fail: bool,
    asked: Mutex<Vec<(String, Vec<String>, usize)>>,
}

#[async_trait]
impl SeedSearcher for FakeSearcher {
    async fn search_seed_node_ids(
        &self,
        query: &str,
        collections: &[&str],
        wide_search_top_k: usize,
    ) -> Result<Vec<String>, SeedSearchError> {
        self.asked.lock().unwrap().push((
            query.to_string(),
            collections.iter().map(|c| c.to_string()).collect(),
            wide_search_top_k,
        ));
        if self.fail {
            return Err("vector store unreachable".into());
        }
        Ok(self.hits.clone())
    }
}

// --- defaults and validation -------------------------------------------------

#[test]
fn defaults_match_python() {
    let bounds = VisualizationBounds::default();
    assert!(!bounds.full);
    assert_eq!(bounds.depth, 2);
    assert_eq!(bounds.seed_top_k, 10);
    assert_eq!(bounds.max_nodes, 500);
    assert_eq!(
        (
            DEFAULT_NEIGHBORHOOD_DEPTH,
            DEFAULT_SEED_TOP_K,
            DEFAULT_MAX_NODES
        ),
        (2, 10, 500)
    );
    assert!(VisualizationBounds::full_graph().full);
    assert_eq!(
        SEED_VECTOR_COLLECTIONS,
        [
            "Entity_name",
            "TextSummary_text",
            "EntityType_name",
            "DocumentChunk_text"
        ]
    );
}

#[test]
fn validate_rejects_zero_bounds() {
    for (bounds, field) in [
        (
            VisualizationBounds {
                depth: 0,
                ..Default::default()
            },
            "neighborhood_depth",
        ),
        (
            VisualizationBounds {
                seed_top_k: 0,
                ..Default::default()
            },
            "seed_top_k",
        ),
        (
            VisualizationBounds {
                max_nodes: 0,
                ..Default::default()
            },
            "max_nodes",
        ),
    ] {
        assert_eq!(
            bounds.validate(),
            Err(BoundsError::BelowMinimum { field, min: 1 })
        );
    }
    assert_eq!(VisualizationBounds::default().validate(), Ok(()));
}

#[test]
fn validate_request_matches_the_route_ranges() {
    let ok = VisualizationBounds {
        depth: 10,
        seed_top_k: 100,
        max_nodes: STREAM_MAX_NODES,
        ..Default::default()
    };
    assert_eq!(ok.validate_request(STREAM_MAX_NODES), Ok(()));
    assert_eq!(
        ok.validate_request(JSON_MAX_NODES),
        Err(BoundsError::AboveMaximum {
            field: "max_nodes",
            max: 5000
        })
    );
    let deep = VisualizationBounds {
        depth: 11,
        ..Default::default()
    };
    assert_eq!(
        deep.validate_request(JSON_MAX_NODES),
        Err(BoundsError::AboveMaximum {
            field: "neighborhood_depth",
            max: 10
        })
    );
    let wide = VisualizationBounds {
        seed_top_k: 101,
        ..Default::default()
    };
    assert_eq!(
        wide.validate_request(JSON_MAX_NODES),
        Err(BoundsError::AboveMaximum {
            field: "neighborhood_seed_top_k",
            max: 100
        })
    );
    let zero = VisualizationBounds {
        seed_top_k: 0,
        ..Default::default()
    };
    assert_eq!(
        zero.validate_request(JSON_MAX_NODES),
        Err(BoundsError::BelowMinimum {
            field: "neighborhood_seed_top_k",
            min: 1
        })
    );
}

#[tokio::test]
async fn fetch_validates_bounds() {
    let db = MockGraphDB::new();
    for bounds in [
        VisualizationBounds {
            depth: 0,
            ..Default::default()
        },
        VisualizationBounds {
            seed_top_k: 0,
            ..Default::default()
        },
        VisualizationBounds {
            max_nodes: 0,
            ..Default::default()
        },
    ] {
        let err = fetch_visualization_graph_data(&db, &bounds, None)
            .await
            .unwrap_err();
        assert!(matches!(err, VisualizationError::InvalidBounds(_)), "{err}");
    }
}

// --- recall seed extraction ------------------------------------------------

#[test]
fn recall_node_ids_dict() {
    assert_eq!(
        resolve_seeds_from_recall(&json!({"node_ids": ["a", "b", "a"]})),
        vec!["a", "b"]
    );
}

#[test]
fn recall_qa_entries_provenance() {
    let entries = json!([
        {"used_graph_element_ids": {"node_ids": ["9", "8"]}},
        {"used_graph_element_ids": {"node_ids": ["8", "7"]}},
    ]);
    assert_eq!(resolve_seeds_from_recall(&entries), vec!["9", "8", "7"]);
}

#[test]
fn recall_dict_items_skip_those_without_provenance() {
    let items = json!([
        {"used_graph_element_ids": {"node_ids": ["9", "8"]}},
        {"text": "no provenance"},
    ]);
    assert_eq!(resolve_seeds_from_recall(&items), vec!["9", "8"]);
    // A single QA entry as a mapping.
    assert_eq!(
        resolve_seeds_from_recall(&json!({"used_graph_element_ids": ["x"]})),
        vec!["x"]
    );
}

#[test]
fn recall_ignores_shapes_without_node_ids() {
    let item = json!([{"text": "an answer", "metadata": {"chunk_id": "c1"}}]);
    assert!(resolve_seeds_from_recall(&item).is_empty());
    assert!(resolve_seeds_from_recall(&Value::Null).is_empty());
}

#[test]
fn recall_malformed_node_ids_returns_empty() {
    assert!(resolve_seeds_from_recall(&json!({"node_ids": 5})).is_empty());
    assert!(resolve_seeds_from_recall(&json!({"node_ids": "a1b2"})).is_empty());
}

// --- query seed ranking ----------------------------------------------------

#[test]
fn query_hits_rank_by_best_distance() {
    // "a" appears in two collections; its best (lowest) score ranks it first.
    let hits = vec![
        ("c".to_string(), Some(0.9)),
        ("a".to_string(), Some(0.1)),
        ("b".to_string(), Some(0.5)),
        ("a".to_string(), Some(0.3)),
        (String::new(), Some(0.0)),
        ("d".to_string(), None),
    ];
    assert_eq!(rank_seed_hits(hits), vec!["a", "b", "c", "d"]);
}

// --- seed priority ---------------------------------------------------------

#[tokio::test]
async fn seed_priority_explicit_over_recall_query_degree() {
    let db = chain_db(5).await;
    let searcher = FakeSearcher {
        hits: vec!["q1".to_string()],
        ..Default::default()
    };
    let bounds = VisualizationBounds {
        seed_node_ids: vec!["explicit".to_string(), "explicit".to_string()],
        recall_node_ids: vec!["r1".to_string()],
        query: Some("ignored".to_string()),
        ..Default::default()
    };
    let resolved = resolve_seed_node_ids(&db, &bounds, Some(&searcher))
        .await
        .unwrap();
    assert_eq!(
        resolved,
        (vec!["explicit".to_string()], SeedSource::Explicit)
    );
    assert!(searcher.asked.lock().unwrap().is_empty());
}

#[tokio::test]
async fn seed_priority_recall_over_query() {
    let db = chain_db(5).await;
    let searcher = FakeSearcher {
        hits: vec!["q1".to_string()],
        ..Default::default()
    };
    let bounds = VisualizationBounds {
        recall_node_ids: vec!["r1".to_string(), "r2".to_string(), "r3".to_string()],
        query: Some("ignored".to_string()),
        seed_top_k: 2,
        ..Default::default()
    };
    let resolved = resolve_seed_node_ids(&db, &bounds, Some(&searcher))
        .await
        .unwrap();
    assert_eq!(
        resolved,
        (vec!["r1".to_string(), "r2".to_string()], SeedSource::Recall)
    );
}

#[tokio::test]
async fn query_seeds_use_the_four_collections_and_top_k() {
    let db = chain_db(5).await;
    let searcher = FakeSearcher {
        hits: vec!["3".into(), "4".into(), "3".into(), "1".into()],
        ..Default::default()
    };
    let bounds = VisualizationBounds {
        query: Some("python".to_string()),
        seed_top_k: 2,
        ..Default::default()
    };
    let resolved = resolve_seed_node_ids(&db, &bounds, Some(&searcher))
        .await
        .unwrap();
    assert_eq!(
        resolved,
        (vec!["3".to_string(), "4".to_string()], SeedSource::Query)
    );
    let asked = searcher.asked.lock().unwrap();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].0, "python");
    assert_eq!(asked[0].1, SEED_VECTOR_COLLECTIONS);
    assert_eq!(asked[0].2, DEFAULT_WIDE_SEARCH_TOP_K);
}

#[tokio::test]
async fn a_query_without_hits_or_searcher_falls_through_to_degree() {
    let db = chain_db(3).await;
    let bounds = VisualizationBounds {
        query: Some("nothing".to_string()),
        seed_top_k: 1,
        ..Default::default()
    };
    let empty = FakeSearcher::default();
    let (seeds, source) = resolve_seed_node_ids(&db, &bounds, Some(&empty))
        .await
        .unwrap();
    assert_eq!(source, SeedSource::Degree);
    // The middle of a 3-chain has degree 2.
    assert_eq!(seeds, vec!["1"]);
    let (_, source) = resolve_seed_node_ids(&db, &bounds, None).await.unwrap();
    assert_eq!(source, SeedSource::Degree);
}

#[tokio::test]
async fn a_failed_seed_search_is_an_error() {
    let db = chain_db(3).await;
    let failing = FakeSearcher {
        fail: true,
        ..Default::default()
    };
    let bounds = VisualizationBounds {
        query: Some("q".to_string()),
        ..Default::default()
    };
    let err = fetch_visualization_graph_data(&db, &bounds, Some(&failing))
        .await
        .unwrap_err();
    assert!(matches!(err, VisualizationError::SeedSearch(_)), "{err}");
}

#[tokio::test]
async fn seed_none_on_empty_graph() {
    let db = MockGraphDB::new();
    let resolved = resolve_seed_node_ids(&db, &VisualizationBounds::default(), None)
        .await
        .unwrap();
    assert_eq!(resolved, (vec![], SeedSource::None));
    assert_eq!(SeedSource::None.as_str(), "none");
    assert_eq!(
        serde_json::to_value(SeedSource::Explicit).unwrap(),
        json!("explicit")
    );
}

// --- truncation ------------------------------------------------------------

#[test]
fn truncate_caps_keeps_seeds_and_drops_dangling_edges() {
    let (nodes, edges) = chain_graph(6);
    let ((kept_nodes, kept_edges), truncated) =
        truncate_subgraph(nodes, edges, &["0".to_string()], 3);
    assert!(truncated);
    let kept: HashSet<&str> = ids(&kept_nodes).into_iter().collect();
    assert_eq!(kept, HashSet::from(["0", "1", "2"]));
    assert!(
        kept_edges
            .iter()
            .all(|e| kept.contains(e.0.as_str()) && kept.contains(e.1.as_str()))
    );
}

#[test]
fn truncate_orders_by_hop_then_input_order() {
    let (nodes, edges) = chain_graph(6);
    let ((kept_nodes, _), _) = truncate_subgraph(nodes, edges, &["3".to_string()], 4);
    assert_eq!(ids(&kept_nodes), vec!["3", "2", "4", "1"]);
}

#[test]
fn truncate_noop_under_cap() {
    let (nodes, edges) = chain_graph(4);
    let ((kept_nodes, kept_edges), truncated) =
        truncate_subgraph(nodes.clone(), edges.clone(), &["0".to_string()], 500);
    assert!(!truncated);
    assert_eq!((kept_nodes, kept_edges), (nodes, edges));
}

// --- fetch orchestration ---------------------------------------------------

#[tokio::test]
async fn fetch_full_graph_is_get_graph_data_unchanged() {
    let db = chain_db(5).await;
    let graph = fetch_visualization_graph_data(&db, &VisualizationBounds::full_graph(), None)
        .await
        .unwrap();
    let (nodes, edges) = db.get_graph_data().await.unwrap();
    assert_eq!((graph.nodes, graph.edges), (nodes, edges));
    assert!(graph.meta.is_none());
}

#[tokio::test]
async fn fetch_subgraph_expands_explicit_seeds() {
    let db = chain_db(20).await;
    let bounds = VisualizationBounds {
        seed_node_ids: vec!["10".to_string()],
        ..Default::default()
    };
    let graph = fetch_visualization_graph_data(&db, &bounds, None)
        .await
        .unwrap();
    // The seed first, then nodes by hop distance (neighbours by id).
    assert_eq!(ids(&graph.nodes), vec!["10", "11", "9", "12", "8"]);
    assert_eq!(graph.edges.len(), 4);
    let meta = graph.meta.unwrap();
    assert_eq!(meta.seeds, vec!["10"]);
    assert_eq!(meta.seed_source, SeedSource::Explicit);
    assert_eq!((meta.depth, meta.max_nodes), (2, 500));
}

#[tokio::test]
async fn fetch_caps_an_oversized_neighbourhood() {
    let db = chain_db(20).await;
    let bounds = VisualizationBounds {
        seed_node_ids: vec!["0".to_string()],
        depth: 5,
        max_nodes: 4,
        ..Default::default()
    };
    let graph = fetch_visualization_graph_data(&db, &bounds, None)
        .await
        .unwrap();
    let kept: HashSet<&str> = ids(&graph.nodes).into_iter().collect();
    assert_eq!(graph.nodes.len(), 4);
    assert!(kept.contains("0"));
    assert!(
        graph
            .edges
            .iter()
            .all(|e| kept.contains(e.0.as_str()) && kept.contains(e.1.as_str()))
    );
}

#[tokio::test]
async fn fetch_uses_query_seeds() {
    let db = chain_db(6).await;
    let searcher = FakeSearcher {
        hits: vec!["3".into(), "4".into()],
        ..Default::default()
    };
    let bounds = VisualizationBounds {
        query: Some("python".to_string()),
        depth: 1,
        ..Default::default()
    };
    let graph = fetch_visualization_graph_data(&db, &bounds, Some(&searcher))
        .await
        .unwrap();
    assert_eq!(ids(&graph.nodes), vec!["3", "4", "2", "5"]);
    assert_eq!(graph.meta.unwrap().seed_source, SeedSource::Query);
}

#[tokio::test]
async fn fetch_no_seeds_renders_empty() {
    let db = MockGraphDB::new();
    let graph = fetch_visualization_graph_data(&db, &VisualizationBounds::default(), None)
        .await
        .unwrap();
    assert!(graph.nodes.is_empty() && graph.edges.is_empty());
    assert_eq!(graph.meta.unwrap().seed_source, SeedSource::None);
}

// --- semantic types outside the read (SDK-794) -----------------------------

#[tokio::test]
async fn neighbours_keep_their_type_when_the_type_node_is_outside_the_read() {
    let db = typed_store().await;
    let bounds = VisualizationBounds {
        seed_node_ids: vec!["chunk".to_string()],
        depth: 1,
        ..Default::default()
    };
    let graph = fetch_visualization_graph_data(&db, &bounds, None)
        .await
        .unwrap();
    let by_id: HashMap<&str, _> = graph.nodes.iter().map(|(i, d)| (i.as_str(), d)).collect();
    assert!(!by_id.contains_key("person"));
    assert_eq!(by_id["alice"][SEMANTIC_TYPE_KEY], json!("Person"));
    assert!(!by_id["chunk"].contains_key(SEMANTIC_TYPE_KEY));
}

#[tokio::test]
async fn a_budget_cut_never_drops_an_entity_type() {
    let db = typed_store().await;
    let bounds = VisualizationBounds {
        seed_node_ids: vec!["chunk".to_string()],
        depth: 2,
        max_nodes: 2,
        ..Default::default()
    };
    let graph = fetch_visualization_graph_data(&db, &bounds, None)
        .await
        .unwrap();
    let by_id: HashMap<&str, _> = graph.nodes.iter().map(|(i, d)| (i.as_str(), d)).collect();
    assert_eq!(
        by_id.keys().copied().collect::<HashSet<_>>(),
        HashSet::from(["chunk", "alice"])
    );
    assert_eq!(by_id["alice"][SEMANTIC_TYPE_KEY], json!("Person"));
}

#[tokio::test]
async fn resolve_entity_types_only_looks_up_entities() {
    let db = typed_store().await;
    let (mut nodes, _) = db.get_graph_data().await.unwrap();
    let types = resolve_entity_types(&db, &nodes).await.unwrap();
    assert_eq!(
        types,
        HashMap::from([
            ("alice".to_string(), "Person".to_string()),
            ("bob".to_string(), "Person".to_string()),
        ])
    );
    apply_entity_types(&mut nodes, &types);
    for (id, data) in &nodes {
        assert_eq!(
            data.contains_key(SEMANTIC_TYPE_KEY),
            id == "alice" || id == "bob",
            "{id}"
        );
    }
    // No entity: no store call, an empty map.
    let only_chunk: Vec<GraphNode> = nodes.into_iter().filter(|(id, _)| id == "chunk").collect();
    assert_eq!(
        resolve_entity_types(&db, &only_chunk).await,
        Some(HashMap::new())
    );
}
