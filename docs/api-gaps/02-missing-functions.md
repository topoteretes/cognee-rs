# Gap 2: Functions Missing from Rust Entirely

This document details every high-level API function present in the Python SDK v1 that has no equivalent in the Rust codebase.

**Implementation plan:** [`impl/02-missing-functions-plan.md`](impl/02-missing-functions-plan.md)

---

## 1. `remember()` -- Smart Ingestion with Session Bridging

**Status: Not Started**

### Python Reference

**File:** `cognee/api/v1/remember/remember.py`

```python
async def remember(
    data: Union[BinaryIO, list[BinaryIO], str, list[str], DataItem, list[DataItem]],
    dataset_name: str = "main_dataset",
    *,
    session_id: Optional[str] = None,
    chunk_size: Optional[int] = None,
    chunker: Optional[Any] = None,
    custom_prompt: Optional[str] = None,
    run_in_background: bool = False,
    self_improvement: bool = True,
    session_ids: Optional[List[str]] = None,
    **kwargs: Unpack[RememberKwargs],
) -> RememberResult
```

### What It Does

`remember()` is a convenience composition of `add()` + `cognify()` + optionally `improve()`, with two distinct modes:

**Permanent Memory Mode** (no `session_id`):
1. Calls `add(data, dataset_name, ...)` to ingest
2. Calls `cognify(datasets=[dataset_id], ...)` to extract knowledge graph
3. If `self_improvement=True`, calls `improve(dataset)` to enrich with triplet embeddings

**Session Memory Mode** (with `session_id`):
1. Converts data to text via `_data_to_text()`
2. Stores in session cache as a Q&A entry: `sm.add_qa(user_id, session_id, question="", context="", answer=text)`
3. If `self_improvement=True`, bridges session data to permanent graph via `improve(session_ids=[session_id])` in background

### Return Type: `RememberResult`

A promise-like object with:
- `status`: `"running"` | `"completed"` | `"errored"` | `"session_stored"`
- `dataset_name`, `dataset_id`, `session_ids`, `pipeline_run_id`
- `elapsed_seconds`, `content_hash`, `items_processed`
- `items`: list of per-item details (id, name, content_hash, token_count, mime_type, data_size)
- Supports `await` for blocking on background tasks
- `done` property for completion checks

### Rust Primitives Available

- `AddPipeline::add()` -- `crates/ingestion/src/pipeline.rs`
- `cognify()` -- `crates/cognify/src/tasks.rs`
- `run_memify()` (aliased from `memify()`) -- `crates/cognify/src/memify/pipeline.rs` (Stage 3 of improve)
- `SessionManager::save_qa()` -- `crates/session/src/session_manager.rs`

---

## 2. `recall()` -- Smart Search with Session Routing

**Status: Not Started**

### Python Reference

**File:** `cognee/api/v1/recall/recall.py`

```python
async def recall(
    query_text: str,
    query_type: Optional[SearchType] = None,
    *,
    datasets: Optional[list[str]] = None,
    top_k: int = 10,
    auto_route: bool = True,
    **kwargs: Unpack[RecallKwargs],
) -> list
```

### What It Does

`recall()` wraps `search()` with two additional capabilities:

**Session-First Routing** (when `session_id` provided without `datasets`/`query_type`):
1. Tokenizes query into word-boundary tokens (regex `\b\w+\b`, min length 2)
2. Searches session Q&A entries by keyword overlap (intersection count)
3. Returns top_k results tagged with `_source: "session"`
4. Falls through to graph search if no session matches found

**Auto Query-Type Selection** (when `auto_route=True` and no explicit `query_type`):
1. Calls `route_query()` -- a rule-based weighted-scoring classifier (not LLM-based)
2. Routes to optimal SearchType based on regex pattern matching with negation detection
3. Falls back to `GRAPH_COMPLETION` if `auto_route=False` or no patterns match

**Result Tagging:**
- All results tagged with `_source: "session"` or `_source: "graph"` for caller awareness

### Rust Primitives Available

- `SearchOrchestrator::search()` -- `crates/search/src/orchestration/search_orchestrator.rs`
- `SessionManager::load_history_both()` -- `crates/session/src/session_manager.rs`
- `SessionStore::get_all_qa_entries()` -- `crates/session/src/session_store.rs`
- No session keyword search exists
- No auto query-type router exists

