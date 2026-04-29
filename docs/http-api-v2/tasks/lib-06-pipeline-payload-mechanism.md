# LIB-06 — Generic pipeline payload mechanism + library-side CamelCase remember status

| | |
|---|---|
| Wire path | n/a — library + database task |
| Status | **Not Started** |
| Phase | **0 — Pre-port enabler** (TASK 0-2) |
| Depends on | none. |
| Blocks | E-01 (consumes the watcher event hook + the new library `RememberStatus` enum + the `Option<f64>` `elapsed_seconds`), E-02 (consumes `RememberResult.entry_type` / `entry_id` added here, and the same status enum), LIB-01 (consumes `RememberResult.entry_type` / `entry_id`). |
| Effort | ~1.5 days (broader than originally scoped because Q-H now requires a full DB-backed accumulator: trait extension + SeaORM migration + entity + repo impl + 4 tests). |
| Owner crates | `cognee-core` (runtime / watcher event), `cognee-database` (repo trait + SeaORM impl + migration), `cognee-lib` (`RememberStatus` serde flip + `RememberResult` field additions). |

> **Decision (2026-04-29) — Decision 15 — two-layer status convention**:
>
> - **Library layer** (`cognee_lib::api::remember::RememberStatus`): emits CamelCase `"PipelineRunStarted"` / `"PipelineRunCompleted"` / `"PipelineRunErrored"` / `"SessionStored"`. This matches the `cognee_core::PipelineRunStatus` family used by `/cognify`, `/memify`, `/add`, `/improve` so callers of the in-process Rust SDK see one consistent status vocabulary.
> - **HTTP layer** (`crates/http-server/src/dto/remember.rs`, owned by **E-01**): translates the library's CamelCase enum to Python's lowercase wire format `"running"` / `"completed"` / `"errored"` / `"session_stored"` for **strict Python wire parity**. Translation lives at the HTTP DTO boundary, not inside the library. The translation is "temporary" only in the sense that it's a transitional adapter — if Python ever switches its remember status enum to CamelCase, the translation simply goes away.
>
> **No new wire divergence is introduced.** The v2 `/remember` and `/remember/entry` HTTP responses match Python byte-for-byte. The two-layer split is purely a Rust-internal design choice. **Investigation agent: do not re-litigate.**

> **Resolved design questions (2026-04-29)** — answers are baked into §4 below; included here so a fresh-session implementer doesn't re-litigate:
>
> - **Q-A — Scope of "all background pipelines":** Only pipelines that go through `cognee_core::execute()` participate in the new payload mechanism. The `cognify` / `memify` / `add` *convenience* functions used by `cognee_lib::api::remember::remember()` today are **hand-rolled** (they bypass `cognee_core::execute()` — see §3 finding 1). LIB-06 lands the runtime + watcher + DB plumbing for the `execute()`-routed pipelines and adds an explicit TODO in each convenience function pointing to a future task that routes them through `execute()`.
> - **Q-B / Q-G — Mutation channel:** Payload mutations flow through `PipelineWatcher`, **not** through shared mutable state on `TaskContext`. New trait method `on_payload_field(run_id, key, value)`. Tasks call a thin `TaskContext::publish_payload_field(...)` helper that delegates to the attached watcher. This mirrors how lifecycle events (`on_pipeline_run_started/completed/errored`) already flow.
> - **Q-C — `Started` variant:** Include it in `RememberStatus` (4 variants total). Variant is unused by today's synchronous SDK `remember()` but exists for symmetry with `cognee_core::PipelineRunStatus` and for future async/HTTP-background-mode emission.
> - **Q-D — `RememberResult.elapsed_seconds`:** Flip from `f64` to `Option<f64>` now. Defensive against future async paths; matches Python `RememberResult.elapsed_seconds: Optional[float]`.
> - **Q-E — HTTP-side wire-status type (E-01 reference):** E-01 introduces a `WireRememberStatus` enum in `crates/http-server/src/dto/remember.rs` with per-variant `#[serde(rename = "<lowercase>")]` and a `From<cognee_lib::api::remember::RememberStatus>` impl. The DTO field is typed (not `String`) so the translation is type-safe.
> - **Q-F — `entry_type` / `entry_id` on library `RememberResult`:** LIB-06 adds them. LIB-01 (`remember_entry()` facade) just populates them; it no longer needs to extend the struct.
> - **Q-H — Default accumulator:** LIB-06 lands a **DB-backed** default accumulator (new `pipeline_run_payload_fields` table + `PipelineRunRepository` trait extension + `SeaOrmPipelineRunRepository` impl + a registry helper that delegates). See §4 steps 7-12.
> - **Q-I — `PipelineContext.run_id`:** Yes, add `run_id: Option<Uuid>`. The executor sets it when it creates `run_info`. Required so `TaskContext::publish_payload_field(...)` can attribute events.
> - **Q-J — `on_payload_field` signature:** `async fn on_payload_field(&self, run_id: Uuid, key: &str, value: serde_json::Value) {}` — single-field updates with explicit `run_id` parameter, default no-op.

