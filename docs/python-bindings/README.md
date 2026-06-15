# Python Bindings — Parity Analysis

The Python bindings (`python/`) expose both the **pipeline engine tier** (generic task-pipeline
machinery from `cognee-core`) and the **SDK tier** (high-level cognee operations: add, cognify,
search, delete, memory management, etc.). All three bindings — Python (PyO3), C API (FFI), and
TypeScript/Node (Neon) — are now at full parity across both tiers (T1–T11 complete, June 2026).

This document tracks every feature group and its implementation status across all three binding
layers.

To execute future plans with a sub-agent-driven workflow (plan-check → implement → review →
commit, one task at a time), use [IMPLEMENTATION-PROMPT.md](../.internal/python-bindings/IMPLEMENTATION-PROMPT.md); live
progress is tracked in [STATUS.md](../.internal/python-bindings/STATUS.md).

---

## Legend

| Symbol | Meaning |
|--------|---------|
| ✅ | Fully implemented |
| ⚠️ | Partially implemented (gaps noted) |
| ❌ | Not implemented |

---

## Feature Matrix

### Pipeline Engine Tier (cognee-core)

| Feature | C API | TS/JS | Python | Notes |
|---------|-------|-------|--------|-------|
| Runtime init / shutdown | ✅ | ✅ | ✅ | `cg_init` / `init()` / automatic on import |
| Pipeline builder | ✅ | ✅ | ✅ | `CgPipeline` / `Pipeline` class |
| Task factories — sync/async/iter/stream | ✅ | ✅ | ✅ | C: 8 explicit factories; JS: 3 typed; Python: auto-detects callable type |
| TaskInfo (name, batch_size, weight, summary) | ✅ | ✅ | ✅ | Python integrates into `add_task()` kwargs |
| Pipeline execution — blocking | ✅ | ✅ | ✅ | `execute_sync` |
| Pipeline execution — async | ✅ | ✅ | ✅ | `execute` (awaitable) |
| Pipeline execution — background | ✅ | ✅ | ✅ | `execute_in_background` / `PipelineRunHandle` |
| RunHandle (is_finished, abort, wait) | ✅ | ✅ | ✅ | `PipelineRunHandle.wait()` |
| TaskContext — mock | ✅ | ✅ | ✅ | `TaskContext.mock()` |
| CancellationHandle | ✅ | ✅ | ✅ | via `ctx.cancellation_handle` |
| CancellationToken (separate object) | ✅ | ✅ | ✅ | `PyCancellationToken` + `cancellation_pair()` |
| `cancellation_pair()` factory | ✅ | ✅ | ✅ | Module-level `cancellation_pair()` function |
| ProgressToken — set / fraction / split | ✅ | ✅ | ✅ | |
| ProgressToken — width / subtoken | ✅ | ✅ | ✅ | Added in T11 |
| PipelineWatcher | ✅ | ✅ | ✅ | Typed `Watcher` class with event-dict constructor added in T11 |
| ExecStatusManager | ✅ | ❌ | ❌ | Noop only in C API; not surfaced in TS or Python |
| RayonThreadPool (explicit) | ✅ | ❌ | ❌ | Only in C API; others use implicit pool |
| DataIdFn (custom ID extractor) | ✅ | ❌ | ❌ | Only in C API |
| Retry policy (constant + exponential) | ✅ | ✅ | ✅ | |
| setup_logging | ✅ | ✅ | ✅ | |
| setup_telemetry (OTLP) | ✅ | ✅ | ✅ | |
| setup_telemetry_analytics | ✅ | ✅ | ✅ | |

### SDK Tier (cognee-lib / cognee-bindings-common)

