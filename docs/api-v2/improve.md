# API v2: `improve()`

**Python source:** `cognee/api/v1/improve/improve.py`
**Rust status:** Partial (Stage 3 complete; Stages 1, 2, 4 are stubs)
**Implementation plan:** [impl/improve-plan.md](impl/improve-plan.md)

---

## 1. What it does (4 stages)

The `improve()` function is a bidirectional session-graph bridge that runs up to four stages in sequence. Each stage reads from the session cache and/or existing graph DB and writes back to the graph and session cache.

### Stage 1: Apply Feedback Weights (when `session_ids` provided)

**Input:**  
- List of session IDs from which to read Q&A entries  
- `feedback_alpha` parameter (default 0.1) — controls learning rate  
- Each Q&A entry may have:  
  - `feedback_score` (1-5 rating provided by user)  
  - `used_graph_element_ids` dict with `node_ids` and `edge_ids` lists — the graph elements that contributed to the answer  

**Processing:**  
1. For each session, load all Q&A entries via `SessionStore::get_all_qa_entries()`  
2. Filter to entries with both `feedback_score` and `used_graph_element_ids`  
3. Normalize feedback score from [1..5] to [0..1] range: `normalized = (score - 1) / 4`  
4. For each referenced node/edge ID, fetch its current `feedback_weight` from graph DB  
5. Apply streaming update: `new_weight = old_weight + alpha * (normalized - old_weight)`, then clip to [0, 1]  
6. Write updated `feedback_weight` back to the graph  
7. Mark QA entry in session `memify_metadata` to prevent reprocessing  

**Side effects:**  
- Modifies `feedback_weight` property on graph nodes and edges  
- Updates `memify_metadata.feedback_weights_applied` flag in session Q&A entry  

**Python implementation:**  
- Pipeline: `/tmp/cognee-python/cognee/memify_pipelines/apply_feedback_weights.py` (calls via `memify()` task pipeline)  
- Core logic: `/tmp/cognee-python/cognee/tasks/memify/apply_feedback_weights.py`  
- Key functions: `normalize_feedback_score()`, `stream_update_weight()`, `_process_feedback_item()`  
- Graph engine methods used: `graph_engine.get_node_feedback_weights()`, `graph_engine.set_node_feedback_weights()`, `graph_engine.get_edge_feedback_weights()`, `graph_engine.set_edge_feedback_weights()`  

---

### Stage 2: Persist Session Q&A to Graph (when `session_ids` provided)

**Input:**  
- List of session IDs containing Q&A entries to extract knowledge from  

**Processing:**  
1. For each session, load all Q&A entries via `SessionStore::get_all_qa_entries()`  
2. Concatenate question and answer text from all entries  
3. Run `cognify()` on the concatenated text with `node_set="user_sessions_from_cache"` to extract entities/relationships  
4. Tag extracted nodes in the graph with the special `node_set` value  
5. This persists the conversation knowledge permanently in the graph  

**Side effects:**  
- Adds new nodes and edges to the graph derived from session content  
- Tags these nodes with `source_node_set="user_sessions_from_cache"` for tracking origin  

**Python implementation:**  
- Pipeline: `/tmp/cognee-python/cognee/memify_pipelines/persist_sessions_in_knowledge_graph.py`  
- Extraction task: `extract_user_sessions()` in `/tmp/cognee-python/cognee/tasks/memify/extract_user_sessions.py`  
- Enrichment task: `cognify_session()` in `/tmp/cognee-python/cognee/tasks/memify/cognify_session.py`  

---

### Stage 3: Default Enrichment — Triplet Embeddings (always runs)

**Input:**  
- Entire knowledge graph (optional filter by `node_name`)  

**Processing:**  
1. Extract all edges from graph as triplets: `(source_label → relationship_name → target_label)`  
2. Embed triplet text using the embedding engine  
3. Index triplet embeddings into `"Triplet"/"text"` vector collection  
4. Enables `SearchType::TripletCompletion` queries  

**Side effects:**  
- Adds embeddings to vector DB; does not modify graph  

**Python implementation:**  
- Implemented in Python `improve()` as direct call to `memify()` at `/tmp/cognee-python/cognee/modules/memify/memify.py`  
- Extracts triplets via graph traversal, embeds them, indexes into vector DB  

