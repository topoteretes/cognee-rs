# LIB-06-02 — Route `memify::memify` through `pipeline::execute`

**Status**: implemented in commit 64b6182 (memify and run_memify grew thread_pool + database params per decision 1; HTTP route untouched since it's a P5 TODO stub; lib API improve/remember skip-with-warn when handles missing)
**Owner**: _unassigned_
**Depends on**: — (independent of LIB-06-01 in code; runbook sequences after 01 for narrative continuity)
**Blocks**:
- [LIB-06-05 — Cleanup TODOs](05-cleanup-todos.md) — removes the matching `TODO(LIB-06 follow-up)` at `crates/cognify/src/memify/pipeline.rs:48`.

**Parent doc**: [LIB-06 — Executor-Routed Convenience Pipelines](../lib-06-executor-routed-convenience.md)
**Locked decisions consulted**: 1 (signatures may grow), 8 (`Vec<Triplet>` passed directly — no placeholder ZST; locked 2026-05-13), 9 (per-pipeline downcast helper), 10 (existing fixtures updated), 11 (`NoopWatcher` only), 12 (no new traits), 14 (pipeline name byte-stable).

---

## 1. Problem statement

`cognify::memify::memify` at
[`crates/cognify/src/memify/pipeline.rs:57`](../../crates/cognify/src/memify/pipeline.rs#L57)
(as of `205bc8a`) is a two-stage flow (`extract_triplets` →
`index_triplets`) implemented as direct function calls. Divergences from
the executor route:

1. **No `Pipeline` builder exists today.** Must add
   `build_memify_index_only_pipeline`.
2. **Custom-data branch** (lines 70-104) — when `config.custom_data` is
   set, synthesises triplets from JSON instead of calling
   `extract_triplets_from_graph_db`. Handled as a pre-flight step.
3. **Empty-triplets short-circuit** (lines 110-119) — returns zero
   `MemifyResult` without calling `index_triplets`. Handled as a
   pre-flight check before `execute()`.
4. **No `Data` inputs.** Memify operates on the existing graph. The
   executor requires at least one input — per Decision 8 (locked
   2026-05-13) the convenience function passes the pre-extracted
   `Vec<Triplet>` directly. No placeholder ZST.
5. **`MemifyResult` shape** wraps a flat `IndexResult` — needs downcast
   helper (Decision 9).

## 2. Locked decisions consulted

- **Decision 1** — `memify::memify` already accepts `graph_db`, `vector_db`,
  `embedding_engine` as `&dyn` borrows. The signature gains `thread_pool:
  Arc<dyn CpuPool>` and `database: Arc<DatabaseConnection>` to satisfy
  `TaskContextBuilder::build()`. Borrows become `Arc`s since the executor
  spawns work into a task context.
- **Decision 8** (locked 2026-05-13) — No placeholder ZST. The
  convenience function runs triplet extraction (graph-DB query or
  custom-data synthesis) up-front and passes the resulting
  `Vec<Triplet>` directly as `Arc::new(triplets) as Arc<dyn Value>`.
  The pipeline is a one-task "index-only" shape whose first (and only)
  task consumes `Vec<Triplet>`. `Vec<Triplet>` (or a thin wrapper) must
  implement the executor's `Value` trait — see §4.2.
- **Decision 9** — Add a private `extract_memify_outputs(outputs) ->
  Result<MemifyResult, MemifyError>` helper at the bottom of
  `memify/pipeline.rs`.
- **Decision 10** — Existing tests in
  `crates/cognify/src/memify/` and `crates/cognify/tests/` update to pass
  the new arguments (use `MockGraphDB`, `MockVectorDB`,
  `MockEmbeddingEngine`).
- **Decision 11** — `NoopWatcher` only. No `pipeline_run_repo` parameter.
- **Decision 14** — Pipeline name: `"memify"` (matches the new
  `pub fn build_memify_index_only_pipeline`). Sub-agent A confirms
  (`rg "memify_pipeline\|\"memify\"" crates/cognify/src/memify/`,
  against `362cc9b`) that memify does **not** stamp provenance manually
  anywhere — no `stamp_provenance` calls, no `"memify_pipeline"` string
  literal exists. The builder name `"memify"` is therefore trivially
  byte-stable; no inline literal needs rewriting.

## 3. Pre-conditions

- `cargo check --all-targets` passes on HEAD.
- `MockGraphDB`, `MockVectorDB`, `MockEmbeddingEngine` available via
  `cognee-test-utils` (or `cognee-graph` / `cognee-vector` /
  `cognee-embedding` with `testing` feature).
- Pre-flight check: `rg "stamp_provenance" crates/cognify/src/memify/` —
  if memify stamps provenance manually, the post-refactor pipeline must
  rely on `stamp_tree_dyn` and the strings must align. As of `205bc8a`
  memify does **not** appear to stamp provenance manually (sub-agent A
  must confirm).

## 4. Step-by-step

### 4.1 Ensure `Vec<Triplet>` is `Value`-compatible

Per locked Decision 8 (2026-05-13) there is **no placeholder ZST**. The
executor input is the pre-extracted `Vec<Triplet>` itself.

**Confirmed (sub-agent A, against `362cc9b`):** `Value` is defined in
[`crates/core/src/task.rs:16`](../../crates/core/src/task.rs#L16) with a
blanket auto-impl `impl<T: Any + Send + Sync + 'static> Value for T`.
`Triplet` is a plain `Send + Sync + 'static` struct in `cognee_models`,
so `Vec<Triplet>` already implements `Value`. **No new impl needs to
land** — sub-agent B passes `Arc::new(triplets) as Arc<dyn Value>`
directly. Do **not** reintroduce a `MemifyTrigger` placeholder.

### 4.2 Build the typed-task closure (index-only)

Per locked Decision 8 (2026-05-13), the pipeline is a single-task
"index-only" shape. Triplet extraction (graph-DB query or custom-data
synthesis) happens outside the pipeline as a pre-flight step (§4.3).
Add one helper in `crates/cognify/src/memify/pipeline.rs`:

```rust
fn make_index_triplets_task(
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    dataset_id: Option<Uuid>,
    user_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
) -> TypedTask<Vec<Triplet>, IndexResult> {
    TypedTask::async_fn(move |triplets: &Vec<Triplet>, _ctx| {
        let triplets = triplets.clone();
        let vector_db = Arc::clone(&vector_db);
        let embedding_engine = Arc::clone(&embedding_engine);
        Box::pin(async move {
            // index_triplets currently takes `&dyn ...`; adapt to Arc here.
            index_triplets(
                &triplets, &*vector_db, &*embedding_engine,
                dataset_id, user_id, tenant_id,
            )
            .await
            .map(Box::new)
            .map_err(|e| format!("{e}").into())
        })
    })
}
```

### 4.3 Add `build_memify_index_only_pipeline` and route the convenience function

Per locked Decision 8 (2026-05-13), the design collapses to **one**
pipeline shape ("index-only"). Triplet extraction and the empty-triplets
short-circuit are pre-flight steps inside the convenience function:

```rust
pub fn build_memify_index_only_pipeline(
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    dataset_id: Option<Uuid>,
    user_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
) -> Pipeline {
    PipelineBuilder::new_with_task(
        "memify",
        make_index_triplets_task(
            vector_db,
            embedding_engine,
            dataset_id,
            user_id,
            tenant_id,
        ),
    )
    .with_name("memify")
    .build()
}
```

The convenience function:

1. Runs `extract_triplets_from_graph_db` (or builds custom triplets) up
   front.
2. If empty, returns the zero `MemifyResult` immediately, skipping the
   pipeline entirely.
3. Otherwise builds the one-task index-only pipeline and calls
   `execute()` with the pre-extracted triplets as the single input
   (`Arc::new(triplets) as Arc<dyn Value>`).

```rust
pub async fn memify(
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    thread_pool: Arc<dyn CpuPool>,
    database: Arc<DatabaseConnection>,
    dataset_id: Option<Uuid>,
    user_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
    config: &MemifyConfig,
) -> Result<MemifyResult, MemifyError> {
    config.validate()?;

    // 1. Extract or synthesise triplets (pre-flight).
    let triplets = if let Some(custom) = &config.custom_data {
        build_triplets_from_custom_data(custom)
    } else {
        extract_triplets_from_graph_db(&*graph_db, config).await?
    };

    // 2. Empty short-circuit.
    if triplets.is_empty() {
        return Ok(MemifyResult { triplet_count: 0, index_result: IndexResult::empty() });
    }

    // 3. Build and run the one-task pipeline.
    let pipeline = build_memify_index_only_pipeline(
        Arc::clone(&vector_db),
        Arc::clone(&embedding_engine),
        dataset_id,
        user_id,
        tenant_id,
    );

    let pipeline_ctx = PipelineContext {
        user_id, tenant_id, dataset_id,
        ..Default::default()
    };
    let (_handle, ctx) = TaskContextBuilder::new()
        .thread_pool(thread_pool)
        .database(database)
        .graph_db(graph_db)
        .vector_db(vector_db)
        .pipeline_context(pipeline_ctx)
        .build()
        .map_err(|e| MemifyError::Context(e.to_string()))?;
    let ctx = Arc::new(ctx);

    let inputs = vec![Arc::new(triplets.clone()) as Arc<dyn Value>];
    let outputs = cognee_core::pipeline::execute(&pipeline, inputs, ctx, &NoopWatcher)
        .await
        .map_err(|e| MemifyError::Execute(e.to_string()))?;

    let index_result = extract_memify_outputs(outputs)?;

    Ok(MemifyResult {
        triplet_count: triplets.len(),
        index_result,
    })
}
```

**This collapses 4 design problems into one cleaner shape.** Per locked
Decision 8 (2026-05-13) there is **no** `MemifyTrigger` placeholder —
`Vec<Triplet>` is the executor input directly. The pipeline's only task
takes `Vec<Triplet>` and produces `IndexResult`.

### 4.4 `extract_memify_outputs`

```rust
fn extract_memify_outputs(outputs: Vec<Arc<dyn Value>>) -> Result<IndexResult, MemifyError> {
    let first = outputs.into_iter().next()
        .ok_or(MemifyError::OutputTypeMismatch { expected: "IndexResult", actual: "empty" })?;
    first
        .downcast_ref::<IndexResult>()
        .cloned()
        .ok_or(MemifyError::OutputTypeMismatch { expected: "IndexResult", actual: "unknown" })
}
```

Same caveat as LIB-06-01 §4.6 — confirm `Value` is downcastable.

### 4.5 `MemifyError` new variants

Add to [`crates/cognify/src/memify/error.rs`](../../crates/cognify/src/memify/error.rs)
(or wherever `MemifyError` is defined):

```rust
#[error("task context build failed: {0}")]
Context(String),

#[error("pipeline execution failed: {0}")]
Execute(String),

#[error("output type mismatch: expected {expected}, got {actual}")]
OutputTypeMismatch { expected: &'static str, actual: &'static str },
```

### 4.6 Update CLI

[`crates/cli/src/commands/memify.rs`](../../crates/cli/src/commands/memify.rs)
constructs `Arc`s for all backends + a `RayonThreadPool` and passes them
to `memify(...)`. The CLI already has all five (graph, vector, embedding,
database, owner/tenant); the `thread_pool` parameter needs construction
from `RayonThreadPool::with_default_threads()?`.

### 4.7 Update bindings + examples + tests

```bash
rg "run_memify\(|memify::memify\(|cognify::memify\b" capi/ js/ python/ examples/ crates/
```

Confirmed call sites (sub-agent A, against `362cc9b`):

- `crates/cli/src/commands/memify.rs:84` — gains `thread_pool` +
  `database` parameters.
- `crates/lib/src/api/improve.rs:281` — gains the new parameters; the
  caller already owns `Arc<DatabaseConnection>` (`improve.rs` references
  `cognee_database::DatabaseConnection`).
- `crates/lib/src/api/remember.rs:388` — same.
- `crates/http-server/src/routers/memify.rs:22` — the HTTP route handler
  calls `run_memify`; gains the same arguments (server already owns
  `Arc<DatabaseConnection>` + a `CpuPool` via app state).
- Tests: `crates/cognify/tests/integration_memify.rs`,
  `crates/cognify/tests/e2e_memify.rs`,
  `crates/cognify/tests/e2e_full_pipeline_memify.rs`,
  `crates/cognify/tests/e2e_delete_preview_accuracy.rs`,
  `crates/cognify/tests/e2e_triplet_vector_cleanup.rs`,
  `crates/lib/tests/improve_e2e.rs`,
  `crates/lib/tests/improve_sync_only.rs`. Each test constructs an
  in-memory SQLite via `cognee_database::connect("sqlite::memory:")`
  and a `RayonThreadPool::with_default_threads()?`.
- No `examples/memify_*.rs` exists today; nothing to update under
  `examples/`.
- No standalone `capi/`, `js/`, `python/` memify entry points exist (the
  bindings expose memify only transitively through `improve`/`remember`,
  which already get updated via the `crates/lib/src/api/` changes
  above). Confirm with the rg above before committing.

Bindings use their own DB connection (already owned for the existing
relational work).

### 4.8 Leave the `TODO(LIB-06 follow-up)` comment

Decision 13 keeps it until LIB-06-05.

## 5. Files modified

- [`crates/cognify/src/memify/pipeline.rs`](../../crates/cognify/src/memify/pipeline.rs):
  - Add `make_index_triplets_task` and
    `build_memify_index_only_pipeline` (per locked Decision 8 — no
    placeholder ZST, no `make_extract_triplets_task`; triplet extraction
    is pre-flight).
  - Rewrite `memify` body to call `pipeline::execute` with
    `Vec<Triplet>` as the executor input.
  - Add `extract_memify_outputs` helper.
  - **No `Value` impl needed for `Vec<Triplet>`** — the executor's
    `Value` trait has a blanket auto-impl
    (`crates/core/src/task.rs:16`); `Vec<Triplet>` qualifies
    automatically. Confirmed by sub-agent A against `362cc9b`.
- [`crates/cognify/src/memify/error.rs`](../../crates/cognify/src/memify/error.rs)
  (or inline) — new error variants.
- [`crates/cognify/src/memify/mod.rs`](../../crates/cognify/src/memify/mod.rs) —
  re-export `build_memify_index_only_pipeline` if it should be public.
- [`crates/cognify/src/lib.rs`](../../crates/cognify/src/lib.rs) — re-export.
- [`crates/cli/src/commands/memify.rs`](../../crates/cli/src/commands/memify.rs) — pass new arguments.
- `capi/src/`, `js/src/`, `python/src/` memify entry points.
- `examples/memify_*.rs` (if any).
- `crates/cognify/src/memify/tests` and `crates/cognify/tests/*memify*`.

## 6. Verification

```bash
# 1. Workspace compiles.
cargo check --all-targets

# 2. Memify unit + integration tests.
cargo test -p cognee-cognify memify

# 3. Memify CLI E2E.
cargo test -p cognee-cli --test cli_e2e -- memify

# 4. Bindings smoke.
bash python/scripts/check.sh
bash js/scripts/check.sh
bash capi/scripts/check.sh

# 5. Full check.
scripts/check_all.sh

# 6. Cross-SDK smoke (the parity test does NOT cover memify today, but
#    confirm nothing else breaks).
cd e2e-cross-sdk && docker compose up --build --abort-on-container-exit && cd -
```

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `Vec<Triplet>` may not implement `Value` | **None — confirmed by sub-agent A against `362cc9b`** | `Value` has a blanket auto-impl for any `T: Any + Send + Sync + 'static` (`crates/core/src/task.rs:16`); `Vec<Triplet>` satisfies it automatically. No new impl required. |
| Custom-data branch and graph-extraction branch use different *first* tasks; conditional `PipelineBuilder` complicates the design | Low — collapsed by Decision 8 | Pre-flight check produces `Vec<Triplet>`; one-task index-only pipeline indexes them. Both branches converge. |
| `index_triplets` takes `&dyn` today; switching to `Arc<dyn>` for the executor closure is a noisy refactor | Low | `&*arc` deref is acceptable; no signature change to `index_triplets` itself. |
| Memify lacks any provenance stamping today, so equivalence is trivial — but check via `rg "stamp_provenance" crates/cognify/src/memify/` | Low | Sub-agent A confirms. If stamping is added by `stamp_tree_dyn` only on the new path, that's a behaviour change — flag in commit body. |
| `MockGraphDB` / `MockVectorDB` / `MockEmbeddingEngine` may not implement all the trait methods used by `extract_triplets_from_graph_db` and `index_triplets` | Medium | If mock gaps exist, extend the mocks in `cognee-test-utils`; small mechanical addition. |

## 8. Out of scope

- Wiring `DbPipelineWatcher` — gap 08-07.
- Refactoring `MemifyConfig.custom_data` to be a typed `Vec<Triplet>` —
  out of scope; keep the JSON shape.
- Adding `data_id_fn` to the memify pipeline. Memify has no `Data`
  inputs — leave `data_id_fn = None`. The watcher's `data_ids` carrier
  stays `vec![]` (Python's `"None"` branch).
- Splitting `index_triplets` into a batched task that takes one
  `Triplet` per pipeline input. Out of scope; preserve the existing
  bulk-index shape.
- Reintroducing a placeholder ZST input (the earlier `MemifyTrigger`
  draft) — Decision 8 locks this design out.