| Feature | C API | TS/JS | Python | Plan |
|---------|-------|-------|--------|------|
| SDK handle (`Cognee` / `CgSdk`) | ✅ | ✅ | ✅ | [sdk-handle.md](sdk-handle.md) |
| `warm()` — engine pre-build | ✅ | ✅ | ✅ | [sdk-handle.md](sdk-handle.md) |
| `owner_id()` | ✅ | ✅ | ✅ | [sdk-handle.md](sdk-handle.md) |
| Config surface — granular setters | ✅ | ✅ | ✅ | [config-surface.md](config-surface.md) |
| Config surface — bulk setters | ✅ | ✅ | ✅ | [config-surface.md](config-surface.md) |
| Config read-back (`get_config`) | ✅ | ✅ | ✅ | [config-surface.md](config-surface.md) |
| `add()` — ingest data | ✅ | ✅ | ✅ | [core-pipeline-ops.md](core-pipeline-ops.md) |
| `cognify()` — KG extraction | ✅ | ✅ | ✅ | [core-pipeline-ops.md](core-pipeline-ops.md) |
| `add_and_cognify()` | ✅ | ✅ | ✅ | [core-pipeline-ops.md](core-pipeline-ops.md) |
| `search()` — 15 search types | ✅ | ✅ | ✅ | [retrieval-ops.md](retrieval-ops.md) |
| `recall()` — session-first routing | ✅ | ✅ | ✅ | [retrieval-ops.md](retrieval-ops.md) |
| `remember()` — add+cognify+improve | ✅ | ✅ | ✅ | [memory-ops.md](memory-ops.md) |
| `remember_entry()` — typed entries | ✅ | ✅ | ✅ | [memory-ops.md](memory-ops.md) |
| `memify()` — triplet embeddings | ✅ | ✅ | ✅ | [memory-ops.md](memory-ops.md) |
| `improve()` — session-graph bridge | ✅ | ✅ | ✅ | [memory-ops.md](memory-ops.md) |
| `forget()` — unified deletion | ✅ | ✅ | ✅ | [data-ops.md](data-ops.md) |
| `update()` — replace data item | ✅ | ✅ | ✅ | [data-ops.md](data-ops.md) |
| `prune_data()` — wipe files | ✅ | ✅ | ✅ | [data-ops.md](data-ops.md) |
| `prune_system()` — selective wipe | ✅ | ✅ | ✅ | [data-ops.md](data-ops.md) |
| `list_datasets()` | ✅ | ✅ | ✅ | [dataset-management.md](dataset-management.md) |
| `list_data(dataset_id)` | ✅ | ✅ | ✅ | [dataset-management.md](dataset-management.md) |
| `has_data(dataset_id)` | ✅ | ✅ | ✅ | [dataset-management.md](dataset-management.md) |
| `dataset_status(ids)` | ✅ | ✅ | ✅ | [dataset-management.md](dataset-management.md) |
| `empty_dataset(id)` | ✅ | ✅ | ✅ | [dataset-management.md](dataset-management.md) |
| `delete_data(dataset_id, data_id)` | ✅ | ✅ | ✅ | [dataset-management.md](dataset-management.md) |
| `delete_all_datasets()` | ✅ | ✅ | ✅ | [dataset-management.md](dataset-management.md) |
| `get_session(id)` | ✅ | ✅ | ✅ | [session-admin-ops.md](session-admin-ops.md) |
| `add_feedback(session, qa)` | ✅ | ✅ | ✅ | [session-admin-ops.md](session-admin-ops.md) |
| `delete_feedback(session, qa)` | ✅ | ✅ | ✅ | [session-admin-ops.md](session-admin-ops.md) |
| `get_graph_context(session)` | ✅ | ✅ | ✅ | [session-admin-ops.md](session-admin-ops.md) |
| `set_graph_context(session)` | ✅ | ✅ | ✅ | [session-admin-ops.md](session-admin-ops.md) |
| `reset_pipeline_run_status` | ✅ | ✅ | ✅ | [session-admin-ops.md](session-admin-ops.md) |
| `reset_dataset_pipeline_run_status` | ✅ | ✅ | ✅ | [session-admin-ops.md](session-admin-ops.md) |
| `get_or_create_default_user()` | ✅ | ✅ | ✅ | [session-admin-ops.md](session-admin-ops.md) |
| `list_notebooks()` | ✅ | ✅ | ✅ | [session-admin-ops.md](session-admin-ops.md) |
| `create_notebook()` | ✅ | ✅ | ✅ | [session-admin-ops.md](session-admin-ops.md) |
| `update_notebook()` | ✅ | ✅ | ✅ | [session-admin-ops.md](session-admin-ops.md) |
| `delete_notebook()` | ✅ | ✅ | ✅ | [session-admin-ops.md](session-admin-ops.md) |
| `visualize()` — HTML output | ✅ | ✅ | ✅ | [visualization-ops.md](visualization-ops.md) |
| `visualize_to_file()` | ✅ | ✅ | ✅ | [visualization-ops.md](visualization-ops.md) |
| `serve()` — cloud connect | ✅ | ✅ | ✅ | [cloud-ops.md](cloud-ops.md) |
| `disconnect()` — cloud teardown | ✅ | ✅ | ✅ | [cloud-ops.md](cloud-ops.md) |