**Rust implementation:**  
- ✅ **Fully implemented** as `memify()` / `run_memify()` in `crates/cognify/src/memify/pipeline.rs`  
- Reads graph via `GraphDBTrait::get_graph_data()`, creates `Triplet` objects, indexes via `VectorDB`  

---

### Stage 4: Sync Graph to Session Cache (when `session_ids` provided, not in background mode)

**Input:**  
- List of session IDs to update with graph knowledge  
- Maintains per-session checkpoint (last sync timestamp)  

**Processing:**  
1. Retrieve last sync checkpoint from cache engine (Redis or Fs)  
2. Query relational DB for edges created after checkpoint (batched, 500 at a time)  
3. For each edge, fetch source and target nodes to get full metadata (label, type, description)  
4. Render each edge as structured JSON-line: `{"source": {...}, "relationship": "...", "target": {...}}`  
5. Load existing graph context from session store  
6. Merge: append new lines; if total exceeds cap (default 500 lines), drop oldest  
7. Write merged context back to session store via `SessionStore::set_graph_context()`  
8. Update checkpoint timestamp in cache for next sync  

**Side effects:**  
- Updates `graph_context` field on session via `SessionStore::set_graph_context()`  
- Updates checkpoint timestamp in cache engine  
- Does not modify graph or session Q&A entries  

**Python implementation:**  
- `/tmp/cognee-python/cognee/tasks/memify/sync_graph_to_session.py`  
- Functions: `_fetch_new_edges()`, `_edge_to_text()`, `_load_checkpoint()`, `_save_checkpoint()`  
- Uses relational DB queries (SQLAlchemy) and cache engine (Redis/Fs)  

---

## 2. memify vs improve — exact mapping

**Critical finding: The existing gap doc claim "Stage 3 only" is CORRECT but INCOMPLETE.**

### What memify does (Stage 3)
- Reads existing graph  
- Extracts triplets (all edges as text)  
- Embeds triplets  
- Indexes into vector DB  
- **Does NOT touch session cache**  
- **Does NOT apply feedback weights**  
- **Does NOT persist session Q&A to graph**  
- **Does NOT sync graph back to sessions**  

### What improve does (Stages 1-4)
- Stage 1: **Feedback weights** — reads session feedback scores, updates graph node/edge properties  
- Stage 2: **Session Q&A persistence** — cognifies session text, adds to graph with special `node_set` tag  
- Stage 3: **Triplet embeddings** — calls `memify()` to embed triplets  
- Stage 4: **Graph-to-session sync** — reads new graph edges, stores as JSON-lines in session context  

### Mapping table

| Operation | Python `improve()` | Rust `improve()` | Rust `memify()` |
|-----------|-------------------|------------------|-----------------|
| Read feedback scores from sessions | Stage 1 pipeline | Stage 1 (partial) | ✗ |
| Update graph feedback_weight | Stage 1 pipeline | Stage 1 (partial) | ✗ |
| Cognify session Q&A text | Stage 2 pipeline | Stage 2 (stub) | ✗ |
| Persist session knowledge to graph | Stage 2 pipeline | Stage 2 (stub) | ✗ |
| Extract graph triplets | Stage 3 (via memify) | Stage 3 (via run_memify) | ✅ Extract only |
| Embed triplet text | Stage 3 (via memify) | Stage 3 (via run_memify) | ✅ Embed & index |
| Index triplets in vector DB | Stage 3 (via memify) | Stage 3 (via run_memify) | ✅ |
| Read new graph edges | Stage 4 task | Stage 4 (stub) | ✗ |
| Merge with existing session context | Stage 4 task | Stage 4 (stub) | ✗ |
| Store graph context in session | Stage 4 task | Stage 4 (stub) | ✗ |

---

## 3. Building blocks (Python)

### Session Management
- **Module:** `cognee.infrastructure.session` / `cognee.modules.users.methods`  
- **Main APIs:**  
  - `SessionManager::load_history()` — load Q&A entries by session ID  
  - `SessionManager::add_feedback()` — add feedback score/text to Q&A entry  
  - `SessionManager::update_qa()` — update QA entry metadata  
  - `SessionManager::get_graph_context()` — retrieve session's graph knowledge snapshot  
  - `SessionManager::set_graph_context()` — store session's graph knowledge snapshot  