## 1. Goal

Two cohesive pieces of work travel under LIB-06:

**Piece 1 — pipeline runtime extensions and the payload event channel.** Today the Rust pipeline runtime ([`cognee_core::execute()`](../../../crates/core/src/pipeline.rs) at [pipeline.rs:462-578](../../../crates/core/src/pipeline.rs#L462-L578)) emits a fixed set of lifecycle events on a `PipelineRunInfo` snapshot: started, completed, errored. Tasks running inside the pipeline have **no way** to attach run-scoped metadata that flows to observers. Python's solution is the free-form `PipelineRunInfo.payload: Optional[Union[Any, List[Data]]]` field ([`PipelineRunInfo.py:11-13`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRunInfo.py#L11-L13)) which carriers ferry per-data-item info through; `RememberResult._resolve` then reads it back ([`remember.py:425-518`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py#L425-L518)) to populate `items`, `items_processed`, `content_hash`, etc.

LIB-06 ports that pattern — **but as a watcher event channel rather than a shared-state field on the snapshot** (Q-G). Any task running inside `cognee_core::execute()` can call `ctx.publish_payload_field(key, value)`. The runtime forwards to the watcher via `on_payload_field(run_id, key, value)`. The registry-side default watcher persists the field through the new DB-backed accumulator. Consumers later query the registry to retrieve the run's accumulated payload.

While we're touching the runtime, we also add native `started_at` → `completed_at` accounting on `PipelineRunInfo` so callers that need wall-clock duration don't have to re-track timestamps on every site.

**Piece 2 — `cognee_lib::api::remember` library type touch-ups.** Three small but cohesive changes that prepare the library surface for E-01 and E-02:

- Flip `RememberStatus` serde to per-variant CamelCase `PipelineRun*` strings (and add the `Started` variant) so the library status vocabulary matches the rest of the runtime.
- Add `From<cognee_core::PipelineRunStatus>` for ergonomic translation when SDK-side consumers receive a generic pipeline status.
- Make `RememberResult.elapsed_seconds` `Option<f64>` (Python parity) and add `entry_type` / `entry_id` fields (Q-F — LIB-01 will populate them in the typed-entry path; HTTP-side DTO wiring is owned by E-02 alongside the new route).

## 2. Python source-of-truth (mechanism shape AND HTTP wire format)

| Symbol | File | Lines |
|---|---|---|
| `PipelineRunInfo.payload` field | [`cognee/modules/pipelines/models/PipelineRunInfo.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRunInfo.py) | 11–13 |
| `RememberResult._resolve` (reads payload) | [`cognee/api/v1/remember/remember.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py) | 425–518 |
| `RememberResult.elapsed_seconds` accounting (Python uses `Optional[float]`) | [`remember.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py) | 379–380, 521 |
| `RememberResult.to_dict()` HTTP wire shape (lowercase status) | [`remember.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py) | 415–437 |
| `RememberResult.status` lowercase values (`running` / `completed` / `errored` / `session_stored`) | [`remember.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py) | 323–324, 480, 521, 720, 751 |
| `RememberResult.entry_type` / `entry_id` slots | [`remember.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py) | 397–402 |

The Rust HTTP wire format **matches** Python's `to_dict()` byte-for-byte, including the lowercase status values. Only the in-process Rust library API uses CamelCase for internal consistency; the HTTP DTO translates back at the boundary.

## 3. Current Rust state (verified 2026-04-29)

Three findings drive §4's design and are baked into the implementation steps below.

### Finding 1 — `cognify`, `memify`, `add` convenience functions bypass `cognee_core::execute()`

The convenience entry points used by `cognee_lib::api::remember::remember()` today are **hand-rolled sequential task chains**, not `cognee_core::execute()` invocations:

- [`cognee_cognify::cognify()` at `crates/cognify/src/tasks.rs:1718`](../../../crates/cognify/src/tasks.rs#L1718) — its own doc-comment says: *"For composable pipeline-based execution (with concurrency, retry, progress tracking), use [`build_cognify_pipeline`] + [`cognee_core::execute`]."* The convenience function does **not** route through `execute()`.
- [`cognee_ingestion::AddPipeline::add()` at `crates/ingestion/src/pipeline.rs:776`](../../../crates/ingestion/src/pipeline.rs#L776) — likewise: *"use [`build_add_pipeline`] + [`cognee_core::execute`] directly"*.
- [`cognee_cognify::memify::memify()`](../../../crates/cognify/src/memify) — same pattern.

`cognee_lib::api::remember::remember()` calls all three convenience functions, so its current items population at [`crates/lib/src/api/remember.rs:259-275`](../../../crates/lib/src/api/remember.rs#L259-L275) inspects `add_pipeline.add()`'s `Vec<Data>` return directly — bypassing the runtime entirely.

**Implication for LIB-06**: the new payload event channel only fires for pipelines that route through `cognee_core::execute()`. The convenience functions stay as-is **with explicit TODO markers** added in §4 step 14 pointing to a future task that routes them through `execute()`. `cognee_lib::api::remember::remember()` continues to populate `RememberResult.items` via direct inspection of `add_pipeline.add()` output until that future refactor lands.

### Finding 2 — `PipelineRunInfo` has no shared interior mutability

`PipelineRunInfo` ([pipeline.rs:289-307](../../../crates/core/src/pipeline.rs#L289-L307)) is `Debug + Clone`, owned by value inside `execute()` ([pipeline.rs:490-498](../../../crates/core/src/pipeline.rs#L490-L498)), and passed to watchers as `&run_info`. Tasks running inside the pipeline see `TaskContext` ([task_context.rs:38-62](../../../crates/core/src/task_context.rs#L38-L62)) but have no back-reference to the live `run_info`. **There is no path today for a task to mutate run-scoped state.**

`TaskContext.pipeline_ctx: Option<PipelineContext>` ([task_context.rs:22-34](../../../crates/core/src/task_context.rs#L22-L34)) carries pipeline identity but **lacks `run_id`** — `pipeline_id` is the deterministic `uuid5(user+name+dataset)`, not the random per-invocation UUID stored in `PipelineRunInfo.run_id`.

**Implication for LIB-06**: payload mutations flow through the watcher (Q-G) — `TaskContext` does **not** gain shared mutable state. To attribute events, `PipelineContext` gains `run_id: Option<Uuid>` (Q-I) which `execute()` sets when it creates `run_info`. The `TaskContext::publish_payload_field(...)` helper reads the run id from `pipeline_ctx.run_id` and delegates to the attached watcher.

### Finding 3 — Existing `pipeline_runs.run_info` JSON column is unsuitable for incremental updates

The existing `pipeline_runs` table ([m20250101_000001_initial_schema.rs:291](../../../crates/database/src/migrator/m20250101_000001_initial_schema.rs#L291)) has a `run_info: Option<Json>` column. It's already in use:

- Set at run-log time via [`PipelineRunRepository::log_pipeline_run(... run_info: Option<Value>)`](../../../crates/database/src/pipelines/repository.rs#L43-L55).
- Set by the orphan-reset path with a `reason_info` payload at [`sea_orm_impl.rs:301`](../../../crates/database/src/pipelines/sea_orm_impl.rs#L301).

Reusing it for incremental task-emitted payload would require read-modify-write semantics (race-prone) and would conflict with its existing single-shot usage.

**Implication for LIB-06**: payload lives in a **new `pipeline_run_payload_fields` table** with PK `(pipeline_run_id, key)`. Concurrent task updates upsert by primary key — no race. `get_payload(run_id)` reconstructs the map by SELECT. No FK to `pipeline_runs` (matches the loose-coupling style of the existing schema; `pipeline_run_id` is a UUID-as-string).

### Type-level state to be modified

- `cognee_core::PipelineRunInfo` ([pipeline.rs:289-307](../../../crates/core/src/pipeline.rs#L289-L307)): identity + `status` + `started_at`. **No `completed_at`, no `elapsed_seconds()` accessor.**
- `cognee_core::PipelineContext` ([task_context.rs:22-34](../../../crates/core/src/task_context.rs#L22-L34)): `pipeline_id`, `pipeline_name`, `user_id`, `dataset_id`, `current_data`. **No `run_id`.**
- `cognee_core::PipelineWatcher` ([pipeline.rs:386-425](../../../crates/core/src/pipeline.rs#L386-L425)): lifecycle methods only. **No `on_payload_field` event.**
- `cognee_core::TaskContext` ([task_context.rs:38-62](../../../crates/core/src/task_context.rs#L38-L62)): has `pipeline_watcher: Option<Arc<dyn PipelineWatcher>>` already (good — `publish_payload_field` reuses it).
- `cognee_database::PipelineRunRepository` ([repository.rs:40-110](../../../crates/database/src/pipelines/repository.rs)): has `log_pipeline_run`, `latest_status`, `list_recent`, `list_recent_with_attribution`, `reset_orphans`. **No `set_payload_field` / `get_payload`.**
- `cognee_lib::api::remember::RememberStatus` ([remember.rs:32-41](../../../crates/lib/src/api/remember.rs#L32-L41)): `Completed | Errored | SessionStored`, currently `#[serde(rename_all = "snake_case")]` → emits `"completed"` / `"errored"` / `"session_stored"`. **Missing `Started` variant; serde format wrong for Q-C/Q-E.**
- `cognee_lib::api::remember::RememberResult` ([remember.rs:60-78](../../../crates/lib/src/api/remember.rs#L60-L78)): has all the fields except `entry_type` / `entry_id`; `elapsed_seconds: f64` (sentinel meaning) needs to be `Option<f64>`.

## 4. Implementation steps

### Phase 1 — Pipeline runtime (cognee-core)

**Step 1. Add `run_id: Option<Uuid>` to `PipelineContext`.** Field default `None`; the executor sets it when it creates `PipelineRunInfo`.

```rust
// crates/core/src/task_context.rs
pub struct PipelineContext {
    pub pipeline_id: Uuid,
    pub pipeline_name: String,
    pub user_id: Option<Uuid>,
    pub dataset_id: Option<Uuid>,
    pub current_data: Option<Arc<dyn Value>>,
    /// Random per-invocation run id. Set by `cognee_core::execute()` when it
    /// creates `PipelineRunInfo`. Used by tasks (via
    /// `TaskContext::publish_payload_field`) to attribute payload events.
    pub run_id: Option<Uuid>,
}
```

Update test fixtures and `with_progress`/`with_current_data` clone paths to preserve `run_id`.

**Step 2. Set `run_id` in `execute()`.** Inside [pipeline.rs:482-498](../../../crates/core/src/pipeline.rs#L482-L498), after `let run_id = Uuid::new_v4();`, also propagate it into the `pipeline_ctx` of the `TaskContext` clones used by tasks. The cleanest path: `let ctx = ctx.with_run_id(run_id);` (new helper on `TaskContext`).

```rust
impl TaskContext {
    /// Create a new `Arc<TaskContext>` with `run_id` set on the pipeline
    /// context. Returns the original `Arc` unchanged if no `pipeline_ctx`
    /// is present.
    pub fn with_run_id(self: &Arc<Self>, run_id: Uuid) -> Arc<Self> {
        let mut pipeline_ctx = match &self.pipeline_ctx {
            Some(ctx) => ctx.clone(),
            None => return Arc::clone(self),
        };
        pipeline_ctx.run_id = Some(run_id);
        Arc::new(TaskContext {
            // ...all other fields shallow-cloned (mirror with_current_data)
            pipeline_ctx: Some(pipeline_ctx),
            // ...
        })
    }
}
```

**Step 3. Extend `PipelineRunInfo`** with `completed_at` and an `elapsed_seconds()` accessor:

```rust
// crates/core/src/pipeline.rs
#[derive(Debug, Clone)]
pub struct PipelineRunInfo {
    pub run_id: Uuid,
    pub pipeline_id: Uuid,
    pub pipeline_name: String,
    pub user_id: Option<Uuid>,
    pub dataset_id: Option<Uuid>,
    pub status: PipelineRunStatus,
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Set by the runtime when the run reaches `Completed` or `Errored`.
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl PipelineRunInfo {
    /// Wall-clock seconds between `started_at` and `completed_at`. Returns
    /// `None` while the run is still in flight (i.e. `completed_at` is unset).
    pub fn elapsed_seconds(&self) -> Option<f64> {
        let end = self.completed_at?;
        let dur = (end - self.started_at).num_milliseconds();
        Some(dur as f64 / 1000.0)
    }
}
```

Note the **deliberate omission of a `payload` field on `PipelineRunInfo`** (Q-G): payload is communicated via the watcher event channel, not as shared state on the snapshot. Consumers who need the accumulated payload query the registry (see Steps 11-12).

Update every `PipelineRunInfo { ... }` literal in the codebase + tests to default `completed_at: None`. Use `cargo check --all-targets` to find them all.

**Step 4. Wire `completed_at` in `execute()`.** At [pipeline.rs:534, 548, 557](../../../crates/core/src/pipeline.rs) (the three terminal-state transitions), set `run_info.completed_at = Some(chrono::Utc::now());` **before** calling `watcher.on_pipeline_run_completed/errored(...)`. Watchers see the timestamp; consumers reading the snapshot get a populated `elapsed_seconds()`.

**Step 5. Add `PipelineWatcher::on_payload_field(...)` trait method (default no-op).**

```rust
// crates/core/src/pipeline.rs
#[async_trait]
pub trait PipelineWatcher: Send + Sync {
    // ...existing methods...

    /// Tasks emit run-scoped key/value payload via this hook. Default no-op
    /// (matches the rich `on_pipeline_run_*` defaults). The registry-side
    /// `ScopedRunWatcher` (`crates/core/src/pipeline_run_registry/scoped_watcher.rs`)
    /// overrides this to persist the field through `PipelineRunRepository`.
    ///
    /// Mirrors Python's free-form `PipelineRunInfo.payload` field but as an
    /// event stream rather than shared state on the snapshot. Consumers who
    /// need the accumulated payload query the registry's `get_payload(run_id)`
    /// helper (Step 12).
    async fn on_payload_field(
        &self,
        _run_id: Uuid,
        _key: &str,
        _value: serde_json::Value,
    ) {}
}
```

**Step 6. Add `TaskContext::publish_payload_field(...)` helper.** Thin delegator: looks up `pipeline_ctx.run_id`, delegates to `pipeline_watcher.on_payload_field(...)` if a watcher is attached. Silently no-ops when either is missing (matches the existing pattern for the `pipeline_watcher` field — see [task_context.rs:55-62](../../../crates/core/src/task_context.rs#L55-L62)).

```rust
// crates/core/src/task_context.rs
impl TaskContext {
    /// Publish a run-scoped payload field. Tasks running inside
    /// `cognee_core::execute()` call this to attach metadata that downstream
    /// observers read via `PipelineRunRegistry::get_payload(run_id)`.
    ///
    /// Silently no-ops if no `pipeline_watcher` is attached or if the
    /// `pipeline_ctx.run_id` was never set (i.e. the task is not running
    /// inside `execute()`).
    pub async fn publish_payload_field(&self, key: &str, value: serde_json::Value) {
        let Some(w) = self.pipeline_watcher.as_ref() else { return };
        let Some(pctx) = self.pipeline_ctx.as_ref() else { return };
        let Some(run_id) = pctx.run_id else { return };
        w.on_payload_field(run_id, key, value).await;
    }
}
```

### Phase 2 — Database persistence (cognee-database)

**Step 7. Extend `PipelineRunRepository` trait.** Two new methods:

```rust
// crates/database/src/pipelines/repository.rs
#[async_trait]
pub trait PipelineRunRepository: Send + Sync {
    // ...existing methods...

    /// Upsert a single payload field for a run. Concurrent calls with the
    /// same `(run_id, key)` last-write-wins per row; calls with different
    /// keys do not contend.
    async fn set_payload_field(
        &self,
        run_id: Uuid,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), DbError>;

    /// Read all payload fields for a run as a `serde_json::Map`. Returns
    /// an empty map (not `None`) when the run has no payload events;
    /// returns `Err` only on actual DB failures.
    async fn get_payload(
        &self,
        run_id: Uuid,
    ) -> Result<serde_json::Map<String, serde_json::Value>, DbError>;
}
```

**Step 8. New SeaORM migration** `m20260429_000002_pipeline_run_payload_fields.rs` (sequential after the existing `m20260429_000001_sync_operations.rs`):

```rust
// crates/database/src/migrator/m20260429_000002_pipeline_run_payload_fields.rs
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbError> {
        manager
            .create_table(
                Table::create()
                    .table(PipelineRunPayloadFields::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(PipelineRunPayloadFields::PipelineRunId).text().not_null())
                    .col(ColumnDef::new(PipelineRunPayloadFields::Key).text().not_null())
                    .col(ColumnDef::new(PipelineRunPayloadFields::Value).json().not_null())
                    .col(ColumnDef::new(PipelineRunPayloadFields::CreatedAt).timestamp_with_time_zone().not_null())
                    .col(ColumnDef::new(PipelineRunPayloadFields::UpdatedAt).timestamp_with_time_zone().not_null())
                    .primary_key(
                        Index::create()
                            .col(PipelineRunPayloadFields::PipelineRunId)
                            .col(PipelineRunPayloadFields::Key),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_pipeline_run_payload_fields_run_id")
                    .table(PipelineRunPayloadFields::Table)
                    .col(PipelineRunPayloadFields::PipelineRunId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbError> {
        manager
            .drop_table(Table::drop().table(PipelineRunPayloadFields::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum PipelineRunPayloadFields {
    Table,
    PipelineRunId,
    Key,
    Value,
    CreatedAt,
    UpdatedAt,
}
```

Register in `crates/database/src/migrator/mod.rs` after the existing migrations.

**Step 9. New SeaORM entity** `crates/database/src/entities/pipeline_run_payload_field.rs`:

```rust
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "pipeline_run_payload_fields")]
pub struct Model {
    /// Random per-invocation run id (matches `cognee_core::PipelineRunInfo.run_id`)
    /// stored as a string for cross-DB portability.
    #[sea_orm(primary_key, auto_increment = false)]
    pub pipeline_run_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    #[sea_orm(column_type = "Json")]
    pub value: Json,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
```

Re-export from `crates/database/src/entities/mod.rs`.

**Step 10. Implement `set_payload_field` / `get_payload` on `SeaOrmPipelineRunRepository`** ([crates/database/src/pipelines/sea_orm_impl.rs](../../../crates/database/src/pipelines/sea_orm_impl.rs)):

```rust
async fn set_payload_field(
    &self,
    run_id: Uuid,
    key: &str,
    value: serde_json::Value,
) -> Result<(), DbError> {
    use sea_orm::sea_query::OnConflict;

    let now = chrono::Utc::now();
    let model = pipeline_run_payload_field::ActiveModel {
        pipeline_run_id: sea_orm::ActiveValue::Set(run_id.to_string()),
        key: sea_orm::ActiveValue::Set(key.to_owned()),
        value: sea_orm::ActiveValue::Set(value),
        created_at: sea_orm::ActiveValue::Set(now),
        updated_at: sea_orm::ActiveValue::Set(now),
    };

    pipeline_run_payload_field::Entity::insert(model)
        .on_conflict(
            OnConflict::columns([
                pipeline_run_payload_field::Column::PipelineRunId,
                pipeline_run_payload_field::Column::Key,
            ])
            .update_columns([
                pipeline_run_payload_field::Column::Value,
                pipeline_run_payload_field::Column::UpdatedAt,
            ])
            .to_owned(),
        )
        .exec(&*self.conn)
        .await
        .map_err(DbError::from)?;
    Ok(())
}

async fn get_payload(
    &self,
    run_id: Uuid,
) -> Result<serde_json::Map<String, serde_json::Value>, DbError> {
    let rows = pipeline_run_payload_field::Entity::find()
        .filter(pipeline_run_payload_field::Column::PipelineRunId.eq(run_id.to_string()))
        .all(&*self.conn)
        .await
        .map_err(DbError::from)?;

    Ok(rows
        .into_iter()
        .map(|m| (m.key, m.value))
        .collect())
}
```

The `OnConflict` upsert ensures concurrent task emits with the same `(run_id, key)` are last-write-wins per row without losing other keys.

### Phase 3 — Registry watcher integration (cognee-core)

**Step 11. `ScopedRunWatcher` impl `on_payload_field`.** The registry's per-run watcher ([crates/core/src/pipeline_run_registry/scoped_watcher.rs](../../../crates/core/src/pipeline_run_registry/scoped_watcher.rs)) already overrides the lifecycle methods to forward to the repo. Add an override for `on_payload_field` that calls `repo.set_payload_field(run_id, key, value)`:

```rust
// in scoped_watcher.rs
async fn on_payload_field(
    &self,
    run_id: Uuid,
    key: &str,
    value: serde_json::Value,
) {
    if let Err(e) = self.db.set_payload_field(run_id, key, value).await {
        tracing::warn!(%run_id, %key, error=%e, "failed to persist payload field");
    }
}
```

Errors are logged but not propagated — payload persistence is best-effort, mirroring how the existing `on_pipeline_run_started/completed` overrides handle DB errors.

**Step 12. Add `DefaultPipelineRunRegistry::get_payload(run_id)` accessor.** Convenience for consumers:

```rust
// crates/core/src/pipeline_run_registry/default_impl.rs
impl DefaultPipelineRunRegistry {
    /// Fetch the accumulated payload for a run. Empty map if the run has
    /// no payload events; `Err` on DB failure.
    pub async fn get_payload(
        &self,
        run_id: Uuid,
    ) -> Result<serde_json::Map<String, serde_json::Value>, DbError> {
        self.repo.get_payload(run_id).await
    }
}
```

### Phase 4 — `cognee-lib` library type updates

**Step 13. `RememberStatus` serde flip + `Started` variant + `From<PipelineRunStatus>`.**

```rust
// crates/lib/src/api/remember.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RememberStatus {
    /// Pipeline has been initiated/started but has not yet finished.
    /// Currently unused by the synchronous SDK `remember()` (which always
    /// returns a terminal state), but exists for symmetry with
    /// `cognee_core::PipelineRunStatus` and for future async / HTTP
    /// background-mode emission.
    #[serde(rename = "PipelineRunStarted")]
    Started,
    /// Pipeline finished successfully.
    #[serde(rename = "PipelineRunCompleted")]
    Completed,
    /// Pipeline finished with an error.
    #[serde(rename = "PipelineRunErrored")]
    Errored,
    /// Session-only mode: data was stored in the session cache without
    /// running through the cognify pipeline.
    #[serde(rename = "SessionStored")]
    SessionStored,
}

impl From<cognee_core::pipeline::PipelineRunStatus> for RememberStatus {
    fn from(s: cognee_core::pipeline::PipelineRunStatus) -> Self {
        use cognee_core::pipeline::PipelineRunStatus;
        match s {
            PipelineRunStatus::Initiated | PipelineRunStatus::Started => Self::Started,
            PipelineRunStatus::Completed => Self::Completed,
            PipelineRunStatus::Errored => Self::Errored,
        }
    }
}
```

Drop the old `#[serde(rename_all = "snake_case")]`.

The HTTP-side translation (`WireRememberStatus` enum + `From<RememberStatus>`) is owned by **E-01 §4 step 3** — not landed here.

**Step 14. `RememberResult` field updates** ([crates/lib/src/api/remember.rs:60-78](../../../crates/lib/src/api/remember.rs#L60-L78)):

- `pub elapsed_seconds: f64` → `pub elapsed_seconds: Option<f64>` (Q-D).
- Add `pub entry_type: Option<String>` — values `"qa"` / `"trace"` / `"feedback"` (Q-F).
- Add `pub entry_id: Option<String>` — typed-entry id from `SessionManager` (Q-F).

Update `Display` impl to handle `Option<f64>` (skip the elapsed line when `None`; mirror Python's `__repr__` which only includes elapsed when set).

Existing tests in [`crates/lib/tests/remember_tests.rs`](../../../crates/lib/tests/remember_tests.rs) that hard-code an `f64` for `elapsed_seconds` need to be updated to wrap in `Some(...)`.

**Step 15. Convenience-function TODO markers** (resolves Q-A scope): add a doc-comment + `// TODO(LIB-06)` line in each of:

- `crates/cognify/src/tasks.rs::cognify` (around line 1718)
- `crates/cognify/src/memify/mod.rs::memify` (find the public entry)
- `crates/ingestion/src/pipeline.rs::AddPipeline::add` and `::add_with_params`

```rust
// TODO(LIB-06 follow-up): this convenience function bypasses
// cognee_core::execute() and therefore does not emit payload events via
// PipelineWatcher::on_payload_field. Tasks running inside this function
// cannot publish run-scoped payload that downstream consumers (e.g.
// cognee_lib::api::remember::remember()) can read via
// PipelineRunRegistry::get_payload(run_id). To enable that, this
// function would need to route through cognee_core::execute() with a
// pipeline built via build_*_pipeline. Tracked in
// docs/http-api-v2/tasks/lib-06-pipeline-payload-mechanism.md §3
// finding 1.
```

These TODOs are the canonical record that the convenience-function refactor is deferred. No code change beyond the comment.

## 5. Tests

Add tests in this order (each must pass before the next is written, so the implementation agent can keep `cargo check --all-targets` clean throughout).

**5.1 Inline `crates/core/src/pipeline.rs` tests** (extend the existing `mod tests`):

- `pipeline_run_info_elapsed_seconds_returns_none_before_completion` — construct a `PipelineRunInfo` with `completed_at: None`, assert `.elapsed_seconds() == None`.
- `pipeline_run_info_elapsed_seconds_returns_positive_after_completion` — construct with `started_at` 100ms ago and `completed_at: Some(now)`, assert `.elapsed_seconds().unwrap() > 0.0` and `< 1.0`.

**5.2 New `crates/core/tests/pipeline_payload_events.rs`**:

- `tasks_can_publish_payload_field_during_execute` — build a pipeline whose single task calls `ctx.publish_payload_field("key", json!("value"))`, run via `execute()`, assert the registered watcher's `on_payload_field` was called once with the right run_id/key/value.
- `multiple_tasks_can_publish_concurrent_payload_fields` — pipeline with `concurrency = 4`, all tasks emit different keys, assert the registry-side watcher accumulates all of them.
- `publish_payload_field_silently_noops_when_no_watcher` — call helper from a `TaskContext` built without a `pipeline_watcher`, assert no panic.
- `publish_payload_field_silently_noops_when_no_run_id` — call helper from a `TaskContext` whose `pipeline_ctx.run_id` is `None`, assert no panic.

These tests use a `MockPipelineWatcher` that records all payload events.

**5.3 New `crates/database/tests/pipeline_run_payload_fields.rs`**:

- `set_payload_field_inserts_new_row` — call `set_payload_field`, then `get_payload`, assert key present.
- `set_payload_field_upserts_existing_key` — same key set twice with different values; `get_payload` returns the second value; created_at preserved, updated_at advanced.
- `set_payload_field_concurrent_different_keys_succeeds` — spawn 8 tokio tasks each setting a distinct key on the same run_id; `get_payload` returns all 8 fields.
- `get_payload_returns_empty_map_for_unknown_run` — call `get_payload(Uuid::new_v4())` with no prior writes; assert `Ok(Map::new())` (not `Err`).

All four use in-memory SQLite (`sqlite::memory:`) per the project test pattern (see `.claude/CLAUDE.md`).

**5.4 New `crates/core/tests/scoped_watcher_payload_persistence.rs`**:

- `scoped_watcher_persists_payload_via_repo` — wire a `DefaultPipelineRunRegistry` with a real in-memory SQLite repo, run a pipeline whose task emits payload, assert `registry.get_payload(run_id)` returns the emitted fields.
- `scoped_watcher_logs_warning_on_payload_persist_failure` — wire with a `FailingPipelineRunRepository` test mock whose `set_payload_field` always returns `Err`; assert the pipeline still completes successfully (best-effort persistence).

**5.5 Inline `crates/lib/src/api/remember.rs` tests**:

- `remember_status_serializes_to_pipeline_run_camelcase` — `serde_json::to_string(&RememberStatus::Started)` returns `"\"PipelineRunStarted\""`; same for the other three variants.
- `remember_status_deserializes_from_pipeline_run_camelcase` — round-trip the four variants.
- `remember_status_from_pipeline_run_status_translation_table` — exhaustive match over `cognee_core::pipeline::PipelineRunStatus` (4 variants → 3 `RememberStatus` outcomes).
- `remember_result_elapsed_seconds_serializes_as_null_when_none` — `to_dict()` on a result with `elapsed_seconds: None` includes `"elapsed_seconds": null` in the JSON (Python parity).

**5.6 Update `crates/lib/tests/remember_tests.rs`** — fix the hard-coded `elapsed_seconds: f64` literals to be `Some(...)`. The existing `is_success` truth table is unchanged.

**No HTTP-server-level tests in this task** — those land in E-01 (which consumes both the watcher event channel via P5 future wiring and the new `RememberStatus` enum at the DTO boundary).

## 6. Acceptance criteria

- [ ] `PipelineContext` has `run_id: Option<Uuid>`; `TaskContext::with_run_id` exists.
- [ ] `cognee_core::execute()` sets `pipeline_ctx.run_id` on the `TaskContext` clones it builds for tasks.
- [ ] `PipelineRunInfo` has `completed_at: Option<DateTime<Utc>>` and `elapsed_seconds() -> Option<f64>`.
- [ ] `cognee_core::execute()` sets `completed_at` at terminal-state transitions before notifying watchers.
- [ ] `PipelineWatcher::on_payload_field(run_id, key, value)` exists with default no-op.
- [ ] `TaskContext::publish_payload_field(key, value)` exists, delegates to attached watcher, silently no-ops when watcher / run_id is missing.
- [ ] `PipelineRunRepository::set_payload_field` and `::get_payload` exist on the trait.
- [ ] New `pipeline_run_payload_fields` table created via migration `m20260429_000002_pipeline_run_payload_fields.rs`; `up()` and `down()` both reversible.
- [ ] `pipeline_run_payload_field` SeaORM entity exists and is re-exported from `entities/mod.rs`.
- [ ] `SeaOrmPipelineRunRepository` implements both new methods; concurrent upserts on different keys do not contend.
- [ ] `ScopedRunWatcher::on_payload_field` is overridden to call `repo.set_payload_field(...)`; failures are logged not propagated.
- [ ] `DefaultPipelineRunRegistry::get_payload(run_id)` accessor exists.
- [ ] `RememberStatus` has 4 variants and per-variant `#[serde(rename = "PipelineRun*")]` (or `"SessionStored"`) — emits CamelCase strings.
- [ ] `From<cognee_core::pipeline::PipelineRunStatus> for RememberStatus` exhaustive.
- [ ] `RememberResult.elapsed_seconds: Option<f64>`; `Display` updated.
- [ ] `RememberResult.entry_type: Option<String>`, `entry_id: Option<String>` exist.
- [ ] TODO markers added to `cognify` / `memify` / `add` convenience functions referencing this task.
- [ ] All tests in §5 (5.1–5.6) pass.
- [ ] `cargo check --all-targets` clean.
- [ ] `cargo test --workspace` green (debug mode).
- [ ] `scripts/check_all.sh` green (modulo pre-existing JS / OPENAI_TOKEN failures).
- [ ] **No** D-2 entry in README §1.2 (Decision 15 is recorded as a two-layer convention, not a wire divergence).
- [ ] **No** wire-shape change to `/cognify` `/memify` `/add` `/improve` — verified by existing wire-shape tests.

## 7. References

- [README.md §1.1 wire conventions](../README.md#11-wire-conventions-project-wide-set-by-decision-6) and [§1.2 v2 acknowledged divergences](../README.md#12-v2-acknowledged-divergences-changes-to-steady-state-wire-output) — Decision 15 is **not** a wire divergence; the lowercase translation in E-01 keeps the wire byte-correct.
- [IMPLEMENTATION-PROMPT.md §0.5 Decisions log](../IMPLEMENTATION-PROMPT.md#05-decisions-log) — Decision 15 entry.
- Sibling: [E-01 §3.1](e-01-remember.md#31-divergences-from-python-wire-output-investigation-2026-04-29) — the original divergence inventory that motivated this task; [E-01 §4 step 3](e-01-remember.md#4-implementation-steps-revised-by-2026-04-29-investigation) — the HTTP-side `WireRememberStatus` translation.
- Sibling: [E-02 §1](e-02-remember-entry.md) — consumes `entry_type` / `entry_id` added here.
- Sibling: [LIB-01](lib-01-remember-entry-facade.md) — populates `entry_type` / `entry_id` in the typed-entry path.
- Python: [`PipelineRunInfo.py:11-13`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRunInfo.py#L11-L13), [`remember.py:316-518`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py#L316-L518).
- Existing Rust types: [`crates/core/src/pipeline.rs:289-425`](../../../crates/core/src/pipeline.rs), [`crates/core/src/task_context.rs`](../../../crates/core/src/task_context.rs), [`crates/database/src/pipelines/repository.rs`](../../../crates/database/src/pipelines/repository.rs), [`crates/database/src/entities/pipeline_run.rs`](../../../crates/database/src/entities/pipeline_run.rs), [`crates/lib/src/api/remember.rs:32-78`](../../../crates/lib/src/api/remember.rs).
