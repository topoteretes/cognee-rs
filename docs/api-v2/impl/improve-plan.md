# Implementation Plan: `improve()`

**Gap doc:** [../improve.md](../improve.md)  
**Python reference:** `cognee/api/v1/improve/improve.py`  
**Rust entry point:** `crates/lib/src/api/improve.rs`

---

## 1. Goal & Scope

Bring Rust `improve()` to full parity with Python `cognee.api.v1.improve.improve()` in `/tmp/cognee-python/cognee/api/v1/improve/improve.py:36-161`. The Python function runs four stages; Stage 3 (`memify`) is already complete in Rust (`crates/cognify/src/memify/pipeline.rs:47`). Stages 1, 2, and 4 are stubs (`crates/lib/src/api/improve.rs:156-280`). This plan closes those three gaps and wires them into the existing orchestrator, preserving the Python semantics: non-fatal failures per stage, idempotency via `memify_metadata.feedback_weights_applied`, skip of Stage 4 when `run_in_background=true`, session-scoped checkpoints.

In scope:
- Stage 1: feedback-weight apply pipeline (port of `cognee/tasks/memify/apply_feedback_weights.py` + `extract_feedback_qas.py`).
- Stage 2: session Q&A persistence (port of `cognee/memify_pipelines/persist_sessions_in_knowledge_graph.py` + `tasks/memify/extract_user_sessions.py` + `cognify_session.py`).
- Stage 4: incremental graph→session sync (port of `cognee/tasks/memify/sync_graph_to_session.py`).
- New `GraphDBTrait` batch feedback-weight methods and an in-place Ladybug implementation.
- New `CheckpointStore` trait with a SeaORM-backed default.
- New `DatabaseConnection` op `get_edges_since()`.

Out of scope: Phases 6–8 of Gap-6 (session search / LLM completion / auto-feedback), `get_graph_engine()` multi-backend plumbing, Postgres-backed graph feedback methods (leave as `update_edge_property` warning stub), Redis cache engine for checkpoints (optional fallback).

---

## 2. Design Overview

Per-stage mapping of Python → Rust:

| Stage | Python entry | Rust placement |
|---|---|---|
| 1. Feedback weights | `apply_feedback_weights_pipeline()` → `extract_feedback_qas()` → `apply_feedback_weights()` | New module `crates/cognify/src/memify/feedback_weights.rs` + trait methods on `GraphDBTrait` |
| 2. Persist sessions | `persist_sessions_in_knowledge_graph_pipeline()` → `extract_user_sessions()` → `cognify_session()` | New module `crates/cognify/src/memify/persist_sessions.rs` using `AddPipeline::add` + `cognify()` |
| 3. Triplet embeddings | `memify()` in `modules/memify/memify.py` | Already done: `cognee_cognify::memify::memify()` |
| 4. Graph→session sync | `sync_graph_to_session()` | New module `crates/cognify/src/memify/sync_graph_session.rs` using `DatabaseConnection` edge ops + `CheckpointStore` |

### 2.1 Stage 1 design — batch graph property updates

**The trait needs new methods.** Python calls four entry points on `graph_engine` (see `apply_feedback_weights.py:38-41` and :180-189): `get_node_feedback_weights`, `set_node_feedback_weights`, `get_edge_feedback_weights`, `set_edge_feedback_weights`. The existing `update_node_property()` in `crates/graph/src/traits.rs:317-346` has a *cascade-delete default* that rebuilds the node for every call and drops to `add_edges` for restoration (poor atomicity, O(deg)). `update_edge_property()` at :359-373 is a no-op warning. Both are unacceptable for batch feedback-weight writes over potentially hundreds of nodes/edges per Q&A entry.

Add four new async trait methods to `GraphDBTrait`. Ladybug will override them with a single transactional Cypher-like SET query. The default implementation falls back to the existing per-element path.