### Feedback Pipeline (Stage 1)
- **Pipeline:** `/tmp/cognee-python/cognee/memify_pipelines/apply_feedback_weights.py`  
- **Task:** `/tmp/cognee-python/cognee/tasks/memify/apply_feedback_weights.py`  
- **Functions:**  
  - `normalize_feedback_score(score: int) -> float` — map [1..5] to [0..1]  
  - `stream_update_weight(old, normalized, alpha) -> float` — update with clipping  
  - `apply_feedback_weights(data, alpha)` — main task entry point  
- **Graph engine methods** (expected, from Python code inspection):  
  - `graph_engine.get_node_feedback_weights(node_ids: list[str]) -> dict[str, float]`  
  - `graph_engine.set_node_feedback_weights(updates: dict[str, float]) -> dict[str, bool]`  
  - `graph_engine.get_edge_feedback_weights(edge_ids: list[str]) -> dict[str, float]`  
  - `graph_engine.set_edge_feedback_weights(updates: dict[str, float]) -> dict[str, bool]`  

### Session Q&A Persistence (Stage 2)
- **Pipeline:** `/tmp/cognee-python/cognee/memify_pipelines/persist_sessions_in_knowledge_graph.py`  
- **Extraction task:** `/tmp/cognee-python/cognee/tasks/memify/extract_user_sessions.py`  
  - `extract_user_sessions(session_ids: list[str])` — load Q&A from sessions  
- **Enrichment task:** `/tmp/cognee-python/cognee/tasks/memify/cognify_session.py`  
  - `cognify_session(qa_data, dataset_id)` — extract entities/relationships via LLM with `node_set` tag  
- **Depends on:** Full `cognify()` pipeline for entity/relationship extraction  

### Triplet Embedding (Stage 3)
- **Module:** `cognee.modules.memify`  
- **Function:** `memify()` in `/tmp/cognee-python/cognee/modules/memify/memify.py`  
- **Steps:**  
  - Extract all graph edges as triplets via `graph_engine.get_graph_data()`  
  - Embed via `embedding_engine.embed(triplet_text)`  
  - Index via `vector_engine.upsert()`  

### Graph-to-Session Sync (Stage 4)
- **Module:** `/tmp/cognee-python/cognee/tasks/memify/sync_graph_to_session.py`  
- **Functions:**  
  - `sync_graph_to_session(user_id, session_id, dataset_id, dataset_name, max_lines)`  
  - `_fetch_new_edges(db_engine, dataset_id, since, limit)` — query relational DB for new edges  
  - `_edge_to_text(edge, node_map)` — render edge as JSON-line with node metadata  
  - `_load_checkpoint(cache_engine, key)` — retrieve last sync timestamp from cache  
  - `_save_checkpoint(cache_engine, key, ts)` — persist high-water mark timestamp  
- **Depends on:**  
  - Relational DB (SQLAlchemy Edge/Node ORM models)  
  - Cache engine (Redis or FileSystem)  

### Supporting Building Blocks
- **Graph engine** (`get_graph_engine()`)  
  - Methods: `get_graph_data()`, `get_node_feedback_weights()`, `set_node_feedback_weights()`, etc.  
- **Embedding engine** — vectorize triplet text  
- **Vector engine** — index triplet embeddings with metadata  
- **LLM** — for Stage 2 cognify on session text  
- **Relational DB** — for Stage 4 edge queries  
- **Cache engine** — for Stage 4 checkpoint persistence  

---

## 4. Rust implementation status per building block

