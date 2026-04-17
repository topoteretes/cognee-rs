# Phase 5 — Cognify Pipeline Stages

**Files:** `crates/cognify/src/tasks.rs`, `crates/cognify/src/pipeline.rs`, `crates/cli/src/main.rs`  
**Status:** Done

---

## Goal

Wire the extractors from Phases 3–4 into a complete temporal cognify pipeline and expose it through the CLI.

---

## Python Reference — Pipeline Structure

A critical correction from the original plan: in Python, `temporal_cognify=True` runs an **entirely separate pipeline** that replaces the standard one — it does not append stages to it:

```python
# cognee/api/v1/cognify/cognify.py lines 224-241
if temporal_cognify:
    tasks = await get_temporal_tasks(user, chunker, chunk_size, chunks_per_batch)
else:
    tasks = await get_default_tasks(...)
```

```python
# get_temporal_tasks() pipeline order:
1. classify_documents
2. extract_chunks_from_documents
3. extract_events_and_timestamps      # ← adds Event/Timestamp DataPoints to chunk.contains
4. extract_knowledge_graph_from_events # ← enriches events in chunk.contains with entities
5. add_data_points                     # ← persists all DataPoints (incl. events) to graph+vector
```

The standard entity/KG extraction (`extract_graph_from_data`, `summarize_text`) does **not** run in temporal mode. The Rust implementation must follow the same pattern.

---

## New Task Functions in `tasks.rs`

### `extract_temporal_events`

Replaces `extract_graph_from_data` and `summarize_text` in the temporal pipeline.

```rust
pub async fn extract_temporal_events(
    input: &ExtractedChunks,
    llm: Arc<dyn Llm>,
    config: &CognifyConfig,
) -> Result<Vec<TemporalEvent>, CognifyError>
```

**Implementation:**

1. Collect all non-DLT `DocumentChunk`s from `input`.
2. Batch chunks by `config.data_per_batch` (default: 20).
3. For each batch, run `TemporalEventExtractor::extract_events` on each chunk in parallel (bounded semaphore with capacity `config.max_parallel_extractions`).
4. Flatten all per-chunk results into a single `Vec<TemporalEvent>`.
5. Call `TemporalEntityEnricher::enrich(batch_events)` on each batch.
6. Return flattened results.

### `add_temporal_data_points`

Replaces `add_data_points` in the temporal pipeline. Persists `TemporalEvent`, `CognifyTimestamp`, `CognifyInterval`, and entity nodes to the graph and vector databases.

```rust
pub async fn add_temporal_data_points(
    events: &[TemporalEvent],
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
) -> Result<(), CognifyError>
```

**For each event:**

1. **Create `Event` graph node:**
   ```
   id   = uuid5(NAMESPACE_OID, "event:{event.name}")
   type = "Event"
   name = event.name
   description = event.description (may be None)
   location = event.location (may be None)
   ```

2. **Create `Timestamp` graph node(s):**
   - For `event.at`:
     ```
     id           = uuid5(NAMESPACE_OID, "timestamp:{time_at}")
     type         = "Timestamp"
     time_at      = timestamp.time_at  (i64 milliseconds — matches Python)
     timestamp_str = timestamp.timestamp_str
     year/month/day/hour/minute/second = ...
     ```
   - Edge: `Event -[at]-> Timestamp`

   - For `event.during` (creates an intermediate `Interval` node — matches Python exactly):
     ```
     Timestamp node for time_from
     Timestamp node for time_to
     Interval node:
       id   = uuid5(NAMESPACE_OID, "interval:{ts_from.time_at}:{ts_to.time_at}")
       type = "Interval"
     Edges:
       Event    -[during]->    Interval
       Interval -[time_from]-> Timestamp(from)
       Interval -[time_to]->   Timestamp(to)
     ```
     **Note:** The original plan incorrectly described direct `Event -[starts_at/ends_at]-> Timestamp` edges. Python stores an intermediate `Interval` node. This is required for cross-SDK compatibility.

3. **Create entity attribute nodes and edges:**
   For each `EventAttribute` in `event.attributes`:
   - Look up existing entity node by name in the graph (to avoid duplicates).
   - If not found, create:
     ```
     id   = uuid5(NAMESPACE_OID, "entity:{attribute.entity}")
     type = attribute.entity_type
     name = attribute.entity
     ```
   - Edge: `Event -[{attribute.relationship}]-> Entity`

4. **Index in vector DB:**
   - Embed `event.name` with `embedding_engine`.
   - Upsert to collection `"Event_name"` with point ID = event UUID.
   - Follow the same batching pattern as `generate_embeddings` in the standard pipeline.

---

## Pipeline Wiring in `pipeline.rs`

Add `build_temporal_cognify_pipeline` as a parallel function to `build_cognify_pipeline`:

```rust
pub fn build_temporal_cognify_pipeline(
    llm: Arc<dyn Llm>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    config: CognifyConfig,
) -> Pipeline {
    PipelineBuilder::new_with_task("temporal-cognify", make_classify_documents_task())
        .add_task(make_extract_chunks_task(/* same args as standard */))
        .add_task(make_extract_temporal_events_task(llm, config))
        .add_task(make_add_temporal_data_points_task(graph_db, vector_db, embedding_engine))
        .with_name("temporal-cognify")
        .build()
}
```

In `CognifyPipeline::run` (or wherever the pipeline is selected), check the flag:

```rust
let pipeline = if config.temporal_cognify {
    build_temporal_cognify_pipeline(llm, graph_db, vector_db, embedding_engine, config)
} else {
    build_cognify_pipeline(llm, graph_db, vector_db, embedding_engine, db, ontology_resolver, config)
};
```

---

## CLI Flag

**File:** `crates/cli/src/main.rs` (cognify subcommand)

Add `--temporal-cognify` boolean flag:

```rust
#[arg(long, default_value_t = false)]
temporal_cognify: bool,
```

Map it to `CognifyConfig::with_temporal_cognify(true)` when building the config.

Also propagate through the `run-sequence` subcommand if it passes cognify config options.

---

## Verification

```bash
cargo check --all-targets
```

Run with a mock LLM (unit test, no OpenAI key required) to verify the pipeline compiles and the flag routes to the correct builder.
