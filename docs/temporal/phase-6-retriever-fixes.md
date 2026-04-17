# Phase 6 — Temporal Retriever Fixes

**File:** `crates/search/src/retrievers/temporal_retriever.rs`  
**Status:** Done

---

## Goal

Fix the `TemporalRetriever` to use the graph structure that Phase 5 creates (`Timestamp` and `Event` nodes with defined `type` properties) instead of the heuristic node detection that was a workaround for the missing pipeline.

---

## Python Reference — Retriever Logic

```python
# cognee/modules/retrieval/temporal_retriever.py
async def get_retrieved_objects(self, query):
    time_from, time_to = await self.extract_time_from_query(query)

    if time_from or time_to:
        ids = await graph_engine.collect_time_ids(time_from=time_from, time_to=time_to)
        if ids:
            events = await graph_engine.collect_events(ids=ids)
            if events:
                # vector rank + return
                ...
    # fallback
    return {"triplets": await self.get_triplets(query)}
```

**`collect_time_ids` Cypher (Neo4j):**
```cypher
MATCH (n) WHERE n.type = 'Timestamp'
  AND n.time_at >= $time_from
  AND n.time_at <= $time_to
RETURN n.id AS id
```
`time_from` / `time_to` are in **milliseconds** since epoch.

**`collect_events` Cypher:**
```cypher
UNWIND ['id1', 'id2', ...] AS uid
MATCH (start {id: uid})
MATCH (start)-[*1..2]-(event)
WHERE event.type = 'Event'
WITH DISTINCT event
RETURN collect(event) AS events
```
Traverses 1–2 hops from matching `Timestamp` nodes to reach `Event` nodes (via the optional intermediate `Interval` node: `Timestamp -[*1]-> Event` or `Timestamp -[*2 via Interval]-> Event`).

---

## Fix 1 — Replace In-Memory Graph Scan with Typed Node Query

**Current:** `get_graph_data()` loads every node/edge, then Rust filters by heuristic.

**Fix:** Use `get_filtered_graph_data` to pre-filter by `type = "Timestamp"`, then apply the `time_at` range filter in Rust (range filtering is not available on `get_filtered_graph_data`, so a post-filter step is still needed, but only over Timestamp nodes rather than the entire graph):

```rust
// Step 1: find Timestamp nodes in the interval.
let (candidate_timestamps, _) = self
    .graph_db
    .get_filtered_graph_data(HashMap::from([
        ("type".to_string(), serde_json::json!("Timestamp")),
    ]))
    .await?;

// time_at is in milliseconds — convert QueryInterval bounds from DateTime<Utc> to ms.
let interval_from_ms = interval.starts_at.map(|dt| dt.timestamp_millis());
let interval_to_ms   = interval.ends_at.map(|dt| dt.timestamp_millis());

let matching_ts_ids: HashSet<String> = candidate_timestamps
    .into_iter()
    .filter_map(|(id, props)| {
        let time_at = props.get("time_at")?.as_i64()?;
        let after_start = interval_from_ms.map_or(true, |from| time_at >= from);
        let before_end  = interval_to_ms.map_or(true, |to|   time_at <= to);
        (after_start && before_end).then_some(id)
    })
    .collect();
```

---

## Fix 2 — Hop from Timestamp to Event Nodes

Replace the old `rank_temporal_events` path with a neighbour traversal:

```rust
// Step 2: collect Event nodes reachable within 1-2 hops from matching Timestamps.
// Hop 1: direct neighbours of Timestamp (catches Event -[at]-> Timestamp).
// Hop 2: neighbours of Interval nodes (catches Event -[during]-> Interval -[time_from/to]-> Timestamp).
let mut event_node_ids = HashSet::new();

for ts_id in &matching_ts_ids {
    // Direct neighbours (hop 1)
    for (node_id, node_props) in self.graph_db.get_neighbors(ts_id).await? {
        if node_props.get("type").and_then(|v| v.as_str()) == Some("Event") {
            event_node_ids.insert(node_id);
        }
        // Hop through Interval (hop 2)
        if node_props.get("type").and_then(|v| v.as_str()) == Some("Interval") {
            for (inner_id, inner_props) in self.graph_db.get_neighbors(&node_id).await? {
                if inner_props.get("type").and_then(|v| v.as_str()) == Some("Event") {
                    event_node_ids.insert(inner_id);
                }
            }
        }
    }
}
```

Keep the existing fallback path intact: if `event_node_ids` is empty after this traversal, fall back to graph-edge results (existing behaviour).

---

## Fix 3 — `is_within_interval_unix` Helper

Add a focused helper for unix-ms comparison, replacing the old `is_within_interval` that operated on parsed `DateTime` values from node properties:

```rust
fn is_within_interval_ms(
    time_at_ms: i64,
    from_ms: Option<i64>,
    to_ms: Option<i64>,
) -> bool {
    from_ms.map_or(true, |from| time_at_ms >= from)
        && to_ms.map_or(true, |to| time_at_ms <= to)
}
```

---

## Fix 4 — Remove Dead Heuristic Code

Delete the following functions that were workarounds for the missing pipeline (they can never produce correct results once real `Event`/`Timestamp` nodes exist):

| Function | Approximate location |
|---|---|
| `is_event_node` | lines ~483–494 |
| `value_contains_event_marker` | lines ~496–502 |
| `extract_event_time` | lines ~504–523 |
| `parse_temporal_value` | lines ~525–539 |
| `parse_bound` | lines ~541–592 |
| `is_within_interval` (old DateTime-based version) | lines ~604–618 |

Keep the `QueryInterval` struct, `extract_interval` (LLM interval extraction), `rank_temporal_events` (vector ranking — still valid), and the fallback path.

---

## Fix 5 — Unit Test Updates

The existing unit tests in `temporal_retriever.rs` mock a `MockGraphDB`. Update the mock setup to:

1. Return a `Timestamp` node with `type = "Timestamp"` and `time_at` in **milliseconds** (not seconds) from `get_filtered_graph_data`.
2. Return an `Event` node with `type = "Event"` from `get_neighbors` on the Timestamp ID.
3. Remove any mock usage of the old heuristic keys (`timestamp`, `event_time`, etc.).

The test that verifies fallback behaviour (`falls_back_to_graph_context_when_interval_extraction_fails`) requires no changes.

---

## Verification

```bash
cargo check --all-targets -p cognee-search
cargo test -p cognee-search -- temporal --nocapture
```
