# Gap 1: Missing Parameters on Existing Functions

**Status: Implemented**

This document details every parameter present in the Python SDK's `add`, `cognify`, `search`, and `memify` functions that is absent from the corresponding Rust implementation.

For the detailed implementation plan, see [impl/01-missing-parameters-plan.md](impl/01-missing-parameters-plan.md).

---

## 1. `add()` -- Ingestion Pipeline

### Python Signature

**File:** `cognee/api/v1/add/add.py` (lines 22-43)

```python
async def add(
    data,                        # Union[BinaryIO, list[BinaryIO], str, list[str], DataItem, list[DataItem], Any]
    dataset_name="main_dataset", # str
    user=None,                   # User
    node_set=None,               # Optional[List[str]]
    vector_db_config=None,       # dict
    graph_db_config=None,        # dict
    dataset_id=None,             # Optional[UUID]
    preferred_loaders=None,      # Optional[List[Union[str, dict[str, dict[str, Any]]]]]
    incremental_loading=True,    # bool
    data_per_batch=20,           # Optional[int]
    importance_weight=0.5,       # Optional[float]
    run_in_background=False,     # bool  (EXCLUDED from this analysis)
    **kwargs,                    # extraction_rules, tavily_config, soup_crawler_config
)
```

### Rust Signature

**File:** `crates/ingestion/src/pipeline.rs` (lines 713-719)

```rust
pub async fn add(
    &self,
    inputs: Vec<DataInput>,
    dataset_name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
) -> Result<Vec<Data>, Box<dyn std::error::Error>>
```

### Missing Parameters

| # | Parameter | Python Type | Default | What It Does | Implementation Status |
|---|-----------|-------------|---------|--------------|----------------------|
| 1 | `node_set` | `Optional[List[str]]` | `None` | List of node identifiers for graph organization and access control grouping. Used to tag ingested data so it can be filtered during cognify/search. The `Data.node_set` field exists in the Rust model (JSON string), and downstream `Document` classification parses `external_metadata.node_set` into `source_node_set`/`belongs_to_set`. However, `AddPipeline::add()` does not accept `node_set` as a parameter and does not populate `Data.node_set` -- it is always `None` after ingestion. | Not Started |
| 2 | `dataset_id` | `Optional[UUID]` | `None` | Target an existing dataset by UUID instead of name. Avoids ambiguity when multiple datasets share a prefix. The `IngestDb` trait only has `get_dataset_by_name()`, not a `get_dataset(id)` method (though the ops module does have one). | Not Started |
| 3 | `preferred_loaders` | `Optional[List[Union[str, dict]]]` | `None` | Specifies which document loaders to prefer for different file types. Allows overriding the default loader selection per call. Python transforms the list into a dict mapping loader names to config dicts. | Not Started |
| 4 | `incremental_loading` | `bool` | `True` | When True, skips data items whose content hash already exists in the dataset. The Rust pipeline always performs deduplication by content hash; there is no way to force re-ingestion of unchanged content. | Not Started |
| 5 | `data_per_batch` | `Optional[int]` | `20` | Number of data items to process in a single batch. Controls memory usage vs. throughput tradeoff. The Rust pipeline processes items sequentially in a loop with no batching control. | Not Started |
| 6 | `importance_weight` | `Optional[float]` | `0.5` | Weight factor (0.0-1.0) for importance scoring during ingestion. Influences ranking of this data in the knowledge graph. No `importance_weight` field exists anywhere in the Rust codebase. | Not Started |
| 7 | `vector_db_config` | `dict` | `None` | Per-call override for the vector database backend. Allows routing embeddings to a different vector store for this specific add operation. | Not Started |
| 8 | `graph_db_config` | `dict` | `None` | Per-call override for the graph database backend. Allows routing graph data to a different store for this specific add operation. | Not Started |

---

## 2. `cognify()` -- Knowledge Graph Extraction

### Python Signature

**File:** `cognee/api/v1/cognify/cognify.py` (lines 44-59)

```python
async def cognify(
    datasets=None,               # Union[str, list[str], list[UUID]]
    user=None,                   # User
    graph_model=KnowledgeGraph,  # BaseModel
    chunker=TextChunker,         # chunker class
    chunk_size=None,             # int
    chunks_per_batch=None,       # int
    config=None,                 # Config (ontology configuration)
    vector_db_config=None,       # dict
    graph_db_config=None,        # dict
    run_in_background=False,     # bool  (EXCLUDED)
    incremental_loading=True,    # bool
    custom_prompt=None,          # Optional[str]
    temporal_cognify=False,      # bool
    data_per_batch=20,           # int
    **kwargs,
)
```

### Rust Signature

**File:** `crates/cognify/src/tasks.rs` (lines 1718-1731)

```rust
pub async fn cognify(
    data_items: Vec<Data>,
    dataset_id: Uuid,
    user_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    db: Option<Arc<DatabaseConnection>>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    config: &CognifyConfig,
) -> Result<CognifyResult, CognifyError>
```