---

## 3. `improve()` -- Bidirectional Session-Graph Bridge

**Status: Not Started** (Stage 3 exists as `memify()`)

### Python Reference

**File:** `cognee/api/v1/improve/improve.py`

```python
async def improve(
    dataset: Union[str, UUID] = "main_dataset",
    *,
    run_in_background: bool = False,
    node_name: Optional[List[str]] = None,
    session_ids: Optional[List[str]] = None,
    **kwargs: Unpack[ImproveKwargs],
)
```

### What It Does -- Four Stages

**Stage 1: Apply Feedback Weights** (only when `session_ids` provided):
- Reads session Q&A entries that have feedback scores
- For each entry, identifies graph nodes/edges that were used during retrieval (via `used_graph_element_ids`)
- Updates `feedback_weight` property on those graph nodes/edges by `alpha * feedback_score`
- Implemented in Python via `apply_feedback_weights_pipeline()` in `cognee/memify_pipelines/apply_feedback_weights.py`

**Stage 2: Persist Session Q&A to Graph** (only when `session_ids` provided):
- Reads question/answer text from session entries
- Runs `cognify()` on the Q&A text to extract entities/relationships
- Tags extracted nodes with `node_set="user_sessions_from_cache"`
- Implemented in Python via `persist_sessions_in_knowledge_graph_pipeline()` in `cognee/memify_pipelines/persist_sessions_in_knowledge_graph.py`

**Stage 3: Default Enrichment -- Triplet Embeddings** (always runs):
- Extracts all edges from graph as triplets (`source -> relationship -> target`)
- Embeds triplet text via embedding engine
- Indexes into `"Triplet"/"text"` vector collection
- **This stage already exists in Rust** as `memify()` / `run_memify()`

**Stage 4: Sync Graph to Session Cache** (only when `session_ids` provided, not in background mode):
- Reads new edges from relational DB since last checkpoint
- Stores as structured summaries in each session's graph context
- Enables future `recall()` calls to access graph knowledge via session
- Implemented in Python via `sync_graph_to_session()` in `cognee/tasks/memify/sync_graph_to_session.py`

### Rust Primitives Available

- **Stage 3 only:** `memify()` in `crates/cognify/src/memify/pipeline.rs`
- `feedback_weight` field exists on `DataPoint` model (`crates/models/src/data_point.rs`) and is already read by the triplet ranking system (`crates/search/src/graph_retrieval/triplet_ranking.rs`)
- `used_graph_element_ids` exists as internal field in FS/Redis session store backends but is not exposed on the public `SessionQAEntry` type
- Stages 1, 2, 4 are completely missing as orchestrated pipelines

---

## 4. `update()` -- Data Replacement

**Status: Not Started**

### Python Reference

**File:** `cognee/api/v1/update/update.py`

```python
async def update(
    data_id: UUID,
    data: Union[BinaryIO, list[BinaryIO], str, list[str]],
    dataset_id: UUID,
    user: User = None,
    node_set: Optional[List[str]] = None,
    vector_db_config: dict = None,
    graph_db_config: dict = None,
    preferred_loaders: dict[str, dict[str, Any]] = None,
    incremental_loading: bool = True,
) -> Union[Dict[str, PipelineRunInfo], List[PipelineRunInfo]]
```

### What It Does

Three-step replacement:
1. **Delete old data:** `datasets.delete_data(dataset_id, data_id, user)` -- cascade delete from DB, graph, vector
2. **Re-add new data:** `add(data, dataset_id=dataset_id, user=user, ...)` -- ingest replacement
3. **Re-cognify:** `cognify(datasets=[dataset_id], user=user, ...)` -- rebuild knowledge graph

Note: The Python `update()` takes `dataset_id: UUID` (not name), meaning the caller must already know the dataset UUID.

### Rust Primitives Available

- `DeleteService::execute()` -- `crates/delete/src/lib.rs`
- `AddPipeline::add()` -- `crates/ingestion/src/pipeline.rs`
- `cognify()` -- `crates/cognify/src/tasks.rs`

---

