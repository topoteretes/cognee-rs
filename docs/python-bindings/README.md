# Python Bindings — Parity Analysis

The Python bindings (`python/`) expose the **pipeline engine tier** only — the generic task-pipeline
machinery from `cognee-core`. The **SDK tier** (high-level cognee operations: add, cognify, search,
delete, memory management, etc.) is fully implemented in both the C API (`capi/`) and the
TypeScript / Node bindings (`js/`), but has no Python surface yet.

This document tracks every feature group, its implementation status across all three binding layers,
and links to implementation plans for the missing Python pieces.

To execute the plans with a sub-agent-driven workflow (plan-check → implement → review → commit,
one task at a time), use [IMPLEMENTATION-PROMPT.md](IMPLEMENTATION-PROMPT.md); live progress is
tracked in [STATUS.md](STATUS.md).

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
| CancellationToken (separate object) | ✅ | ✅ | ❌ | Python only exposes the handle, not the token |
| `cancellation_pair()` factory | ✅ | ✅ | ❌ | No independent pair creation in Python |
| ProgressToken — set / fraction / split | ✅ | ✅ | ✅ | |
| ProgressToken — width / subtoken | ✅ | ✅ | ❌ | Two methods missing in Python |
| PipelineWatcher | ✅ | ✅ | ⚠️ | Python uses duck-typing bridge; no typed `Watcher` class / factory |
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
| `serve()` — cloud connect | ✅ | ✅ | ❌ | [cloud-ops.md](cloud-ops.md) |
| `disconnect()` — cloud teardown | ✅ | ✅ | ❌ | [cloud-ops.md](cloud-ops.md) |

---

## Summary

### What is implemented

The Python binding (`cognee_pipeline`) is a **complete, production-quality implementation of the
pipeline engine tier**. It covers everything in `cognee-core`:

- Full `Pipeline` builder with retry, batch, concurrency controls
- All four callable types (sync, async, generator, async generator) auto-detected
- Three execution modes (sync-blocking, async, background)
- `CancellationHandle`, `ProgressToken`, `PipelineRunHandle`
- Duck-typed `PipelineWatcher` bridge
- Logging (`setup_logging`), OTLP tracing (`setup_telemetry`), and product analytics
  (`setup_telemetry_analytics`) with the same idempotency guarantees as C API and TS
- Structured exception hierarchy (`PipelineError` and five subclasses)

### What is missing

**The entire SDK tier is absent.** This is the layer that makes `cognee` useful as a
knowledge-management SDK: ingesting data, building the knowledge graph, searching it, and managing
memory. All 40+ SDK-tier operations present in both the C API and the TypeScript binding have no
Python equivalent yet.

The missing work is grouped into eight implementation plans:

| Document | Operations | Complexity |
|----------|-----------|------------|
| [sdk-handle.md](sdk-handle.md) | `Cognee` class, `warm`, `owner_id` | Low |
| [config-surface.md](config-surface.md) | All `config_set_*` / `get_config` | Low |
| [core-pipeline-ops.md](core-pipeline-ops.md) | `add`, `cognify`, `add_and_cognify` | Medium |
| [retrieval-ops.md](retrieval-ops.md) | `search`, `recall` | Medium |
| [memory-ops.md](memory-ops.md) | `remember`, `remember_entry`, `memify`, `improve` | Medium |
| [data-ops.md](data-ops.md) | `forget`, `update`, `prune_data`, `prune_system` | Medium |
| [dataset-management.md](dataset-management.md) | 7 dataset/data CRUD ops | Low–Medium |
| [session-admin-ops.md](session-admin-ops.md) | Sessions, feedback, users, notebooks | Medium |
| [visualization-ops.md](visualization-ops.md) | `visualize`, `visualize_to_file` | Low |
| [cloud-ops.md](cloud-ops.md) | `serve`, `disconnect` | Low |

Additionally there are minor gaps in the engine tier:
- `CancellationToken` object and `cancellation_pair()` factory
- `ProgressToken.width` property and `ProgressToken.subtoken()` method
- Typed `Watcher` class / factory (currently duck-typed only)

These are tracked in [minor-engine-gaps.md](minor-engine-gaps.md).

### Architecture note: where the SDK-op logic lives

`cognee-bindings-common` (`crates/bindings-common/`) provides only the *foundation*: `HandleState`
(config + lazy `CogneeServices`), `SdkError`, and a few wire helpers. The actual op bodies
(input marshaling, dataset resolution, result-JSON assembly) are currently **duplicated** between
`capi/cognee-capi/src/sdk_*.rs` and `js/cognee-neon/src/sdk_*.rs`. The Python plans therefore
start with a shared "Step 0" — hoisting those op bodies into a `cognee_bindings_common::ops`
module so all three bindings call one implementation — described in detail in
[core-pipeline-ops.md](core-pipeline-ops.md). Without that hoist, Python would become the third
copy of ~2,000 lines of op logic.

### Recommended implementation order

0. Hoist shared op bodies into `cognee-bindings-common` ([core-pipeline-ops.md](core-pipeline-ops.md) Step 0)
1. `sdk-handle.md` + `config-surface.md` — foundation everything else depends on
2. `core-pipeline-ops.md` — the primary user-facing workflow
3. `retrieval-ops.md` — completes the add→cognify→search loop
4. `data-ops.md` + `dataset-management.md` — lifecycle management
5. `memory-ops.md` — advanced memory features
6. `session-admin-ops.md` — session and notebook support
7. `visualization-ops.md` + `cloud-ops.md` — feature-gated extras
8. `minor-engine-gaps.md` — polish existing engine surface