| Building Block | Python Path | Rust Path | Status | Notes |
|---|---|---|---|---|
| **SessionManager APIs** | `cognee.infrastructure.session` | `crates/session/src/session_manager.rs` | ✅ Partial | Has `save_qa()`, `add_feedback()`, `get_graph_context()`, `set_graph_context()`. Missing: `update_qa()` for memify metadata (exists but not fully wired). |
| **SessionStore trait** | Python CacheDBInterface | `crates/session/src/session_store.rs` | ✅ Complete | Has `get_all_qa_entries()`, `update_qa_entry()`, `get_graph_context()`, `set_graph_context()`. |
| **SessionQAEntry type** | Python QAEntry dict | `crates/session/src/types.rs:21-41` | ✅ Complete | Has `feedback_text`, `feedback_score`, `used_graph_element_ids`, `memify_metadata` fields. All 4 fields are `Option<T>`. |
| **DataPoint model** | Python DataPoint | `crates/models/src/data_point.rs:35-84` | ✅ Complete | Has `feedback_weight: f64` field (default 0.5). Read by triplet ranking system. |
| **GraphDBTrait** | Python GraphEngine | `crates/graph/src/traits.rs` | ⚠️ Partial | Has `update_node_property()` (line 317) with default cascading implementation. Has `update_edge_property()` (line 359) as stub (logs warning, no-op). **Missing:** batch methods `get_node_feedback_weights()`, `set_node_feedback_weights()`, etc. |
| **Graph property update APIs** | — | `crates/graph/src/traits.rs:317-373` | ⚠️ Basic | `update_node_property()` exists but uses delete-add-restore pattern (cascade risk, poor atomicity). No batch variants. No edge update implementation (stub only). |
| **Memify pipeline** | `cognee.modules.memify` | `crates/cognify/src/memify/pipeline.rs:47-133` | ✅ Complete | Extracts triplets, embeds, indexes. Stage 3 is fully implemented. |
| **improve() orchestrator** | `cognee.api.v1.improve.improve()` | `crates/lib/src/api/improve.rs:58-153` | ⚠️ Partial | Stage 3 (memify) runs fully. Stage 1 partial (reads sessions, attempts graph updates, but property update APIs are stubs). Stage 2 stub (logs intent only). Stage 4 stub (logs intent only). |
| **Stage 1: Apply feedback weights** | `apply_feedback_weights.py` | `crates/lib/src/api/improve.rs:156-222` | ⚠️ Partial | Reads sessions, parses `feedback_score` from context, calls `graph_db.update_node_property()` but implementation is incomplete because graph DB property update is a stub for edges. Relies on Python logic: normalize score to [0..1], streaming update formula. **Not yet ported:** batch weight getters/setters for efficiency. |
| **Stage 2: Persist sessions** | `persist_sessions_in_knowledge_graph.py` | `crates/lib/src/api/improve.rs:229-250` | ❌ Stub | Logs intent but does not cognify session text or add to graph. Full implementation requires wiring `cognify()` with session input and `node_set` tagging. |
| **Stage 4: Sync graph to session** | `sync_graph_to_session.py` | `crates/lib/src/api/improve.rs:258-280` | ❌ Stub | Logs intent but does not read edges, maintain checkpoints, or write context. Would require relational DB edge queries and cache checkpoint persistence. |
| **Cache engine abstraction** | `CacheEngine` (Redis/Fs) | ❌ Missing | Missing | No abstraction in Rust for persistent checkpoints. SeaORM DB could be used as fallback, but Redis is not integrated. |
| **Relational DB edge queries** | SQLAlchemy Edge ORM | `crates/database/src/` | ⚠️ Partial | SeaORM database exists but does not expose Edge/Node ORM models for direct queries in memify context. Would need to be extended. |
| **Ontology resolution** | `OntologyResolver` | `crates/ontology/src/` | ✅ Complete | Exists but not used by memify. Not required for `improve()`. |
| **Embedding engine** | `EmbeddingEngine` | `crates/embedding/src/` | ✅ Complete | Multi-provider (ONNX, OpenAI, Ollama, Mock). Used by memify. |
| **Vector DB abstraction** | `VectorDB` | `crates/vector/src/` | ✅ Complete | Supports upsert, search, collections. Used by memify. |

---

## 5. Gaps — what Rust needs

### Critical Gaps for Stage 1 (Feedback Weights)