---

## Summary

### What is implemented (as of June 2026, T1–T11 complete)

The Python binding (`cognee_pipeline`) is a **complete, production-quality implementation of both
the pipeline engine tier and the SDK tier**. It is at full parity with the C API and TypeScript
bindings across all 40+ operations.

**Pipeline engine tier** (`cognee-core`):
- Full `Pipeline` builder with retry, batch, concurrency controls
- All four callable types (sync, async, generator, async generator) auto-detected
- Three execution modes (sync-blocking, async, background)
- `CancellationHandle`, `CancellationToken`, `cancellation_pair()`, `ProgressToken`, `PipelineRunHandle`
- Typed `Watcher` class with event-dict constructor
- Logging (`setup_logging`), OTLP tracing (`setup_telemetry`), and product analytics
  (`setup_telemetry_analytics`) with the same idempotency guarantees as C API and TS
- Structured exception hierarchy (`CogneeError` and five subclasses)

**SDK tier** (`cognee-lib` / `cognee-bindings-common`):
- `Cognee` class with `warm()`, `owner_id()`
- Full config surface: granular setters, bulk setters, `get_config()` with secret redaction
- Core pipeline: `add()`, `cognify()`, `add_and_cognify()`
- Retrieval: `search()` (15 search types), `recall()` (session-first routing)
- Memory: `remember()`, `remember_entry()`, `memify()`, `improve()`
- Data lifecycle: `forget()`, `update()`, `prune_data()`, `prune_system()`
- Dataset management: 7 CRUD ops (`list_datasets`, `list_data`, `has_data`, `dataset_status`,
  `empty_dataset`, `delete_data`, `delete_all_datasets`)
- Sessions: `get_session`, `add_feedback`, `delete_feedback`, `get_graph_context`, `set_graph_context`
- Users / admin: `get_or_create_default_user`, `reset_pipeline_run_status`, `reset_dataset_pipeline_run_status`
- Notebooks: `list_notebooks`, `create_notebook`, `update_notebook`, `delete_notebook`
- Visualisation: `visualize()`, `visualize_to_file()`
- Cloud: `serve()`, `disconnect()`

**Drop-in upstream SDK compatibility** (`cognee_pipeline.compat`, `ID-1`):
- Module-level `add`, `cognify`, `add_and_cognify`, `search`, `prune` matching the upstream
  Python `cognee` SDK surface, with input coercion for `str`, `Path`, URL, `bytes`, `dict`, `list`
- `SearchType` as `(str, Enum)` — `SearchType.CHUNKS == "CHUNKS"` is `True`
- Optional `cognee/` alias package (`pip install "cognee_pipeline[drop-in]"`)

### Intentional gaps (C-API-only, not surfaced in Python or TS)

Three low-level surfaces are present only in the C API:

| Feature | Reason omitted from Python / TS |
|---|---|
| `ExecStatusManager` | Noop only; not useful at higher language levels |
| `RayonThreadPool` (explicit) | Others use the implicit pool |
| `DataIdFn` (custom ID extractor) | C-only embedding use-case |

### Architecture note: shared op bodies

All SDK-op logic (input marshaling, dataset resolution, result-JSON assembly) lives in
`crates/bindings-common/` so all three bindings call one implementation. Per-binding
shims in `capi/cognee-capi/src/sdk_*.rs`, `js/cognee-neon/src/sdk_*.rs`, and
`python/src/sdk_*.rs` are thin wrappers around the shared op bodies.
