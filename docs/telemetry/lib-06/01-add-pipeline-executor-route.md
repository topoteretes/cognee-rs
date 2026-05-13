# LIB-06-01 — Route `AddPipeline::add` through `pipeline::execute`

**Status**: implemented in commit 82aac59 (fixed `extract_data_outputs` downcast to use `(*o).as_any()` to bypass blanket `Arc<dyn Value>` impl; added `sqlite` feature to cognee-database dev-dep; grew `ComponentHandles` with `vector_db` + `thread_pool` slots)
**Owner**: _unassigned_
**Depends on**: —
**Blocks**:
- [LIB-06-03 — Route cognify standard branch](03-cognify-standard-executor-route.md) — uses this task as a worked example of the executor route + downcast pattern.
- [LIB-06-05 — Cleanup TODOs](05-cleanup-todos.md) — removes the matching `TODO(LIB-06 follow-up)` markers at `crates/ingestion/src/pipeline.rs:771` and `:804` once all three convenience functions have landed.

**Parent doc**: [LIB-06 — Executor-Routed Convenience Pipelines](../lib-06-executor-routed-convenience.md)
**Locked decisions consulted**: 1 (signatures may grow), 7 (`AddParams` injected via task closure), 9 (per-pipeline downcast helper), 10 (existing fixtures updated, not bypassed), 11 (`NoopWatcher` only), 12 (no new traits), 14 (pipeline names byte-stable).

---

## 1. Problem statement