1. **Graph batch property update methods** (Priority: HIGH)  
   - Add to `GraphDBTrait` (file: `crates/graph/src/traits.rs`):  
     ```rust
     async fn get_node_feedback_weights(&self, node_ids: &[String]) -> GraphDBResult<HashMap<String, f64>>;
     async fn set_node_feedback_weights(&self, updates: &HashMap<String, f64>) -> GraphDBResult<HashMap<String, bool>>;
     async fn get_edge_feedback_weights(&self, edge_ids: &[(String, String, String)]) -> GraphDBResult<HashMap<String, f64>>;
     async fn set_edge_feedback_weights(&self, updates: &HashMap<String, f64>) -> GraphDBResult<HashMap<String, bool>>;
     ```
   - Implement efficiently in Ladybug backend (avoid cascading deletes)  
   - Implement in PostgreSQL backend if used  

2. **SessionQAEntry.used_graph_element_ids population** (Priority: HIGH)  
   - Currently internal field in FS/Redis stores, not exposed on public type  
   - Need to wire up search pipeline to populate this field when answer is generated  
   - Field already exists in type: `crates/session/src/types.rs:37`  
   - Missing: Logic in search retrievers to capture node/edge IDs used during retrieval  

3. **Feedback weight normalization and streaming update** (Priority: MEDIUM)  
   - Port Python logic to Rust in `stage1_apply_feedback_weights()`:  
     ```rust
     fn normalize_feedback_score(score: i32) -> Result<f64, ApiError> { /* map [1..5] to [0..1] */ }
     fn stream_update_weight(old: f64, normalized: f64, alpha: f64) -> f64 { /* streaming formula + clip to [0,1] */ }
     ```
   - Already partially present in Python `improve.rs` but could be extracted to shared utility module  

### Critical Gaps for Stage 2 (Persist Sessions)

4. **Cognify with session input and node_set tag** (Priority: HIGH)  
   - Extend `cognify()` or create variant to:  
     - Accept concatenated Q&A text from sessions as input  
     - Tag output nodes with `source_node_set="user_sessions_from_cache"`  
     - Integrate into `improve()` pipeline  
   - File: `crates/cognify/src/tasks.rs`  
   - Requires: Extract session Q&A data, concatenate, feed to fact extraction, tag results  

### Critical Gaps for Stage 4 (Sync Graph to Session)

5. **Relational DB edge query by timestamp** (Priority: HIGH)  
   - Extend `DatabaseConnection` to expose edge queries:  
     ```rust
     async fn get_edges_since(&self, dataset_id: Uuid, since: Option<DateTime<Utc>>, limit: usize) -> Result<Vec<EdgeRecord>>;
     ```
   - Files: `crates/database/src/lib.rs` and SeaORM entity definitions  
   - Need: Access to both Edge and Node ORM models with their creation timestamps  

6. **Cache/checkpoint abstraction** (Priority: MEDIUM)  
   - Create trait or use existing cache layer for high-water mark persistence:  
     ```rust
     pub trait CheckpointStore: Send + Sync {
         async fn load_checkpoint(&self, key: &str) -> Result<Option<DateTime<Utc>>>;
         async fn save_checkpoint(&self, key: &str, ts: DateTime<Utc>) -> Result<()>;
     }
     ```
   - Implementation: Redis (if available), or fallback to SeaORM table, or Fs  
   - File: New module `crates/*/src/checkpoint.rs` or extend existing  

7. **SessionStore API enhancement for context versioning** (Priority: LOW)  
   - Current `set_graph_context()` is simple string write  
   - Consider if versioning, size limits, or incremental merging should be handled at trait level  
   - File: `crates/session/src/session_store.rs:94-99`  
   - Current implementation in `crates/session/src/fs_store.rs` and `sea_orm_store.rs` is sufficient for now  

### Supporting Gaps

8. **Feedback weight persistence in graph schema** (Priority: MEDIUM)  
   - Verify Ladybug and PostgreSQL backends persist `feedback_weight` property on nodes  
   - Files: `crates/graph/src/ladybug.rs`, `crates/graph/src/pg_graph_adapter.rs`  
   - Should already work since it's a JSON property, but confirm in integration tests  

9. **DataPoint.feedback_weight usage in search ranking** (Priority: LOW)  
   - Already read by `crates/search/src/graph_retrieval/triplet_ranking.rs`  
   - Verify that feedback weights are considered in ranking formulas  
   - May need documentation update  

