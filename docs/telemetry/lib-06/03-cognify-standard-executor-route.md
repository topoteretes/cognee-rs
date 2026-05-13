# LIB-06-03 — Route `cognify::cognify` standard branch through `pipeline::execute`

**Status**: not yet implemented (⬜)
**Owner**: _unassigned_
**Depends on**: LIB-06-01 (working executor-route + downcast example).
**Blocks**:
- [LIB-06-04 — Cognify temporal branch](04-cognify-temporal-executor-route.md) — reuses the cognify entry-point structure this task lands.
- [LIB-06-05 — Cleanup TODOs](05-cleanup-todos.md) — removes the `TODO(LIB-06 follow-up)` at `crates/cognify/src/tasks.rs:1762`.

**Parent doc**: [LIB-06 — Executor-Routed Convenience Pipelines](../lib-06-executor-routed-convenience.md)
**Locked decisions consulted**: 1 (signatures may grow), 3 (provenance equivalence verified per task), 5 (`extract_dlt_fk_edges` stays as post-pipeline teardown), 6 (auto-chunk-size mutation happens before `execute()`), 9 (per-pipeline downcast helper), 10 (existing fixtures updated), 11 (`NoopWatcher` only), 12 (no new traits), 14 (pipeline names byte-stable).

---

## 1. Problem statement

