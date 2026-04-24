# Implementation Plan: `remember()`

**Gap doc:** [../remember.md](../remember.md)  
**Python reference:** `cognee/api/v1/remember/remember.py`  
**Rust entry point:** `crates/lib/src/api/remember.rs`

## Status

- Implemented: yes (RememberParams/RememberContext refactor deferred)
- Commit: `4da7623`
- Date: 2026-04-24

---

## 1. Goal & scope

Close the gaps identified in `docs/api-v2/remember.md` so that `cognee_lib::remember()` matches the Python `cognee.remember()` contract in both modes (permanent memory and session memory), with a promise-like `RememberResult` that supports introspection, JSON serialization, background completion, and async awaiting.

### Final user-visible API (Rust)

```rust
// crates/lib/src/api/remember.rs
pub async fn remember(
    data: Vec<DataInput>,
    dataset_name: &str,
    params: RememberParams,                    // NEW: bundled optional params
    ctx: &RememberContext,                     // NEW: bundled backend handles
) -> Result<RememberResult, ApiError>;
```

`RememberParams` carries everything currently passed as positional/optional args: `session_id`, `session_ids`, `self_improvement`, `run_in_background`, `owner_id`, `tenant_id`, plus forward-compatible overrides (`chunk_size`, `custom_prompt`, `node_set`, `importance_weight`). `RememberContext` groups the backend Arcs (llm, storage, graph_db, vector_db, embedding_engine, db, session_store, ontology_resolver, add_pipeline, cognify_config).

Back-compat: the current flat function is kept as `remember_simple()` (or the existing `remember()` is renamed internally), and a thin wrapper forwards to the new API — so callers in `crates/lib/src/lib.rs` and any bindings don't break.

### Modes to support

1. **Permanent Memory (blocking)** — no `session_id`, `run_in_background=false`: `add()` → `cognify()` → optional `improve()`. Returns fully-populated `RememberResult` with `status=Completed`.
2. **Permanent Memory (background)** — no `session_id`, `run_in_background=true`: spawn the pipeline via `tokio::spawn`, return immediately with `status=Running` and a `JoinHandle` attached. Awaiting the result drives completion.
3. **Session Memory (store-only)** — `session_id` set, `self_improvement=false`: text-ify → `SessionStore::create_qa_entry()` → return `status=SessionStored`.
4. **Session Memory (bridged)** — `session_id` set, `self_improvement=true`: same as (3) plus a background `improve(session_ids=[session_id])` task, which runs all four improve stages (including the newly-implemented stages 2 & 4).

---

## 2. Design overview

### New / modified types

| Item | File | Action |
|------|------|--------|
| `RememberStatus::Running` variant | `crates/lib/src/api/remember.rs` | **Add** variant |
| `RememberItemInfo` | `crates/lib/src/api/remember.rs` | **Add fields** `token_count: Option<i64>`, `data_size: Option<i64>` |
| `RememberResult` | `crates/lib/src/api/remember.rs` | **Add fields** `pipeline_run_id`, `content_hash`, `error`; **add methods** `to_dict()`, `is_success()`, `done()`, `await_completion()` |
| `RememberParams` struct | `crates/lib/src/api/remember.rs` | **New** builder-style params bundle |
| `RememberContext` struct | `crates/lib/src/api/remember.rs` | **New** backend-handles bundle |
| `stage2_persist_sessions_impl` | `crates/lib/src/api/improve.rs` | **Replace stub** with real implementation |
| `stage4_sync_graph_to_session_impl` | `crates/lib/src/api/improve.rs` | **Replace stub** with real implementation |
| `ApiError::Join` | `crates/lib/src/api/error.rs` | **Add** for `tokio::task::JoinError` |

### File layout

All changes stay inside `crates/lib/src/api/` (the top-level API facade). No new crates; no new traits. Improve-stage changes stay inside the already-existing `improve.rs`.

The promise-like behavior uses `Arc<tokio::sync::Mutex<RememberResultInner>>` behind the scenes so that the background task can mutate elapsed / status / error while the caller still holds a clone of the handle. `RememberResult` is renamed to a thin public wrapper that either owns its data (sync mode) or points at shared inner state + a `JoinHandle<()>` (background mode).

