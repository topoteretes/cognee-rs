# LIB-06 — Executor-Routed Convenience Pipelines

## Overview

Cognee-Rust exposes three library-level "convenience" entry points that today
bypass `cognee_core::pipeline::execute`:

| Entry point | Location (as of `205bc8a`) | TODO marker |
|---|---|---|
| `cognify::cognify` | [`crates/cognify/src/tasks.rs:1773`](../../crates/cognify/src/tasks.rs#L1773) | `TODO(LIB-06 follow-up)` at line 1762 |
| `cognify::memify::memify` | [`crates/cognify/src/memify/pipeline.rs:57`](../../crates/cognify/src/memify/pipeline.rs#L57) | `TODO(LIB-06 follow-up)` at line 48 |
| `ingestion::AddPipeline::add` / `add_with_params` | [`crates/ingestion/src/pipeline.rs:786`](../../crates/ingestion/src/pipeline.rs#L786) | `TODO(LIB-06 follow-up)` at lines 771 + 804 |

Each function calls the underlying task functions (e.g. `classify_documents`,
`extract_chunks_from_documents`, `process_input`, `persist_data_with_acl`)
*directly* rather than going through `Pipeline` + `execute()`. As a result:

- They never fire the `PipelineWatcher` lifecycle hooks
  (`on_pipeline_run_initiated`, `on_pipeline_run_started`,
  `on_pipeline_run_completed`, `on_pipeline_run_errored`, `on_payload_field`).
- The `pipeline_runs` audit trail (telemetry gap 08) cannot be produced from
  CLI / library calls — only the HTTP-server's `dispatch_pipeline` writes
  rows.
- `Pipeline::telemetry_settings` is never threaded through, so the
  `Pipeline Run Started/Completed/Errored` analytics events (gap 03/04) never
  fire for library callers either.
- Tasks cannot publish run-scoped payload via
  `TaskContext::publish_payload_field`, blocking
  [`docs/http-api-v2/tasks/lib-06-pipeline-payload-mechanism.md`](../http-api-v2/tasks/lib-06-pipeline-payload-mechanism.md).

Telemetry gap **08-07** ("Wire `PipelineRunRepository` through library
pipelines") and the LIB-06 payload-event work both *require* the executor
route to be in place first. This document is the design landing for that
refactor.

> **Scope split.** This gap (LIB-06) only routes the three convenience
> functions through `pipeline::execute` with a `NoopWatcher`. Wiring a real
> `DbPipelineWatcher` and producing the four-state `pipeline_runs` trail is
> [gap 08 task 07](08/07-library-pipeline-wiring.md), which depends on
> LIB-06 closing first. See [Decisions § 11](#design-decisions-locked).

---

## Current Rust state (as of `205bc8a`)

### `cognify::cognify` ([`crates/cognify/src/tasks.rs:1773-1948`](../../crates/cognify/src/tasks.rs#L1773-L1948))

Inline pipeline. Six concrete divergences from the `pipeline::execute` path:

1. **Auto-chunk-size mutation** (lines 1794-1806). When the caller passed the
   default `max_chunk_size`, the function recomputes it via
   `with_auto_chunk_size(embedding_engine.as_ref(), llm.as_ref())` and shadows
   the borrowed config. The mutation happens *before* any task runs and is
   reflected in subsequent task arguments.
2. **Per-task provenance stamping** (lines 1832-1839, 1858-1865, 1905-1918,
   1924-1931). Each emitted batch of DataPoints is hand-stamped via
   `stamp_provenance(...)` with the literal pipeline name `"cognify_pipeline"`
   and the per-task name. The `pipeline::execute` path stamps via
   `stamp_tree_dyn` inside the executor's task harness (see
   [`crates/core/src/provenance.rs`](../../crates/core/src/provenance.rs) and
   [`crates/core/src/pipeline.rs::execute_one_item`](../../crates/core/src/pipeline.rs#L831)).
   **The semantic equivalence between hand-stamping and `stamp_tree_dyn` is
   the load-bearing claim of this refactor and is not yet proven.**
3. **Empty-document short-circuit** (lines 1841-1843) and
   **empty-chunk short-circuit** (lines 1867-1869). Both return
   `CognifyResult::empty()` without invoking the rest of the pipeline.
   `pipeline::execute` has no such short-circuit; the per-item executor
   simply runs zero further tasks if no items flow.
4. **Temporal branch** (lines 1874-1892). When `config.temporal_cognify` is
   set, runs an entirely different two-task sub-pipeline
   (`extract_temporal_events` + `add_temporal_data_points`) and returns. A
   builder for the temporal branch already exists at
   [`build_temporal_cognify_pipeline`](../../crates/cognify/src/tasks.rs#L2908).
5. **Post-pipeline `extract_dlt_fk_edges` teardown** (line 1945). Runs after
   all five tasks; not part of any `Pipeline` builder today.
6. **User-string derivation** (lines 1816-1819). Computes `user_str_ref` for
   the provenance stamper. Equivalent logic lives at
   `PipelineContext::user_label()`
   ([`crates/core/src/task_context.rs:67`](../../crates/core/src/task_context.rs#L67))
   so this becomes redundant once we hand off to `execute()`.

The existing builder `build_cognify_pipeline`
([line 2826](../../crates/cognify/src/tasks.rs#L2826)) produces the standard
five-task pipeline but does not currently encode the auto-chunk-size mutation
or the empty short-circuits.

### `cognify::memify::memify` ([`crates/cognify/src/memify/pipeline.rs:57-143`](../../crates/cognify/src/memify/pipeline.rs#L57-L143))

Two-stage flow: extract triplets → index triplets. Divergences:

1. **No `Pipeline` builder exists.** No `build_memify_pipeline` helper today.
2. **Custom-data branch** (lines 70-104). When `config.custom_data` is set,
   the function synthesises `Triplet` values directly from JSON instead of
   calling `extract_triplets_from_graph_db`. Maps to a different first task.
3. **Empty-triplets short-circuit** (lines 110-119). Returns a zero
   `MemifyResult` if no triplets are produced.
4. **`MemifyResult` shape** wraps a flat `IndexResult`; the executor needs an
   output-extraction adapter to recover this shape from the typed pipeline
   outputs.
5. **No `Data` inputs.** Memify operates on the existing graph, not a list of
   `Data`. The `pipeline::execute` API takes `inputs: Vec<Arc<dyn Value>>`;
   memify pre-flights triplet extraction and then passes the resulting
   `Vec<Triplet>` directly as the single executor input — no placeholder
   ZST is needed (see [Decision 8](#design-decisions-locked)).

### `ingestion::AddPipeline::add` / `add_with_params` ([`crates/ingestion/src/pipeline.rs:786-859`](../../crates/ingestion/src/pipeline.rs#L786-L859))

Explicit per-input loop. Divergences:

1. **`AddParams` (`node_set`, `importance_weight`, `dataset_id` override)** is
   injected on `ProcessedInput` *between* `process_input` (task 1) and
   `persist_data_with_acl` (task 2). Today the two-task pipeline produced by
   `build_add_pipeline_with_acl`
   ([line 691](../../crates/ingestion/src/pipeline.rs#L691)) does **not**
   accept `AddParams`; the convenience function patches the intermediate
   `ProcessedInput` directly.
2. **Pre-serialised `node_set_json`** (lines 822-827). Done once outside the
   loop, then cloned per input.
3. **Per-input output collection.** Each successful `Data` is pushed into
   `Vec<Data>` directly; with the executor route, outputs come back as
   `Vec<Arc<dyn Value>>` and must be downcast.

> **Builder note.** Contrary to the prior LIB-06 design notes,
> `build_add_pipeline` *does* exist
> ([line 670](../../crates/ingestion/src/pipeline.rs#L670)) but lacks the
> `AddParams` plumbing. The refactor must extend it (or add an
> `add_params`-aware variant), not invent it.

### `extract_dlt_fk_edges` ([`crates/cognify/src/tasks.rs`](../../crates/cognify/src/tasks.rs))

Operates on `(chunks, documents)` post-graph-build. **Not** a typed task
today — would need either:

- A new `make_extract_dlt_fk_edges_task` that consumes the
  `SummarizedText` output of `summarize_text`, runs the existing function,
  and re-emits the same value (no-op transform with a side effect); **or**
- Stay as post-pipeline teardown invoked by the convenience function after
  `execute()` returns. **Locked decision: [§ 5](#design-decisions-locked)
  keeps it as teardown.**

---

## Python parity reference

Source: clone via
`git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python`.

### `cognee.add` / `cognee.cognify` / `cognee.memify`

Python's `cognee/api/v1/{add,cognify,memify}/...py` all build a list of
task callables and pass them to
[`run_tasks`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_tasks.py).
`run_tasks` is the Python analogue of `cognee_core::pipeline::execute` — it
writes the four-state `pipeline_runs` trail, dispatches tasks sequentially,
and emits lifecycle telemetry. There is **no** "bypass" path in Python:
every `cognee.cognify()` call goes through `run_tasks`.

What we are matching:

- All three SDK entry points route through the executor.
- Post-pipeline teardown (Python doesn't have a direct analogue of
  `extract_dlt_fk_edges`; Python's DLT FK extraction happens as a regular
  pipeline task) — we deliberately keep teardown outside the executor for
  this gap to minimise blast radius. See
  [Decision § 5](#design-decisions-locked).

What we are **not** matching:

- Python serialises tasks differently (`Task(fn, **kwargs)` objects) — Rust
  uses `TypedTask` with strongly-typed inputs/outputs. Builder shape is
  unchanged; this gap only swaps the runner.
- Python's auto-chunk-size lives inside `cognify_pipeline()` *before*
  `run_tasks` is called — we mirror this (see
  [Decision § 6](#design-decisions-locked)).

---

## Design decisions (locked)

Approved by the project owner on 2026-05-13 — sub-agents may not re-litigate
these without escalating. If implementation surfaces evidence a decision is
wrong, sub-agent A escalates to the user.

| #  | Decision | Rationale | Affected tasks |
|----|---|---|---|
| 1  | **Convenience-function signatures may grow new required parameters.** `cognify::cognify` already has `graph_db`, `vector_db`, etc.; `memify::memify` and `AddPipeline::add` gain whichever of `thread_pool: Arc<dyn CpuPool>`, `graph_db: Arc<dyn GraphDBTrait>`, `vector_db: Arc<dyn VectorDB>`, `database: Arc<DatabaseConnection>` they need to satisfy `TaskContextBuilder::build()`. CLI / bindings / tests / examples are updated in the same sub-task that grows the signature; no "shim helpers" or default-argument tricks. | The executor's `TaskContext` requires those four fields. Working around it (e.g. building a partial context with mock backends inside the convenience function) leaks complexity into every call site and obscures the runtime contract. Better to surface the dependency at the boundary. | [LIB-06-01](lib-06/01-add-pipeline-executor-route.md), [LIB-06-02](lib-06/02-memify-executor-route.md) |
| 2  | **The temporal cognify branch stays a separate `Pipeline`.** The convenience function chooses between `build_cognify_pipeline` and `build_temporal_cognify_pipeline` *before* calling `pipeline::execute`, then calls `execute()` exactly once with whichever pipeline was selected. No conditional task inside a single pipeline. | Today's temporal branch is a clean swap of two of the five tasks. Encoding it as a runtime conditional inside one pipeline would conflate two different task DAGs and complicate downstream pipeline-id derivation (the pipeline name differs: `"cognify"` vs `"temporal-cognify"`). | [LIB-06-04](lib-06/04-cognify-temporal-executor-route.md) |
| 3  | **Provenance equivalence is verified per task, not assumed.** Each sub-task that touches a convenience function that does inline `stamp_provenance(...)` calls must run the *full* cognify E2E suite (including `e2e-cross-sdk/test_cognify_structural.py` against Docker) and compare graph/vector outputs against a pre-refactor baseline. Equivalence is declared by checking: (a) the count of stamped DataPoints is identical, (b) every `source_pipeline` / `source_task` / `source_user` / `source_node_set` / `source_content_hash` value matches one-for-one, (c) the cross-SDK structural test still passes with the existing 50% / 0.3-Jaccard tolerances. | The executor's `stamp_tree_dyn` walks the typed pipeline output graph; the convenience function's hand-stamping walks specific known-shape DataPoints. The two **should** be equivalent (gap 05 closure asserts this), but the only way to be sure is to compare actual outputs. Calling it out as a gate per task prevents "checks pass, integration test fails three commits later". | All sub-tasks that touch cognify (LIB-06-03, LIB-06-04). |
| 4  | **`data_id_fn` extractors are wired so the executor populates `data_ids` correctly.** For cognify, the input is `CognifyInput { data_items: Vec<Data>, ... }`; `data_id_fn` returns `Some(data.id.to_string())` for each `Data` inside `data_items`. **However**, the executor today expects one extractor over `Arc<dyn Value>`, with one ID per pipeline input — but cognify's pipeline takes *one* `CognifyInput` as its only input, which carries N `Data` values inside. **Therefore**: the cognify pipeline's `data_id_fn` extracts the first `Data.id` (or stays unset) for `pipeline.data_id_fn`, and the watcher's per-run `data_ids` carrier is populated from `CognifyInput.data_items` via a *separate* extraction at the convenience-function entry point (i.e. the convenience function builds the `PipelineRunInfo.data_ids` and passes it in via a new optional hook on `Pipeline` or via `PipelineContext`). Detailed wiring lives in [LIB-06-03](lib-06/03-cognify-standard-executor-route.md). For memify, no `Data` inputs — `data_ids = vec![]`. For ingestion, one `DataInput` per pipeline input → `data_id_fn` extracts the resulting `Data.id` after `persist_data` runs (post-hoc, via task output rather than input — see LIB-06-01 §4.5 for the exact mechanism). | The executor surface today expects a 1:1 input/data-id mapping. Cognify's "one input wrapping N Data" violates that. Surfacing this here so the implementor doesn't invent ad-hoc workarounds. | [LIB-06-01](lib-06/01-add-pipeline-executor-route.md), [LIB-06-03](lib-06/03-cognify-standard-executor-route.md) |
| 5  | **Post-pipeline teardown (cognify's `extract_dlt_fk_edges`) stays in the convenience function, after `pipeline::execute` returns.** The watcher's `on_pipeline_run_completed` hook fires *inside* `execute` (before teardown runs). If teardown fails, the convenience function returns `Err(...)` but the pipeline_runs row is already `COMPLETED`. This is acceptable because: (a) `extract_dlt_fk_edges` is a no-op for non-DLT data (the common case); (b) DLT FK extraction failures should not invalidate the knowledge graph the executor already wrote; (c) it keeps the executor migration minimal. | The alternative — converting `extract_dlt_fk_edges` to a typed task and pinning it as task 6 in `build_cognify_pipeline` — is correct in the long run, but adds a typed-task-wrapping change to a refactor that's already touching six call sites. Deferred to a follow-up. | [LIB-06-03](lib-06/03-cognify-standard-executor-route.md) |
| 6  | **Auto-chunk-size config mutation happens before `pipeline::execute` is called.** The convenience function clones the config, calls `with_auto_chunk_size(...)` if the caller used the default, then passes the cloned config to `build_cognify_pipeline`. The pipeline builder consumes the final config; the executor sees no mutation. | Matches Python's `cognify_pipeline()` which computes `chunk_size = get_max_chunk_tokens(...)` outside `run_tasks`. Keeps the executor stateless w.r.t. config. | [LIB-06-03](lib-06/03-cognify-standard-executor-route.md) |
| 7  | **For `AddPipeline::add`, `node_set` and `importance_weight` are injected via the `Pipeline` task closures.** Concretely: `make_persist_data_task_with_acl` gains a new optional `AddParams` parameter; the closure clones the params per invocation and patches the incoming `ProcessedInput` before calling `persist_data_with_acl`. Equivalent: the existing free-function `persist_data_with_acl` gets a new `add_params: &AddParams` parameter, or the persist closure does the `ProcessedInput.node_set = ...; ProcessedInput.importance_weight = ...` patch itself before delegating. They are NOT new `RunSpec` fields and NOT new `TaskContext` extension fields. | Task-local state belongs in the task closure. Threading `AddParams` through `RunSpec` would force every non-ingestion pipeline (cognify, memify) to plumb a parameter they don't use. Threading via `TaskContext` would force the persist task to query a fresh context per item — wasteful and awkward. | [LIB-06-01](lib-06/01-add-pipeline-executor-route.md) |
| 8  | **Memify passes `Vec<Triplet>` directly through the executor; no placeholder ZST.** Locked 2026-05-13. The convenience function runs the triplet-extraction (or custom-data synthesis) up-front as a pre-flight step, applies the empty-triplets short-circuit, and then runs a one-task "index-only" pipeline whose input is `Arc::new(triplets) as Arc<dyn Value>`. The earlier `MemifyTrigger` zero-sized placeholder is dropped — it would have been unused in the final design. `Vec<Triplet>` (or a thin wrapper) implements the executor's `Value` trait. | The placeholder existed only because the first design draft kept `extract_triplets_from_graph_db` as a pipeline task. Once we move triplet extraction to pre-flight (so the empty-triplets short-circuit can return without invoking `execute()` at all), the pipeline's first task is `index_triplets`, which already takes `Vec<Triplet>`. Removing the ZST simplifies the design and surface. | [LIB-06-02](lib-06/02-memify-executor-route.md) |
| 9  | **Output downcasting lives in a per-pipeline helper.** Each convenience function defines a private `extract_outputs(outputs: Vec<Arc<dyn Value>>) -> Result<ConcreteResult, ConvenienceError>` helper that downcasts the final stage's output. If the downcast fails, the function returns `ConvenienceError::OutputTypeMismatch { expected: "...", actual: "..." }`. No new top-level helper crate; each crate owns its downcast. | Keeps the executor's `Vec<Arc<dyn Value>>` shape unchanged. Bespoke downcast errors give better diagnostics than a generic helper. | All sub-tasks |
| 10 | **Existing test fixtures that invoke the convenience functions get updated, not bypassed.** Tests that previously called `cognify(...)` directly continue to call `cognify(...)` — they just pass the new `thread_pool` / `graph_db` / `vector_db` / `database` arguments. Tests that invoke the lower-level task functions (`classify_documents`, `extract_chunks_from_documents`, etc.) are untouched. Where a test needs to run without a real backend, it uses existing mocks: `MockGraphDB`, `MockVectorDB`, and an in-memory SQLite database from `cognee_database::connect("sqlite::memory:")`. The `testing` feature must remain opt-in; do **not** make `MockGraphDB` available unconditionally. | Avoids inventing parallel "fast-path" entry points that diverge from the production code path. Mocks are existing, supported, and well-tested. | All sub-tasks |
| 11 | **Watchers stay `NoopWatcher` for the duration of LIB-06.** This gap routes the convenience functions through `execute()` — full stop. Hooking a real `DbPipelineWatcher` is gap-08 task 07's job, which depends on LIB-06 closing first. The convenience functions construct `&NoopWatcher` inline and pass it to `execute()`. | Strict scope separation. Letting LIB-06 also wire a real watcher conflates the executor-route refactor with the persistence change, doubling the blast radius of a regression. Decision recorded so sub-agent B doesn't optimistically add a `pipeline_run_repo` parameter. | All sub-tasks; gap 08-07 unblocked once LIB-06 closes. |
| 12 | **No new `Pipeline`, `TaskContext`, or `PipelineWatcher` traits are introduced.** Reuse what's there. If the executor's API forces a slight rework of one of the convenience signatures (e.g. cognify gains `thread_pool` / `database` becomes required not `Option`), that is acceptable; inventing a parallel `LibraryPipelineRunner` trait is not. | Decision 11 already protects against watcher-side scope creep. This protects against executor-side scope creep. | All sub-tasks |
| 13 | **The three `LIB-06 follow-up` TODO comments are removed only by [LIB-06-05](lib-06/05-cleanup-todos.md), the cleanup sub-task.** Each of the implementation sub-tasks (01-04) leaves the comment in place until the cleanup pass runs at the end — so a partial landing keeps the marker visible. | Preserves an audit trail through the multi-commit refactor. | [LIB-06-05](lib-06/05-cleanup-todos.md) |
| 14 | **Pipeline names align on the builder strings, not the legacy `_pipeline` suffix.** Locked 2026-05-13. The canonical pipeline names are `build_cognify_pipeline` → `"cognify"`, the new memify builder → `"memify"`, and `build_add_pipeline` → `"ingestion"` (whatever the existing builder name is). The legacy inline `stamp_provenance(..., "cognify_pipeline", ...)` calls in the convenience function are **updated to use `"cognify"`** as part of the refactor; matching applies symmetrically to memify and ingestion if any `_pipeline`-suffixed strings exist there. This is a deliberate, one-time byte-shift of `source_pipeline` on DataPoints from `"cognify_pipeline"` to `"cognify"`. After the shift the value is stable. The cross-SDK structural test (Decision 15) covers node/edge counts and Jaccard similarity, not literal `source_pipeline` strings, so equivalence still holds. | The earlier draft of this decision insisted on byte stability of the legacy value (`"cognify_pipeline"`), which would have required renaming the builder. The locked answer flips that: the builder's name string wins, and the inline stamps are aligned to it. This keeps the public pipeline name (the one users see in CLI / bindings / `source_pipeline`) consistent with the builder name. Sub-task A must still `rg "cognify_pipeline\|memify_pipeline\|ingestion_pipeline\|\"cognify\"\|\"memify\"\|\"ingestion\""` and confirm every inline stamp is rewritten to the builder string. | All sub-tasks |
| 15 | **The cross-SDK harness is the equivalence gate.** [LIB-06-06](lib-06/06-tests-and-closure-summary.md) runs `e2e-cross-sdk/` end-to-end (`docker compose up --build --abort-on-container-exit`) as the final verification step before the gap closes. If structural similarity drops below the existing thresholds (50% node/edge counts, 0.3 Jaccard), the gap does not close. | Provenance + pipeline equivalence is impossible to assert from unit tests alone; the cross-SDK harness is the only thing today that compares against the Python reference output on real-ish data. | [LIB-06-06](lib-06/06-tests-and-closure-summary.md) |

---

## Action items

Each item below has a dedicated implementation sub-document under
[`lib-06/`](lib-06/) with rationale, pre-conditions, step-by-step
source-level changes, verification commands, files modified, and risks. The
sub-docs are **authoritative**: when they refine details based on the locked
decisions, follow the sub-doc rather than this high-level summary.

| #  | Action item | Sub-doc | Depends on | Status |
|----|---|---|---|---|
| 01 | Route `AddPipeline::add` / `add_with_params` through `pipeline::execute`. Extend `build_add_pipeline_with_acl` (or add a `_with_params` variant) to inject `AddParams` into the persist closure; grow `AddPipeline::add` to construct a `TaskContext` (no new public AddPipeline fields needed — `TaskContextBuilder` accepts the existing `Arc<dyn IngestDb>` / storage / etc., plus a real or mock graph/vector backend). Add `extract_outputs` helper to downcast pipeline output to `Vec<Data>`. Update CLI / bindings / examples / tests to pass the new arguments. | [lib-06/01-add-pipeline-executor-route.md](lib-06/01-add-pipeline-executor-route.md) | — | ✅ 82aac59 |
| 02 | Add `build_memify_index_only_pipeline` helper. Route `memify::memify` through `pipeline::execute` by pre-flighting triplet extraction (graph-DB query or custom-data synthesis), applying the empty-triplets short-circuit, and passing `Vec<Triplet>` directly as the executor input (no placeholder ZST per Decision 8). Update CLI / bindings / examples / tests. | [lib-06/02-memify-executor-route.md](lib-06/02-memify-executor-route.md) | — (independent of 01) | ✅ 64b6182 |
| 03 | Route `cognify::cognify` (non-temporal branch) through `pipeline::execute`. Preserve auto-chunk-size mutation as a pre-flight step. Drop inline `stamp_provenance` calls; rely on the executor's `stamp_tree_dyn`. Keep `extract_dlt_fk_edges` as post-pipeline teardown. Verify provenance equivalence by running the full cognify E2E suite and `e2e-cross-sdk/test_cognify_structural.py`. | [lib-06/03-cognify-standard-executor-route.md](lib-06/03-cognify-standard-executor-route.md) | 01 (provenance shake-out from a simpler pipeline) | ✅ a9701db |
| 04 | Route the cognify temporal branch through `pipeline::execute` via `build_temporal_cognify_pipeline`. Selection between standard and temporal happens before `execute()`. Verify temporal-specific tests. | [lib-06/04-cognify-temporal-executor-route.md](lib-06/04-cognify-temporal-executor-route.md) | 03 | ✅ 8ffcd88 |
| 05 | Cleanup pass: remove the three `LIB-06 follow-up` TODO comments. Audit `rg "LIB-06 follow-up" crates/` returns no matches in `tasks.rs`, `memify/pipeline.rs`, or `ingestion/src/pipeline.rs`. Ensure the comment is preserved only in tests / docs that legitimately reference the historical gap. | [lib-06/05-cleanup-todos.md](lib-06/05-cleanup-todos.md) | 01, 02, 03, 04 | ✅ d1b7f75 |
| 06 | Tests + cross-SDK parity + closure summary. Run the full cognify E2E suite under `scripts/run_tests_with_openai.sh`; run `e2e-cross-sdk` via Docker. Update [gap-analysis.md](gap-analysis.md) "Future work" bullet about `Pipeline::telemetry_settings` to note LIB-06 closure. Write the closure summary at the bottom of this doc. | [lib-06/06-tests-and-closure-summary.md](lib-06/06-tests-and-closure-summary.md) | 01, 02, 03, 04, 05 | ⬜ |

---

## Implementation runbook

See [`lib-06/00-implementation-runbook.md`](lib-06/00-implementation-runbook.md)
for the orchestrator prompt and five-sub-agent workflow per sub-task.

---

## References

- Rust source (frozen at `205bc8a`):
  - [`crates/cognify/src/tasks.rs`](../../crates/cognify/src/tasks.rs) —
    `cognify`, `build_cognify_pipeline`, `build_temporal_cognify_pipeline`.
  - [`crates/cognify/src/memify/pipeline.rs`](../../crates/cognify/src/memify/pipeline.rs) —
    `memify`.
  - [`crates/ingestion/src/pipeline.rs`](../../crates/ingestion/src/pipeline.rs) —
    `AddPipeline`, `build_add_pipeline`, `build_add_pipeline_with_acl`.
  - [`crates/core/src/pipeline.rs`](../../crates/core/src/pipeline.rs) —
    `execute`, `Pipeline`, `PipelineRunInfo`, `PipelineWatcher`,
    `NoopWatcher`.
  - [`crates/core/src/task_context.rs`](../../crates/core/src/task_context.rs) —
    `TaskContext`, `TaskContextBuilder`, `PipelineContext`.
  - [`crates/core/src/provenance.rs`](../../crates/core/src/provenance.rs) —
    `stamp_tree_dyn`, `stamp_provenance`.
  - [`crates/http-server/src/pipelines/dispatch.rs`](../../crates/http-server/src/pipelines/dispatch.rs) —
    canonical executor caller.
- Adjacent telemetry gaps:
  - [`08-pipeline-run-status.md`](08-pipeline-run-status.md) — the consumer
    of LIB-06; task 08-07 begins as soon as LIB-06 closes.
  - [`05-datapoint-provenance.md`](05-datapoint-provenance.md) — closed gap;
    documents the `stamp_tree_dyn` machinery LIB-06 will rely on for
    equivalence.
  - [`03-pipeline-task-api-events.md`](03-pipeline-task-api-events.md) — the
    `Pipeline::telemetry_settings` carrier whose wiring depends on LIB-06.
- Python reference (clone via
  `git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python`):
  - [`cognee/api/v1/cognify/cognify.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/cognify.py)
  - [`cognee/api/v1/add/add.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/add.py)
  - [`cognee/api/v1/memify/memify.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/memify/memify.py)
  - [`cognee/modules/pipelines/operations/run_tasks.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_tasks.py)
- LIB-06 sidecar (the payload mechanism whose unblocking depends on this
  gap): [`docs/http-api-v2/tasks/lib-06-pipeline-payload-mechanism.md`](../http-api-v2/tasks/lib-06-pipeline-payload-mechanism.md).
