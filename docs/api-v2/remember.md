# API v2: `remember()`

**Python source:** `cognee/api/v1/remember/remember.py` (603 lines)  
**Rust status:** Partial (core composition exists; session bridging incomplete)  
**Implementation plan:** [impl/remember-plan.md](impl/remember-plan.md)

---

## 1. What it does

### High-Level Behavior

`remember()` is a convenience API that handles data ingestion with optional knowledge graph enrichment. It operates in two distinct modes:

#### Permanent Memory Mode (no `session_id`)
1. **Ingest data** via `add()` — converts input (text, files, binary streams) to `Data` records with content hashing, deduplication, and storage
2. **Extract knowledge graph** via `cognify()` — classifies documents, chunks text, extracts entities/relationships via LLM, stores in graph and vector DBs
3. **Enrich with triplet embeddings** via `improve()` (when `self_improvement=True`) — extracts all graph edges as triplets, embeds them, indexes into vector DB for semantic search

#### Session Memory Mode (with `session_id`)
1. **Convert data to text** via `_data_to_text()` — transforms input (text, file paths, binary streams) into string representation
2. **Store in session cache** via `SessionManager.add_qa()` — persists text as a Q&A entry (question="", answer=text) in session store (Redis/FS/SeaORM)
3. **Optional session-to-graph bridge** (when `self_improvement=True`) — runs `improve(session_ids=[session_id])` in background to cognify the Q&A text and sync results back to the session cache

### Return Type: `RememberResult`

A **promise-like** object supporting async/await and direct attribute access:

**Fields:**
- `status`: `"running"` | `"completed"` | `"errored"` | `"session_stored"`
- `dataset_name`, `dataset_id`, `session_ids`, `pipeline_run_id`
- `elapsed_seconds`: wall-clock time from start to completion
- `content_hash`: MD5 hash of first item (for deduplication tracking)
- `items_processed`: count of data items handled
- `items`: list of per-item dicts with `{id, name, content_hash, token_count, mime_type, data_size}`
- `raw_result`: the original cognify result dict for advanced inspection
- `error`: exception message if pipeline failed

**Methods & Properties:**
- `__repr__()` / `__str__()` — human-readable summary
- `to_dict()` — JSON-serializable dict
- `__await__()` — await background task (blocks until completion)
- `done` property — `True` if pipeline finished (success or failure)
- `__bool__()` — `True` if status is `"completed"` or `"session_stored"`

### Input & Output Summary

| Input | Processing | Output |
|-------|-----------|--------|
| Text string | add() → cognify() → improve() | RememberResult with dataset_id, elapsed_seconds, items |
| File path (absolute) | same | same + content_hash from file MD5 |
| Binary stream (BinaryIO) | same | same + mime_type from stream |
| List of mixed types | all processed together | single dataset, aggregated stats |
| + session_id | add_qa() in session store | RememberResult with session_ids, status="session_stored" |
| + session_id + self_improvement | + background improve() | async bridge to permanent graph |

---

## 2. Building blocks (Python)

Python `remember()` directly depends on these modules and functions:

| Building Block | Python Path | Role |
|---|---|---|
| **add()** | `cognee/api/v1/add/add.py` | Ingest data into dataset (file hashing, deduplication, storage) |
| **cognify()** | `cognee/api/v1/cognify/cognify.py` | Extract knowledge graph from dataset |
| **improve()** | `cognee/api/v1/improve/improve.py` | Enrich graph with feedback weights, session Q&A, triplet embeddings, and session sync |
| **SessionManager** | `cognee/infrastructure/session/session_manager.py` | Session cache abstraction (add_qa, load_history, feedback) |
| **_data_to_text()** | `cognee/api/v1/remember/remember.py:95–111` | Converts DataInput variants to text (inline utility) |
| **_estimate_data_size()** | `cognee/api/v1/remember/remember.py:78–92` | Estimates byte size for telemetry (inline utility) |
| **TextChunker** | `cognee/modules/chunking/TextChunker.py` | Text chunking strategy (configurable; default for cognify) |
| **RememberKwargs** | `cognee/api/v1/remember/remember.py:47–75` | TypedDict for parameter routing to add/cognify/both |
| **RememberResult** | `cognee/api/v1/remember/remember.py:139–337` | Promise-like result wrapper with async/await support |
| **User resolution** | `cognee/modules/users/methods.py` | get_default_user() to auto-create if needed |
| **Database setup** | `cognee/modules/engine/operations/setup.py` | setup() to initialize relational DB before first add/cognify |
| **Remote client** | `cognee/api/v1/serve/state.py` | get_remote_client() for cloud-mode routing (optional) |
| **Vector migrations** | `cognee/run_migrations.py` | run_vector_migrations() to migrate stale LanceDB schemas (once-per-process) |
| **Telemetry** | `cognee/shared/utils.py` | send_telemetry() with mode, dataset, size, item count, session_id, etc. |
| **Observability** | `cognee/modules/observability/__init__.py` | OpenTelemetry span attributes (COGNEE_DATASET_NAME, COGNEE_OPERATION_MODE, etc.) |
| **Logging** | `cognee/shared/logging_utils.py` | get_logger() for structured logging |