**CognifyConfig** (`crates/cognify/src/config.rs`) already has:
- `max_chunk_size`, `chunk_overlap`, `chunk_strategy` (Paragraph/Recursive)
- `chunks_per_batch`, `max_parallel_extractions`
- `custom_extraction_prompt`, `enable_summarization`
- `incremental_loading`, `temporal_cognify`, `data_per_batch`
- `token_counter_kind`

**Additionally:** `crates/cognify/src/dataset_resolver.rs` provides `cognify_datasets()` which accepts `dataset_names: Vec<String>` and resolves them to data items via the `DatasetResolver` trait, covering the `datasets` parameter partially.

### Missing Parameters

| # | Parameter | Python Type | Default | What It Does | Rust Status | Implementation Status |
|---|-----------|-------------|---------|--------------|-------------|----------------------|
| 1 | `datasets` | `Union[str, list[str], list[UUID]]` | `None` | Resolve dataset names/IDs to data items. Python auto-fetches data. | `cognify_datasets()` exists in `dataset_resolver.rs` and accepts `Vec<String>` names. UUID-based resolution is not yet supported. | Not Started |
| 2 | `graph_model` | `BaseModel` | `KnowledgeGraph` | Custom Pydantic model defining extraction schema (entity types, relationship types). Allows domain-specific graph structures. | The `GraphModel` trait exists and `FactExtractor::extract::<M>()` is generic over it, but the `cognify()` pipeline function and `extract_graph_from_data()` are hardcoded to `KnowledgeGraph`. The generic extraction infrastructure exists but is not wired into the top-level pipeline. | Not Started |
| 3 | `chunker` | Class | `TextChunker` | Pluggable chunker class. Python supports `TextChunker` (paragraph) and `LangchainChunker` (recursive char). | `ChunkStrategy` enum exists with `Paragraph` and `Recursive` variants but is not trait-based/pluggable at the pipeline level. | Not Started |
| 4 | `chunk_size` | `int` | Auto-calculated | Max tokens per chunk. Python auto-calculates: `min(embedding_max, llm_max // 2)`. | `CognifyConfig.max_chunk_size` exists. Auto-calculation via `with_auto_chunk_size()` is implemented and is automatically applied in `cognify()` when max_chunk_size equals the default (1500). Matches Python formula. | Not Started |
| 5 | `vector_db_config` | `dict` | `None` | Per-call vector DB override. | Same as add() -- needs `BackendOverrides` | Not Started |
| 6 | `graph_db_config` | `dict` | `None` | Per-call graph DB override. | Same as add() -- needs `BackendOverrides` | Not Started |

**Already present in Rust `CognifyConfig`:** `chunks_per_batch`, `incremental_loading`, `custom_prompt` (as `custom_extraction_prompt`), `temporal_cognify`, `data_per_batch`.

---

## 3. `search()` -- Search Pipeline

### Python Signature

**File:** `cognee/api/v1/search/search.py` (lines 27-48)

```python
async def search(
    query_text,                     # str
    query_type=SearchType.GRAPH_COMPLETION,  # SearchType
    user=None,                      # Optional[User]
    datasets=None,                  # Optional[Union[list[str], str]]
    dataset_ids=None,               # Optional[Union[list[UUID], UUID]]
    system_prompt_path="answer_simple_question.txt",  # str
    system_prompt=None,             # Optional[str]
    top_k=10,                       # int
    node_type=NodeSet,              # Optional[Type]
    node_name=None,                 # Optional[List[str]]
    node_name_filter_operator="OR", # str
    only_context=False,             # bool
    session_id=None,                # Optional[str]
    wide_search_top_k=100,          # Optional[int]
    triplet_distance_penalty=6.5,   # Optional[float]
    feedback_influence=0.0,         # float
    verbose=False,                  # bool
    retriever_specific_config=None, # Optional[dict]
    neighborhood_depth=None,        # Optional[int]
    neighborhood_seed_top_k=None,   # Optional[int]
)
```

### Rust `SearchRequest`

**File:** `crates/search/src/types/search_request.rs` (lines 9-46)

```rust
pub struct SearchRequest {
    pub query_text: String,
    pub search_type: SearchType,
    pub top_k: Option<usize>,
    pub datasets: Option<Vec<String>>,
    pub dataset_ids: Option<Vec<Uuid>>,
    pub system_prompt: Option<String>,
    pub system_prompt_path: Option<String>,
    pub only_context: Option<bool>,
    pub use_combined_context: Option<bool>,
    pub session_id: Option<String>,
    pub node_type: Option<String>,
    pub node_name: Option<Vec<String>>,
    pub wide_search_top_k: Option<usize>,
    pub triplet_distance_penalty: Option<f32>,
    pub save_interaction: Option<bool>,
    pub user_id: Option<Uuid>,
    pub verbose: Option<bool>,
    pub feedback_influence: Option<f32>,
    pub retriever_specific_config: Option<HashMap<String, serde_json::Value>>,
    pub response_schema: Option<serde_json::Value>,
    pub custom_search_type: Option<String>,
    pub auto_feedback_detection: Option<bool>,
}
```