Node IDs are strings; edge IDs in Python are strings too, but Rust's `EdgeData` keys edges by `(source_id, target_id, relationship_name)`. To keep symmetry and avoid breaking Ladybug, we treat edge IDs as `(String, String, String)` tuples internally and expose a string-key API (`"source|||target|||rel"`) on the public trait.

### 2.2 Stage 2 design — cognify from session text

Python (`cognify_session.py:25-35`) does exactly this:
```python
await cognee.add(data, dataset_id=dataset_id, node_set=["user_sessions_from_cache"])
await cognee.cognify(datasets=[dataset_id])
```
It reuses the full `add` + `cognify` pipelines; there is no "cognify variant." Rust already has both:
- `AddPipeline::add()` in `crates/ingestion/src/pipeline.rs:776` with `AddParams::node_set: Option<Vec<String>>` (:36-49).
- `cognee_cognify::cognify()` in `crates/cognify/src/tasks.rs:1718`.

Stage 2 therefore does **not** need a new cognify entry point. It needs a thin coordinator that:
1. For each session, calls `SessionStore::get_all_qa_entries()`.
2. Concatenates `"Session ID: {sid}\n\nQuestion: {q}\n\nAnswer: {a}\n\n"` blocks (matches `extract_user_sessions.py:62-67`).
3. Skips empty concatenations.
4. Calls `AddPipeline::add(DataInput::Text(session_text), dataset_name, owner_id, tenant_id, AddParams { node_set: Some(vec!["user_sessions_from_cache".into()]), ..Default::default() })`.
5. Calls `cognify(...)` on the resulting data row.

### 2.3 Stage 4 design — incremental graph→session sync

Python (`sync_graph_to_session.py:74-106`, :142-234) reads **relational-DB edges** (SeaORM `edges` table in Rust — confirmed at `crates/database/src/entities/edge.rs:23`, which has `created_at: DateTimeUtc`). It joins nodes for label/type/description, renders JSON-lines, merges with existing session `graph_context`, caps total lines, and persists a checkpoint timestamp.

Rust implementation:
- New `DatabaseConnection` op: `get_edges_since(dataset_id, since: Option<DateTime<Utc>>, limit: usize) -> Vec<edge::Model>` alongside `get_edges_by_dataset` at `crates/database/src/ops/graph_storage.rs:97`.
- New `CheckpointStore` trait (minimal) with a SeaORM-backed impl. Storage table `graph_sync_checkpoints(key TEXT PK, ts TIMESTAMPTZ NOT NULL)` managed via a new migration. Redis support deferred.
- The sync loop paginates with `limit=BATCH_SIZE=500`, rolls `latest_ts` forward, merges with `SessionStore::get_graph_context(..)`, caps at `DEFAULT_MAX_LINES=500`, writes back via `SessionStore::set_graph_context(..)`, and saves the checkpoint on progress.

### 2.4 Stages 2 and 4 interdependence

Stage 2 writes new edges into the graph + SeaORM `edges` table via `cognify()`. Stage 4 reads from that same SeaORM table. For a fresh session (no prior checkpoint), Stage 4 will replay **all** edges, including those just produced by Stage 2. Python accepts this (idempotent line append + cap). Rust must match: run order is Stage 1 → Stage 2 → Stage 3 → Stage 4 in the orchestrator, and Stage 4 must be skipped when `run_in_background=true` (matches `improve.py:152`).

---

## 3. Prerequisites (Gap-6 Session Management)

This plan depends on — but does **not** duplicate — the Gap-6 plan at `docs/api-gaps/impl/06-session-management-plan.md`. Gap-6 already landed most of what we need:
- `SessionQAEntry.feedback_text`, `feedback_score`, `used_graph_element_ids`, `memify_metadata` fields.
- `UsedGraphElementIds { node_ids, edge_ids }` typed struct.
- `SessionQAUpdate` DTO.
- `SessionStore::update_qa_entry`, `get_graph_context`, `set_graph_context` on all three backends.
- `SessionManager::add_feedback`, `delete_feedback`, `update_qa`, `get_graph_context`, `set_graph_context`.

