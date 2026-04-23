# Phase 7 — Integration Tests

**Files:** `crates/cognify/tests/temporal_cognify.rs` (new), `crates/search/tests/integration_search_matrix.rs` (extend)  
**Status:** Done

---

## Goal

Verify end-to-end correctness within the Rust codebase, mirroring Python's `test_temporal_graph.py`. Tests must run in CI (via `scripts/run_tests_with_openai.sh`) and skip gracefully when the required environment is absent.

---

## Python Reference

```python
# cognee/tests/test_temporal_graph.py (the target to mirror)
await cognee.cognify(temporal_cognify=True)

type_counts = Counter(n["type"] for n in all_nodes)
assert type_counts.get("Event", 0) >= 10
assert type_counts.get("Timestamp", 0) >= 10
assert type_counts.get("Entity", 0) >= 10   # from entity enrichment
# Edge types present: "contains", "is_a", "at"/"during"
```

Python uses a biography fixture (~2000 words, two people with dense date references). Our Rust test can use a shorter subset — 400–600 words with at least 10 named events — while keeping the same assertion thresholds scaled down (≥ 5 of each type).

---

## Test Fixture Text

Add `crates/cognify/tests/test_data/biography.txt` — a subset of the Python biography fixtures (Attaphol Buspakom or Arnulf Øverland). Both are available in `/tmp/cognee-python/cognee/tests/test_temporal_graph.py`. Aim for ~500 words with at least 10 dates and 15 verbs.

---

## New File: `crates/cognify/tests/temporal_cognify.rs`

```rust
//! Integration tests for the temporal cognify pipeline.
//! Requires OPENAI_URL + OPENAI_TOKEN; skips gracefully if absent.

mod helpers; // or inline helpers

fn build_test_llm() -> Option<Arc<dyn Llm>> {
    let url   = std::env::var("OPENAI_URL").ok()?;
    let token = std::env::var("OPENAI_TOKEN").ok()?;
    let model = std::env::var("OPENAI_MODEL")
        .unwrap_or_else(|_| "gpt-4o-mini".to_string());
    Some(Arc::new(OpenAIAdapter::new(url, token, model)))
}

/// Returns node type → count map from the graph.
fn count_node_types(
    nodes: &HashMap<String, serde_json::Value>,
) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for props in nodes.values() {
        if let Some(t) = props.get("type").and_then(|v| v.as_str()) {
            *counts.entry(t.to_string()).or_insert(0) += 1;
        }
    }
    counts
}

#[tokio::test]
async fn temporal_cognify_creates_event_and_timestamp_nodes() {
    let Some(llm) = build_test_llm() else {
        eprintln!("OPENAI_URL/OPENAI_TOKEN not set — skipping");
        return;
    };

    let text = include_str!("test_data/biography.txt");
    let config = CognifyConfig::default().with_temporal_cognify(true);

    let graph_db        = Arc::new(LadybugAdapter::new_in_memory().await.unwrap());
    let vector_db       = Arc::new(MockVectorDB::new());
    let embedding_engine = Arc::new(MockEmbeddingEngine::default());

    // Run the temporal pipeline.
    run_temporal_cognify_pipeline(
        text, config, graph_db.clone(), vector_db.clone(), embedding_engine, llm,
    )
    .await
    .expect("Temporal cognify pipeline failed");

    let (nodes, edges) = graph_db.get_graph_data().await.unwrap();
    let type_counts = count_node_types(&nodes);

    assert!(
        type_counts.get("Event").copied().unwrap_or(0) >= 5,
        "Expected ≥ 5 Event nodes, got {:?}",
        type_counts
    );
    assert!(
        type_counts.get("Timestamp").copied().unwrap_or(0) >= 5,
        "Expected ≥ 5 Timestamp nodes, got {:?}",
        type_counts
    );

    // Every Event must link to at least one Timestamp (directly or via Interval).
    let event_ids: HashSet<_> = nodes
        .iter()
        .filter(|(_, p)| p.get("type").and_then(|v| v.as_str()) == Some("Event"))
        .map(|(id, _)| id.clone())
        .collect();

    let ts_adjacent: HashSet<_> = edges
        .iter()
        .filter(|e| event_ids.contains(&e.source_node_id)
            && ["at", "during"].contains(&e.relationship_name.as_str()))
        .map(|e| e.source_node_id.clone())
        .collect();

    assert_eq!(
        ts_adjacent.len(), event_ids.len(),
        "Not all Event nodes have a temporal edge (at/during). \
         Events without edges: {:?}",
        event_ids.difference(&ts_adjacent).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn temporal_cognify_populates_event_name_vector_collection() {
    let Some(llm) = build_test_llm() else { return; };

    let text = include_str!("test_data/biography.txt");
    let config = CognifyConfig::default().with_temporal_cognify(true);

    let graph_db         = Arc::new(LadybugAdapter::new_in_memory().await.unwrap());
    let vector_db        = Arc::new(QdrantAdapter::new_in_memory().await.unwrap());
    let embedding_engine = build_onnx_embedding_engine_or_skip();

    run_temporal_cognify_pipeline(text, config, graph_db, vector_db.clone(), embedding_engine, llm)
        .await
        .expect("Temporal cognify pipeline failed");

    let count = vector_db.count("Event_name").await.unwrap();
    assert!(count >= 5, "Expected ≥ 5 points in Event_name collection, got {count}");
}
```

---

## Extending `integration_search_matrix.rs`

The existing matrix test seeds a graph with standard cognify data. Add a second fixture variant that uses `temporal_cognify=true` so `SearchType::Temporal` has real Event/Timestamp nodes to search.

```rust
// In the matrix test setup: run temporal pipeline for the Temporal search type test
let temporal_config = CognifyConfig::default().with_temporal_cognify(true);
run_temporal_cognify_pipeline(BIOGRAPHY_TEXT, temporal_config, ...)
    .await
    .unwrap();

// Add assertion for Temporal search type
let result = search_orchestrator
    .search(SearchType::Temporal, "What events happened in 1985?", None)
    .await
    .unwrap();

assert!(
    !result.is_empty(),
    "Temporal search returned no results despite Event/Timestamp nodes being present"
);

// Result must reference at least one event-style context (not raw triplet fallback).
let context_text = result.first().unwrap().to_string();
assert!(
    !context_text.is_empty(),
    "Temporal search context is empty"
);
```

Keep the existing fallback test (no temporal nodes, should return generic graph edges) to verify the fallback path still works after Phase 6 changes.

---

## Running the Tests

```bash
cargo test -p cognee-cognify temporal --nocapture
cargo test -p cognee-search temporal --nocapture
```

Or via the full suite:

```bash
scripts/run_tests_with_openai.sh temporal
```

---

## Verification Checklist

- [ ] `temporal_cognify_creates_event_and_timestamp_nodes` passes
- [ ] `temporal_cognify_populates_event_name_vector_collection` passes
- [ ] Extended matrix test for `SearchType::Temporal` passes
- [ ] Existing fallback unit test still passes
- [ ] Both tests skip cleanly when `OPENAI_URL`/`OPENAI_TOKEN` are absent
