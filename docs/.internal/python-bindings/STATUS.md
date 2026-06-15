# Python Bindings Implementation — Status

Tracking file for the task plan in [IMPLEMENTATION-PROMPT.md](IMPLEMENTATION-PROMPT.md).
Updated by the Phase-4 finalizer agent after each task. Do not edit rows for tasks that are
not yet finished.

Status values: `pending` | `in-progress` | `done` | `blocked` | `skipped`

| ID | Title | Plan document | Status | Date | Commit |
|----|-------|---------------|--------|------|--------|
| T1 | Python SDK handle + error hierarchy | [sdk-handle.md](../../python-bindings/sdk-handle.md) | done | 2026-06-12 | 0593444 |
| T2 | Config surface | [config-surface.md](../../python-bindings/config-surface.md) | done | 2026-06-12 | a5de80c |
| T3 | Hoist pipeline ops + Python add/cognify/add_and_cognify | [core-pipeline-ops.md](../../python-bindings/core-pipeline-ops.md) | done | 2026-06-12 | T3a: 05dfc36, T3b: ff6400f |
| T4 | Hoist + Python search/recall | [retrieval-ops.md](../../python-bindings/retrieval-ops.md) | done | 2026-06-12 | 8cc5c69 |
| T5 | Hoist + Python forget/update/prune | [data-ops.md](../../python-bindings/data-ops.md) | done | 2026-06-12 | 7fa0b25 |
| T6 | Hoist + Python dataset management | [dataset-management.md](../../python-bindings/dataset-management.md) | done | 2026-06-12 | dcecea0 |
| T7 | Hoist + Python remember/memify/improve | [memory-ops.md](../../python-bindings/memory-ops.md) | done | 2026-06-12 | 8887224 |
| T8 | Hoist + Python sessions/admin/notebooks | [session-admin-ops.md](../../python-bindings/session-admin-ops.md) | done | 2026-06-12 | 049cc1d |
| T9 | Python visualization ops | [visualization-ops.md](../../python-bindings/visualization-ops.md) | done | 2026-06-12 | a86688f |
| T10 | Python cloud serve/disconnect | [cloud-ops.md](../../python-bindings/cloud-ops.md) | done | 2026-06-12 | e22a9d9 |
| T11 | Minor engine-tier gaps | [minor-engine-gaps.md](../../python-bindings/minor-engine-gaps.md) | done | 2026-06-12 | cd85a46 |

## Upstream `cognee` SDK parity (ID-1)

The following module-level functions are available via `cognee_pipeline.compat`
(also importable as `cognee_pipeline.compat as cognee` for a drop-in alias):

| Function | Notes |
|---|---|
| `await cognee.add(data, dataset_name="main_dataset")` | `data` may be `str`, `pathlib.Path`, URL `str`, `bytes`/`bytearray`, typed `dict`, or `list` of these |
| `await cognee.cognify(datasets=None)` | `datasets` accepts a dataset name string or `None` (uses `"main_dataset"`) |
| `await cognee.add_and_cognify(data, dataset_name="main_dataset")` | Combined ingest + cognify |
| `await cognee.search(query_text, query_type=SearchType.GRAPH_COMPLETION, top_k=10)` | `query_type` accepts `SearchType` members or plain strings |
| `await cognee.prune.prune_data()` | Deletes all ingested data |
| `await cognee.prune.prune_system(graph, vector, metadata, cache)` | Wipes knowledge-graph / vector / metadata stores |

### Input coercion rules (for `add`)

| Python type | Descriptor produced |
|---|---|
| `str` starting with `http://`, `https://`, `ftp://` | `{"type": "url", "url": "..."}` |
| `str` (other) | `{"type": "text", "text": "..."}` |
| `pathlib.Path` / `os.PathLike` | `{"type": "file", "path": "..."}` |
| `bytes` / `bytearray` | `{"type": "binary", "bytes": ..., "name": "upload.bin"}` |
| `dict` with `"type"` key | forwarded unchanged |
| `list` / `tuple` | each element coerced independently |
| anything else | raises `CogneeValidationError` |

### `SearchType` parity

`SearchType` is now a `(str, Enum)` subclass (matching upstream), so
`SearchType.CHUNKS == "CHUNKS"` is `True` and `str(SearchType.CHUNKS)` returns
`"CHUNKS"`.

The two upstream types below are **not yet implemented** in the Rust core and
are absent from `SearchType` — passing their string values raises
`CogneeValidationError`:

- `AGENTIC_COMPLETION`
- `GRAPH_COMPLETION_DECOMPOSITION`

See `docs/python-bindings/minor-engine-gaps.md` for tracking.

### Optional `cognee` top-level alias

Install with `pip install cognee-pipeline[drop-in]` to make `import cognee`
resolve to the compat shim.  Do **not** use this extra alongside the real
`cognee` package — they collide on the `cognee` import name.

## Log

Free-form notes appended by agents (task splits, deferrals, blockers encountered and how they
were resolved). Newest entries last.

- T3 was split into T3a (hoist+rewire, done) and T3b (Python surface, pending). T3a completed 2026-06-12.
- T3b completed 2026-06-12: add/cognify/add_and_cognify async methods on PyCognee in python/src/sdk_ops.rs, test isolation via relational_db_url key, ConfigManager::set() relational_db_url support, ComponentManager::init_database() SQLite directory creation. All 81 Python tests pass.
- 2026-06-12: T1–T11 all complete (23 commits: T1–T11 implementation commits plus interleaved doc/status commits). The Python bindings now achieve full parity with the C API and TypeScript bindings across both the pipeline-engine tier and the SDK tier (all 40+ SDK operations). Three minor C-API-only features — `ExecStatusManager`, `RayonThreadPool` (explicit), and `DataIdFn` — remain intentionally absent from both Python and TypeScript as they are low-level C-only surfaces. All `scripts/check_all.sh` checks (fmt, cargo check, clippy, capi, python, js) pass.
- 2026-06-14: ID-1 implemented — drop-in upstream cognee SDK parity. Added `cognee_pipeline/compat.py` (module-level add/cognify/add_and_cognify/search/prune with input coercion), converted `SearchType` to `(str, Enum)`, added optional `cognee/` alias package (drop-in extra), added `tests/test_compat_api.py` (26 tests).
