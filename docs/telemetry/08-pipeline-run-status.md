# 08 — Pipeline Run Status Persistence

## Overview

Cognee tracks every pipeline execution (cognify, memify, ingestion, etc.) as a row in a `pipeline_runs` relational table. Each status transition writes a **new row** (audit-trail / append-only style). The Python SDK uses a four-state lifecycle:

```
INITIATED → STARTED → (COMPLETED | ERRORED)
```

`run_info` is a flexible JSON column carrying lifecycle context (the `data` payload at start, the error message at failure, etc.).

The Rust port already has a `pipeline_runs` table, a `SeaOrmPipelineRunRepository`, a `DefaultPipelineRunRegistry`, and an HTTP endpoint (`/api/v1/activity/pipeline-runs`). The schema and four enum variants exist. The remaining gap is **call-site coverage**: `INITIATED` is defined in code but never written by production pipelines, and a Python parity helper (`reset_pipeline_run_status`) is not exposed for library users. Library-level pipelines (`cognify`, `memify`, ingestion) do **not** call the registry at all today — only the http-server's `dispatch_pipeline` does.

This document maps Python ↔ Rust line-by-line and lists the work needed to reach full lifecycle parity.

---

## Python schema

Source: [`cognee/modules/pipelines/models/PipelineRun.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRun.py)

```python
class PipelineRunStatus(enum.Enum):
    DATASET_PROCESSING_INITIATED = "DATASET_PROCESSING_INITIATED"
    DATASET_PROCESSING_STARTED   = "DATASET_PROCESSING_STARTED"
    DATASET_PROCESSING_COMPLETED = "DATASET_PROCESSING_COMPLETED"
    DATASET_PROCESSING_ERRORED   = "DATASET_PROCESSING_ERRORED"


class PipelineRun(Base):
    __tablename__ = "pipeline_runs"

    id              = Column(UUID, primary_key=True, default=uuid4)
    created_at      = Column(DateTime(timezone=True), default=lambda: datetime.now(timezone.utc))
    status          = Column(Enum(PipelineRunStatus))
    pipeline_run_id = Column(UUID, index=True)
    pipeline_name   = Column(String)
    pipeline_id     = Column(UUID, index=True)
    dataset_id      = Column(UUID, index=True)
    run_info        = Column(JSON)
```

| Column            | Python type                              | Nullable | Default       | Indexed | Notes |
|-------------------|------------------------------------------|----------|---------------|---------|-------|
| `id`              | `UUID`                                   | NO (PK)  | `uuid4()`     | PK      | Per-row PK; one PK per status transition. |
| `created_at`      | `DateTime(timezone=True)`                | YES      | `datetime.now(tz=utc)` | no      | When the row was inserted. |
| `status`          | `Enum(PipelineRunStatus)`                | YES      | none          | no      | Postgres uses native enum; SQLite stores the string value. |
| `pipeline_run_id` | `UUID`                                   | YES      | none          | YES     | Logical run id — `uuid5(NAMESPACE_OID, "{pipeline_id}_{dataset_id}")`. **Reused across re-runs of the same (pipeline, dataset).** |
| `pipeline_name`   | `String`                                 | YES      | none          | no      | Human name (`"cognify_pipeline"`, etc.). |
| `pipeline_id`     | `UUID`                                   | YES      | none          | YES     | `uuid5(NAMESPACE_OID, "{user_id}{name}{dataset_id}")`. |
| `dataset_id`      | `UUID`                                   | YES      | none          | YES     | Dataset being processed. **No FK** in the Python model (pure column). |
| `run_info`        | `JSON`                                   | YES      | none          | no      | Free-form payload; contents differ per status (see lifecycle). |

ID derivation utilities:
- [`generate_pipeline_id`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/utils/generate_pipeline_id.py) — `uuid5(NAMESPACE_OID, f"{user_id}{name}{dataset_id}")`.
- [`generate_pipeline_run_id`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/utils/generate_pipeline_run_id.py) — `uuid5(NAMESPACE_OID, f"{pipeline_id}_{dataset_id}")`.

Postgres migration history note: enum value `DATASET_PROCESSING_INITIATED` was added by Alembic revision `1d0bb7fede17_add_pipeline_run_status` ([file](https://github.com/topoteretes/cognee/blob/main/cognee/alembic/versions/1d0bb7fede17_add_pipeline_run_status.py)). SQLite users have always seen all four values because the column stores the string verbatim.

---

## Python lifecycle

Each helper in `cognee/modules/pipelines/operations/` inserts **one new row** — there are no UPDATEs.

### 1. `INITIATED`
[`log_pipeline_run_initiated.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/log_pipeline_run_initiated.py)

```python
PipelineRun(
    pipeline_run_id=generate_pipeline_run_id(pipeline_id, dataset_id),
    pipeline_name=pipeline_name,
    pipeline_id=pipeline_id,
    status=DATASET_PROCESSING_INITIATED,
    dataset_id=dataset_id,
    run_info={},   # empty dict
)
```

- **Where called:** [`reset_pipeline_run_status.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/methods/reset_pipeline_run_status.py) only. Used to invalidate a previously-completed run so a re-cognify is not skipped by [`check_pipeline_run_qualification.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/layers/check_pipeline_run_qualification.py). Cognee's `prune.py` and the dataset-reset HTTP endpoint call `reset_dataset_pipeline_run_status` which iterates and calls this.
- **`run_info`** = `{}`.

### 2. `STARTED`
[`log_pipeline_run_start.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/log_pipeline_run_start.py)

```python
PipelineRun(
    pipeline_run_id=generate_pipeline_run_id(pipeline_id, dataset_id),
    pipeline_name=pipeline_name, pipeline_id=pipeline_id,
    status=DATASET_PROCESSING_STARTED, dataset_id=dataset_id,
    run_info={"data": data_info},
)
```

- **Where called:** [`run_tasks.py:75`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_tasks.py#L75) at the very start of `run_tasks()` — i.e. once a pipeline actually begins executing.
- **`run_info`**: `{"data": <list-of-data-ids> | str | "None">}` — `data_info` is a list of stringified `Data.id`s when input is `list[Data]`, else `repr(data)` or `"None"`.

### 3. `COMPLETED`
[`log_pipeline_run_complete.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/log_pipeline_run_complete.py)