---

## 3. Rust status per building block

| Building Block | Python Path | Rust Path | Status | Gap/Note |
|---|---|---|---|---|
| **add()** | cognee/api/v1/add/add.py | crates/ingestion/src/pipeline.rs + crates/lib/src/api/mod.rs | ✅ Implemented | AddPipeline::add() exists; supports AddParams (node_set, dataset_id, preferred_loaders, importance_weight) |
| **cognify()** | cognee/api/v1/cognify/cognify.py | crates/cognify/src/tasks.rs + crates/lib/src/api/mod.rs | ✅ Implemented | cognify() function exists; supports CognifyConfig (chunking, custom prompts, batch sizes) |
| **improve()** | cognee/api/v1/improve/improve.py | crates/lib/src/api/improve.rs | ⚠️ Partial | Stages 1 & 3 implemented; stages 2 & 4 are stubs (logged intent only) |
| **memify()** | cognee/modules/memify/__init__.py | crates/cognify/src/memify/pipeline.rs | ✅ Implemented | run_memify() / memify() extracts triplets and indexes them; used by improve stage 3 |
| **SessionManager** | cognee/infrastructure/session/session_manager.py | crates/session/src/session_manager.rs | ✅ Partial | SessionManager methods exist (load_history, save_qa, add_feedback, delete_feedback); exposed via SessionStore trait |
| **SessionStore trait** | (abstraction) | crates/session/src/session_store.rs | ✅ Implemented | Trait with create_qa_entry(), get_all_qa_entries(), update_qa_entry(), add_feedback(), etc. |
| **_data_to_text()** | cognee/api/v1/remember/remember.py:95–111 | crates/lib/src/api/remember.rs:208–215 | ✅ Implemented | Same logic (Text → String, FilePath → "[file: path]", others → debug format) |
| **TextChunker** | cognee/modules/chunking/TextChunker.py | crates/chunking/src/ | ✅ Implemented | chunk_text() function; TokenCounterKind::from_env() auto-selects counter |
| **RememberResult** | cognee/api/v1/remember/remember.py:139–337 | crates/lib/src/api/remember.rs:48–60 | ⚠️ Partial | Struct exists with status, dataset_name, dataset_id, session_ids, elapsed_seconds, items; missing: `to_dict()`, `__bool__()`, `__repr__()`, async/await support |
| **Uuid generation** | (Python's uuid.uuid5) | crates/ingestion/src/id_generation.rs | ✅ Implemented | generate_data_id(), generate_dataset_id() use Uuid::new_v5 with NAMESPACE_OID |
| **Telemetry** | cognee/shared/utils.py | N/A | ❌ Not Needed | Rust crate uses tracing; no telemetry integration |
| **Observability (OpenTelemetry)** | cognee/modules/observability/ | N/A | ⚠️ Minimal | tracing crate used; OTel integration not in scope for remember() |
| **User resolution** | cognee/modules/users/methods.py | crates/lib/src/api/user.rs | ✅ Implemented | get_or_create_default_user() exists |
| **Database setup** | cognee/modules/engine/operations/setup.py | Not exposed | ❌ Missing | DB initialization happens implicitly; no explicit setup() call in Rust API |
| **Remote client** | cognee/api/v1/serve/state.py | N/A | ❌ Not Planned | Rust is not a drop-in proxy; cloud routing not in scope |
| **Vector migrations** | cognee/run_migrations.py | N/A | ❌ Not Needed | Qdrant/Ladybug are embedded; no migration layer needed |
| **Improve Stage 1: Feedback Weights** | cognee/memify_pipelines/apply_feedback_weights.py | crates/lib/src/api/improve.rs:155–218 | ✅ Implemented | stage1_apply_feedback_weights() reads session Q&A, updates graph node/edge feedback_weight property |
| **Improve Stage 2: Persist Q&A** | cognee/memify_pipelines/persist_sessions_in_knowledge_graph.py | crates/lib/src/api/improve.rs:220–237 | ⚠️ Stub | stage2_persist_sessions() logs intent but does not run cognify on Q&A text; missing: LLM call, graph insert |
| **Improve Stage 3: Memify** | cognee/modules/memify/__init__.py | crates/cognify/src/memify/pipeline.rs | ✅ Implemented | Triplet extraction and indexing |
| **Improve Stage 4: Sync Graph to Session** | cognee/tasks/memify/sync_graph_to_session.py | crates/lib/src/api/improve.rs:239–255 | ⚠️ Stub | stage4_sync_graph_to_session() logs intent but does not persist edge summaries to session store; missing: graph query, session context update |

---

## 4. Gaps — what Rust needs

### Critical Gaps

#### A. Improve Function Stages 2 & 4 (Stubs Only)

**Stage 2: Persist Session Q&A to Graph**  
*Current code:* `/home/dmytro/dev/cognee/cognee-rust/crates/lib/src/api/improve.rs:220–237`
- **What's missing:**
  - Load Q&A text from session cache (query SessionStore for user_id + session_ids)
  - For each Q&A entry, run cognify on the combined text (question + answer)
  - Tag extracted nodes/edges with `node_set="user_sessions_from_cache"` (metadata field)
  - Insert cognified entities and relationships into the graph DB

- **Required additions:**
  - New private function `stage2_persist_sessions_impl()` to:
    - Accept: `session_ids: &[String]`, `user_id: Uuid`, `session_store: Option<&dyn SessionStore>`, `llm: Arc<dyn Llm>`, graph + vector DBs, embedding engine, cognify_config
    - Load all Q&A entries for each session
    - Run cognify pipeline on aggregated Q&A text
    - Persist results to graph with node_set tag
  - Optional: Batch by session or combine all sessions into one cognify run

**Stage 4: Sync Graph to Session Cache**  
*Current code:* `/home/dmytro/dev/cognee/cognee-rust/crates/lib/src/api/improve.rs:239–255`
- **What's missing:**
  - Query graph DB for new edges (since last sync checkpoint)
  - Serialize edges as structured JSON or summaries (e.g., "Entity A → relationship → Entity B")
  - Store summaries in session store's graph context (via `SessionStore::set_graph_context()`)

- **Required additions:**
  - New private function `stage4_sync_graph_to_session_impl()` to:
    - Accept: `session_ids: &[String]`, `user_id: Uuid`, `session_store: Option<&dyn SessionStore>`, `graph_db: &dyn GraphDBTrait`
    - Query recent edges from graph DB (all or filtered by dataset)
    - Format as JSON-lines or summaries
    - Update each session's graph context via `SessionStore::set_graph_context()`
  - Potential optimization: Use checkpoint table to track last-synced edge ID

#### B. RememberResult Incomplete Interface

**Missing methods/properties:**
- `to_dict()` — serialize to JSON-compatible dict (needed for HTTP responses)
- `__repr__()` / `__str__()` — human-readable output (needed for CLI/logging)
- `async fn await()` — support awaiting background tasks (only relevant if Rust were to support background task spawning)
- `done` property — check task completion status
- `__bool__()` — treat as success flag (True if completed/session_stored)

**Required additions:**
- Implement Display + Serialize + Deserialize on RememberResult
- Add helper methods for introspection
- Consider: if background tasks are added to Rust API later, provide task handle + await support

#### C. Session Mode Missing Complete Flow

**Python behavior:**
- When `session_id` is provided: data goes to session cache only (no permanent dataset)
- RememberResult status is `"session_stored"` (not `"completed"`)
- Optional background `improve(session_ids=[session_id])` to bridge to permanent graph
- Caller can await result to wait for background bridge to finish

**Rust current state:**
- `remember_session()` exists and stores to session cache
- Returns RememberResult with status=SessionStored
- **Missing:** Background task spawning for session bridge
  - Currently, improve is called synchronously (blocking)
  - Need: tokio::spawn or similar to enable async fire-and-forget

**Required additions:**
- Modify `remember()` to support background task spawning when `self_improvement=True` in session mode
- Return RememberResult with task handle (or just status "running" if task is background-only)
- Ensure proper error handling: background task failures logged but don't fail the remember call

#### D. Parameter Routing Missing (RememberKwargs equivalence)

**Python:**
- `remember()` accepts `**kwargs: Unpack[RememberKwargs]`
- Kwargs are routed to add(), cognify(), or both via frozensets (_ADD_ONLY, _COGNIFY_ONLY, _SHARED)

**Rust current state:**
- `remember()` signature is flat (takes all parameters explicitly)
- No kwargs routing layer

**Required additions:**
- If full kwargs support is needed: create a RememberParams builder struct (cleaner than **kwargs)
- Or: extend the existing remember() signature with optional parameter sets (e.g., `add_params: Option<AddParams>`, `cognify_config: Option<&CognifyConfig>`)

---

### Minor Gaps

#### E. Content Hash Tracking in RememberResult

**Python:** Items list includes `content_hash` from Data.content_hash (MD5 or SHA256)

**Rust:** RememberItemInfo has `content_hash: Option<String>` but may not be populated correctly

**Required addition:**
- Ensure Data objects passed to cognify() include content_hash field
- Copy hash to RememberItemInfo during result assembly

#### F. Token Count in RememberResult

**Python:** Items list includes `token_count` from Data.token_count (set during cognify)

**Rust:** RememberItemInfo lacks `token_count` field

**Required addition:**
- Add `token_count: Option<usize>` to RememberItemInfo
- Populate from cognify result's CognifyResult::data_points or similar

#### G. Data Size Telemetry

**Python:** `_estimate_data_size()` computes byte size for telemetry

**Rust:** No telemetry layer; not critical but helpful for observability

**Required addition:**
- Optional: compute data size in remember() and pass to tracing span attributes

---

## 5. Effort estimate

### Summary Table

| Component | Size | Effort | Notes |
|-----------|------|--------|-------|
| RememberResult interface | S | 2–3h | Add Display, Serialize, helper methods |
| Session mode background task | S | 2–4h | tokio::spawn improve() call; task handle return |
| Improve Stage 2 (Persist Q&A) | M | 6–8h | Load Q&A, cognify, insert to graph with node_set tag |
| Improve Stage 4 (Sync Graph) | M | 6–8h | Query graph, serialize edges, update session context |
| Parameter routing (RememberKwargs) | S | 2–3h | Builder pattern or extend signature (low priority) |
| Content hash + token count fields | S | 1–2h | Propagate from cognify result |
| Integration tests | M | 4–6h | Test both modes, background task, improve stages |
| **Total** | — | **23–34 hours** | |

### T-Shirt Estimate

**XL** (extra-large, 5–6 days of continuous work)

### Rationale

- **Core remember() logic:** Already exists and works (add + cognify)
- **Main complexity:** Improve stages 2 & 4 require LLM integration, graph querying, and session sync — this is the bulk of the work
- **Session mode:** Needs background task support (tokio integration), which is straightforward once the patterns are established
- **Testing:** Multiple modes (permanent/session), with/without self-improvement, all combinations of stages
- **Risk factors:** None critical, but improve stages 2 & 4 need careful interaction with LLM and session store APIs

---

## Implementation Roadmap (Recommended Order)

1. **Fix RememberResult interface** (S, 2–3h) — unblock all callers
2. **Add content_hash + token_count fields** (S, 1–2h) — complete result object
3. **Implement Improve Stage 2** (M, 6–8h) — bridge Q&A text to graph
4. **Implement Improve Stage 4** (M, 6–8h) — sync graph back to session
5. **Add background task support for session mode** (S, 2–4h) — enable async bridging
6. **Integration tests** (M, 4–6h) — verify all modes and stage combinations
7. **Parameter routing (optional, low priority)** — only if kwargs flexibility becomes a requirement