### Trade-offs considered

- **Separate `RememberResultFuture` vs. unified `RememberResult`** — Python's `__await__` blurs the line between a data object and a future. Rust can do the same via `impl IntoFuture for RememberResult`, but that's surprising and clashes with `Clone`. Chosen approach: provide an explicit `async fn await_completion(self) -> Self` method on `RememberResult` (plus `done()` / `is_success()` accessors). Matches Python semantics without abusing `IntoFuture`.
- **Builder struct vs. kwargs-style HashMap** — Python uses `**kwargs` with runtime validation. Rust uses a `RememberParams` struct with `Default::default()` + `with_*` setters — compile-time safe and matches existing `MemifyConfig` / `CognifyConfig` patterns (see `crates/cognify/src/config.rs:29`).
- **Background task ownership** — `tokio::spawn` requires `'static` futures. The `RememberContext` fields are `Arc<dyn Trait>` already (see `crates/lib/src/api/remember.rs:82–89`), which clones cleanly into the spawned task. No extra lifetime gymnastics.
- **Stage 2 scope** — We cognify session text in-place using the same pipeline (`cognee_cognify::cognify` at `crates/cognify/src/tasks.rs:1718`) rather than inventing a new "session-text" pipeline. We synthesize `Data` rows on the fly via `Data::builder(...).build()` so cognify's usual classify→chunk→extract loop runs unchanged.
- **Stage 4 scope** — Use existing `GraphDBTrait::get_graph_data()` (`crates/graph/src/traits.rs:181`) to dump nodes + edges, then format a compact human-readable summary stored via `SessionStore::set_graph_context()` (`crates/session/src/session_store.rs:94`). No new graph-query method needed.

---

## 3. Step-by-step implementation

### Step 1 — Extend `RememberResult` surface (2h, no deps) — [x] done in commit `4da7623`

**File:** `crates/lib/src/api/remember.rs`