- **Where called:** [`run_tasks.py:151`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_tasks.py#L151) once `asyncio.gather` returns and no items errored.
- **`run_info`**: `{"data": data_info}` (same shape as STARTED).

### 4. `ERRORED`
[`log_pipeline_run_error.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/log_pipeline_run_error.py)

- **Where called:** `run_tasks.py:171` (top-level exception handler) and `run_tasks_distributed.py:165`.
- **`run_info`**: `{"data": data_info, "error": str(e)}`.

### Sequence

```
                Python
─────────────────────────────────
prune / explicit reset
  → log_pipeline_run_initiated()      [row 1: INITIATED, run_info={}]

run_tasks(...)                        # called by cognee.cognify(), cognee.add(), etc.
  → log_pipeline_run_start()          [row 2: STARTED,  run_info={"data": ...}]
  → ... task execution ...
  → success: log_pipeline_run_complete()  [row 3: COMPLETED, run_info={"data": ...}]
  → failure: log_pipeline_run_error()     [row 3: ERRORED,   run_info={"data": ..., "error": ...}]
```

`pipeline_run_id` is identical for all four rows of a given (pipeline, dataset) pair — the latest row by `created_at` defines the current state.

### `run_info` sanitization

`data_info` is computed by all three runtime helpers:

```python
if not data:                                        data_info = "None"
elif isinstance(data, list) and all(Data instances): data_info = [str(item.id) for item in data]
else:                                               data_info = str(data)
```

i.e. the raw payload is **never** persisted in `run_info` — only the IDs of `Data` objects, or a `repr` fallback.

---

## Python consumers

| File | Lines | Purpose |
|------|-------|---------|
| [`modules/pipelines/operations/get_pipeline_status.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/get_pipeline_status.py) | full file | Latest status per dataset for a pipeline name (window function over `created_at`). |
| [`modules/pipelines/methods/get_pipeline_run.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/methods/get_pipeline_run.py) | full file | Fetch by `pipeline_run_id` (returns most recent matching). |
| [`modules/pipelines/methods/get_pipeline_run_by_dataset.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/methods/get_pipeline_run_by_dataset.py) | full file | Latest run for a dataset+name (used by `check_pipeline_run_qualification`). |
| [`modules/pipelines/methods/get_pipeline_runs_by_dataset.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/methods/get_pipeline_runs_by_dataset.py) | full file | One latest row per `(dataset_id, pipeline_name)` — used by `reset_dataset_pipeline_run_status`. |
| [`modules/pipelines/layers/check_pipeline_run_qualification.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/layers/check_pipeline_run_qualification.py) | 36–58 | Skips the cognify run when `STARTED` (already running) or `COMPLETED` (already done). Keys off `latest_status()`. |
| [`modules/pipelines/layers/reset_dataset_pipeline_run_status.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/layers/reset_dataset_pipeline_run_status.py) | 17–28 | Walks every run for a dataset, skips ones already INITIATED, calls `reset_pipeline_run_status` (writes new INITIATED row). |
| [`api/v1/activity/routers/get_activity_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py) | 21–83 | `GET /pipeline-runs` — joins `PipelineRun ⨝ Dataset ⨝ User`, last 50 rows. |
| `api/v1/activity/routers/get_activity_router.py` | 142–173 | `GET /agents` — counts pipeline runs per dataset over the last 24h (computed but ultimately discarded). |
| [`modules/metrics/operations/get_pipeline_run_metrics.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/metrics/operations/get_pipeline_run_metrics.py) | 32–74 | Reads `pipeline_run.pipeline_run_id` to look up `GraphMetrics` rows. |
| [`alembic/versions/1d0bb7fede17_add_pipeline_run_status.py`](https://github.com/topoteretes/cognee/blob/main/cognee/alembic/versions/1d0bb7fede17_add_pipeline_run_status.py) | 23–29 | Postgres-only ALTER TYPE adding `DATASET_PROCESSING_INITIATED`. |

---

## Rust current state

### Schema (matches Python at the column level)

[`crates/database/src/migrator/m20250101_000001_initial_schema.rs:291-353`](../../crates/database/src/migrator/m20250101_000001_initial_schema.rs)

Creates the `pipeline_runs` table with columns `id`, `created_at`, `status`, `pipeline_run_id`, `pipeline_name`, `pipeline_id`, `dataset_id`, `run_info`. Indexes are created on `pipeline_run_id`, `pipeline_id`, and `dataset_id` (matches Python). UUIDs are stored as 32-char hex strings (text columns) — Python uses native `UUID` types in Postgres, but stringified in SQLite.

Note the **Rust-only divergence**: a foreign key `dataset_id REFERENCES datasets(id) ON DELETE CASCADE` is declared, and `dataset_id` is `NOT NULL`. Python has no FK and `dataset_id` is nullable.

A second migration [`m20260501_000002_pipeline_run_payload_fields.rs`](../../crates/database/src/migrator/m20260501_000002_pipeline_run_payload_fields.rs) creates a sidecar `pipeline_run_payload_fields` table with composite PK `(pipeline_run_id, key)` for concurrent payload upserts. **No Python equivalent** — this is a Rust-only addition for LIB-06.

### Entity

[`crates/database/src/entities/pipeline_run.rs`](../../crates/database/src/entities/pipeline_run.rs)

```rust
pub enum PipelineRunStatus {
    Initiated  → "DATASET_PROCESSING_INITIATED"
    Started    → "DATASET_PROCESSING_STARTED"
    Completed  → "DATASET_PROCESSING_COMPLETED"
    Errored    → "DATASET_PROCESSING_ERRORED"
}

pub struct Model {
    pub id: String,                       // UUID hex
    pub created_at: DateTimeUtc,
    pub status: PipelineRunStatus,
    pub pipeline_run_id: String,          // indexed
    pub pipeline_name: String,
    pub pipeline_id: String,              // indexed
    pub dataset_id: String,               // indexed, FK to datasets.id (NOT NULL)
    pub run_info: Option<Json>,
}
```

All four status variants are present.

### Domain type

[`crates/database/src/types.rs:50-68`](../../crates/database/src/types.rs)

```rust
pub enum PipelineRunStatus { Initiated, Started, Completed, Errored }

pub struct PipelineRun {
    pub id: Uuid, pub created_at: DateTime<Utc>, pub status: PipelineRunStatus,
    pub pipeline_run_id: Uuid, pub pipeline_name: String, pub pipeline_id: Uuid,
    pub dataset_id: Uuid,           // <-- NOT Option<Uuid>
    pub run_info: Option<serde_json::Value>,
}
```

### Repository trait

[`crates/database/src/pipelines/repository.rs:39-126`](../../crates/database/src/pipelines/repository.rs)

```rust
async fn log_pipeline_run(
    &self,
    pipeline_run_id: Uuid, pipeline_id: Uuid, pipeline_name: &str,
    dataset_id: Option<Uuid>,                       // ← Optional at the API surface
    status: PipelineRunStatus,
    run_info: Option<serde_json::Value>,
) -> Result<Uuid, DbError>;
```

Plus `latest_status`, `list_recent`, `list_recent_with_attribution`, `reset_orphans`, `set_payload_field`, `get_payload`.

The `SeaOrmPipelineRunRepository` impl ([`pipelines/sea_orm_impl.rs:54-58`](../../crates/database/src/pipelines/sea_orm_impl.rs)) has a quirk: when `dataset_id` is `None`, it **does not write** the row at all — it returns the generated id without persistence (because the table FK requires a valid dataset). This silently drops ad-hoc runs without a dataset.

### Registry

[`crates/core/src/pipeline_run_registry/`](../../crates/core/src/pipeline_run_registry/)

- `RunPhase` enum: `Pending | Running | Completed | Errored { message }` — note that `Pending` corresponds to `INITIATED` in Python but is **never persisted as INITIATED** today.
- `DefaultPipelineRunRegistry::run_work_inline` ([`default_impl.rs:271-347`](../../crates/core/src/pipeline_run_registry/default_impl.rs#L271)) writes `STARTED` → terminal (`COMPLETED` / `ERRORED`). It **never writes `INITIATED`**.
- `register_background` ([`default_impl.rs:365-533`](../../crates/core/src/pipeline_run_registry/default_impl.rs#L365)) — same: `STARTED` then terminal.
- `ScopedRunWatcher` ([`scoped_watcher.rs:84-217`](../../crates/core/src/pipeline_run_registry/scoped_watcher.rs)) maps `cognee_core::PipelineRunStatus::{Initiated, Started, Completed, Errored}` to the DB enum, but `on_pipeline_run_started` always writes `core_to_db_status(&run.status)` and the `run_info` it receives at that point in `pipeline.rs` is `Started`, never `Initiated` (see [`pipeline.rs:520`](../../crates/core/src/pipeline.rs#L520)).

### Pipeline executor

[`crates/core/src/pipeline.rs`](../../crates/core/src/pipeline.rs)

- `PipelineRunStatus` enum at lines 325-330 has all four variants.
- `execute()` (line 486 onwards) emits **only**:
  - `PipelineRunStatus::Started` at line 520 + `watcher.on_pipeline_run_started` at line 532;
  - `PipelineRunStatus::Completed` at line 563 + `on_pipeline_run_completed` at 574;
  - `PipelineRunStatus::Errored` at lines 578 / 588 + `on_pipeline_run_errored` at 584 / 604.
- `Initiated` is **defined but never produced by the executor.**

### HTTP endpoint

[`crates/http-server/src/routers/activity.rs:42-99`](../../crates/http-server/src/routers/activity.rs)

`GET /api/v1/activity/pipeline-runs`. Calls `list_recent_with_attribution(filter.dataset_id, 50)` and serializes via `status_to_str` which emits the four `DATASET_PROCESSING_*` strings. Wire shape matches Python ([`get_activity_router.py:70-83`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L70)).

### Production callers of the registry

Only the http-server's `dispatch_pipeline` ([`crates/http-server/src/pipelines/dispatch.rs:85-120`](../../crates/http-server/src/pipelines/dispatch.rs)) uses `register_inline` / `register_background`. Library-level pipelines (cognify, memify, ingestion, search) and the CLI do not call the registry; they call `cognee_core::pipeline::execute` directly with a `NoopWatcher` (or none). **Result:** `pipeline_runs` rows are written only for HTTP-driven runs.

---

## Side-by-side comparison

### `pipeline_runs` table

| Column            | Python                          | Rust (entity)                         | Rust (domain `PipelineRun`)        | Status |
|-------------------|---------------------------------|----------------------------------------|------------------------------------|--------|
| `id`              | `UUID PRIMARY KEY` (default `uuid4`) | `String` PK (32-char UUID hex)    | `Uuid`                              | match (representation differs but value-equal) |
| `created_at`      | `DateTime(timezone=True)`, default `now(utc)` | `DateTimeUtc`, NOT NULL (no DB-level default) | `DateTime<Utc>`         | match (Rust supplies value at insert; Python via column default) |
| `status`          | `Enum(PipelineRunStatus)` nullable | `PipelineRunStatus` (text, NOT NULL) | `PipelineRunStatus`               | minor: Rust `NOT NULL`, Python nullable. No real-world impact (always set on write). |
| `pipeline_run_id` | `UUID`, indexed, nullable       | `String`, indexed, NOT NULL           | `Uuid`                              | minor: Rust `NOT NULL`. |
| `pipeline_name`   | `String`, nullable              | `String`, NOT NULL                    | `String`                            | minor: Rust `NOT NULL`. |
| `pipeline_id`     | `UUID`, indexed, nullable       | `String`, indexed, NOT NULL           | `Uuid`                              | minor: Rust `NOT NULL`. |
| `dataset_id`      | `UUID`, indexed, nullable, **no FK** | `String`, indexed, NOT NULL, **FK → datasets ON DELETE CASCADE** | `Uuid` (not `Option`) | **divergent**: Rust adds FK + NOT NULL. Affects ad-hoc runs (no dataset) and cross-SDK reads. |
| `run_info`        | `JSON` nullable                 | `Json` nullable                        | `Option<serde_json::Value>`        | match |
| _(indexes)_       | `pipeline_run_id`, `pipeline_id`, `dataset_id` | same three indexes | — | match |

### `PipelineRunStatus` enum

| Variant                       | Python string                      | Rust string (`#[sea_orm(string_value)]`) | Status |
|-------------------------------|------------------------------------|------------------------------------------|--------|
| `Initiated`                   | `"DATASET_PROCESSING_INITIATED"`   | `"DATASET_PROCESSING_INITIATED"`         | match |
| `Started`                     | `"DATASET_PROCESSING_STARTED"`     | `"DATASET_PROCESSING_STARTED"`           | match |
| `Completed`                   | `"DATASET_PROCESSING_COMPLETED"`   | `"DATASET_PROCESSING_COMPLETED"`         | match |
| `Errored`                     | `"DATASET_PROCESSING_ERRORED"`     | `"DATASET_PROCESSING_ERRORED"`           | match |

### Operations

| Python op                          | Rust counterpart                                      | Status |
|------------------------------------|-------------------------------------------------------|--------|
| `log_pipeline_run_initiated`       | _none_ (only the `Initiated` enum variant exists; `repo.log_pipeline_run(... PipelineRunStatus::Initiated, ...)` would work but no caller does so) | **missing** |
| `log_pipeline_run_start`           | implicit in `DefaultPipelineRunRegistry::run_work_inline` and `ScopedRunWatcher::on_pipeline_run_started` | match (functionally) |
| `log_pipeline_run_complete`        | implicit in registry / `ScopedRunWatcher::on_pipeline_run_completed` | match |
| `log_pipeline_run_error`           | implicit in registry / `ScopedRunWatcher::on_pipeline_run_errored` | match |
| `reset_pipeline_run_status`        | _none_ (no public helper; users must call `repo.log_pipeline_run(..., Initiated, ...)` directly) | **missing** |
| `reset_dataset_pipeline_run_status`| _none_ (no caller; `prune` / dataset-reset paths in Rust don't write `INITIATED` rows) | **missing** |
| `get_pipeline_status`              | `PipelineRunRepository::latest_status`                | match |
| `get_pipeline_run`                 | _no equivalent in trait_ (could be `list_recent` filtered) | **missing** (low priority) |
| `get_pipeline_run_by_dataset`      | _no equivalent in trait_                              | **missing** (low priority) |
| `get_pipeline_runs_by_dataset`     | partially: `latest_status` returns map; full row form not exposed | **missing** (low priority) |
| `check_pipeline_run_qualification` | _none_ (Rust pipelines do not skip already-completed runs) | **missing** (functional gap, distinct from persistence) |

### `run_info` JSON shape

| State        | Python                                  | Rust (today)                              | Status |
|--------------|-----------------------------------------|-------------------------------------------|--------|
| `INITIATED`  | `{}`                                    | _not written by production code_          | gap |
| `STARTED`    | `{"data": <ids \| str \| "None">}`      | `None` (no run_info on the Started row)   | **divergent** |
| `COMPLETED`  | `{"data": <ids \| str \| "None">}`      | `None`                                    | **divergent** |
| `ERRORED`    | `{"data": ..., "error": str(e)}`        | `{"error": <message>}` (no `data` key)    | **partial divergent** |

---

## Detailed gap analysis

1. **No `INITIATED` row is ever written by Rust production code.** The variant is wired through every layer (executor → watcher → repo → DB), but `pipeline.rs::execute` jumps straight to `Started`. Python writes `INITIATED` from `reset_pipeline_run_status` to invalidate completed runs. Rust has no equivalent of this re-cognify gating.
2. **No `reset_pipeline_run_status` / `reset_dataset_pipeline_run_status` helper.** Users cannot mark a previously-completed dataset for reprocessing through a public API. (`reset_orphans` is similar but writes `ERRORED`, not `INITIATED`, and is server-restart-only.)
3. **`run_info` content drift.** All three runtime statuses (`STARTED`, `COMPLETED`, `ERRORED`) write a different shape than Python. Rust `STARTED` and `COMPLETED` write `None`; Rust `ERRORED` writes `{"error": ...}` and omits the `"data"` key. Python always writes `{"data": data_info}`, optionally with an `"error"` key.
4. **`dataset_id` schema divergence.** Rust adds `NOT NULL` and a `FK ON DELETE CASCADE`; Python is nullable and FK-less. Cross-SDK rows that Python writes with `dataset_id IS NULL` (theoretically possible) cannot be inserted into the Rust DB. Conversely the Rust repo silently drops rows when the caller passes `dataset_id = None` ([`sea_orm_impl.rs:54-58`](../../crates/database/src/pipelines/sea_orm_impl.rs#L54)) — a subtle data-loss path.
5. **Library-level pipelines (cognify, memify, ingestion) do not write any pipeline_run rows.** They call `cognee_core::pipeline::execute` with a `NoopWatcher`. Only http-server-driven runs leave a trail. CLI users see an empty `/api/v1/activity/pipeline-runs` even after running pipelines.
6. **No `check_pipeline_run_qualification` equivalent.** Python's [`run_pipeline.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_pipeline.py) consults the latest status before kicking off `run_tasks` and **skips** if `STARTED` or `COMPLETED`. Rust has no such gate, so a re-cognify on an already-completed dataset always re-runs.
7. **Missing reader helpers in `PipelineRunRepository`.** Python exposes `get_pipeline_run`, `get_pipeline_run_by_dataset`, `get_pipeline_runs_by_dataset` — useful for resume / bookkeeping. Rust currently exposes only `latest_status` (map) and `list_recent*`.
8. **No automatic `INITIATED` row on dataset reset / prune.** Python's prune chain calls `reset_dataset_pipeline_run_status`; Rust's prune does not.

---

## Proposed design

### A. Persist `INITIATED` and full lifecycle from the executor (or watcher)

Two equivalent options — pick one:

**Option 1 — Emit Initiated from the executor.**
- In [`pipeline.rs::execute`](../../crates/core/src/pipeline.rs) (line 514), build a `PipelineRunInfo` with `status = Initiated`, call a new `watcher.on_pipeline_run_initiated(&run_info)` immediately, then transition to `Started` once the first task is about to run (after the task subtoken setup, before `execute_items_*`).

**Option 2 — Keep INITIATED at the registry boundary.**
- Have `DefaultPipelineRunRegistry::register_inline` / `register_background` write the `INITIATED` row at the moment of registration (before spawning work), and `STARTED` once `run_work_inline` actually begins. This is closer to Python: `register_*` ≈ "queue created", first work tick ≈ `STARTED`.

Option 2 is more Python-faithful: Python's `INITIATED` always denotes "queued / about-to-run / reset-to-pending".

### B. Add a `reset_pipeline_run_status` library helper

```rust
// crates/lib/src/api/pipeline_runs.rs (new file)
pub async fn reset_pipeline_run_status(
    repo: Arc<dyn PipelineRunRepository>,
    user_id: Uuid,
    dataset_id: Uuid,
    pipeline_name: &str,
) -> Result<(), DatabaseError> {
    let pipeline_id = uuid5_pipeline_id(user_id, dataset_id, pipeline_name);
    let pipeline_run_id = uuid5_pipeline_run_id(pipeline_id, dataset_id);
    repo.log_pipeline_run(
        pipeline_run_id, pipeline_id, pipeline_name,
        Some(dataset_id), PipelineRunStatus::Initiated,
        Some(serde_json::Map::new().into()), // == {}
    ).await.map(drop)
}
```

Plus a `reset_dataset_pipeline_run_status` analogue iterating all pipeline names per dataset.

### C. Align `run_info` content

In `ScopedRunWatcher` and in `DefaultPipelineRunRegistry::run_work_inline` / `register_background`:

- On `STARTED`: write `Some(json!({"data": data_info(...)}))` instead of `None`.
- On `COMPLETED`: write `Some(json!({"data": data_info(...)}))` instead of `None`.
- On `ERRORED`: write `Some(json!({"data": data_info(...), "error": message}))` instead of just `{"error": ...}`.

`data_info` requires propagating the input list of `Data` ids into the run. Extend `RunSpec` (or `PipelineRunInfo`) with `data_ids: Vec<Uuid>`, populate from the dispatch site, and serialize as `[String]` for parity. When unset / empty, write `"None"` to match Python's `if not data` branch.

### D. Library-side wiring (write `pipeline_runs` rows from CLI runs)

Add an opt-in pipeline registry handle to the cognify/memify/ingestion library APIs so callers (CLI, examples) can persist runs. Concretely: the top-level `cognify`/`memify` functions in `crates/cognify/src/` accept an optional `Arc<dyn PipelineRunRepository>`, and when present, log the four-state lifecycle around the inner call to `pipeline::execute`. Or expose `register_inline` from the library facade.

> **Telemetry tie-in (carryover from gap 03-04).** When this section lands, the
> same wiring should pass the per-call `Settings::telemetry_snapshot()` through
> `Pipeline::with_telemetry_settings(...)` before invoking `execute()`. Today
> the `Pipeline.telemetry_settings` carrier exists and `execute()` emits
> `Pipeline Run Started`/`Completed`/`Errored` analytics events, but production
> SDK paths bypass `execute()` so the snapshot is never populated and those
> events never fire for cognify/memify/ingestion runs. See
> [03/04-pipeline-lifecycle-events.md](03/04-pipeline-lifecycle-events.md)
> for the carrier API and [03-pipeline-task-api-events.md §Closure summary →
> Known follow-ups](03-pipeline-task-api-events.md#known-follow-ups).

### E. Make `dataset_id` nullable in the schema (Python parity)

A new migration drops the FK, allows `NULL`, and updates the entity:

```rust
// m20260901_000003_pipeline_run_dataset_nullable.rs
manager.alter_table(
    Table::alter()
        .table(PipelineRuns::Table)
        .drop_foreign_key(Alias::new("fk_pipeline_runs_dataset_id"))
        .modify_column(ColumnDef::new(PipelineRuns::DatasetId).text().null())
        .to_owned()
).await
```

This unblocks ad-hoc runs and brings cross-SDK schema agreement.

Adjust `domain::PipelineRun.dataset_id` from `Uuid` to `Option<Uuid>` and the SeaORM model from `String` to `Option<String>`. The `log_pipeline_run` signature already takes `Option<Uuid>` — remove the early-return that drops rows.

### F. Add `check_pipeline_run_qualification` (separate task — functional, not just persistence)

Implement the latest-status check in the cognify pipeline entry point so re-cognify of a `COMPLETED` dataset short-circuits and re-cognify of a `STARTED` dataset is rejected with a "already in flight" outcome. Out of scope for this telemetry doc but listed for visibility.

---

## Migration concerns

1. **Adding `INITIATED` rows in production paths is non-breaking** for both SQLite and Postgres — the enum value is already present in the Rust `DeriveActiveEnum`, and Python's Postgres enum was patched by Alembic revision `1d0bb7fede17` years ago. SQLite stores it as a plain string, no schema change needed.
2. **Making `dataset_id` nullable / dropping the FK** requires a versioned migration. `Migrator::migrations` ([`crates/database/src/migrator/mod.rs`](../../crates/database/src/migrator/mod.rs)) must register a new `m20260901_000003_*`. SQLite cannot drop FKs in place — the migration must rebuild the table (`CREATE TABLE pipeline_runs_new ... INSERT SELECT ... DROP TABLE ... RENAME`). Postgres can use `ALTER TABLE ... DROP CONSTRAINT ... ALTER COLUMN ... DROP NOT NULL`. Both must be handled.
3. **Existing rows are unaffected** by the dataset_id change — they all have non-null values.
4. **`run_info` shape change is a wire-shape break for any consumer that reads the JSON.** Today the http-server endpoint does not surface `run_info` in its response (only `status`, `pipeline_name`, etc.), so this is internal-only. Document the shape.

---

## Cross-SDK compatibility

A Python writer + Rust reader (and vice versa) must read the table identically.

**Today's status — what works:**
- Both write `id` as a 32-char hex UUID string in SQLite (Python via SQLAlchemy's `UUID`, Rust via `uuid_hex::to_hex`). Postgres native UUID round-trips identically.
- Status enum values are byte-identical strings.
- Indexes are byte-identical.
- All three `*_id` columns store hex-string UUIDs in SQLite; Postgres native UUIDs round-trip.

**Today's status — what is broken:**
- Rust requires `dataset_id NOT NULL` and a FK; Python allows null and has no FK. **Rust cannot ingest a Python-written row where `dataset_id IS NULL`.** Python *can* read a Rust-written row, since Python's column has no NOT NULL.
- `run_info` JSON shape differs (see §C of design above). Both sides round-trip JSON without schema validation, so reads succeed — but downstream code that expects `{"data": ...}` (Python's metric module) will see `null` from Rust-written rows.

**After the proposed design:** schemas are byte-equivalent and all four status rows are written identically. Cross-SDK reads + writes round-trip with full fidelity.

The cross-SDK harness ([`e2e-cross-sdk/`](../../e2e-cross-sdk)) currently asserts `add` + `cognify` parity but does **not** check `pipeline_runs`. A new test under `e2e-cross-sdk/test_pipeline_runs_parity.py` should:
1. Run Python `cognify`, then Rust `list_recent_with_attribution` reads the same rows.
2. Run Rust `cognify` (HTTP server route), then Python `get_pipeline_status` reads `DATASET_PROCESSING_COMPLETED`.
3. Assert `run_info` keys match: `{"data": [...]}` on STARTED/COMPLETED, `{"data": [...], "error": "..."}` on ERRORED.

---

## Design decisions (locked)

Approved by the project owner on 2026-05-12. **Do not re-litigate.**
Sub-agents may surface new evidence that contradicts a decision; if so,
escalate to the user before changing course.

| # | Decision | Rationale | Affected tasks |
|---|---|---|---|
| 1 | **INITIATED is emitted by `pipeline::execute` (executor-level, Option A).** Before the first task runs, the executor builds a `PipelineRunInfo` with `status = Initiated`, calls `watcher.on_pipeline_run_initiated(&run_info)`, then transitions to `Started` after the task subtoken setup and immediately before the first `execute_items_*` call. The watcher is responsible for persisting the row; `NoopWatcher` ignores the event. | Pushes the row write down to the executor so every non-HTTP caller (CLI cognify, memify, ingestion, embedded library users) automatically gets the four-state trail without needing to register through the http-server's registry. Registry-level placement (Option B) would have required action item 7 to land first before any CLI run produced an INITIATED row. | [08-04](08/04-initiated-from-executor.md) |
| 2 | **Library pipelines (cognify/memify/ingestion) always run through a registry; CLI defaults to a real repository.** The library entry points (`cognify::pipeline::cognify`, `memify::pipeline::memify`, `ingestion::pipeline::AddPipeline::run`) require an `Arc<dyn PipelineRunRepository>` (not `Option`); when callers do not supply one the library wraps a default `Arc<NoopPipelineRunRepository>` (a new no-op impl in `cognee-database`). The CLI constructs the real SeaORM repo from the existing `DatabaseConnection` and threads it through `crates/cli/src/commands/cognify.rs`, `memify.rs`, `add.rs`, `add_and_cognify.rs`, `run_sequence.rs`. This makes `pipeline_runs` writes the default — opt-out is to pass the no-op explicitly. | Closer to Python where every `cognee.cognify()` writes rows. Avoids the surprise of "the CLI ran but the activity endpoint shows nothing." `NoopPipelineRunRepository` keeps embedded users in raw-library mode from needing a database. | [08-07](08/07-library-pipeline-wiring.md) |
| 3 | **`check_pipeline_run_qualification` is implemented as part of this gap.** The cognify entry point reads the latest `pipeline_runs` row for `(pipeline_name, dataset_id)`; if `Completed`, it short-circuits and emits an `AlreadyCompleted` `RunEvent` without re-running tasks; if `Started`, it returns a `PipelineAlreadyRunning` error. Memify is in scope; ingestion is excluded because Python's ingestion path does not consult this gate. | Functional gap but cheaply unlocked once the four-state trail is in place. Without it, re-running cognify on a completed dataset always re-runs — wasted LLM calls and inconsistent graph state. | [08-08](08/08-check-qualification.md) |
| 4 | **`dataset_id` becomes nullable; FK to `datasets(id)` is dropped.** New migration `m20260901_000003_pipeline_run_dataset_nullable.rs` rebuilds the SQLite table (no in-place DROP FK), Postgres uses `ALTER TABLE … DROP CONSTRAINT … ALTER COLUMN … DROP NOT NULL`. `crates/database/src/entities/pipeline_run.rs::Model::dataset_id` becomes `Option<String>`, `crates/database/src/types.rs::PipelineRun::dataset_id` becomes `Option<Uuid>`, the silent-drop branch at `crates/database/src/pipelines/sea_orm_impl.rs:54-58` is removed, and every downstream consumer (`list_recent_with_attribution` projection, http-server activity router, conversions) is updated to handle the optional. | Schema parity with Python (which is nullable + FK-less) and unblocks the silent-drop data-loss bug for ad-hoc runs. Cross-SDK reads round-trip with full fidelity. | [08-01](08/01-dataset-id-nullable-migration.md) |
| 5 | **`run_info` JSON shape matches Python byte-for-byte.** All four states write `Some(json!({"data": data_info, …}))` instead of the current `None`/`{"error": …}`. `data_info` is computed via a new helper in `crates/core/src/pipeline_run_registry/data_info.rs` that mirrors Python: `[String]` for `Vec<Data>` inputs (each item is `data.id.to_string()`), the literal string `"None"` for empty inputs, and `format!("{:?}", input)` for `repr`-fallback inputs. The `RunSpec` and `PipelineRunInfo` types gain a `data_ids: Vec<Uuid>` field (carrier) that the dispatch site populates. | Cross-SDK parity — Python's metric module reads `run_info["data"]` and would otherwise see `null` from Rust-written rows. Decision 6 below settles the `[Uuid]` vs `[String]` question. | [08-02](08/02-data-info-helper.md), [08-03](08/03-run-info-shape-alignment.md) |
| 6 | **`run_info["data"]` items are JSON strings, not bare UUIDs.** When the input is a `Vec<Data>`, the helper emits `Value::Array(ids.into_iter().map(|id| Value::String(id.to_string())).collect())`. Python serialises `[str(item.id) for item in data]` — both produce `["<hex>", "<hex>", …]` on the wire, but using `String` in Rust avoids ambiguity with `serde_json`'s `Uuid` representation (no implicit hyphen-stripping). | Byte-identical wire format. Decision recorded so the helper's signature is unambiguous. | [08-02](08/02-data-info-helper.md) |
| 7 | **Reader helpers ship with this gap.** `PipelineRunRepository` gains three new methods matching Python's API: `get_pipeline_run(pipeline_run_id) -> Option<PipelineRun>`, `get_pipeline_run_by_dataset(dataset_id, pipeline_name) -> Option<PipelineRun>` (latest by `created_at`), `get_pipeline_runs_by_dataset(dataset_id) -> Vec<PipelineRun>` (one latest row per pipeline name). The existing `latest_status` map is preserved unchanged for back-compat. | Python parity. `get_pipeline_run_by_dataset` is the building block decision 3's qualification check uses. | [08-06](08/06-reader-helpers.md) |
| 8 | **Cross-SDK parity test runs under the existing `e2e-cross-sdk/` Docker harness.** A new `test_pipeline_runs_parity.py` runs Python `cognify`, opens the shared SQLite DB from the Rust side via `cognee-cli internal pipeline-runs list` (a new CLI subcommand) or by direct `sqlx` query in a tiny test-only Rust helper. Asserts schema (`PRAGMA table_info`), cross-write/read on a four-state lifecycle, and byte-identical `run_info` JSON. | The gap's parity claims are only meaningful if they're enforced under the same harness as `test_add_parity.py`. | [08-09](08/09-tests.md) |
| 9 | **No new payload shape — Rust-only `pipeline_run_payload_fields` sidecar stays Rust-only.** The LIB-06 sidecar table (`m20260501_000002_pipeline_run_payload_fields.rs`) has no Python counterpart. Cross-SDK reads do **not** project payload-field rows back into `run_info`. The sidecar remains an enrichment layer the Python SDK does not see. | Open question 3 in the original analysis. Projecting would require Python schema changes and a v2 wire shape; deferred. | (none — explicit non-goal) |
| 10 | **Synchronous writes throughout.** Every `repo.log_pipeline_run(...)` call site awaits the DB write before returning. No fire-and-forget. Matches Python's synchronous SQLAlchemy session pattern. | Open question 4. Simpler error surface (DB failure becomes a `PipelineRunError`); avoids the "row appears eventually" race during tests. | (cross-cutting) |
| 11 | **`SeaOrmPipelineRunRepository` is the single point of truth for the SeaORM persistence path.** `DefaultPipelineRunRegistry`'s `run_work_inline` / `register_background` continue to write through `ScopedRunWatcher`, which now wraps a `PipelineRunRepository`. Library pipelines (decision 2) construct their own `DbPipelineWatcher` (new type introduced in task 08-07 at `crates/core/src/pipeline_run_registry/db_watcher.rs`) holding an `Arc<dyn PipelineRunRepository>` and pass it as `watcher` to `pipeline::execute`. Task 08-07 also refactors the library convenience functions `cognify::cognify`, `memify::memify`, and `AddPipeline::add` onto `cognee_core::pipeline::execute` (the existing LIB-06 follow-up) so the watcher actually sees those runs. Both paths converge on the same trait. | No two parallel writers; new INITIATED/run_info logic lands once in the watcher impl(s) rather than duplicated. | [08-03](08/03-run-info-shape-alignment.md), [08-07](08/07-library-pipeline-wiring.md) |
| 12 | **`pipeline_run_id` reuse semantics preserved.** The same `(pipeline_id, dataset_id)` pair always derives the same `pipeline_run_id`; multiple rows share it, the latest by `created_at` defines current state. Reader helpers (decision 7) sort by `created_at DESC` and take the first row. `RunSpec.run_id: Option<Uuid>` continues to be honoured by `register_inline`/`register_background` (caller-provided id wins; absent id auto-generates a UUIDv4 which only applies to ad-hoc runs without a dataset). | Open question 5; Python's behaviour is unchanged. | (cross-cutting) |
| 13 | **No new `RunEventKind` variant for INITIATED.** The in-memory `RunEvent` channel keeps its four kinds (`Started/Yield/Completed/Errored/AlreadyCompleted`). The four-state DB trail is independent of the in-memory event stream; subscribers don't need a new variant. The watcher's `on_pipeline_run_initiated` triggers the DB write but does **not** broadcast a `RunEvent`. | Avoids broadcasting "we are about to start" events to HTTP subscribers who only care about "is it done yet". `RunHandle::subscribe()` semantics are unchanged. | [08-04](08/04-initiated-from-executor.md) |

---

## Action items

Each item below has a dedicated implementation sub-document under
[`08/`](08/) with rationale, prerequisites, step-by-step source-level
changes, verification commands, files modified, and risks. **The
sub-docs are authoritative**: where they refine details based on the
locked design decisions, follow the sub-doc rather than this
high-level summary.

| #  | Action item | Sub-doc | Depends on | Status |
|----|---|---|---|---|
| 01 | Make `dataset_id` nullable: new migration `m20260901_000003_pipeline_run_dataset_nullable.rs` (SQLite table-rebuild, Postgres ALTER), update entity (`Option<String>`), domain `PipelineRun` (`Option<Uuid>`), remove silent-drop early-return in `sea_orm_impl.rs:54-58`, update every downstream consumer. | [08/01-dataset-id-nullable-migration.md](08/01-dataset-id-nullable-migration.md) | — | ✅ 526c892 |
| 02 | Add `data_info(input)` helper at `crates/core/src/pipeline_run_registry/data_info.rs` matching Python's logic; extend `RunSpec` and `PipelineRunInfo` with `data_ids: Vec<Uuid>`; populate from dispatch sites. No watcher behaviour changes yet — pure plumbing addition. | [08/02-data-info-helper.md](08/02-data-info-helper.md) | 01 | ✅ f05c04e |
| 03 | Align `run_info` JSON shape: `ScopedRunWatcher` and the existing `DbPipelineWatcher` (or equivalent) write `Some(json!({"data": data_info}))` on `Started`/`Completed` and `Some(json!({"data": data_info, "error": msg}))` on `Errored`. Wires through the carrier from task 02. | [08/03-run-info-shape-alignment.md](08/03-run-info-shape-alignment.md) | 02 | ✅ edd47d5 |
| 04 | Emit `INITIATED` from `pipeline::execute`: add `on_pipeline_run_initiated` watcher method (default no-op), call it before the first task runs, write the row with `run_info = {}`. Implementations in `ScopedRunWatcher` + library `DbPipelineWatcher` persist the row. | [08/04-initiated-from-executor.md](08/04-initiated-from-executor.md) | 03 | ✅ 29a99f8 |
| 05 | Add `reset_pipeline_run_status` + `reset_dataset_pipeline_run_status` library helpers in `crates/lib/src/api/pipeline_runs.rs`. Plumb into prune / dataset-reset paths in `crates/delete/src/` and `crates/cli/src/commands/delete.rs`. | [08/05-reset-helpers.md](08/05-reset-helpers.md) | 04 | ✅ ee4a4b2 |
| 06 | Add `get_pipeline_run`, `get_pipeline_run_by_dataset`, `get_pipeline_runs_by_dataset` reader methods on `PipelineRunRepository`; implement on `SeaOrmPipelineRunRepository`. | [08/06-reader-helpers.md](08/06-reader-helpers.md) | 01 | ✅ 78c73c7 |
| 07 | Wire `Arc<dyn PipelineRunRepository>` through `cognify`, `memify`, ingestion (`AddPipeline`) entry points. Add `NoopPipelineRunRepository` to `cognee-database` as the default. Update CLI subcommands (`cognify`, `memify`, `add`, `add_and_cognify`, `run_sequence`) to construct the real repo from the SQLite connection. | [08/07-library-pipeline-wiring.md](08/07-library-pipeline-wiring.md) | 04 | ✅ f64fcac |
| 08 | Implement `check_pipeline_run_qualification` at the cognify entry point (and memify): read latest status via `get_pipeline_run_by_dataset`; short-circuit on `Completed`, reject on `Started`. Emit an `AlreadyCompleted` `RunEvent` for the short-circuit path. | [08/08-check-qualification.md](08/08-check-qualification.md) | 06, 07 | ✅ 506f0d1 |
| 09 | Tests: extend `crates/database/tests/pipeline_run_repository.rs` (four-state round-trip, dataset_id=None, exact `run_info` shape, reset helper); new `crates/core/tests/pipeline_run_lifecycle.rs` (executor emits four rows); new `crates/http-server/tests/activity_pipeline_runs.rs`; new `e2e-cross-sdk/test_pipeline_runs_parity.py`. | [08/09-tests.md](08/09-tests.md) | 02–08 | ✅ 08c5140 |
| 10 | Docs + CI: update `docs/telemetry/gap-analysis.md` row for §7 (if applicable) to point at gap 08 closure; document the four-state lifecycle in `docs/http-server/pipelines.md`; write the "Closure summary" section at the bottom of this doc. | [08/10-docs-and-ci.md](08/10-docs-and-ci.md) | 01–09 | ✅ a9db0c4 |

---

## Open questions

1. Should `INITIATED` be written at the http-server `dispatch_pipeline` boundary (current registry behaviour proposal) or pushed all the way down into `cognee_core::pipeline::execute`? Pushing it down means non-HTTP callers automatically get the four-state trail; keeping it at the registry means cleaner separation but requires every non-HTTP entry point to register manually.
2. `run_info["data"]` semantics — Python serializes a `list[Data]` as a list of `str(item.id)` strings. Should Rust serialize as `[Uuid]` (would be JSON strings anyway) or `[String]` to be byte-equivalent? **Recommendation: `[String]` matching Python.**
3. The Rust-only sidecar table `pipeline_run_payload_fields` (LIB-06) has no Python counterpart. Python's `run_info` JSON column carries everything. Cross-SDK parity needs a decision: do we project the Rust payload-field rows into `run_info` on read, or leave them as a Rust-only enrichment? Currently they are a Rust-only enrichment.
4. Should `register_*` write `INITIATED` synchronously (blocking the caller on a DB write before returning) or fire-and-forget? Python is synchronous; doing the same in Rust simplifies parity.
5. Python's `pipeline_run_id` is reused across re-runs — multiple rows share it, the latest by `created_at` wins. Rust's [`dispatch.rs:46-49`](../../crates/http-server/src/pipelines/dispatch.rs#L46) already does this. Confirm that `register_inline` / `register_background` accept caller-provided `run_id` (yes, via `RunSpec.run_id: Option<Uuid>`) and that the registry maps the run-id collision correctly when a placeholder for the same id existed (handled by `create_slot`).

---

## Testing strategy

### Unit tests (per crate)

1. `crates/database/tests/pipeline_run_repository.rs` — extend the existing tests to assert that **all four** statuses round-trip through `log_pipeline_run` and `latest_status`. Existing tests cover `Initiated`, `Started`, `Completed`; add an `Errored` round-trip.
2. New test: `dataset_id = None` round-trips after the nullability migration.
3. New test: `run_info` JSON shape — assert exact keys / values for each status.
4. New test: `reset_pipeline_run_status` (library helper) writes a fresh `INITIATED` row with `run_info = {}`.

### Integration tests

5. New test under `crates/core/tests/pipeline_run_lifecycle.rs`: `register_inline` writes four rows (`INITIATED → STARTED → COMPLETED`), check by querying the SQLite memory DB.
6. Same for the failure path: `INITIATED → STARTED → ERRORED`, with `run_info["error"]` populated.
7. Verify ordering: `created_at` strictly increases between transitions.

### HTTP integration test

8. `crates/http-server/tests/activity_pipeline_runs.rs` (new) — exercise `GET /api/v1/activity/pipeline-runs` after a real cognify dispatch and assert the response array contains four items with the right status strings.

### Cross-SDK

9. New `e2e-cross-sdk/test_pipeline_runs_parity.py`:
   - **Schema** assertion: open both Python and Rust SQLite DBs, `PRAGMA table_info(pipeline_runs)` matches column-for-column (after the nullability migration).
   - **Cross-write/read**: Python writes a four-state lifecycle, Rust's `list_recent_with_attribution` returns four matching rows.
   - **`run_info` parity**: assert byte-identical `run_info` JSON at every state.
   - **`pipeline_run_id` derivation**: Python `generate_pipeline_run_id(pid, dsid)` == Rust `pipeline_run_id(pid, dsid)`.

### Manual verification

10. Run a CLI cognify, query `pipeline_runs`, expect four rows (after action item 6 lands).

---

## References

- Python:
  - [`PipelineRun.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRun.py)
  - [`log_pipeline_run_initiated.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/log_pipeline_run_initiated.py)
  - [`log_pipeline_run_start.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/log_pipeline_run_start.py)
  - [`log_pipeline_run_complete.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/log_pipeline_run_complete.py)
  - [`log_pipeline_run_error.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/log_pipeline_run_error.py)
  - [`reset_pipeline_run_status.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/methods/reset_pipeline_run_status.py)
  - [`run_tasks.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/operations/run_tasks.py)
  - [`check_pipeline_run_qualification.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/layers/check_pipeline_run_qualification.py)
  - [`get_activity_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py)
  - [`generate_pipeline_run_id.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/utils/generate_pipeline_run_id.py)
  - [`alembic 1d0bb7fede17`](https://github.com/topoteretes/cognee/blob/main/cognee/alembic/versions/1d0bb7fede17_add_pipeline_run_status.py)
- Rust:
  - [`crates/database/src/migrator/m20250101_000001_initial_schema.rs`](../../crates/database/src/migrator/m20250101_000001_initial_schema.rs)
  - [`crates/database/src/migrator/m20260501_000002_pipeline_run_payload_fields.rs`](../../crates/database/src/migrator/m20260501_000002_pipeline_run_payload_fields.rs)
  - [`crates/database/src/entities/pipeline_run.rs`](../../crates/database/src/entities/pipeline_run.rs)
  - [`crates/database/src/types.rs`](../../crates/database/src/types.rs)
  - [`crates/database/src/pipelines/repository.rs`](../../crates/database/src/pipelines/repository.rs)
  - [`crates/database/src/pipelines/sea_orm_impl.rs`](../../crates/database/src/pipelines/sea_orm_impl.rs)
  - [`crates/database/src/conversions.rs`](../../crates/database/src/conversions.rs)
  - [`crates/core/src/pipeline.rs`](../../crates/core/src/pipeline.rs)
  - [`crates/core/src/pipeline_run_registry/default_impl.rs`](../../crates/core/src/pipeline_run_registry/default_impl.rs)
  - [`crates/core/src/pipeline_run_registry/scoped_watcher.rs`](../../crates/core/src/pipeline_run_registry/scoped_watcher.rs)
  - [`crates/core/src/pipeline_run_registry/types.rs`](../../crates/core/src/pipeline_run_registry/types.rs)
  - [`crates/http-server/src/routers/activity.rs`](../../crates/http-server/src/routers/activity.rs)
  - [`crates/http-server/src/pipelines/dispatch.rs`](../../crates/http-server/src/pipelines/dispatch.rs)
- Existing telemetry docs:
  - [`gap-analysis.md`](./gap-analysis.md)
  - [`03-pipeline-task-api-events.md`](./03-pipeline-task-api-events.md)
  - [`07-bindings-auto-init.md`](./07-bindings-auto-init.md)
- Adjacent:
  - [`docs/http-server/pipelines.md`](../http-server/pipelines.md)
  - [`docs/http-api-v2/tasks/lib-06-pipeline-payload-mechanism.md`](../http-api-v2/tasks/lib-06-pipeline-payload-mechanism.md)

---

## Closure summary

Gap 08 closed in 22 commits (plus one pre-flight fix and one mid-stream
decision-wording correction). The table below lists every commit in
landing order — each sub-task lands as a pair (implementation
commit + sub-doc status flip), following the gap-06 / gap-07
convention. The final task ships docs, the four-state lifecycle
documentation, and this closure summary in a single commit.

| # | Commit | Subject | Task |
|---|---|---|---|
| pre-flight | `4f045f2` | database/tests: fix permissions_repository seed + fmt drift on main | (pre-flight to unblock the runbook; not a gap-08 task) |
| 08-00 | `9cd0d67` | telemetry/pipeline-runs-08: land plan docs and mark 08-01 complete | 08-00 (plan-doc commit; bundled with 08-01 doc-flip) |
| 08-01 | `526c892` | telemetry/pipeline-runs-08-01: make pipeline_run.dataset_id nullable | 08-01 |
| 08-02 | `f05c04e` | telemetry/pipeline-runs-08-02: add data_info helper + RunSpec/PipelineRunInfo data_ids plumbing | 08-02 |
| 08-02 | `60826a4` | telemetry/pipeline-runs-08-02: docs — mark action item 02 complete | 08-02 |
| 08-03 | `edd47d5` | telemetry/pipeline-runs-08-03: align run_info JSON shape with Python | 08-03 |
| 08-03 | `528f1ce` | telemetry/pipeline-runs-08-03: docs — mark action item 03 complete | 08-03 |
| 08-04 | `29a99f8` | telemetry/pipeline-runs-08-04: emit INITIATED from pipeline::execute | 08-04 |
| 08-04 | `63e3bf1` | telemetry/pipeline-runs-08-04: docs — mark action item 04 complete | 08-04 |
| 08-05 | `ee4a4b2` | telemetry/pipeline-runs-08-05: reset helpers + delete/CLI plumbing | 08-05 |
| 08-05 | `1a9ab9b` | telemetry/pipeline-runs-08-05: docs — mark action item 05 complete | 08-05 |
| 08-06 | `78c73c7` | telemetry/pipeline-runs-08-06: add three reader methods to PipelineRunRepository | 08-06 |
| 08-06 | `205bc8a` | telemetry/pipeline-runs-08-06: docs — mark action item 06 complete | 08-06 |
| decision-fix | `56f020e` | telemetry/pipeline-runs-08: correct Decision 11 wording on DbPipelineWatcher | (in-stream decision-text correction once LIB-06 design landed) |
| 08-07 | `f64fcac` | telemetry/pipeline-runs-08-07: wire DbPipelineWatcher through library convenience entry points | 08-07 |
| 08-07 | `ade75cd` | telemetry/pipeline-runs-08-07: docs — mark action item 07 complete | 08-07 |
| 08-08 | `506f0d1` | telemetry/pipeline-runs-08-08: add check_pipeline_run_qualification to cognify + memify | 08-08 |
| 08-08 | `5f23fb0` | telemetry/pipeline-runs-08-08: docs — mark action item 08 complete | 08-08 |
| 08-09 | `08c5140` | telemetry/pipeline-runs-08-09: add four-state lifecycle test coverage | 08-09 |
| 08-09 | `0f9e488` | telemetry/pipeline-runs-08-09: docs — mark action item 09 complete | 08-09 |
| 08-10 | `a9db0c4` | telemetry/pipeline-runs-08-10: closure summary + docs + CI | 08-10 |
| 08-10 | _(this doc-flip commit)_ | telemetry/pipeline-runs-08-10: docs — mark action item 10 complete | 08-10 |

### Sibling gap opened mid-stream

Task 08-07 ("wire `Arc<dyn PipelineRunRepository>` through the library
convenience entry points") discovered that `cognify::cognify`,
`cognify::memify::memify`, and `ingestion::AddPipeline::add` ran their
tasks **inline** rather than through `cognee_core::pipeline::execute`,
so a `DbPipelineWatcher` plumbed in at the library API surface would
never receive watcher events. Closing this required its own design and
implementation pass landed as **LIB-06** (parent doc:
[`lib-06-executor-routed-convenience.md`](lib-06-executor-routed-convenience.md);
closed on commit `b5ccc96`, summary-SHA fill-in on `da04bd6`). LIB-06
routed the three convenience functions through `pipeline::execute`,
giving 08-07 the watcher path it needed. The
[gap-analysis.md "Completed work" bullet](gap-analysis.md#completed-work)
records the closure.

### What gap 08 delivered

- **Schema parity (08-01).** `pipeline_runs.dataset_id` is now nullable
  and the FK to `datasets(id)` is gone. SQLite migration rebuilds the
  table (no in-place DROP FK); Postgres uses
  `ALTER TABLE … DROP CONSTRAINT … DROP NOT NULL`. The silent-drop
  branch in `SeaOrmPipelineRunRepository` (formerly at
  `sea_orm_impl.rs:54-58`) that would discard ad-hoc runs with
  `dataset_id = None` is removed. Domain `PipelineRun.dataset_id`
  becomes `Option<Uuid>` and every downstream consumer (HTTP
  activity router, `list_recent_with_attribution`, conversions)
  threads the optional through.
- **`data_info` helper (08-02).** New
  `cognee_core::pipeline_run_registry::data_info::data_info(input)`
  mirrors Python byte-for-byte: `[String]` for `Vec<Data>` inputs
  (each item `data.id.to_string()`), the literal string `"None"`
  for empty inputs, and `format!("{:?}", input)` for repr-fallback
  inputs. `RunSpec` and `PipelineRunInfo` gain `data_ids: Vec<Uuid>`
  carriers populated by the dispatch site.
- **`run_info` shape alignment (08-03).** `ScopedRunWatcher` and the
  library-side `DbPipelineWatcher` now write
  `Some(json!({"data": data_info}))` on `Started` / `Completed` and
  `Some(json!({"data": data_info, "error": msg}))` on `Errored`
  rather than `None` / `{"error": …}` — wire-identical to Python.
  `INITIATED` rows carry `run_info = {}`.
- **`INITIATED` from the executor (08-04, Decision 1, Option A).**
  `cognee_core::pipeline::execute` emits `Initiated` before the first
  task runs via a new `PipelineWatcher::on_pipeline_run_initiated`
  hook (default no-op). The watcher persists the row; non-HTTP
  callers (CLI cognify/memify/ingestion, embedded library users)
  automatically get the four-state trail without registering through
  the HTTP server's registry.
- **Reset helpers (08-05).** `cognee_lib::api::pipeline_runs::{
  reset_pipeline_run_status, reset_dataset_pipeline_run_status}`
  match Python's API. The `pipeline_id` / `pipeline_run_id`
  derivation helpers were promoted from `http-server/dispatch.rs`
  into `cognee_core::pipeline_run_registry::ids` so the library and
  delete crates can use them without dragging in the HTTP server.
  Wired into `crates/delete/src/` and the CLI delete subcommand.
- **Reader helpers (08-06).** `PipelineRunRepository` gains
  `get_pipeline_run(pipeline_run_id) -> Option<PipelineRun>`,
  `get_pipeline_run_by_dataset(dataset_id, pipeline_name) ->
  Option<PipelineRun>` (latest by `created_at`), and
  `get_pipeline_runs_by_dataset(dataset_id) -> Vec<PipelineRun>`
  (one latest row per pipeline name). Existing `latest_status`
  preserved unchanged.
- **Library-pipeline wiring (08-07).** `cognify::cognify`,
  `cognify::memify::memify`, and `ingestion::AddPipeline::add`
  accept `Arc<dyn PipelineRunRepository>` (default
  `NoopPipelineRunRepository` for embedded users with no DB).
  A new `DbPipelineWatcher` in
  `crates/core/src/pipeline_run_registry/db_watcher.rs` wraps the
  repository and is passed as the `watcher` to `pipeline::execute`.
  CLI subcommands (`cognify`, `memify`, `add`, `add_and_cognify`,
  `run_sequence`) construct the real SeaORM repo. This is the
  payoff of the **LIB-06 sibling gap** — without LIB-06's executor
  routing, the watcher would never have seen library runs.
- **Qualification gate (08-08).** `cognify(...)` and `memify(...)`
  consult `check_pipeline_run_qualification` before running tasks.
  `Completed` short-circuits and returns `CognifyResult {
  already_completed: true, .. }` without re-running; `Started`
  rejects with `CognifyError::PipelineAlreadyRunning` /
  `MemifyError::PipelineAlreadyRunning`. Ingestion is intentionally
  excluded (Decision 3) because Python doesn't gate it. Re-cognify
  is unlocked by calling `reset_pipeline_run_status` first.
- **Test coverage (08-09).** Four-state round-trips in
  `crates/database/tests/pipeline_run_repository.rs`, executor-level
  lifecycle in `crates/core/tests/pipeline_run_lifecycle.rs`, HTTP
  surface in `crates/http-server/tests/activity_pipeline_runs.rs`,
  reset-helper assertions in `crates/lib/tests/pipeline_runs_reset.rs`
  (kept in `cognee-lib` to avoid a `cognee-database → cognee-lib`
  cycle), CLI E2E in `crates/cli/tests/cli_pipeline_runs.rs`, and a
  cross-SDK parity test scaffold in
  `e2e-cross-sdk/harness/test_pipeline_runs_parity.py`.

### Notable design pivots

- **Option A in-body stamping in cognify tasks (LIB-06-03).** When
  routing the standard cognify branch through `pipeline::execute`,
  LIB-06 task 03 found that the executor's outer-loop status writes
  could not pick up per-task DataPoint provenance unless the tasks
  themselves stamped fields **inside** their bodies. Option A
  (in-body stamping) was load-bearing for gap-05 provenance parity
  and is recorded in
  [`lib-06-executor-routed-convenience.md`](lib-06-executor-routed-convenience.md).
- **Option (a) temporal `source_pipeline` shift.** The temporal
  cognify branch (LIB-06-04) keeps the same `pipeline_name`
  (`"cognify_pipeline"`) for the `pipeline_runs` row but stamps
  `source_pipeline = "cognify_temporal_pipeline"` on the produced
  DataPoints. The library-API-facing dispatch name and the
  provenance-stamp name are intentionally distinct (see next
  bullet).
- **Dispatch-name vs stamp-name distinction (Decision 11 refinement,
  commit `56f020e`).** Decision 11's original wording conflated the
  watcher dispatch name with the in-DataPoint `source_pipeline`
  stamp. The refinement clarifies that the watcher records the
  dispatch-time pipeline name (one of `"cognify_pipeline"`,
  `"memify_pipeline"`, `"add_pipeline"`) while individual tasks
  stamp their own logical pipeline name. Both paths converge on the
  same `PipelineRunRepository` trait, but the row's `pipeline_name`
  column and the DataPoint's `source_pipeline` field are not
  required to match.

### What's now possible

- **Four-state pipeline_run trail across every surface.** Library
  callers, HTTP-server routes, and CLI subcommands all write the
  same `INITIATED → STARTED → (COMPLETED | ERRORED)` audit trail to
  `pipeline_runs`. `GET /api/v1/activity/pipeline-runs` returns
  full coverage instead of only HTTP-driven runs.
- **`check_pipeline_run_qualification` gates cognify/memify against
  re-runs.** A second `cognify(...)` on an already-completed dataset
  short-circuits (returns `already_completed: true`) instead of
  wasting LLM calls; concurrent calls on a running dataset reject
  cleanly with `PipelineAlreadyRunning`.
- **Reset helpers in `cognee-lib` for prune flows.** Users can call
  `reset_pipeline_run_status(repo, user, dataset, "cognify_pipeline")`
  to invalidate a completed run so the next `cognify(...)` proceeds
  normally. `reset_dataset_pipeline_run_status` does the same for
  every pipeline name attached to a dataset. Plumbed into the
  delete / prune cascade so dataset-reset implicitly invalidates the
  cognify trail.
- **Cross-SDK schema agreement.** Python `cognee` and Rust
  `cognee-rust` now share a byte-identical `pipeline_runs` schema
  and `run_info` JSON shape; both SDKs can read each other's rows
  without translation.

### Known limitations / out of scope

- **Cross-SDK parity test needs the Docker harness to actually run.**
  `e2e-cross-sdk/harness/test_pipeline_runs_parity.py` lands as part
  of 08-09 but is only exercised by `cd e2e-cross-sdk && docker
  compose up --build`. CI runs the harness on push to main; local
  runs are opt-in.
- **HTTP cognify/memify routes are P5 stubs.** The
  `publish_already_completed` helper (08-08) is wire-ready in
  `crates/http-server/src/pipelines/dispatch.rs` but the cognify and
  memify HTTP routes themselves remain P5 stubs — the short-circuit
  path is therefore not yet exercised on the HTTP surface. When
  those routes ship, no further work is needed.
- **Ingestion has no qualification gate (Decision 3).** Python does
  not gate ingestion against re-runs and neither does Rust. A
  re-`add` on the same dataset always proceeds; deduplication
  happens at the content-hash level inside the ingestion pipeline,
  not via `check_pipeline_run_qualification`.
- **`pipeline_run_payload_fields` sidecar stays Rust-only
  (Decision 9).** LIB-06's payload-fields table has no Python
  counterpart and is not projected into `run_info` on cross-SDK
  reads.
- **Shutdown / orphan-recovery semantics unchanged from earlier
  gaps.** Gap 08 does not introduce new graceful-shutdown behaviour;
  the existing `reset_orphans` path documented in
  [`docs/http-server/pipelines.md`](../http-server/pipelines.md)
  §12 still owns server-restart cleanup.
- **`CognifyResult.already_completed` not yet surfaced through
  bindings.** PyO3 / Neon serialise `CognifyResult` and the new
  `already_completed: bool` field appears with `#[serde(default)]`
  so older binding clients ignore it. Add an explicit return-type
  variant when a binding consumer needs to act on the short-circuit.
