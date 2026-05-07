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

## Action items

1. **Decide INITIATED placement** — registry-level (Option B, recommended) or executor-level (Option A). Update [`crates/core/src/pipeline_run_registry/default_impl.rs`](../../crates/core/src/pipeline_run_registry/default_impl.rs) `register_inline` (~L356) and `register_background` (~L365) to write `INITIATED` before spawning work, and let the inner `run_work_inline` keep writing `STARTED` once it begins.
2. **Add `reset_pipeline_run_status` library helper** under `crates/lib/src/api/` (or a new `crates/core/src/pipeline_run_registry/reset.rs`). Expose via `cognee_lib`. Match Python's signature: `(user_id: Uuid, dataset_id: Uuid, pipeline_name: &str)`.
3. **Add `reset_dataset_pipeline_run_status`** that iterates per-pipeline-name; integrate into the prune / dataset-reset CLI subcommands and the http-server reset endpoint (search for callers of [`crates/delete/src/`](../../crates/delete/src/) deletion paths).
4. **Align `run_info` JSON shape** — add `data_ids: Vec<Uuid>` (or similar) to `RunSpec` and the watcher events; update [`scoped_watcher.rs`](../../crates/core/src/pipeline_run_registry/scoped_watcher.rs) `on_pipeline_run_started` / `_completed` / `_errored` to write `{"data": [...]}` (and `{"data": [...], "error": "..."}` for errored).
5. **Make `dataset_id` nullable** — new migration `m20260901_000003_pipeline_run_dataset_nullable.rs` under [`crates/database/src/migrator/`](../../crates/database/src/migrator/), drop FK, update [`crates/database/src/entities/pipeline_run.rs`](../../crates/database/src/entities/pipeline_run.rs) (`Option<String>`), update [`crates/database/src/types.rs`](../../crates/database/src/types.rs) (`Option<Uuid>`), remove the early-return in [`sea_orm_impl.rs:54-58`](../../crates/database/src/pipelines/sea_orm_impl.rs).
6. **Wire the registry into library pipelines** so non-HTTP runs (CLI, examples, embedded uses) also persist rows. Add an optional `Arc<dyn PipelineRunRepository>` parameter (or a `PipelineWatcher` that wraps it) to [`crates/cognify/src/pipeline.rs`](../../crates/cognify/src/) and analogous entry points. Plumb through `crates/cli/src/`.
7. **Add `get_pipeline_run`, `get_pipeline_run_by_dataset`, `get_pipeline_runs_by_dataset` reader methods** to `PipelineRunRepository` for full Python parity.
8. **Add cross-SDK pipeline-runs parity test** under `e2e-cross-sdk/test_pipeline_runs_parity.py`.
9. **(Out of scope for this doc, but blocked on the same plumbing)** — implement `check_pipeline_run_qualification` in the cognify entry point.

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