Add fields and helpers. Sketch:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RememberStatus {
    Running,            // NEW
    Completed,
    Errored,
    SessionStored,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RememberItemInfo {
    pub id: Option<Uuid>,
    pub name: Option<String>,
    pub content_hash: Option<String>,
    pub token_count: Option<i64>,     // NEW  (Python parity, Data::token_count)
    pub data_size: Option<i64>,       // NEW
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RememberResult {
    pub status: RememberStatus,
    pub dataset_name: String,
    pub dataset_id: Option<Uuid>,
    pub session_ids: Option<Vec<String>>,
    pub pipeline_run_id: Option<Uuid>,    // NEW
    pub elapsed_seconds: f64,
    pub content_hash: Option<String>,     // NEW (first item's hash)
    pub items_processed: usize,
    pub items: Vec<RememberItemInfo>,
    pub error: Option<String>,
    #[serde(skip)]
    pub cognify_result: Option<CognifyResult>,
    #[serde(skip)]
    pub memify_result: Option<MemifyResult>,
    #[serde(skip)]
    inner: Option<Arc<tokio::sync::Mutex<RememberResultInner>>>,
}

struct RememberResultInner {
    status: RememberStatus,
    error: Option<String>,
    elapsed_seconds: f64,
    cognify_result: Option<CognifyResult>,
    memify_result: Option<MemifyResult>,
    dataset_id: Option<Uuid>,
    items: Vec<RememberItemInfo>,
    items_processed: usize,
    content_hash: Option<String>,
    join_handle: Option<tokio::task::JoinHandle<()>>,
}

impl RememberResult {
    /// Serialize to a plain JSON object (Python `to_dict()` parity).
    pub fn to_dict(&self) -> serde_json::Value { serde_json::to_value(self).unwrap_or_default() }

    /// `true` if status is Completed or SessionStored (Python `__bool__`).
    pub fn is_success(&self) -> bool {
        matches!(self.status, RememberStatus::Completed | RememberStatus::SessionStored)
    }

    /// `true` if the pipeline has finished (success, error, or stored).
    pub fn done(&self) -> bool { self.status != RememberStatus::Running }

    /// Wait for the background task (if any) and refresh fields from inner state.
    /// Mirrors Python's `await result`.
    pub async fn await_completion(mut self) -> Self {
        if let Some(inner) = self.inner.clone() {
            let handle = {
                let mut guard = inner.lock().await;
                guard.join_handle.take()
            };
            if let Some(h) = handle {
                let _ = h.await;
            }
            let guard = inner.lock().await;
            self.status = guard.status;
            self.error = guard.error.clone();
            self.elapsed_seconds = guard.elapsed_seconds;
            self.dataset_id = guard.dataset_id;
            self.items = guard.items.clone();
            self.items_processed = guard.items_processed;
            self.content_hash = guard.content_hash.clone();
            self.cognify_result = guard.cognify_result.clone();
            self.memify_result = guard.memify_result.clone();
        }
        self
    }
}

impl fmt::Display for RememberResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RememberResult(status={:?}, dataset={:?}", self.status, self.dataset_name)?;
        if let Some(ref ids) = self.session_ids { write!(f, ", session_ids={:?}", ids)?; }
        if let Some(id) = self.dataset_id { write!(f, ", dataset_id={}", id)?; }
        if let Some(id) = self.pipeline_run_id { write!(f, ", pipeline_run_id={}", id)?; }
        if self.items_processed > 0 { write!(f, ", items={}", self.items_processed)?; }
        if let Some(ref h) = self.content_hash { write!(f, ", content_hash={:?}", h)?; }
        write!(f, ", elapsed={:.1}s", self.elapsed_seconds)?;
        if let Some(ref e) = self.error { write!(f, ", error={:?}", e)?; }
        write!(f, ")")
    }
}
```

Implement `Deserialize` manually (skip `inner`) or use `#[serde(default)]`.

### Step 2 — `RememberParams` + `RememberContext` (1.5h, depends on Step 1) — [ ] skipped (low priority; see remember.md Implementation notes)

**File:** `crates/lib/src/api/remember.rs`

```rust
#[derive(Debug, Default, Clone)]
pub struct RememberParams {
    pub session_id: Option<String>,
    pub session_ids: Option<Vec<String>>,
    pub self_improvement: bool,              // default true
    pub run_in_background: bool,             // default false
    pub owner_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub add_params: Option<AddParams>,
    pub chunk_size: Option<usize>,
    pub custom_prompt: Option<String>,
    pub node_set: Option<Vec<String>>,
}

pub struct RememberContext {
    pub add_pipeline: Arc<AddPipeline>,
    pub llm: Arc<dyn Llm>,
    pub storage: Arc<dyn StorageTrait>,
    pub graph_db: Arc<dyn GraphDBTrait>,
    pub vector_db: Arc<dyn VectorDB>,
    pub embedding_engine: Arc<dyn EmbeddingEngine>,
    pub db: Option<Arc<DatabaseConnection>>,
    pub session_store: Option<Arc<dyn SessionStore>>,
    pub ontology_resolver: Arc<dyn OntologyResolver>,
    pub cognify_config: Arc<CognifyConfig>,
}
```

Provide `impl Default for RememberParams` with `self_improvement: true`. Update the `pub fn remember(...)` to accept `params` + `ctx` and forward to split internal fns.

### Step 3 — Permanent-mode refactor + item enrichment (2h, depends on Step 1,2) — [x] done in commit `4da7623`

**File:** `crates/lib/src/api/remember.rs`

Extract the permanent-mode body into `async fn remember_permanent(...)`. After `cognify()` returns, populate `RememberItemInfo::token_count` and `data_size` from the `Data` records returned by `AddPipeline::add()` (they already carry these fields — see `crates/models/src/data.rs:48,50`). Set `content_hash` from `items.first().content_hash`.

```rust
let data_items = ctx.add_pipeline
    .add(data, dataset_name, owner_id, params.tenant_id).await
    .map_err(|e| ApiError::Ingestion(e.to_string()))?;

let items: Vec<RememberItemInfo> = data_items.iter().map(|d| RememberItemInfo {
    id: Some(d.id),
    name: Some(d.name.clone()),
    content_hash: Some(d.content_hash.clone()),
    token_count: (d.token_count >= 0).then_some(d.token_count),
    data_size: (d.data_size >= 0).then_some(d.data_size),
    mime_type: Some(d.mime_type.clone()),
}).collect();

let content_hash_first = items.first().and_then(|i| i.content_hash.clone());
```

`pipeline_run_id`: generate at the start (`Uuid::new_v4()`) and pass it into `RememberResult` — Python exposes this as a UUID from its pipeline tracker (`cognify/v1/cognify.py`); the Rust cognify pipeline doesn't expose one today, so a per-call UUID is an acceptable stand-in and preserves API parity.

### Step 4 — Background task support (3h, depends on Step 3) — [x] done in commit `4da7623`

**File:** `crates/lib/src/api/remember.rs`, `crates/lib/src/api/error.rs`

Add `ApiError::Join(#[from] tokio::task::JoinError)` variant in `error.rs` (currently only has `Ingestion`, `Cognify`, `Search`, etc. — see lines 27–58).

Implement spawning in `remember_permanent()`:

```rust
if params.run_in_background {
    let inner = Arc::new(tokio::sync::Mutex::new(RememberResultInner {
        status: RememberStatus::Running,
        /* ... */
    }));
    let inner_task = Arc::clone(&inner);
    let ctx_cloned = ctx.clone_arcs();
    let params_cloned = params.clone();
    let data_cloned = data;
    let dataset_name_owned = dataset_name.to_string();
    let start = Instant::now();

    let handle = tokio::spawn(async move {
        let outcome = run_permanent_inner(data_cloned, &dataset_name_owned,
                                          params_cloned, ctx_cloned, start).await;
        let mut guard = inner_task.lock().await;
        guard.elapsed_seconds = start.elapsed().as_secs_f64();
        match outcome {
            Ok(done) => {
                guard.status = RememberStatus::Completed;
                guard.dataset_id = done.dataset_id;
                guard.items = done.items;
                guard.items_processed = done.items_processed;
                guard.content_hash = done.content_hash;
                guard.cognify_result = Some(done.cognify_result);
                guard.memify_result = done.memify_result;
            }
            Err(e) => {
                guard.status = RememberStatus::Errored;
                guard.error = Some(e.to_string());
            }
        }
    });

    inner.lock().await.join_handle = Some(handle);
    return Ok(RememberResult {
        status: RememberStatus::Running,
        /* ... minimal fields ... */
        inner: Some(inner),
    });
}
```

### Step 5 — Session mode: background improve bridge (2h, depends on Steps 1,2,4) — [x] done in commit `4da7623`

**File:** `crates/lib/src/api/remember.rs`

Refactor `remember_session()` (currently lines 189–269). After `create_qa_entry()` succeeds, if `self_improvement` is true, spawn `improve(dataset=dataset_name, session_ids=[session_id], ...)` via `tokio::spawn` and attach the `JoinHandle` to `RememberResultInner`. The immediate return value carries `status=SessionStored` and a populated `session_ids`. Awaiting the result blocks until the bridge finishes. Important: background errors are logged and recorded to `inner.error` but never propagated as a failure — matches Python `_session_improve()` (lines 529–540 of `remember.py`).

### Step 6 — Improve Stage 2 real implementation (6h, depends on nothing in this plan) — [x] done in commit `9f0766a`

**File:** `crates/lib/src/api/improve.rs`

Replace `stage2_persist_sessions` stub (current lines 229–250) with a real implementation that runs the full cognify pipeline on session Q&A text.

```rust
async fn stage2_persist_sessions(
    session_ids: &[String],
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    session_store: Option<&dyn SessionStore>,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    db: Option<Arc<DatabaseConnection>>,
    ontology_resolver: Arc<dyn OntologyResolver>,
    cognify_config: &CognifyConfig,
) -> Result<usize, ApiError> {
    let Some(store) = session_store else { return Ok(0); };
    let user_id_str = owner_id.to_string();
    let mut persisted = 0usize;

    for sid in session_ids {
        let entries = store.get_all_qa_entries(sid, Some(&user_id_str)).await?;
        if entries.is_empty() { continue; }
        let combined = entries.iter()
            .map(|e| {
                if e.question.is_empty() { e.answer.clone() }
                else { format!("Q: {}\nA: {}", e.question, e.answer) }
            })
            .collect::<Vec<_>>()
            .join("\n\n---\n\n");

        let content_hash = format!("{:x}", md5::compute(&combined));
        let data_id = cognee_ingestion::generate_data_id(&content_hash, owner_id, tenant_id);
        let data = Data::builder(
                data_id,
                format!("session_{sid}.txt"),
                format!("session://{sid}"),
                format!("session://{sid}"),
                "txt", "text/plain", content_hash, owner_id,
            )
            .node_set(serde_json::to_string(&vec!["user_sessions_from_cache"])?.into())
            .build();

        let dataset_id = cognee_ingestion::generate_dataset_id(
            "_session_bridge_", owner_id, tenant_id);

        let _result = cognee_cognify::cognify(
            vec![data],
            dataset_id,
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
        ).await.map_err(|e| ApiError::Cognify(e.to_string()))?;

        persisted += 1;
    }
    info!(sessions = persisted, "improve stage 2: session persistence complete");
    Ok(persisted)
}
```

### Step 7 — Improve Stage 4 real implementation (5h, depends on Step 6's arg plumbing) — [x] done in commit `9f0766a`

**File:** `crates/lib/src/api/improve.rs`

Replace `stage4_sync_graph_to_session` stub (lines 258–280) with a real impl that:

1. Calls `graph_db.get_graph_data()` (`crates/graph/src/traits.rs:181`) to get nodes + edges.
2. Formats a compact summary, e.g. one line per edge: `"{source_name} -[{relationship}]-> {target_name}"`. Truncate to e.g. 256 edges.
3. Calls `session_store.set_graph_context(sid, Some(&user_id_str), &summary).await?` for each session ID.

```rust
async fn stage4_sync_graph_to_session(
    session_ids: &[String],
    owner_id: Uuid,
    graph_db: &dyn GraphDBTrait,
    session_store: Option<&dyn SessionStore>,
) -> Result<usize, ApiError> {
    let Some(store) = session_store else { return Ok(0); };
    let user_id_str = owner_id.to_string();

    let (nodes, edges) = graph_db.get_graph_data().await
        .map_err(ApiError::Graph)?;

    let name_of: HashMap<String, String> = nodes.iter()
        .map(|n| (n.id.to_string(),
                  n.properties.get("name")
                   .and_then(|v| v.as_str()).unwrap_or(&n.id.to_string()).to_string()))
        .collect();

    let mut lines = Vec::with_capacity(edges.len().min(256));
    for e in edges.iter().take(256) {
        let s = name_of.get(&e.source_node_id).cloned().unwrap_or(e.source_node_id.clone());
        let t = name_of.get(&e.target_node_id).cloned().unwrap_or(e.target_node_id.clone());
        lines.push(format!("{s} -[{}]-> {t}", e.relationship_name));
    }
    let summary = lines.join("\n");

    let mut synced = 0usize;
    for sid in session_ids {
        store.set_graph_context(sid, Some(&user_id_str), &summary).await?;
        synced += 1;
    }
    info!(edges_synced = edges.len(), sessions = synced,
          "improve stage 4: graph-to-session sync complete");
    Ok(edges.len())
}
```

### Step 8 — Wire through public API + re-exports (1h, depends on 1–7) — [x] done in commit `4da7623`

**File:** `crates/lib/src/api/mod.rs`, `crates/lib/src/lib.rs`

- Add `pub use remember::{RememberParams, RememberContext};` alongside the existing `pub use` line at `crates/lib/src/api/mod.rs:30`.
- Update `crates/lib/src/lib.rs` re-export at line 118 to include the new types.
- Update any existing callers of the old flat `remember()` signature.

### Step 9 — Unit + integration tests (5h, depends on all) — [x] done in commit `4da7623`

See test plan below.

---

## 4. Test plan

### Unit tests (in-module, `#[cfg(test)] mod tests` in `crates/lib/src/api/remember.rs`)

1. `remember_status_serde_roundtrip` — ensure `RememberStatus::Running` round-trips through JSON.
2. `remember_result_display_format` — snapshot the `Display` output for each mode.
3. `remember_result_is_success` — completed + session_stored → true; running + errored → false.
4. `remember_result_to_dict_omits_skip_fields` — cognify_result / memify_result should not appear in JSON.
5. `remember_params_default_self_improvement_true` — guard the default.

### Unit tests in `crates/lib/src/api/improve.rs`

6. `stage2_no_store_returns_zero` — unchanged from current stub test.
7. `stage4_no_store_returns_zero` — same.

### Integration tests (new file: `crates/lib/tests/remember_tests.rs`)

Use `MockStorage`, `MockGraphDB`, `MockVectorDB`, `MockEmbeddingEngine` from `cognee-test-utils` (same pattern as `crates/lib/tests/ingest_pipeline_tests.rs`). Tests:

8. `remember_permanent_blocking_populates_items_and_hash`
9. `remember_permanent_background_returns_running_then_completes`
10. `remember_session_stores_qa_entry`
11. `remember_session_self_improvement_spawns_improve`
12. `remember_session_improve_failure_is_non_fatal`
13. `stage2_persists_qa_text_to_graph`
14. `stage4_writes_graph_context_to_session_store`

---

## 5. Effort breakdown

| Step | Description | Hours |
|------|-------------|-------|
| 1 | `RememberResult` / `RememberStatus` / items extended | 2 |
| 2 | `RememberParams` + `RememberContext` bundles | 1.5 |
| 3 | Permanent-mode refactor + content_hash / token_count propagation | 2 |
| 4 | Background-task support (`tokio::spawn`, `JoinHandle`, `ApiError::Join`) | 3 |
| 5 | Session mode: background improve bridge | 2 |
| 6 | Improve Stage 2 real impl (cognify on Q&A text) | 6 |
| 7 | Improve Stage 4 real impl (graph → session context) | 5 |
| 8 | Wire through public API + re-exports | 1 |
| 9 | Unit + integration tests | 5 |
| **Total** | | **27.5** |

Falls cleanly inside the gap-doc's estimate of **23–34 hours (XL)**.

---

## 6. Out of scope

- **OpenTelemetry spans** — Python sets `new_span("cognee.api.remember")` with attributes like `COGNEE_DATASET_NAME` / `COGNEE_OPERATION_MODE`. Rust currently uses `tracing` spans only.
- **`send_telemetry()` / PostHog** — not part of the Rust SDK philosophy.
- **Remote client / cloud mode** — Python's `get_remote_client()` fallback is not mirrored.
- **Vector migrations (`_ensure_migrations_run`)** — Qdrant/Ladybug are embedded with fixed schemas.
- **`setup()` DB initialization call** — Rust DB init happens at `DatabaseConnection::connect()` time.
- **Checkpointed stage 4 sync** — this plan writes the full graph each time (bounded to 256 edges).
- **Full `RememberKwargs` parity via HashMap** — use typed `RememberParams` struct instead.
- **`remember()` CLI subcommand** — CLI already exposes `add-and-cognify`.

---

## Critical files for implementation

- `/home/dmytro/dev/cognee/cognee-rust/crates/lib/src/api/remember.rs`
- `/home/dmytro/dev/cognee/cognee-rust/crates/lib/src/api/improve.rs`
- `/home/dmytro/dev/cognee/cognee-rust/crates/lib/src/api/error.rs`
- `/home/dmytro/dev/cognee/cognee-rust/crates/lib/src/api/mod.rs`
- `/home/dmytro/dev/cognee/cognee-rust/crates/lib/tests/remember_tests.rs` (new)