Still missing (**must land in Gap-6 before Stage 1 can be end-to-end tested** — but not a compile blocker):
- **Capture of `used_graph_element_ids` in retrievers.** The search-side pipeline must record the node/edge IDs it touched when generating an answer and persist them via `SessionManager::update_qa` with `used_graph_element_ids: Some(Some(...))`.
- Confirm `used_graph_element_ids` survives a round-trip through each store's serializer.

---

## 4. Step-by-Step Implementation

### Step 1 — Extend `GraphDBTrait` with batch feedback-weight methods

**File:** `crates/graph/src/traits.rs`  
**Depends on:** nothing.

```rust
async fn get_node_feedback_weights(
    &self,
    node_ids: &[String],
) -> GraphDBResult<HashMap<String, f64>> {
    let mut out = HashMap::with_capacity(node_ids.len());
    for id in node_ids {
        if let Some(node) = self.get_node(id).await? {
            if let Some(v) = node
                .get("feedback_weight")
                .and_then(|v| v.as_f64())
            {
                out.insert(id.clone(), v);
            }
        }
    }
    Ok(out)
}

async fn set_node_feedback_weights(
    &self,
    updates: &HashMap<String, f64>,
) -> GraphDBResult<HashMap<String, bool>> {
    let mut out = HashMap::with_capacity(updates.len());
    for (id, w) in updates {
        let ok = self
            .update_node_property(id, "feedback_weight", serde_json::json!(w))
            .await
            .is_ok();
        out.insert(id.clone(), ok);
    }
    Ok(out)
}

pub type EdgeKey = (String, String, String);

async fn get_edge_feedback_weights(
    &self,
    edge_keys: &[EdgeKey],
) -> GraphDBResult<HashMap<EdgeKey, f64>>;

async fn set_edge_feedback_weights(
    &self,
    updates: &HashMap<EdgeKey, f64>,
) -> GraphDBResult<HashMap<EdgeKey, bool>>;
```

### Step 2 — Implement the four methods in Ladybug

**File:** `crates/graph/src/ladybug.rs`  
**Depends on:** Step 1.

Override all four methods on `LadybugAdapter` using native Cypher-like queries (Ladybug supports `MATCH … SET n.prop = $val`):

```rust
async fn get_node_feedback_weights(
    &self,
    node_ids: &[String],
) -> GraphDBResult<HashMap<String, f64>> {
    if node_ids.is_empty() { return Ok(HashMap::new()); }
    let cypher = "MATCH (n) WHERE n.id IN $ids RETURN n.id, coalesce(n.feedback_weight, 0.5)";
    let params = HashMap::from([(Cow::Borrowed("ids"), json!(node_ids))]);
    let rows = self.query(cypher, Some(params)).await?;
    // parse rows into map
}

async fn set_node_feedback_weights(
    &self,
    updates: &HashMap<String, f64>,
) -> GraphDBResult<HashMap<String, bool>> {
    // UNWIND $pairs AS p MATCH (n {id: p.id}) SET n.feedback_weight = p.w RETURN p.id
    // single round-trip
}
```

Also override `update_node_property` / `update_edge_property` on Ladybug to use single-statement SET — this fixes the cascade-delete bug for all current callers.

### Step 3 — Add `CheckpointStore` trait and SeaORM backend

**File (new):** `crates/database/src/ops/checkpoint.rs`  
**File:** `crates/database/src/entities/graph_sync_checkpoint.rs` (new entity)  
**File:** `crates/database/src/migrator/` — new migration `mYYYYMMDD_NNNNNN_graph_sync_checkpoints.rs`  

```rust
#[async_trait]
pub trait CheckpointStore: Send + Sync {
    async fn load(&self, key: &str) -> Result<Option<DateTime<Utc>>, DatabaseError>;
    async fn save(&self, key: &str, ts: DateTime<Utc>) -> Result<(), DatabaseError>;
}

// Impl on DatabaseConnection via a table:
//   CREATE TABLE graph_sync_checkpoints (
//       key TEXT PRIMARY KEY,
//       ts TIMESTAMP WITH TIME ZONE NOT NULL
//   )
```