## 5. `forget()` -- Unified Deletion API

**Status: Not Started**

### Python Reference

**File:** `cognee/api/v1/forget/forget.py`

```python
async def forget(
    *,
    data_id: Optional[UUID] = None,
    dataset: Optional[Union[str, UUID]] = None,
    everything: bool = False,
    user=None,
) -> dict
```

### What It Does -- Three Scopes

1. **`forget(data_id=id, dataset=ds)`** -- Delete single data item from dataset
2. **`forget(dataset="scientists")`** -- Delete entire dataset (resolves name to UUID)
3. **`forget(everything=True)`** -- Delete all user data + session cache (calls `datasets.delete_all()` then `cache_engine.prune()`)

Validation: `data_id` without `dataset` raises an error.

### Rust Primitives Available

`DeleteService` in `crates/delete/src/lib.rs` already supports:
- `DeleteScope::Data { owner_id, data_id, dataset_name, delete_dataset_if_empty }`
- `DeleteScope::Dataset { owner_id, dataset_name }`
- `DeleteScope::User { owner_id }`
- `DeleteScope::All`

`SessionStore::prune()` exists for session cache cleanup.

Dataset name-to-ID resolution is available via `IngestDb::get_dataset_by_name()`.

---

## 6. `prune` -- System/Data Cleanup

**Status: Not Started**

### Python Reference

**File:** `cognee/api/v1/prune/prune.py`

```python
class prune:
    @staticmethod
    async def prune_data():
        """Remove all files from storage directory."""
        ...

    @staticmethod
    async def prune_system(graph=True, vector=True, metadata=False, cache=True):
        """Selective backend cleanup by layer."""
        ...
```

### What They Do

**`prune_data()`** (`cognee/modules/data/deletion/prune_data.py`):
- Calls `get_file_storage(data_root_directory).remove_all()` -- deletes ALL stored files

**`prune_system(graph, vector, metadata, cache)`** (`cognee/modules/data/deletion/prune_system.py`):
- `graph=True`: calls `graph_engine.delete_graph()` -- wipes all graph nodes/edges
- `vector=True`: calls `vector_engine.prune()` -- wipes all vector collections
- `metadata=True`: calls `db_engine.delete_database()` -- drops relational database (default `False` in Python)
- `cache=True`: calls `cache_engine.prune()` + `delete_cache()` -- wipes session cache and local cache files

Note: Python's `prune_system` also handles multi-tenant "backend access control" mode where it iterates per-dataset graph/vector databases. The Rust codebase does not have this multi-database-per-dataset architecture.

### Rust Primitives Available

- `GraphDBTrait::delete_graph()` -- already exists, wipes all graph nodes/edges
- `VectorDB::delete_collection()` -- exists for individual collections
- `VectorDB::list_collections()` -- exists (needed to iterate and delete all)
- `VectorDB` has no bulk `prune()` method (but can be composed from list + delete)
- `StorageTrait` has `delete()` for individual files but no `remove_all()` method
- `SessionStore::prune()` -- exists
- `DatabaseConnection` has no `delete_database()` method (metadata reset not yet possible)

---

## Summary

| Function | Status | Complexity | Depends On |
|----------|--------|------------|------------|
| `remember()` | **Not Started** | Medium | `improve()`, Session Manager |
| `recall()` | **Not Started** | Medium | Session keyword search, query router |
| `improve()` | **Not Started** (Stage 3 only as `memify()`) | High | Feedback fields, graph property updates, session sync |
| `update()` | **Not Started** | Low | Existing delete + add + cognify |
| `forget()` | **Not Started** | Low | Existing `DeleteService`, session prune |
| `prune` | **Not Started** | Low-Medium | Trait extensions for bulk cleanup |

### Recommended Implementation Order

1. **`forget()`** -- Low complexity, thin wrapper over existing `DeleteService`
2. **`update()`** -- Low complexity, composition of existing primitives
3. **`prune`** -- Low-Medium, requires trait extensions but straightforward
4. **`recall()`** -- Medium, session keyword search is simple; auto-router can start rule-based
5. **`remember()`** -- Medium, depends on `improve()` for full feature parity
6. **`improve()`** -- High, requires feedback system, graph property updates, session sync
