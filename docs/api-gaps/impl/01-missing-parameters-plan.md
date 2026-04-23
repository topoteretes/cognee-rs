# Implementation Plan: Missing Parameters on Existing Functions

Detailed step-by-step implementation plan for each confirmed gap in [../01-missing-parameters.md](../01-missing-parameters.md).

---

## Implementation Order

Gaps are ordered by dependency and priority. Items within the same phase can be implemented in parallel.

### Phase 1: Low-hanging fruit (no new abstractions)

1. **search: `node_name_filter_operator`** -- wire existing field from SearchRequest to SearchParams
2. **add: `node_set`** -- wire existing Data.node_set field from AddPipeline parameter
3. **add: `dataset_id`** -- add alternate lookup path
4. **add: `incremental_loading`** -- add bypass flag for deduplication
5. **search: `neighborhood_depth` / `neighborhood_seed_top_k`** -- add two fields

### Phase 2: Medium complexity

6. **add: `data_per_batch`** -- add batching to sequential loop
7. **add: `importance_weight`** -- new field on Data model + migration
8. **add: `preferred_loaders`** -- extend loader registry with override
9. **cognify: `datasets` UUID resolution** -- extend DatasetResolver
10. **cognify: `graph_model`** -- wire existing GraphModel trait through pipeline
11. **cognify: `chunker`** -- make ChunkStrategy trait-based or extensible

### Phase 3: Cross-cutting (requires new pattern)

12. **BackendOverrides** -- `vector_db_config` / `graph_db_config` on add, cognify, memify
13. **memify: `extraction_tasks` / `enrichment_tasks` / `data`** -- pluggable task pipeline

---

## Phase 1: Low-Hanging Fruit

### 1.1 search: `node_name_filter_operator`

The `SearchParams` struct already has `node_name_filter_operator: Option<String>` (line 34 of `search_params.rs`) and retrievers already consume it. The only missing piece is wiring it from `SearchRequest`.

**Files to modify:**

- `crates/search/src/types/search_request.rs` (line ~12)
- `crates/search/src/types/search_params.rs` (line 79)

**Step 1:** Add field to `SearchRequest`:

```rust
// In SearchRequest struct, after `node_name`:
pub node_name_filter_operator: Option<String>,
```

**Step 2:** Wire in `From<&SearchRequest> for SearchParams`:

```rust
// Replace line 79:
//   node_name_filter_operator: None, // not yet in SearchRequest
// With:
node_name_filter_operator: req.node_name_filter_operator.clone(),
```

**Step 3:** Update CLI search command if it constructs `SearchRequest` manually.

**Tests:** Update existing `SearchRequest` deserialization tests to include `node_name_filter_operator`.

---

### 1.2 add: `node_set`

The `Data.node_set` field (line 44 of `crates/models/src/data.rs`) exists and the database column exists. Downstream document classification already parses `external_metadata.node_set` into `source_node_set`/`belongs_to_set`. The gap is that `AddPipeline::add()` never populates `Data.node_set`.

**Files to modify:**

- `crates/ingestion/src/pipeline.rs` -- `AddPipeline::add()` signature (line 713), `process_input()` (line 117), `ProcessedInput` struct (line 91), `persist_data_with_acl()` (line 307), `build_add_pipeline*` functions, `make_process_input_task`, `make_persist_data_task*`
- `crates/models/src/data.rs` -- `DataBuilder` (add `node_set` setter)

**Step 1:** Add `node_set: Option<Vec<String>>` to `AddPipeline::add()`:

```rust
pub async fn add(
    &self,
    inputs: Vec<DataInput>,
    dataset_name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    node_set: Option<Vec<String>>,  // NEW
) -> Result<Vec<Data>, Box<dyn std::error::Error>>
```

**Step 2:** Add `node_set: Option<String>` to `ProcessedInput` (JSON-serialized).

**Step 3:** In `persist_data_with_acl()`, pass `node_set` to `DataBuilder`:

```rust
if let Some(ref ns) = processed.node_set {
    data_builder = data_builder.node_set(ns.clone());
}
```

**Step 4:** Add `node_set(String)` builder method to `DataBuilder`.

**Step 5:** Thread `node_set` through `make_process_input_task`, `make_persist_data_task*`, and `build_add_pipeline*`.

**Step 6:** In Python, `node_set` is passed to `ingest_data()` which stores it on the Data record. The Rust should serialize `Vec<String>` to a JSON string for the `Data.node_set` field (matches Python behavior where it's stored as a JSON string in SQLite).

**Tests:** Add test verifying `node_set` is stored and retrievable.

---

### 1.3 add: `dataset_id`

**Files to modify:**

- `crates/ingestion/src/pipeline.rs` -- `persist_data_with_acl()` (line 307), `AddPipeline::add()` (line 713)
- `crates/database/src/traits/ingest_db.rs` -- add `get_dataset(Uuid)` method
- `crates/database/src/ops/datasets.rs` -- `get_dataset()` already exists (line 24), just needs trait exposure

**Step 1:** Add `get_dataset(id: Uuid)` to `IngestDb` trait:

```rust
async fn get_dataset(&self, id: Uuid) -> Result<Option<Dataset>, DatabaseError>;
```

**Step 2:** Implement for `DatabaseConnection` using existing `ops::datasets::get_dataset()`.

**Step 3:** Add `dataset_id: Option<Uuid>` parameter to `AddPipeline::add()` and `persist_data_with_acl()`. When `Some`, use `get_dataset(id)` instead of `get_dataset_by_name()`.

**Step 4:** Thread through pipeline builder functions.

**Tests:** Add test verifying dataset_id lookup works.

---

### 1.4 add: `incremental_loading`

**Files to modify:**

- `crates/ingestion/src/pipeline.rs` -- `persist_data_with_acl()` (line 307, around line 350 where deduplication happens)

**Step 1:** Add `incremental: bool` parameter (default `true`) to `persist_data_with_acl()` and `AddPipeline::add()`.

**Step 2:** In `persist_data_with_acl()`, when `incremental == false`, skip the `get_data(data_id)` check that returns early for existing data. Instead, always create or update the record.

Current dedup logic (line 350-354):
```rust
if let Some(existing_data) = database.get_data(data_id).await? {
    database.attach_data_to_dataset(dataset.id, data_id).await?;
    info!(data_id = %data_id, is_duplicate = true, "input processed");
    return Ok(existing_data);
}
```

When `incremental == false`, skip this block entirely (or delete + recreate).

**Step 3:** Thread through pipeline builder functions and task wrappers.

**Tests:** Add test that with `incremental=false`, same content is re-processed.

---

### 1.5 search: `neighborhood_depth` / `neighborhood_seed_top_k`

**Files to modify:**

- `crates/search/src/types/search_request.rs` -- add fields
- `crates/search/src/types/search_params.rs` -- add fields and wire from SearchRequest
- `crates/search/src/retrievers/graph_completion_retriever.rs` -- use the params
- `crates/search/src/retrievers/advanced_graph_retrievers.rs` -- use the params

**Step 1:** Add to `SearchRequest`:

```rust
pub neighborhood_depth: Option<usize>,
pub neighborhood_seed_top_k: Option<usize>,
```

**Step 2:** Add to `SearchParams`:

```rust
pub neighborhood_depth: Option<usize>,
pub neighborhood_seed_top_k: Option<usize>,
```

**Step 3:** Wire in `From<&SearchRequest> for SearchParams`:

```rust
neighborhood_depth: req.neighborhood_depth,
neighborhood_seed_top_k: req.neighborhood_seed_top_k,
```

**Step 4:** Use in graph-based retrievers to control BFS/DFS expansion from seed nodes. `neighborhood_seed_top_k` limits how many top vector-search results become graph traversal seeds. `neighborhood_depth` limits traversal hops from each seed.

**Tests:** Add deserialization test and integration test if graph retriever supports these.

---

## Phase 2: Medium Complexity

### 2.1 add: `data_per_batch`

**Files to modify:**

- `crates/ingestion/src/pipeline.rs` -- `AddPipeline::add()` (line 713)

**Step 1:** Add `batch_size: Option<usize>` parameter to `AddPipeline::add()`.

**Step 2:** Replace the sequential `for input in &inputs` loop with batched processing:

```rust
let batch_size = batch_size.unwrap_or(20);
for batch in inputs.chunks(batch_size) {
    for input in batch {
        // existing processing logic
    }
}
```

Initially this is a structural change for future parallelism (processing within a batch concurrently).

**Tests:** Verify batch boundary behavior with various input sizes.

---

### 2.2 add: `importance_weight`

**Files to modify:**

- `crates/models/src/data.rs` -- add `importance_weight: Option<f64>` field
- `crates/database/src/entities/data.rs` -- add column
- `crates/database/src/migrations/` -- add migration for new column
- `crates/database/src/conversions.rs` -- wire conversion
- `crates/ingestion/src/pipeline.rs` -- accept and store the value

**Step 1:** Add field to `Data`:

```rust
pub importance_weight: Option<f64>,
```

**Step 2:** Add database migration (nullable `REAL` column, default `NULL`).

**Step 3:** Add to `DataBuilder`:

```rust
pub fn importance_weight(mut self, w: f64) -> Self {
    self.importance_weight = Some(w);
    self
}
```

**Step 4:** Wire through ingestion pipeline.

**Step 5:** Future: use in search ranking (multiply relevance scores by importance_weight).

**Tests:** Verify field is stored and retrieved correctly.

---

### 2.3 add: `preferred_loaders`

**Files to modify:**

- `crates/ingestion/src/pipeline.rs` -- accept parameter
- `crates/ingestion/src/loader_registry.rs` -- add override mechanism

**Step 1:** Define a type for loader preferences:

```rust
/// Maps file extensions or MIME types to preferred loader names.
pub type LoaderPreferences = HashMap<String, String>;
```

**Step 2:** Add parameter to `AddPipeline::add()`:

```rust
preferred_loaders: Option<LoaderPreferences>,
```

**Step 3:** In `extract_file_metadata()` / `get_loader_name()`, check preferred_loaders first before falling back to the default registry.

**Tests:** Verify override takes precedence.

---

### 2.4 cognify: `datasets` UUID resolution

**Files to modify:**

- `crates/cognify/src/dataset_resolver.rs` -- extend `DatasetResolver` trait and `cognify_datasets()`

**Step 1:** Create a `DatasetRef` enum:

```rust
pub enum DatasetRef {
    Name(String),
    Id(Uuid),
}
```

**Step 2:** Extend `DatasetResolver` trait with:

```rust
async fn resolve_dataset_by_id(
    &self,
    id: Uuid,
    user_id: Uuid,
    permission: &str,
) -> Result<Option<Dataset>, CognifyError>;
```

**Step 3:** Update `cognify_datasets()` to accept `Vec<DatasetRef>` instead of `Vec<String>`:

```rust
pub async fn cognify_datasets(
    dataset_refs: Vec<DatasetRef>,
    // ... rest unchanged
)
```

**Step 4:** Route each ref to the appropriate resolver method.

**Tests:** Verify UUID-based resolution works alongside name-based.

---

### 2.5 cognify: `graph_model` (custom extraction schema)

This is the most significant gap. The Rust `GraphModel` trait and generic `FactExtractor::extract::<M>()` already exist, but the pipeline functions are hardcoded to `KnowledgeGraph`.

**Files to modify:**

- `crates/cognify/src/tasks.rs` -- `extract_graph_from_data()` (line 308), `cognify()` (line 1718)
- `crates/cognify/src/config.rs` -- add graph schema option

**Approach A (trait-based, complex):** Make `cognify()` and `extract_graph_from_data()` generic over `M: GraphModel`. This requires monomorphization which conflicts with `dyn Trait` patterns.

**Approach B (JSON Schema, recommended):** Add an optional JSON Schema to `CognifyConfig` that is used as the LLM structured output schema instead of the default `KnowledgeGraph` schema. When a custom schema is provided, the extracted JSON is stored directly in chunk metadata (matching Python's behavior for non-KnowledgeGraph models).

**Step 1:** Add to `CognifyConfig`:

```rust
/// Optional JSON Schema for custom graph extraction model.
/// When Some, uses this schema instead of the default KnowledgeGraph.
/// Extracted data is stored as-is in chunk metadata.
pub graph_schema: Option<serde_json::Value>,
```

**Step 2:** In `extract_graph_from_data()`, branch on `config.graph_schema`:

```rust
if let Some(ref schema) = config.graph_schema {
    // Use LLM's create_structured_output_with_messages_raw() with custom schema
    // Store result in DocumentChunk.contains as serialized JSON
} else {
    // Existing KnowledgeGraph extraction flow
}
```

This mirrors the Python branching at `extract_graph_from_data.py:99-103`.

**Step 3:** Add `with_graph_schema()` builder method.

**Tests:** Add test with a custom schema to verify extraction stores JSON in chunk metadata.

---

### 2.6 cognify: `chunker` (pluggable chunker)

**Files to modify:**

- `crates/cognify/src/config.rs` -- expand `ChunkStrategy`
- `crates/chunking/src/lib.rs` -- add trait for custom chunkers (optional)

**Approach:** The current `ChunkStrategy` enum with `Paragraph` and `Recursive` covers the two Python chunkers (`TextChunker` and `LangchainChunker`). For full parity, we could either:

A. Add more enum variants as needed (simpler)
B. Make `ChunkStrategy` support a `Custom(Arc<dyn Chunker>)` variant (more flexible)

**Step 1 (minimal):** Document that `ChunkStrategy::Paragraph` = Python `TextChunker` and `ChunkStrategy::Recursive` = Python `LangchainChunker`. This may be sufficient for now.

**Step 2 (full parity):** Define a `Chunker` trait:

```rust
#[async_trait]
pub trait Chunker: Send + Sync {
    fn chunk_text<'a>(&self, text: &'a str, max_tokens: usize) -> Vec<TextChunk<'a>>;
}
```

Add `Custom(Arc<dyn Chunker>)` variant to `ChunkStrategy`.

---

## Phase 3: Cross-Cutting

### 3.1 BackendOverrides (vector_db_config / graph_db_config)

Affects: `add()`, `cognify()`, `memify()` -- 6 parameters total across 3 functions.

**Files to modify:**

- New type definition (suggest `crates/models/src/backend_overrides.rs` or in `cognee-lib`)
- `crates/ingestion/src/pipeline.rs`
- `crates/cognify/src/tasks.rs`
- `crates/cognify/src/memify/pipeline.rs`

**Step 1:** Define `BackendConfig`:

```rust
/// Configuration for dynamically creating a backend from a dict-like config.
/// Mirrors Python's vector_db_config / graph_db_config parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    pub provider: String,
    pub params: HashMap<String, serde_json::Value>,
}
```

**Step 2:** Define `BackendOverrides`:

```rust
pub struct BackendOverrides {
    pub vector_db: Option<Arc<dyn VectorDB>>,
    pub graph_db: Option<Arc<dyn GraphDBTrait>>,
}
```

**Step 3:** Add a factory function that creates backend instances from `BackendConfig`:

```rust
pub fn create_vector_db_from_config(config: &BackendConfig) -> Result<Arc<dyn VectorDB>, Error>;
pub fn create_graph_db_from_config(config: &BackendConfig) -> Result<Arc<dyn GraphDBTrait>, Error>;
```

**Step 4:** Accept `Option<BackendOverrides>` in `AddPipeline::add()`, `cognify()`, and `memify()`. When present, use the override instead of the default component.

**Prerequisite:** Requires a factory/registry that can instantiate backends from config dicts. This is a larger architectural addition.

---

### 3.2 memify: `extraction_tasks` / `enrichment_tasks` / `data`

**Files to modify:**

- `crates/cognify/src/memify/pipeline.rs`
- `crates/cognify/src/memify/config.rs`

**Step 1:** The Rust `cognee-core` crate already has `Task` and `Pipeline` abstractions. Add to `MemifyConfig`:

```rust
pub custom_extraction_tasks: Option<Vec<TypedTask<..>>>,
pub custom_enrichment_tasks: Option<Vec<TypedTask<..>>>,
```

However, `TypedTask` is generic and cannot be stored in a non-generic config easily. Alternative: accept tasks as function parameters rather than config.

**Step 2 (recommended):** Change `memify()` signature:

```rust
pub async fn memify(
    graph_db: &dyn GraphDBTrait,
    vector_db: &dyn VectorDB,
    embedding_engine: &dyn EmbeddingEngine,
    dataset_id: Option<Uuid>,
    user_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
    config: &MemifyConfig,
    custom_data: Option<Vec<serde_json::Value>>,  // NEW: skip graph read
    custom_pipeline: Option<Pipeline>,             // NEW: override default pipeline
) -> Result<MemifyResult, MemifyError>
```

When `custom_data` is `Some`, skip the `extract_triplets_from_graph_db()` call and use the provided data.

When `custom_pipeline` is `Some`, run it instead of the default extract+index pipeline.

**Step 3:** For ergonomic task composition, add a `MemifyPipelineBuilder`:

```rust
let result = MemifyPipelineBuilder::new(graph_db, vector_db, embedding_engine)
    .with_extraction_task(my_custom_task)
    .with_enrichment_task(my_enrichment_task)
    .with_data(my_data)
    .run()
    .await?;
```

**Tests:** Test with custom data, test with custom pipeline.

---

## Dependency Graph

```
Phase 1 (independent, parallel):
  1.1 search: node_name_filter_operator
  1.2 add: node_set
  1.3 add: dataset_id
  1.4 add: incremental_loading
  1.5 search: neighborhood_depth / neighborhood_seed_top_k

Phase 2 (after Phase 1, mostly parallel):
  2.1 add: data_per_batch
  2.2 add: importance_weight (requires DB migration)
  2.3 add: preferred_loaders
  2.4 cognify: datasets UUID resolution
  2.5 cognify: graph_model (largest single item)
  2.6 cognify: chunker

Phase 3 (after Phase 2):
  3.1 BackendOverrides (blocks vector_db_config / graph_db_config on all 3 functions)
  3.2 memify: extraction_tasks / enrichment_tasks / data
```

---

## Estimated Effort

| Gap | Effort | Risk |
|-----|--------|------|
| search: node_name_filter_operator | Small (< 1 hour) | Low |
| add: node_set | Small-Medium (1-2 hours) | Low |
| add: dataset_id | Small (< 1 hour) | Low |
| add: incremental_loading | Small (< 1 hour) | Low |
| search: neighborhood_depth/seed_top_k | Small (1 hour) | Low |
| add: data_per_batch | Small (< 1 hour) | Low |
| add: importance_weight | Medium (2-3 hours, includes migration) | Low |
| add: preferred_loaders | Medium (2 hours) | Low |
| cognify: datasets UUID resolution | Medium (2 hours) | Low |
| cognify: graph_model | Large (4-6 hours) | Medium |
| cognify: chunker | Medium (2-3 hours) | Low |
| BackendOverrides (3 functions) | Large (4-6 hours) | Medium |
| memify: tasks + data | Medium-Large (3-4 hours) | Medium |