Key format mirrors Python (`sync_graph_to_session.py:38-39`): `"graph_sync_checkpoint:{user_id}:{dataset_id}:{session_id}"`.

### Step 4 — Add `get_edges_since()` DB op

**File:** `crates/database/src/ops/graph_storage.rs`  
**Depends on:** nothing.

```rust
pub async fn get_edges_since(
    db: &DatabaseConnection,
    dataset_id: Uuid,
    since: Option<DateTime<Utc>>,
    limit: u64,
) -> Result<Vec<GraphEdge>, DatabaseError> {
    let mut q = edge::Entity::find()
        .filter(edge::Column::DatasetId.eq(uuid_hex::to_hex(dataset_id)))
        .order_by_asc(edge::Column::CreatedAt)
        .limit(limit);
    if let Some(ts) = since {
        q = q.filter(edge::Column::CreatedAt.gt(ts));
    }
    q.all(db).await.map_err(map_sea_err)
        .map(|v| v.into_iter().map(GraphEdge::from).collect())
}

pub async fn get_nodes_by_ids(
    db: &DatabaseConnection,
    ids: &[String],
) -> Result<Vec<GraphNode>, DatabaseError> { /* used by Stage 4 for label/description lookup */ }
```

### Step 5 — Port Stage 1: feedback-weight pipeline

**File (new):** `crates/cognify/src/memify/feedback_weights.rs`  
**Depends on:** Steps 1, 2; Gap-6 session fields.

```rust
/// Map feedback score 1..5 to 0..1. Matches Python normalize_feedback_score().
pub fn normalize_feedback_score(score: i32) -> Result<f64, FeedbackError> {
    if !(1..=5).contains(&score) {
        return Err(FeedbackError::InvalidScore(score));
    }
    Ok((score as f64 - 1.0) / 4.0)
}

/// Streaming update: w' = w + α*(r - w), clipped to [0, 1], rounded to 4 dp.
pub fn stream_update_weight(prev: f64, rating: f64, alpha: f64) -> Result<f64, FeedbackError> {
    if !(0.0 < alpha && alpha <= 1.0) { return Err(FeedbackError::InvalidAlpha(alpha)); }
    let updated = prev + alpha * (rating - prev);
    let clipped = updated.clamp(0.0, 1.0);
    Ok((clipped * 10_000.0).round() / 10_000.0)
}

pub struct FeedbackApplyResult {
    pub processed: usize,
    pub applied: usize,
    pub skipped: usize,
}

pub async fn apply_feedback_weights_pipeline(
    session_ids: &[String],
    owner_id: Uuid,
    alpha: f64,
    graph_db: &dyn GraphDBTrait,
    session_store: &dyn SessionStore,
    session_manager: &SessionManager,
) -> Result<FeedbackApplyResult, FeedbackError> {
    // 1. extract_feedback_qas equivalent: for each session, get_all_qa_entries,
    //    filter via _is_eligible (valid 1-5 score, not applied, has ids).
    // 2. For each eligible entry:
    //    - normalize score, build node_ids & edge_ids vectors.
    //    - batch fetch existing weights via get_{node,edge}_feedback_weights.
    //    - compute updates via stream_update_weight.
    //    - batch write via set_{node,edge}_feedback_weights.
    //    - session_manager.update_qa(.., memify_metadata = {"feedback_weights_applied": success}).
    // 3. Track (processed, applied, skipped); log each entry.
}
```

Eligibility matches `extract_feedback_qas.py:15-41` exactly.

### Step 6 — Port Stage 2: persist sessions

**File (new):** `crates/cognify/src/memify/persist_sessions.rs`  
**Depends on:** nothing new.