---

## 6. Effort estimate

**Overall: L (Large) — approximately 5–8 engineer-weeks for production-quality implementation**

### Breakdown by stage

| Stage | Work Items | Size | Notes |
|-------|-----------|------|-------|
| **Stage 1** | Batch graph property update methods (4h), Wire `used_graph_element_ids` in search (12h), Normalize/update logic (4h), Integration tests (8h) | M | Dependent on graph backend optimization; consider whether Ladybug supports in-place property updates |
| **Stage 2** | Variant of cognify() with node_set tag (16h), Session text extraction and concatenation (8h), Integration with memify pipeline (8h), End-to-end tests (8h) | M | Moderate complexity; leverages existing cognify pipeline |
| **Stage 3** | ✅ **Complete** — already shipped as `memify()` | S | No work needed |
| **Stage 4** | Relational DB edge queries by timestamp (12h), Checkpoint store abstraction (8h), Graph sync logic and merging (12h), Cache persistence (4h), Integration tests (8h) | M | Moderate complexity; most work is in DB layer and cache abstraction |
| **Cross-cutting** | Bug fixes (graph property stubs), Integration tests (16h), Documentation (4h) | S | Testing and polish work |

### Risks & Dependencies

1. **Graph backend optimization (Stage 1, 4)** — Ladybug may not support efficient in-place property updates. May require reimplementing graph backend or accepting cascading-delete performance hit.  
2. **Cognify variant (Stage 2)** — Requires careful integration with existing LLM and embedding logic. May uncover missing abstractions in the pipeline.  
3. **Relational DB schema** — Edge/Node ORM models must be exposed and queryable by timestamp. SeaORM migrations may be needed.  
4. **Feedback capture in search (Stage 1)** — Search retrievers currently do not populate `used_graph_element_ids`. This is a cross-cutting concern and may affect multiple retriever implementations.  

### Recommended sequence

1. **Stage 1 (Feedback)** — Start here; unblocks testing of the rest. Requires graph backend optimization upfront.  
2. **Stage 3 (Triplets)** — Already done; validate existing implementation.  
3. **Stage 4 (Sync)** — Next; builds on Session store + relational DB queries.  
4. **Stage 2 (Persistence)** — Last; depends on full cognify integration.  

---

## Implementation notes

### Stage 1 key logic (streaming weight update)

Python formula (from `stream_update_weight()`):
```python
updated = old_weight + alpha * (normalized_rating - old_weight)
final_score = max(0.0, min(1.0, updated))
return round(final_score, 4)  # 4 decimal places
```

This is an exponential moving average: each feedback event moves the weight partway toward the normalized rating, controlled by `alpha` (learning rate).

### Stage 4 JSON-line format

Python `_edge_to_text()` produces:
```json
{
  "source": {"label": "...", "type": "...", "description": "..."},
  "relationship": "...",
  "target": {"label": "...", "type": "...", "description": "..."}
}
```

These are stored as newline-delimited JSON in `graph_context` field of session. When merging, if total exceeds cap, drop oldest lines (earliest created_at).

### Idempotency

- Stage 1: Marked as processed via `memify_metadata["feedback_weights_applied"] = true`. Rerunning skips already-processed entries.  
- Stage 2: Nodes tagged with `source_node_set="user_sessions_from_cache"`. Idempotency not enforced; rerunning may add duplicates. Consider deduplication by content hash.  
- Stage 3 (memify): Extracts all edges fresh each run. Vector DB upsert is idempotent (by embedding ID).  
- Stage 4: Checkpoint timestamp ensures incremental sync. Rerunning from same checkpoint fetches same edges.  

---

## Summary of the claim

**The gap doc's claim "Stage 3 only" is accurate:** `memify()` in Rust implements exactly the triplet extraction, embedding, and indexing workflow of Python's Stage 3. Stages 1, 2, and 4 are completely absent or stub implementations. The `improve()` orchestrator in `crates/lib/src/api/improve.rs` was started as a scaffold but does not yet implement the full bidirectional session-graph bridge. All four building blocks are in the Rust codebase (sessions, graph DB, embedding, vector DB), but the glue logic for stages 1, 2, and 4 is missing.