`cognify::cognify` at
[`crates/cognify/src/tasks.rs:1773`](../../crates/cognify/src/tasks.rs#L1773)
runs the five-stage cognify pipeline inline. The function has six concrete
divergences from the executor route (see parent doc "Current Rust state"):

1. Auto-chunk-size config mutation (lines 1794-1806).
2. Per-task `stamp_provenance` calls (lines 1832-1839, 1858-1865,
   1905-1918, 1924-1931).
3. Empty-document and empty-chunk short-circuits (lines 1841-1843,
   1867-1869).
4. Temporal branch — out of scope here (LIB-06-04 handles it).
5. Post-pipeline `extract_dlt_fk_edges` (line 1945).
6. `user_str_ref` derivation (lines 1816-1819).

This task routes the **non-temporal** branch through the existing
`build_cognify_pipeline`
([line 2826](../../crates/cognify/src/tasks.rs#L2826)) +
`pipeline::execute`. The temporal branch detection stays in the convenience
function but defers to LIB-06-04 for the executor route.

## 2. Locked decisions consulted

- **Decision 3 (CRITICAL)** — Provenance equivalence is the load-bearing
  claim. Verified per task by:
  1. Running `bash scripts/run_tests_with_openai.sh test_fact_extraction`
     (or the equivalent cognify suite) against a known fixture, capturing
     the output graph + vector DB state.
  2. Running it again after the refactor and comparing: identical
     `source_pipeline` / `source_task` / `source_user` /
     `source_node_set` / `source_content_hash` per DataPoint, identical
     node/edge counts, identical vector collection contents.
  3. `cd e2e-cross-sdk && docker compose up --build --abort-on-container-exit`
     must pass with the existing 50% / 0.3-Jaccard tolerances.
- **Decision 5** — `extract_dlt_fk_edges` stays as post-pipeline
  teardown:

  ```rust
  let outputs = pipeline::execute(&pipeline, inputs, ctx, &NoopWatcher).await?;
  let result = extract_cognify_outputs(outputs)?;
  // ── post-pipeline teardown ──
  extract_dlt_fk_edges(&result.chunks_for_dlt, &result.documents_for_dlt, graph_db).await?;
  Ok(result)
  ```

  This requires the cognify pipeline output to *include* the chunks and
  documents lists the DLT helper needs. Today they live on `summarized`
  (`SummarizedText`) inside the convenience function. The final task
  (`make_add_data_points_task`) currently returns `CognifyResult`; we
  must either:
    - (a) Extend `CognifyResult` with `chunks_for_dlt: Vec<DocumentChunk>`
      and `documents_for_dlt: Vec<Document>` fields, or
    - (b) Run `extract_dlt_fk_edges` inside a no-op typed task that sits
      *before* `make_add_data_points_task` and consumes the
      `SummarizedText` directly.

  **Locked choice**: (a). Reasons: (i) keeps the teardown semantics
  identical to today, (ii) avoids re-architecting `make_add_data_points_task`,
  (iii) the new fields are populated from the existing `summarized` value
  that already flows into that task — zero new computation. Sub-agent A:
  verify the field-add doesn't break any binding that serialises
  `CognifyResult`.
- **Decision 6** — Auto-chunk-size: clone the config, call
  `with_auto_chunk_size(...)` if default, pass the cloned config to
  `build_cognify_pipeline`. The pipeline builder consumes the final
  config.
- **Decision 9** — `extract_cognify_outputs(outputs) ->
  Result<CognifyResult, CognifyError>` helper.
- **Decision 14** (locked 2026-05-13) — Pipeline name is **`"cognify"`**,
  matching `build_cognify_pipeline`'s `with_name("cognify")`. The legacy
  inline `stamp_provenance(..., "cognify_pipeline", ...)` calls in the
  convenience function are **updated to use `"cognify"`** as part of this
  refactor. After the refactor every `source_pipeline` value flowing
  through `stamp_tree_dyn` reads `"cognify"`. This is a one-time
  one-character-shift of `source_pipeline` from `"cognify_pipeline"` to
  `"cognify"`; afterwards the value is stable. Sub-agent A verifies:
  1. The executor's `stamp_tree_dyn` stamps `pipeline.name` (i.e.
     `"cognify"`) as `source_pipeline`.
  2. No inline `stamp_provenance(..., "cognify_pipeline", ...)` call
     survives the refactor in `tasks.rs`.
  3. Cross-SDK structural test (Decision 15) still passes within its
     50% / 0.3-Jaccard tolerances — `source_pipeline` is not part of the
     structural comparison metric.

## 3. Pre-conditions

- LIB-06-01 committed (working executor-route example for ingestion).
- LIB-06-02 committed (working downcast helper example for memify).
- `cargo check --all-targets` passes on HEAD.
- Cognify E2E baseline captured: run
  `bash scripts/run_tests_with_openai.sh test_fact_extraction` once, save
  the resulting graph DB + vector DB state (or assert specific node/edge
  counts in a baseline file) so the post-refactor comparison is
  meaningful.
- Cross-SDK harness baseline: `cd e2e-cross-sdk && docker compose up
  --build --abort-on-container-exit` passes on HEAD.

## 4. Step-by-step

### 4.1 Audit pipeline-name strings (Decision 14)

```bash
rg "stamp_provenance\(.*\"cognify" crates/cognify/
rg "\"cognify_pipeline\"\|\"cognify\"" crates/cognify/src/tasks.rs
```

The audit surfaces the divergence between the builder's
`with_name("cognify")` and the inline `stamp_provenance(...,
"cognify_pipeline", ...)` calls. **Resolution (locked 2026-05-13):**
rewrite every inline `"cognify_pipeline"` literal in the convenience
function (and in any helper this gap removes) to `"cognify"`. After the
rewrite, the only `source_pipeline` value stamped by the executor is
`"cognify"`. Sub-agent B applies the rewrite; sub-agent A confirms no
`"cognify_pipeline"` literal survives in `crates/cognify/src/tasks.rs`
after the refactor (legacy mentions in tests / docs that reference the
historical name are fine).

### 4.2 Extend `CognifyResult` with DLT-teardown carriers

Edit [`crates/cognify/src/tasks.rs`](../../crates/cognify/src/tasks.rs) (or
wherever `CognifyResult` is defined):

```rust
pub struct CognifyResult {
    // ... existing fields ...
    /// Documents and chunks needed by the post-pipeline
    /// `extract_dlt_fk_edges` teardown step. Populated by the final
    /// task in `build_cognify_pipeline`; empty in the temporal branch.
    pub documents_for_dlt: Vec<Document>,
    pub chunks_for_dlt: Vec<DocumentChunk>,
}
```

Update `make_add_data_points_task` and `add_data_points` to populate
these fields from the `SummarizedText` input.

If `CognifyResult` is serialised by bindings (PyO3, Neon) or by tests,
sub-agent A audits via `rg "CognifyResult" crates/ capi/ js/ python/` and
the implementor adds `#[serde(skip)]` on the new fields if they should
not appear in the binding wire shape. **Locked choice**: do `#[serde(skip)]`
the two new fields — they are internal teardown carriers, not part of the
public result shape.

### 4.3 Rewrite the convenience function

```rust
pub async fn cognify(
    data_items: Vec<Data>,
    dataset_id: Uuid,
    user_id: Option<Uuid>,
    user_email: Option<String>,
    tenant_id: Option<Uuid>,
    llm: Arc<dyn Llm>,
    storage: Arc<dyn StorageTrait>,
    graph_db: Arc<dyn GraphDBTrait>,
    vector_db: Arc<dyn VectorDB>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    database: Arc<DatabaseConnection>,           // ← was Option<Arc<...>>; now required
    thread_pool: Arc<dyn CpuPool>,               // ← NEW
    ontology_resolver: Arc<dyn OntologyResolver>,
    config: &CognifyConfig,
) -> Result<CognifyResult, CognifyError> {
    config.validate().map_err(|e| CognifyError::ConfigError(e.to_string()))?;

    // Decision 6: auto-chunk-size mutation happens before execute().
    let effective_config = if config.max_chunk_size == CognifyConfig::default().max_chunk_size {
        config.clone().with_auto_chunk_size(embedding_engine.as_ref(), llm.as_ref())
    } else {
        config.clone()
    };
    info!("Cognify config: chunks_per_batch={}, max_chunk_size={}",
          effective_config.chunks_per_batch, effective_config.max_chunk_size);

    // Empty short-circuit before pipeline build (matches current behaviour).
    if data_items.is_empty() {
        return Ok(CognifyResult::empty());
    }

    // Temporal vs standard selection (Decision 2).
    let pipeline = if effective_config.temporal_cognify {
        return cognify_temporal_branch(/* …LIB-06-04 handles this… */).await;
    } else {
        build_cognify_pipeline(
            Arc::clone(&storage),
            Arc::clone(&graph_db),
            Arc::clone(&vector_db),
            Arc::clone(&embedding_engine),
            Arc::clone(&llm),
            Some(Arc::clone(&database)),
            Arc::clone(&ontology_resolver),
            effective_config.clone(),
        )
    };

    // PipelineContext carries user/tenant/dataset.
    let pipeline_ctx = PipelineContext {
        user_id, user_email: user_email.clone(), tenant_id, dataset_id: Some(dataset_id),
        ..Default::default()
    };

    let (_cancellation_handle, ctx) = TaskContextBuilder::new()
        .thread_pool(thread_pool)
        .database(Arc::clone(&database))
        .graph_db(Arc::clone(&graph_db))
        .vector_db(Arc::clone(&vector_db))
        .pipeline_context(pipeline_ctx)
        .build()
        .map_err(|e| CognifyError::ContextBuild(e.to_string()))?;
    let ctx = Arc::new(ctx);

    let input = CognifyInput {
        data_items: data_items.clone(),
        dataset_id,
        user_id,
        tenant_id,
    };
    let inputs: Vec<Arc<dyn Value>> = vec![Arc::new(input) as _];

    let outputs = cognee_core::pipeline::execute(&pipeline, inputs, ctx, &NoopWatcher)
        .await
        .map_err(|e| CognifyError::Execute(e.to_string()))?;

    let result = extract_cognify_outputs(outputs)?;

    // Decision 5: post-pipeline teardown.
    extract_dlt_fk_edges(&result.chunks_for_dlt, &result.documents_for_dlt, Arc::clone(&graph_db)).await?;

    Ok(result)
}
```

Illustrative. Implementor finalises:

- Whether `database` should be required `Arc<DatabaseConnection>` (matches
  `TaskContextBuilder`'s mandatory field) or stay `Option<Arc<...>>` with
  a missing-backend error. **Locked choice**: required. The two
  bindings (Python, Node) already construct a `DatabaseConnection` at
  startup. CLI does too. Examples that don't have a DB get an in-memory
  SQLite (`connect("sqlite::memory:")` is `async`, so the example would
  await it once at the top).
- Whether `cognify_temporal_branch` is a separate helper (cleaner) or
  inline (less hopping). Implementer's call.

### 4.4 Drop inline `stamp_provenance` calls

Removed entirely. The executor's `stamp_tree_dyn` walks each task's
output and stamps `source_pipeline` = pipeline name, `source_task` =
task name, `source_user` = `PipelineContext::user_label()`. Decision 3's
gate verifies that the resulting stamps match byte-for-byte.

If they do **not** match (e.g. `source_user` ends up `None` because
`user_label()` is computed differently from `user_str_ref`):

- Sub-agent A escalates to the user.
- Either align `user_label()` with the existing logic, or accept the
  semantic shift (only if the user explicitly approves).

### 4.5 `data_id_fn` for cognify

The cognify pipeline takes a single `CognifyInput`. Set `data_id_fn` to
extract a sentinel — sub-agent A confirms the pre-refactor cognify path
populated `PipelineRunInfo.data_ids` (via `dispatch_pipeline`'s
`data_ids: Vec::new()` at
[`dispatch.rs:103`](../../crates/http-server/src/pipelines/dispatch.rs#L103)).
So today's `data_ids` is empty. For LIB-06-03, keep it empty. **Gap-08
task 07 will revisit** — see the parent doc Decision 4. The cleanest
shape: leave `data_id_fn = None` on the pipeline; rely on the
convenience function (in gap 08-07) to push `data_ids` via a future
`PipelineContext::data_ids` field. Out of scope here.

### 4.6 `extract_cognify_outputs`

```rust
fn extract_cognify_outputs(outputs: Vec<Arc<dyn Value>>) -> Result<CognifyResult, CognifyError> {
    let first = outputs.into_iter().next()
        .ok_or(CognifyError::OutputTypeMismatch { expected: "CognifyResult", actual: "empty" })?;
    first
        .downcast_ref::<CognifyResult>()
        .cloned()
        .ok_or(CognifyError::OutputTypeMismatch { expected: "CognifyResult", actual: "unknown" })
}
```

### 4.7 New `CognifyError` variants

```rust
#[error("task context build failed: {0}")]
ContextBuild(String),

#[error("pipeline execution failed: {0}")]
Execute(String),

#[error("output type mismatch: expected {expected}, got {actual}")]
OutputTypeMismatch { expected: &'static str, actual: &'static str },
```

### 4.8 Update CLI

[`crates/cli/src/commands/cognify.rs`](../../crates/cli/src/commands/cognify.rs)
and [`add_and_cognify.rs`](../../crates/cli/src/commands/add_and_cognify.rs)
construct a `RayonThreadPool::with_default_threads()?` (if not already)
and pass `Arc::clone(&database)` instead of `Some(Arc::clone(&database))`.

### 4.9 Update bindings + examples + tests

```bash
rg "cognify::cognify\|cognee_lib::cognify\|cognify\(" capi/ js/ python/ examples/ crates/ | grep -v test
```

Each call site passes the new arguments. Bindings already own
`Arc<DatabaseConnection>`; they construct a `RayonThreadPool` at startup
(or reuse an existing one — sub-agent A audits).

### 4.10 Leave the `TODO(LIB-06 follow-up)` comment

Decision 13 keeps it until LIB-06-05.

## 5. Files modified

- [`crates/cognify/src/tasks.rs`](../../crates/cognify/src/tasks.rs):
  - Rewrite `cognify` to call `pipeline::execute`.
  - Add `extract_cognify_outputs` helper.
  - Extend `CognifyResult` with `documents_for_dlt` and `chunks_for_dlt`
    (both `#[serde(skip)]`).
  - Update `make_add_data_points_task` / `add_data_points` to populate
    those fields.
  - Rewrite every inline `stamp_provenance(..., "cognify_pipeline", ...)`
    literal to `"cognify"` so the post-refactor `source_pipeline` value
    matches `build_cognify_pipeline`'s `with_name("cognify")`. The
    builder name itself does **not** change.
- [`crates/cognify/src/error.rs`](../../crates/cognify/src/error.rs) (or
  inline) — new error variants.
- [`crates/cli/src/commands/cognify.rs`](../../crates/cli/src/commands/cognify.rs),
  [`add_and_cognify.rs`](../../crates/cli/src/commands/add_and_cognify.rs),
  [`run_sequence.rs`](../../crates/cli/src/commands/run_sequence.rs) — pass
  new arguments.
- `capi/src/`, `js/src/`, `python/src/` cognify entry points.
- `examples/cognify_*.rs`, `examples/fact_extraction*.rs`,
  `examples/*ladybug*.rs` etc. — update call sites.
- `crates/cognify/tests/*`, `crates/cli/tests/*` — same.

## 6. Verification

```bash
# 1. Workspace compiles.
cargo check --all-targets

# 2. Cognify unit tests.
cargo test -p cognee-cognify

# 3. FULL COGNIFY E2E SUITE (Decision 3 gate).
bash scripts/run_tests_with_openai.sh

# 4. Cognify CLI E2E.
cargo test -p cognee-cli --test cli_e2e -- cognify

# 5. CROSS-SDK PARITY (Decision 15 + Decision 3 gate).
cd e2e-cross-sdk && docker compose up --build --abort-on-container-exit && cd -

# 6. Bindings smoke.
bash python/scripts/check.sh
bash js/scripts/check.sh
bash capi/scripts/check.sh

# 7. Full check.
scripts/check_all.sh
```

**Sub-agent C must NOT mark this task complete until steps 3 and 5 pass.**
If `test_cognify_structural.py` regresses (node-count similarity drops
below 50%, type Jaccard below 0.3, etc.), the refactor has broken
provenance equivalence. Fix it; do **not** loosen the tolerances.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| **Provenance equivalence breaks silently** — `stamp_tree_dyn` stamps differently from inline `stamp_provenance` | **High** — riskiest part of LIB-06 | Decision 3 gate. Sub-agent C runs the full E2E + cross-SDK suite. Concrete failure modes: `source_pipeline` differs (Decision 14 audit catches this), `source_user` differs (Decision 3's user_label() audit), `source_task` differs (executor uses `task.name`, inline uses literal strings like `"classify_documents"` — these may not match; sub-agent A inspects `make_*_task`'s name strings). |
| `CognifyResult` field addition breaks PyO3 / Neon serialisation | Medium | `#[serde(skip)]` on the new fields. If bindings use a non-serde path (manual conversion), audit each binding's cognify result handling. |
| Auto-chunk-size mutation timing — if a task reads `CognifyConfig` from `TaskContext` (it doesn't today; it captures it at builder time), the mutation must happen before `build_cognify_pipeline` | Low | The `build_cognify_pipeline` already consumes the final config; mutation is before the call. |
| Empty-data short-circuit is removed inadvertently | Low | Keep `if data_items.is_empty() { return Ok(CognifyResult::empty()); }` before pipeline build. Empty-chunk short-circuit becomes a task-level no-op (the `extract_chunks_from_documents` task returns an empty `ExtractedChunks` and the rest of the pipeline naturally produces empty outputs). Verify the empty-chunk path still returns a usable `CognifyResult`. |
| `extract_dlt_fk_edges` failure occurs **after** the pipeline `COMPLETED` watcher hook fires, leaving the pipeline_runs row in a misleading state | Low (today's watcher is `NoopWatcher` — Decision 11) | Document the issue. Gap-08 task 07 will need to think about it; for LIB-06 it's a known limitation. |
| `RayonThreadPool` construction in bindings adds startup cost / global state | Low | Bindings already initialise other backends at startup; the thread pool is small. If thread-pool creation fails, bindings surface the error at init time. |
| Cross-SDK test runs against real OpenAI and is slow / flaky | Medium | Pin the OpenAI model in `e2e-cross-sdk` to a known-stable version. The harness already has tolerances; sub-agent C does not loosen them. If a single run flakes, re-run once. If two runs flake on different metrics, escalate. |
| `Arc<dyn CpuPool>` not exported / not constructable from `cognee_core` | Low | Sub-agent A confirms `cognee_core::RayonThreadPool::with_default_threads` is public; if not, expose it. |

## 8. Out of scope

- Wiring `DbPipelineWatcher` — gap 08-07.
- Converting `extract_dlt_fk_edges` to a typed task — Decision 5 defers.
- Temporal branch — LIB-06-04.
- Removing the `Option<Arc<DatabaseConnection>>` plumbing inside
  `make_*_task` helpers. Those still accept `Option`; only the
  convenience function's signature becomes `Arc<DatabaseConnection>`.
- Adding a `Pipeline::with_data_id_fn` that handles
  `CognifyInput.data_items` — gap 08-07.
- Switching the cognify result shape. `CognifyResult` keeps its existing
  semantics; only the new teardown-carrier fields are added.
- Cleaning up the `TODO(LIB-06 follow-up)` comment — LIB-06-05.