`AddPipeline::add` and `AddPipeline::add_with_params`
([`crates/ingestion/src/pipeline.rs:786`](../../crates/ingestion/src/pipeline.rs#L786)
and `:814` as of `205bc8a`) run two task functions in a per-input loop,
sequentially, without going through `cognee_core::pipeline::execute`. They
patch the intermediate `ProcessedInput` between the two stages to inject
`AddParams.node_set` (serialised JSON) and `AddParams.importance_weight`.

Routing through the executor requires:

1. The existing builder `build_add_pipeline_with_acl`
   ([line 691](../../crates/ingestion/src/pipeline.rs#L691)) does not
   accept `AddParams`. Extend it (or add an `add_params`-aware variant) so
   the persist task closure patches the `ProcessedInput` itself.
2. `AddPipeline` does not own a `thread_pool`, `graph_db`, or `vector_db`
   today (it doesn't need them — its tasks don't touch graph/vector). But
   `TaskContextBuilder::build()`
   ([`crates/core/src/task_context.rs:299`](../../crates/core/src/task_context.rs#L299))
   requires non-optional values for those four fields. The convenience
   signature grows accordingly (Decision 1).
3. The executor's output is `Vec<Arc<dyn Value>>`; `add` currently returns
   `Vec<Data>`. Add a private `extract_outputs(outputs) -> Result<Vec<Data>, _>`
   helper (Decision 9).
4. CLI / bindings / examples / tests must pass the new arguments
   (Decision 10).

## 2. Locked decisions consulted

- **Decision 1** — signatures may grow.
- **Decision 7** — `AddParams` injection lives in the persist task closure.
  No new `RunSpec` field; no new `TaskContext` extension.
- **Decision 9** — `extract_outputs` is a private helper in
  `crates/ingestion/src/pipeline.rs`. On downcast failure it returns
  `IngestionError::OutputTypeMismatch { expected: "Data", actual: "..." }`
  (the actual type-name string comes from the `Arc<dyn Value>` runtime
  type tag if accessible, else "unknown").
- **Decision 10** — every call site updates; mocks come from
  `cognee-test-utils` behind the `testing` feature.
- **Decision 11** — `NoopWatcher` only. Do not add a `pipeline_run_repo`
  parameter; that belongs to gap-08 task 07.
- **Decision 12** — Reuse existing traits.
- **Decision 14** — Pipeline name stays `"ingestion"` (as
  `build_add_pipeline_with_acl` sets it today — confirm via
  `rg "ingestion_pipeline\|\"ingestion\"" crates/ingestion/`).

## 3. Pre-conditions

- `cargo check --all-targets` passes on HEAD.
- `git status` shows only the docs changes from this design landing (or a
  clean tree). No other in-flight LIB-06 work.
- The `testing` feature on `cognee-test-utils` re-exports `MockGraphDB`
  and `MockVectorDB`. Confirm via
  `rg "MockGraphDB\|MockVectorDB" crates/test-utils/`.
- `cognee_database::connect("sqlite::memory:")` is available (used in
  existing ingestion tests; see
  [`crates/ingestion/src/pipeline.rs::tests::make_pipeline`](../../crates/ingestion/src/pipeline.rs#L874)).
- `TaskContextBuilder` accepts `Arc<DatabaseConnection>`, `Arc<dyn GraphDBTrait>`,
  `Arc<dyn VectorDB>`, `Arc<dyn CpuPool>`. Confirm by reading
  [`crates/core/src/task_context.rs:243-330`](../../crates/core/src/task_context.rs#L243-L330).

## 4. Step-by-step

### 4.1 Extend `make_persist_data_task_with_acl` to accept `AddParams`

Edit [`crates/ingestion/src/pipeline.rs`](../../crates/ingestion/src/pipeline.rs)
around lines 636-663. Add an `add_params: Option<AddParams>` parameter
(cheap to clone — `AddParams` is small) and patch `ProcessedInput` inside
the closure before delegating to `persist_data_with_acl`. Illustrative
(implementor must finalise the exact field names):

```rust
// as of 205bc8a; refresh line numbers in sub-agent A's audit
pub fn make_persist_data_task_with_acl(
    database: Arc<dyn IngestDb>,
    dataset_name: String,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    acl_db: Option<Arc<dyn AclDb>>,
    add_params: Option<AddParams>,                  // ← NEW
) -> TypedTask<ProcessedInput, Data> {
    let node_set_json = add_params
        .as_ref()
        .and_then(|p| p.node_set.as_ref())
        .map(serde_json::to_string)
        .transpose()
        .map_err(/* propagate via task error path */ todo!())?; // pre-serialise once

    TypedTask::async_fn(move |processed: &ProcessedInput, _ctx| {
        let mut processed = processed.clone();
        // Decision 7: inject add-params inside the closure.
        processed.node_set = node_set_json.clone();
        if let Some(p) = &add_params {
            processed.importance_weight = p.importance_weight;
        }
        let database = Arc::clone(&database);
        let dataset_name = dataset_name.clone();
        let acl_db = acl_db.clone();
        let override_dataset_id = add_params.as_ref().and_then(|p| p.dataset_id);
        Box::pin(async move {
            persist_data_with_acl(
                &processed,
                &*database,
                &dataset_name,
                owner_id,
                tenant_id,
                acl_db.as_deref(),
                override_dataset_id,
            )
            .await
            .map(Box::new)
            .map_err(|e| format!("{e}").into())
        })
    })
}
```

**Note on `?` inside a non-fallible function** — the illustrative snippet
above uses `?` for `transpose()` which is wrong (`make_persist_data_task_with_acl`
returns `TypedTask`, not `Result`). The implementor must decide between:

- Surface the serialisation failure earlier (move the `serde_json::to_string`
  call up into the convenience function, before this builder is called).
- Or accept `node_set: Option<String>` already-serialised as the param.

The convenience-function-side serialisation is **preferred** (Decision 7
keeps task-local state local to the task, but the cost of serialising once
upstream is small and the error surface is cleaner). Update
`AddParams` if needed.

Update `build_add_pipeline_with_acl`
([line 691](../../crates/ingestion/src/pipeline.rs#L691)) to thread the
`add_params` parameter through.

### 4.2 Add `MemifyTrigger`-style placeholder? — NO

Ingestion does **not** need a placeholder input. The pipeline already takes
one `DataInput` per pipeline input — the convenience function's loop
becomes the `inputs: Vec<Arc<dyn Value>>` argument to `execute()`.

### 4.3 Refactor `AddPipeline` to own backend handles

Grow the `AddPipeline` struct
([line 729](../../crates/ingestion/src/pipeline.rs#L729)) to own (or
borrow) what `TaskContextBuilder::build()` requires. Two options:

**Option A (preferred): `AddPipeline` borrows `Arc`s passed at `add()` time.**

```rust
pub struct AddPipeline { /* unchanged */ }

impl AddPipeline {
    #[allow(clippy::too_many_arguments)]
    pub async fn add(
        &self,
        inputs: Vec<DataInput>,
        dataset_name: &str,
        owner_id: Uuid,
        tenant_id: Option<Uuid>,
        thread_pool: Arc<dyn CpuPool>,        // ← NEW
        graph_db: Arc<dyn GraphDBTrait>,      // ← NEW (mock-friendly)
        vector_db: Arc<dyn VectorDB>,         // ← NEW (mock-friendly)
        database: Arc<DatabaseConnection>,    // ← NEW (replaces or sits beside Arc<dyn IngestDb>)
    ) -> Result<Vec<Data>, Box<dyn std::error::Error>> {
        self.add_with_params(
            inputs, dataset_name, owner_id, tenant_id,
            thread_pool, graph_db, vector_db, database,
            &AddParams::default(),
        )
        .await
    }
    // add_with_params follows the same shape.
}
```

**Option B: `AddPipeline` stores them as fields.**

The CLI / bindings build `AddPipeline` once and call `.add(...)` many times;
storing as fields avoids passing the same `Arc`s repeatedly. The trade-off
is that `AddPipeline::new` grows from 2 params to 6.

**Locked recommendation**: Option B (fields on the struct), with a
chainable `with_*` builder for the new backends — matches the existing
`with_acl_db` style at line 766. Concretely add:

```rust
impl AddPipeline {
    pub fn with_thread_pool(mut self, pool: Arc<dyn CpuPool>) -> Self { ... }
    pub fn with_graph_db(mut self, db: Arc<dyn GraphDBTrait>) -> Self { ... }
    pub fn with_vector_db(mut self, db: Arc<dyn VectorDB>) -> Self { ... }
    pub fn with_database(mut self, db: Arc<DatabaseConnection>) -> Self { ... }
}
```

If any of `thread_pool` / `graph_db` / `vector_db` / `database` is `None`
at `.add()` time, return `IngestionError::MissingBackend { which: "..." }`.

Choose **Option B** unless sub-agent A surfaces a reason to revisit.

### 4.4 Build `TaskContext` inside `add_with_params`

Replace the per-input loop with a single `execute()` call:

```rust
pub async fn add_with_params(
    &self,
    inputs: Vec<DataInput>,
    dataset_name: &str,
    owner_id: Uuid,
    tenant_id: Option<Uuid>,
    params: &AddParams,
) -> Result<Vec<Data>, Box<dyn std::error::Error>> {
    let thread_pool = self.thread_pool.clone().ok_or(IngestionError::MissingBackend { which: "thread_pool" })?;
    let graph_db = self.graph_db.clone().ok_or(IngestionError::MissingBackend { which: "graph_db" })?;
    let vector_db = self.vector_db.clone().ok_or(IngestionError::MissingBackend { which: "vector_db" })?;
    let database = self.database.clone().ok_or(IngestionError::MissingBackend { which: "database" })?;

    let pipeline = build_add_pipeline_with_acl(
        Arc::clone(&self.storage),
        Arc::clone(&self.database_trait),         // existing Arc<dyn IngestDb> stays as is
        self.hash_algorithm,
        dataset_name,
        owner_id,
        tenant_id,
        self.acl_db.clone(),
        Some(params.clone()),                      // ← NEW
    );

    let pipeline_ctx = PipelineContext {
        user_id: Some(owner_id),
        user_email: None,
        tenant_id,
        dataset_id: params.dataset_id,
        // ... other fields per PipelineContext definition; check task_context.rs
        ..Default::default()
    };

    let (_cancellation_handle, ctx) = TaskContextBuilder::new()
        .thread_pool(thread_pool)
        .database(database)
        .graph_db(graph_db)
        .vector_db(vector_db)
        .pipeline_context(pipeline_ctx)
        .build()
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
    let ctx = Arc::new(ctx);

    let typed_inputs: Vec<Arc<dyn Value>> = inputs
        .into_iter()
        .map(|i| Arc::new(i) as Arc<dyn Value>)
        .collect();

    let outputs = cognee_core::pipeline::execute(&pipeline, typed_inputs, ctx, &NoopWatcher)
        .await
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

    extract_outputs(outputs)
}
```

Illustrative — implementor must finalise:

- Whether `PipelineContext::default()` exists or needs explicit field
  enumeration.
- Whether `DataInput: Value` is already satisfied (it should be via
  `cognee_models`; if not, add a thin `impl Value for DataInput`).
- Where the `_cancellation_handle` is dropped (drop after `execute()`
  returns; no need to keep it alive for the convenience path).

### 4.5 `data_id_fn` for the add pipeline

Set `Pipeline.data_id_fn` to extract `Data.id` from the final `Data`
output (Decision 4). Since `data_id_fn` operates on the *input* of an
individual item per
[`crates/core/src/pipeline.rs:286`](../../crates/core/src/pipeline.rs#L286),
and the input to the add pipeline is `DataInput` (which has no UUID until
after `persist_data` runs), the per-input `data_id_fn` returns `None` for
the input. The watcher's `run_info["data"]` carrier remains
`vec![]` (Python's `"None"` branch). **This is acceptable for LIB-06 —
gap-08 task 07 will revisit `data_id_fn` once the watcher is real.**

### 4.6 `extract_outputs` helper

```rust
fn extract_outputs(outputs: Vec<Arc<dyn Value>>) -> Result<Vec<Data>, IngestionError> {
    outputs
        .into_iter()
        .map(|o| {
            o.downcast_ref::<Data>()
                .cloned()
                .ok_or(IngestionError::OutputTypeMismatch {
                    expected: "Data",
                    // Use std::any::type_name on the trait object if accessible,
                    // else fall back to "unknown" — the cognee-core `Value` trait
                    // does not expose a runtime type name today, so "unknown" is
                    // the honest answer. Sub-agent A: confirm what's available.
                    actual: "unknown",
                })
        })
        .collect()
}
```

If `Value` does not expose a downcast (`Arc<dyn Value>` → concrete), check
whether cognee-core's `Value` is `Any` + downcastable. If not, the typed
pipeline output mechanism already handles this via `TypedTask`'s output
type — investigate `cognee_core::pipeline::execute_one_item` to see how
typed outputs flow back. Sub-agent A must verify the downcast path before
sub-agent B writes the helper.

### 4.7 Update CLI

Edit:
- [`crates/cli/src/commands/add.rs`](../../crates/cli/src/commands/add.rs)
- [`crates/cli/src/commands/add_and_cognify.rs`](../../crates/cli/src/commands/add_and_cognify.rs)
- [`crates/cli/src/commands/run_sequence.rs`](../../crates/cli/src/commands/run_sequence.rs)

Each builds `AddPipeline`, then calls `.add(...)`. After this task,
`AddPipeline::new(...)` is unchanged but the builder receives the new
`.with_thread_pool(...)` / `.with_graph_db(...)` / `.with_vector_db(...)`
/ `.with_database(...)` calls (the CLI already constructs all four).

### 4.8 Update bindings + examples + tests

```bash
rg "AddPipeline::new" crates/ examples/ capi/ js/ python/
rg "\.add_with_params\(" crates/
```

Every direct construction site receives the new builder calls. The
`AddPipeline::new` signature stays unchanged so non-test callers that
don't need the executor route keep working — but `.add()` errors out at
runtime with `IngestionError::MissingBackend` if a backend is missing.
**Document this in the `AddPipeline::new` doc-comment** so callers know to
use the builder.

**Audit (as of `70f2ecc`, this audit landed on 2026-05-13)** — direct
`AddPipeline::new` construction sites:

- [`crates/ingestion/src/pipeline.rs::tests::make_pipeline`](../../crates/ingestion/src/pipeline.rs#L874)
  (unit tests).
- [`examples/add_example.rs`](../../examples/add_example.rs),
  [`examples/cognify_example.rs`](../../examples/cognify_example.rs).
- [`crates/lib/tests/improve_e2e.rs`](../../crates/lib/tests/improve_e2e.rs),
  [`crates/lib/tests/improve_sync_only.rs`](../../crates/lib/tests/improve_sync_only.rs),
  [`crates/lib/tests/remember_sync_only.rs`](../../crates/lib/tests/remember_sync_only.rs),
  [`crates/lib/tests/remember_tests.rs`](../../crates/lib/tests/remember_tests.rs).

`AddPipeline::add` / `add_with_params` call sites (struct receivers):

- [`crates/cli/src/commands/add.rs`](../../crates/cli/src/commands/add.rs#L51).
- [`crates/cli/src/commands/add_and_cognify.rs`](../../crates/cli/src/commands/add_and_cognify.rs#L65).
- [`crates/http-server/src/routers/add.rs`](../../crates/http-server/src/routers/add.rs#L235).
- [`crates/http-server/src/routers/remember.rs`](../../crates/http-server/src/routers/remember.rs#L248).
- [`crates/cognify/src/memify/persist_sessions.rs`](../../crates/cognify/src/memify/persist_sessions.rs#L140).
- Cognify integration tests under `crates/cognify/tests/` (~10 files —
  `e2e_triplet_vector_cleanup`, `e2e_lifecycle_loop`,
  `integration_default_backend`, `e2e_recognify_after_update`,
  `provenance_e2e`, `e2e_shared_entity_graph_delete`,
  `e2e_delete_preview_accuracy`, `e2e_full_pipeline_memify`, etc.).

Functions that pass `&AddPipeline` through (no construction; signature is
unchanged but the embedded `.add(...)` call now needs backends populated
on the underlying struct):

- [`crates/lib/src/api/remember.rs`](../../crates/lib/src/api/remember.rs)
  (4 callsites at lines 205, 318, 427, 493).
- [`crates/lib/src/api/update.rs`](../../crates/lib/src/api/update.rs#L58).
- [`crates/lib/src/api/improve.rs`](../../crates/lib/src/api/improve.rs#L111).

Bindings (`capi/`, `js/`, `python/`) do **not** construct `AddPipeline`
directly — they go through `cognee_lib::api` higher-level entry points
(`remember`, `update`, `improve`). The blast radius for bindings is at
the lib API level: whichever helper builds the `Arc<AddPipeline>` is the
one that must call the new `.with_thread_pool(...)` / etc. builders.
Confirm by reading the actual `Arc::new(AddPipeline::new(...))` site in
each binding harness during implementation.

Each test fixture and lib test must add the `.with_thread_pool` /
`.with_graph_db` / `.with_vector_db` / `.with_database` calls; use mocks
from `cognee-test-utils` (gate behind `#[cfg(feature = "testing")]` if
not already).

### 4.9 Leave the `TODO(LIB-06 follow-up)` comments in place

Decision 13 keeps them until [LIB-06-05](05-cleanup-todos.md) removes them.

## 5. Files modified

- [`crates/ingestion/src/pipeline.rs`](../../crates/ingestion/src/pipeline.rs):
  - Extend `make_persist_data_task_with_acl` with `add_params: Option<AddParams>`.
  - Extend `build_add_pipeline_with_acl` to thread params through.
  - Grow `AddPipeline` struct with optional `thread_pool` / `graph_db` /
    `vector_db` / `database` fields and chainable builders.
  - Replace `add_with_params` body with `execute()`-based flow.
  - Add `extract_outputs` private helper.
  - Add `IngestionError::MissingBackend { which: &'static str }` and
    `IngestionError::OutputTypeMismatch { expected, actual }` variants.
- [`crates/ingestion/src/error.rs`](../../crates/ingestion/src/error.rs)
  (if it exists; else inline in `pipeline.rs`) — new error variants.
- [`crates/cli/src/commands/add.rs`](../../crates/cli/src/commands/add.rs),
  [`add_and_cognify.rs`](../../crates/cli/src/commands/add_and_cognify.rs) —
  wire backends into `AddPipeline` via builders. (`run_sequence.rs` does
  not call `AddPipeline` directly today — verify before editing.)
- [`crates/http-server/src/routers/add.rs`](../../crates/http-server/src/routers/add.rs),
  [`crates/http-server/src/routers/remember.rs`](../../crates/http-server/src/routers/remember.rs) —
  callers of `.add_with_params(...)`. The pipeline they call is built
  upstream (likely in `ComponentManager` / HTTP startup); update the
  build site to attach backends.
- [`crates/cognify/src/memify/persist_sessions.rs`](../../crates/cognify/src/memify/persist_sessions.rs) —
  calls `.add_with_params(...)` on an inherited `AddPipeline`. Sub-agent
  B confirms the upstream construction site is backend-equipped.
- [`crates/lib/src/api/remember.rs`](../../crates/lib/src/api/remember.rs),
  [`update.rs`](../../crates/lib/src/api/update.rs),
  [`improve.rs`](../../crates/lib/src/api/improve.rs) — accept
  `&AddPipeline` / `Arc<AddPipeline>`; no signature change but the
  callers that construct the pipeline must attach backends.
- Bindings (`capi/`, `js/`, `python/`) — do NOT construct `AddPipeline`
  directly. They invoke `cognee_lib::api::{remember,update,improve}`.
  The change ripples through whichever binding-side factory builds the
  underlying `AddPipeline` instance — investigate during implementation.
- [`examples/add_example.rs`](../../examples/add_example.rs),
  [`examples/cognify_example.rs`](../../examples/cognify_example.rs) —
  pass backends.
- [`crates/ingestion/src/pipeline.rs::tests`](../../crates/ingestion/src/pipeline.rs#L866)
  — same.
- [`crates/lib/tests/improve_e2e.rs`](../../crates/lib/tests/improve_e2e.rs),
  [`improve_sync_only.rs`](../../crates/lib/tests/improve_sync_only.rs),
  [`remember_sync_only.rs`](../../crates/lib/tests/remember_sync_only.rs),
  [`remember_tests.rs`](../../crates/lib/tests/remember_tests.rs) — same.
- `crates/cognify/tests/*.rs` (10+ integration tests calling `.add(...)`)
  — same.

## 6. Verification

```bash
# 1. Workspace compiles.
cargo check --all-targets

# 2. Ingestion unit tests pass.
cargo test -p cognee-ingestion

# 3. Existing CLI E2E tests pass.
cargo test -p cognee-cli --test cli_e2e

# 4. Binding smoke tests.
bash capi/scripts/check.sh
bash python/scripts/check.sh
bash js/scripts/check.sh

# 5. Full check suite.
scripts/check_all.sh
```

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `Value` trait doesn't expose a downcast path; `extract_outputs` is impossible as written | Medium | Sub-agent A must verify the downcast mechanism in `cognee_core::pipeline::execute_one_item` before sub-agent B writes the helper. If unsupported, extend `Value` with a downcast helper (small change, scoped to cognee-core). |
| `MissingBackend` runtime error breaks existing callers that previously worked | High — desired | Document loudly in the `AddPipeline::new` doc-comment + commit body. Most callers (CLI, HTTP server) already own the backends. |
| `TaskContextBuilder::build` `pipeline_context` field defaults need new fields | Medium | Sub-agent A audits `PipelineContext` field list and the sub-doc records the exact init pattern. |
| `DataInput` may not implement `Value` | Medium | If not, add `impl Value for DataInput` in cognee-models — small, mechanical. |
| Bindings (PyO3 / Neon / capi) lack a graph DB or vector DB by default — embedded users may not have them set up | Medium | Bindings construct sane defaults (Ladybug-embedded graph + Qdrant-embedded vector) at startup; if not, fall back to mocks under a feature flag. Sub-agent A audits the binding initialisation paths. |
| `extract_outputs` is silently wrong for multi-input pipelines (the loop emits one output per input, so `Vec<Data>` should have the same length as `inputs`) | Low | Add an inline assertion in the helper: `if outputs.len() != expected { Err(OutputTypeMismatch ...) }`. |

## 8. Out of scope

- Wiring `DbPipelineWatcher` — gap 08-07.
- Switching from `Arc<dyn IngestDb>` to `Arc<DatabaseConnection>` everywhere
  in ingestion. Keep both fields side-by-side for now: the trait abstraction
  is still useful for mock-driven tests, while the concrete connection is
  what `TaskContextBuilder` requires.
- Renaming or restructuring `AddParams`. Out of scope.
- Adding a `Pipeline::with_data_id_fn` builder that accepts a post-hoc
  extractor — Decision 4 keeps `data_id_fn` as the input extractor, and
  the watcher's `data_ids` carrier stays empty for ingestion. Revisit
  when gap-08 task 07 lands.
- Making `MockGraphDB` / `MockVectorDB` unconditionally available (the
  `testing` feature gate stays).