```rust
pub struct PersistSessionsResult { pub sessions_persisted: usize }

pub async fn persist_sessions_in_knowledge_graph(
    session_ids: &[String],
    dataset_name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    session_store: &dyn SessionStore,
    add_pipeline: &AddPipeline,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    db: Option<Arc<DatabaseConnection>>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    cognify_config: &CognifyConfig,
) -> Result<PersistSessionsResult, PersistError> {
    let user_id_str = owner_id.to_string();
    let mut count = 0;

    for sid in session_ids {
        let entries = session_store.get_all_qa_entries(sid, Some(&user_id_str)).await?;
        if entries.is_empty() { continue; }

        let mut buf = format!("Session ID: {sid}\n\n");
        for e in &entries {
            buf.push_str(&format!("Question: {}\n\nAnswer: {}\n\n", e.question, e.answer));
        }

        let add_result = add_pipeline.add_with_params(
            DataInput::Text(buf),
            dataset_name,
            owner_id,
            tenant_id,
            &AddParams {
                node_set: Some(vec!["user_sessions_from_cache".into()]),
                ..Default::default()
            },
        ).await?;

        cognify(
            add_result.data_items,
            add_result.dataset_id,
            Some(owner_id),
            tenant_id,
            Arc::clone(&llm),
            Arc::clone(&storage),
            Arc::clone(&graph_db),
            Arc::clone(&vector_db),
            Arc::clone(&embedding_engine),
            db.clone(),
            Arc::clone(&ontology_resolver),
            cognify_config,
        ).await?;

        count += 1;
    }
    Ok(PersistSessionsResult { sessions_persisted: count })
}
```

### Step 7 — Port Stage 4: graph→session sync

**File (new):** `crates/cognify/src/memify/sync_graph_session.rs`  
**Depends on:** Steps 3, 4.

```rust
const BATCH_SIZE: u64 = 500;
const DEFAULT_MAX_LINES: usize = 500;

pub struct SyncResult { pub synced: usize, pub total: usize }

pub async fn sync_graph_to_session(
    user_id: &str,
    session_id: &str,
    dataset_id: Uuid,
    db: &DatabaseConnection,
    session_manager: &SessionManager,
    checkpoint_store: &dyn CheckpointStore,
    max_lines: usize,
) -> Result<SyncResult, SyncError> {
    let ck = format!("graph_sync_checkpoint:{user_id}:{dataset_id}:{session_id}");
    let mut since = checkpoint_store.load(&ck).await?;

    let mut new_lines = Vec::new();
    let mut latest = since;

    loop {
        let edges = get_edges_since(db, dataset_id, latest, BATCH_SIZE).await?;
        if edges.is_empty() { break; }

        let ids: Vec<String> = edges.iter()
            .flat_map(|e| [e.source_node_id.clone(), e.destination_node_id.clone()])
            .collect();
        let nodes = get_nodes_by_ids(db, &ids).await?;
        let nmap: HashMap<String, GraphNode> = nodes.into_iter().map(|n| (n.id.clone(), n)).collect();

        for e in &edges {
            if let Some(line) = edge_to_json_line(e, &nmap) {
                new_lines.push(line);
            }
            if latest.map(|t| e.created_at > t).unwrap_or(true) {
                latest = Some(e.created_at);
            }
        }

        if (edges.len() as u64) < BATCH_SIZE { break; }
    }

    if new_lines.is_empty() { return Ok(SyncResult { synced: 0, total: 0 }); }

    let existing = session_manager.get_graph_context(Some(session_id), Some(user_id)).await?;
    let existing_lines: Vec<&str> = existing.as_deref()
        .map(|s| s.split('\n').filter(|l| !l.is_empty()).collect())
        .unwrap_or_default();
    let mut merged: Vec<String> = existing_lines.iter().map(|s| s.to_string()).collect();
    merged.extend(new_lines.iter().cloned());
    if merged.len() > max_lines {
        let drop = merged.len() - max_lines;
        merged.drain(0..drop);
    }
    let merged_str = merged.join("\n");
    session_manager.set_graph_context(Some(session_id), Some(user_id), &merged_str).await?;

    if let Some(ts) = latest {
        if Some(ts) != since { checkpoint_store.save(&ck, ts).await?; }
    }

    Ok(SyncResult { synced: new_lines.len(), total: merged.len() })
}
```