### Missing Parameters

| # | Parameter | Python Type | Default | What It Does | Implementation Status |
|---|-----------|-------------|---------|--------------|----------------------|
| 1 | `node_name_filter_operator` | `str` | `"OR"` | Controls how multiple `node_name` filters combine: `"OR"` = match any, `"AND"` = match all. `SearchParams` has a `node_name_filter_operator` field and retrievers use it, but `SearchRequest` does not have the field and the `From<&SearchRequest> for SearchParams` impl hardcodes it to `None` (line 79 of `search_params.rs`: `node_name_filter_operator: None, // not yet in SearchRequest`). | Not Started |
| 2 | `neighborhood_depth` | `Optional[int]` | `None` | Number of hops from query result nodes to include in the graph context. Controls context breadth. No matching field exists in `SearchRequest` or `SearchParams`. | Not Started |
| 3 | `neighborhood_seed_top_k` | `Optional[int]` | `None` | Number of initial seed nodes for neighborhood expansion. Controls starting point density. No matching field exists in `SearchRequest` or `SearchParams`. | Not Started |

---

## 4. `memify()` -- Enrichment Pipeline

### Python Signature

**File:** `cognee/modules/memify/memify.py` (lines 25-36)

```python
async def memify(
    extraction_tasks=None,       # Union[List[Task], List[str]]
    enrichment_tasks=None,       # Union[List[Task], List[str]]
    data=None,                   # Optional[Any]
    dataset="main_dataset",      # Union[str, UUID]
    user=None,                   # User
    node_type=NodeSet,           # Optional[Type]
    node_name=None,              # Optional[List[str]]
    vector_db_config=None,       # Optional[dict]
    graph_db_config=None,        # Optional[dict]
    run_in_background=False,     # bool  (EXCLUDED)
)
```

### Rust Signature

**File:** `crates/cognify/src/memify/pipeline.rs` (lines 47-55)

```rust
pub async fn memify(
    graph_db: &dyn GraphDBTrait,
    vector_db: &dyn VectorDB,
    embedding_engine: &dyn EmbeddingEngine,
    dataset_id: Option<Uuid>,
    user_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
    config: &MemifyConfig,
) -> Result<MemifyResult, MemifyError>
```

**MemifyConfig** (`crates/cognify/src/memify/config.rs`):
```rust
pub struct MemifyConfig {
    pub triplet_batch_size: usize,
    pub node_type_filter: Option<String>,
    pub node_name_filter: Option<Vec<String>>,
    pub node_name_filter_operator: String,
}
```

### Missing Parameters

| # | Parameter | Python Type | Default | What It Does | Implementation Status |
|---|-----------|-------------|---------|--------------|----------------------|
| 1 | `extraction_tasks` | `Union[List[Task], List[str]]` | `None` | Custom pipeline tasks for data extraction. Allows injecting domain-specific extraction logic beyond the default triplet embedding. Python defaults to `get_default_memify_extraction_tasks()` when None. | Not Started |
| 2 | `enrichment_tasks` | `Union[List[Task], List[str]]` | `None` | Custom pipeline tasks for enrichment. Allows injecting domain-specific enrichment logic (e.g., community detection, centrality scoring). Python defaults to `get_default_memify_enrichment_tasks()` when None. | Not Started |
| 3 | `data` | `Optional[Any]` | `None` | Custom data to process instead of reading from the existing graph. When None, Python calls `get_memory_fragment()` to load the graph (or subgraph filtered by `node_type`/`node_name`). When provided, skips graph loading and uses data directly as input to extraction tasks. | Not Started |
| 4 | `vector_db_config` | `Optional[dict]` | `None` | Per-call vector DB override. | Not Started |
| 5 | `graph_db_config` | `Optional[dict]` | `None` | Per-call graph DB override. | Not Started |

---

## Summary

| Function | Total Missing Params | Priority Params |
|----------|---------------------|-----------------|
| `add()` | 8 | `incremental_loading`, `dataset_id`, `node_set` |
| `cognify()` | 6 | `graph_model` (custom schema), `datasets` UUID resolution |
| `search()` | 3 | `node_name_filter_operator`, `neighborhood_depth`, `neighborhood_seed_top_k` |
| `memify()` | 5 | `extraction_tasks`, `enrichment_tasks` |
| **Total** | **22** | |

**Cross-cutting concern:** `vector_db_config` / `graph_db_config` per-call overrides appear on 3 of 4 functions. Implementing the `BackendOverrides` pattern once covers all three.