### Step 8 — Wire stages into `improve()` orchestrator

**File:** `crates/lib/src/api/improve.rs`  
**Depends on:** Steps 1–7.

Replace the three stub helpers with calls into the new modules:

```rust
pub async fn improve(
    dataset_name: &str,
    session_ids: Option<Vec<String>>,
    node_name: Option<Vec<String>>,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    feedback_alpha: f64,
    run_in_background: bool,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    db: Option<Arc<DatabaseConnection>>,
    session_store: Option<Arc<dyn SessionStore>>,
    session_manager: Option<Arc<SessionManager>>,
    add_pipeline: Option<&AddPipeline>,
    checkpoint_store: Option<Arc<dyn CheckpointStore>>,
    cognify_config: &CognifyConfig,
) -> Result<ImproveResult, ApiError> { ... }
```

Orchestration body wraps each stage in a try/except that logs a warning but does not abort the pipeline (matches `improve.py:199-225, 246-279`). Stage 4 is skipped when `run_in_background=true` (matches `improve.py:152`).

### Step 9 — Re-exports and public API

**File:** `crates/cognify/src/memify/mod.rs` — `pub mod feedback_weights; pub mod persist_sessions; pub mod sync_graph_session;`  
**File:** `crates/cognify/src/lib.rs` — re-export the three new functions + `FeedbackApplyResult`, `PersistSessionsResult`, `SyncResult`.  
**File:** `crates/lib/src/lib.rs` — already re-exports `cognee_cognify::*`; verify the new symbols surface.  
**File:** `crates/cli/src/main.rs` — optional: plumb `improve` args (`--session-id`, `--alpha`, `--background`).

### Step 10 — Update `CognifyError` / `ApiError` mapping

**File:** `crates/lib/src/api/error.rs`  
Add `ApiError::Improve(String)` so telemetry distinguishes stage-1/2/4 failures from Stage-3 memify failures.

---

## 5. Test Plan

### Stage 1 unit tests (`crates/cognify/src/memify/feedback_weights.rs`)
- `normalize_feedback_score(1) == 0.0`, `(3) == 0.5`, `(5) == 1.0`, `(0)` / `(6)` error.
- `stream_update_weight(0.5, 1.0, 0.1) == 0.55` rounded.
- `stream_update_weight(0.0, 0.0, 1.0) == 0.0`; clip behaviour: `(0.9, 2.0, 0.5)` clamped.
- Alpha validation: `0.0` and `1.1` error.

### Stage 1 integration (`crates/cognify/tests/memify_feedback.rs`)
- Fresh `MockGraphDB` with 3 nodes + 3 edges initialized with `feedback_weight = 0.5`.
- Create session with 2 QA entries, one with `score=5` + `used_graph_element_ids`, one with `score=1`.
- Run pipeline with `alpha=0.1`, verify node weights moved toward 1.0 / 0.0 respectively.
- Re-run: entries already `feedback_weights_applied=true` must be skipped.
- Eligibility: entry without `used_graph_element_ids` is skipped; entry with invalid score is skipped.

### Stage 2 integration (`crates/cognify/tests/memify_persist_sessions.rs`)
- Seeded session with 2 Q&A entries.
- Use `MockLlm` that returns a deterministic fact-extraction response.
- Verify after run: (a) new `Data` row exists with `node_set` containing `"user_sessions_from_cache"`, (b) at least one `Entity` node and `Edge` created in the graph, (c) `source_node_set` field on DataPoints contains the tag.
- Empty sessions: function returns `sessions_persisted=0` without errors.

### Stage 4 integration (`crates/cognify/tests/memify_sync_graph_session.rs`)
- Seed graph with 10 edges distributed across 2 datasets; create a session.
- First run: `synced == number of edges in target dataset`, checkpoint stored, `graph_context` contains expected JSON-lines.
- Add 3 more edges, run again: `synced == 3`, checkpoint updated.
- Add 600 edges (over the 500 cap): verify final `total == 500`, oldest lines dropped.
- `run_in_background=true` in the orchestrator: Stage 4 not called.

### Orchestrator end-to-end (`crates/lib/tests/improve_e2e.rs`)
- Real-ish harness with in-memory SQLite + Ladybug + mock LLM + mock embeddings.
- `improve()` with `session_ids=None`: only Stage 3 runs, `stages_run == ["memify"]`.
- `improve()` with 2 sessions, one with feedback data: `stages_run == ["apply_feedback_weights", "persist_sessions", "memify", "sync_graph_to_session"]`.
- Stage 1 failure injected: Stage 2-4 still run; warning logged.

---

## 6. Effort Breakdown

Scaled to the gap doc's "5–8 engineer-weeks" total.

| Stage | Work item | Hours |
|---|---|---|
| Stage 1 | Trait methods + Ladybug Cypher (Steps 1–2) | 10 |
| Stage 1 | Pipeline port (Step 5) | 8 |
| Stage 1 | Unit + integration tests | 8 |
| Stage 2 | Persist-sessions coordinator (Step 6) | 10 |
| Stage 2 | Integration tests with mock LLM | 8 |
| Stage 2 | Investigate add() signature reuse, fix if needed | 4 |
| Stage 4 | `CheckpointStore` trait + SeaORM impl + migration (Step 3) | 8 |
| Stage 4 | `get_edges_since` + `get_nodes_by_ids` ops (Step 4) | 4 |
| Stage 4 | Sync logic + JSON-line rendering (Step 7) | 10 |
| Stage 4 | Integration tests including cap + checkpoint | 8 |
| Orchestrator | Wire all stages, error handling, new `ImproveResult` fields (Step 8) | 6 |
| Cross-cutting | Re-exports, CLI wiring, docs (Steps 9–10) | 4 |
| Cross-cutting | End-to-end tests + cross-SDK test | 12 |
| Cross-cutting | Code review + polish | 8 |
| **Total** | | **108 h (~2.7 weeks × 1 engineer)** |

Gap-doc range (5–8 weeks) accounts for:
- Gap-6 follow-up work to populate `used_graph_element_ids` in retrievers (not counted here).
- Cross-SDK parity debugging (LLM non-determinism, embedding tolerances).
- Postgres graph backend parity work if scope expands.
- Documentation + migration notes for existing users.

---

## 7. Out of Scope

- **Postgres graph batch feedback methods.** `pg_graph_adapter.rs` keeps the default implementation for nodes and returns `NotSupported` for edges.
- **Redis-backed `CheckpointStore`.**
- **`get_authorized_dataset` resolution from UUID.** Rust `improve()` takes `dataset_name: &str` only.
- **Telemetry spans.** Python emits OpenTelemetry spans; Rust uses `tracing` but not OpenTelemetry.
- **Retriever-side `used_graph_element_ids` capture.** Belongs to Gap-6 / the search pipeline.
- **`auto_feedback` LLM detection** (Gap-6 Phase 8).
- **Concurrent per-session processing.** Python runs sessions sequentially; Rust matches.
- **CLI `improve` subcommand** (optional).

---

## Critical files for implementation

- `/home/dmytro/dev/cognee/cognee-rust/crates/lib/src/api/improve.rs`
- `/home/dmytro/dev/cognee/cognee-rust/crates/graph/src/traits.rs`
- `/home/dmytro/dev/cognee/cognee-rust/crates/graph/src/ladybug.rs`
- `/home/dmytro/dev/cognee/cognee-rust/crates/cognify/src/memify/mod.rs`
- `/home/dmytro/dev/cognee/cognee-rust/crates/database/src/ops/graph_storage.rs`
