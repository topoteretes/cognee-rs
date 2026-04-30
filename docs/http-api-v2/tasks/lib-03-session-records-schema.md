# LIB-03 — `cognee-database` `session_records` + `session_model_usage` schema and entities

| | |
|---|---|
| Scope | SeaORM entities + migration only. The repository trait + impl + tests live in **LIB-05**. |
| Status | **Done** (commit 82728f2) |
| Blocks | LIB-05 (repo + impl), and transitively E-09 / E-10 / E-11 / E-12 (the entire `/sessions` router). |
| Depends on | none. |
| Effort | ~0.75 day. |
| Owner crate | `cognee-database` |

> **Decision (2026-04-29) — Decision 13**: this task is one half of the original LIB-03 scope. The split (per Decision 13, option (b)) puts the schema/entities/migration here as a single commit, and the repository trait + impl + 7 tests in **LIB-05** as a separate single commit. Rationale: smaller review surface per commit; if the migration needs revisiting (column type wrong) it can be amended without touching the trait/impl. Investigation agent: do not re-litigate.

## 1. Goal

Land the relational schema and SeaORM entities Python uses to persist session lifecycle data. Two SQLAlchemy tables — `session_records` and `session_model_usage` — populated by `LLMGateway`/`SessionManager` hooks on the Python side. We need byte-for-byte equivalent SeaORM entities and a migration that creates the tables and indexes.

This task **does NOT** introduce the repository trait or any read methods. Those live in [LIB-05](lib-05-session-records-repo.md). Between LIB-03 and LIB-05, the entity types exist but no consumer uses them — `cargo check` clean but the tables are temporarily "dangling" in the migrator.

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `SessionRecord` (table `session_records`) | `cognee/modules/session_lifecycle/models.py` | 10–86 |
| `SessionModelUsage` (table `session_model_usage`) | `cognee/modules/session_lifecycle/models.py` | 89–126 |

### `session_records` columns

| Column | Type | Nullable | Default | Index |
|---|---|---|---|---|
| `session_id` | `STRING` | NO | — | PK |
| `user_id` | `UUID` | NO | — | PK + index |
| `dataset_id` | `UUID` | YES | — | index |
| `status` | `STRING` | NO | `"running"` | index |
| `started_at` | `TIMESTAMPTZ` | NO | `now()` | — |
| `last_activity_at` | `TIMESTAMPTZ` | NO | `now()` | index |
| `ended_at` | `TIMESTAMPTZ` | YES | — | — |
| `tokens_in` | `INT` | NO | 0 | — |
| `tokens_out` | `INT` | NO | 0 | — |
| `cost_usd` | `FLOAT` | NO | 0.0 | — |
| `error_count` | `INT` | NO | 0 | — |
| `last_model` | `TEXT` | YES | — | — |

### `session_model_usage` columns

PK `(session_id, user_id, model)`. Columns: `tokens_in`, `tokens_out`, `cost_usd`, `updated_at` (`onupdate=now()`).

## 3. Current Rust state

- `crates/database/src/migrator/` has migrations for users, datasets, ACLs, etc. — **no** `session_records` or `session_model_usage` table. The newest migration is `m20260501_000002_pipeline_run_payload_fields.rs` (LIB-06, commit b39cd05); the new LIB-03 migration must be sequenced **after** it in `crates/database/src/migrator/mod.rs::Migrator::migrations()`.
- `crates/database/src/entities/mod.rs` lists 27 entities (data, dataset, dataset_data, query, result_log, edge, graph_metrics, node, pipeline_run, pipeline_run_payload_field, task_run, acl, permission, principal, role, tenant, user, user_api_key, user_role, user_tenant, graph_sync_checkpoint, principal_configuration, role_default_permission, tenant_default_permission, user_default_permission, sync_operation, notebook); neither `session_record` nor `session_model_usage` exist yet.
- `crates/database/src/lib.rs` re-exports `IngestDb`, `SearchHistoryDb`, `DeleteDb`, `AclDb`, `RoleDb`, `TenantDb`, `UserDb`, `NotebookDb` traits — none cover session lifecycle. LIB-05 will add `SessionLifecycleDb` here; LIB-03 only needs to add `pub mod session_record;` + `pub mod session_model_usage;` to `entities/mod.rs` (LIB-05 imports the entity types from there).
- `crates/session/src/sea_orm_backend/` has migrations for the QA cache table; the lifecycle table is a separate concern (lives in the **main** relational DB so the activity feed and ACL filtering can join against it).

## 4. Implementation steps

> **Decision (2026-04-29) — Decision 6**: this task does NOT introduce any wire-visible `DateTime<Utc>` fields — its outputs are SeaORM `Model` types in `cognee-database`, not HTTP DTOs. The downstream HTTP DTOs (`SessionRowDTO` in E-09, etc.) apply the `iso8601_offset` serde helper there. The helper itself is owned by E-03 (A-2 in the §0 order). Investigation agent: do not re-litigate.

1. **SeaORM entities** in `crates/database/src/entities/`:
   - `session_record.rs` — `Entity`, `Model`, `Column`, `PrimaryKey` (composite `(session_id, user_id)`). Match every column type, default, and index from §2.
   - `session_model_usage.rs` — same pattern, three-column composite PK `(session_id, user_id, model)`.
   - Both expose `Model::to_dict()`-equivalent helpers (or `Into<serde_json::Value>`) so LIB-05 can serialize without re-defining field lists.

2. **Migration** `crates/database/src/migrator/m20260501_000003_session_records.rs` (sequenced **after** the existing `m20260501_000002_pipeline_run_payload_fields.rs` from LIB-06; if a wall-clock date later than 2026-05-01 is preferred for clarity, use that instead — the only hard requirement is lexicographic ordering after `m20260501_000002`):
   - `CREATE TABLE session_records (...)` with all 12 columns.
   - `CREATE INDEX ix_session_records_user_id ON session_records(user_id);`
   - `CREATE INDEX ix_session_records_dataset_id ON session_records(dataset_id);`
   - `CREATE INDEX ix_session_records_last_activity_at ON session_records(last_activity_at);`
   - `CREATE INDEX ix_session_records_status ON session_records(status);`
   - `CREATE TABLE session_model_usage (...)` with 3-column PK + `tokens_in`/`tokens_out`/`cost_usd`/`updated_at`.
   - Register the migration as the **11th** entry in `crates/database/src/migrator/mod.rs::Migrator::migrations()` (declare with `mod m20260501_000003_session_records;` and `Box::new(m20260501_000003_session_records::Migration)` appended to the Vec).

3. **Wire the entity modules** into `crates/database/src/entities/mod.rs` (`pub mod session_record;` + `pub mod session_model_usage;`). Re-export from `crates/database/src/lib.rs` is **not** required at this task — LIB-05 imports the types via `crate::entities::session_record::*` when it lands the trait + impl.

## 5. Tests

- `crates/database/tests/test_session_lifecycle_schema.rs` (new), in-memory SQLite:
  - `migration_creates_session_records_table` — apply migration, query `sqlite_master` for the table, assert all 12 columns exist with correct types.
  - `migration_creates_session_model_usage_table` — same, 3 + 4 columns.
  - `migration_creates_expected_indexes` — query `sqlite_master WHERE type='index'`, assert all 4 named indexes are present.
  - `migration_is_idempotent_under_repeat` — apply migration, then again with `up()` — should be a no-op (or a clean error caught and reported, matching the migrator's existing semantics for duplicate migrations).
  - `roundtrip_session_record_entity` — insert a `session_record::ActiveModel`, fetch back via `Entity::find_by_id`, assert all columns match.
  - `roundtrip_session_model_usage_entity` — same.

## 6. Acceptance criteria

- [x] Migration applies cleanly to a fresh SQLite DB. (Postgres parity is implicit via SeaORM's portable `Table::create` builder; the test suite uses in-memory SQLite per the existing crate convention — see `test_session_lifecycle_schema.rs::migration_creates_session_records_table`.)
- [x] Both entities round-trip via SeaORM (insert → find → assert). (`roundtrip_session_record_entity` + `roundtrip_session_model_usage_entity`.)
- [x] All four indexes are present after migration. (`migration_creates_expected_indexes` queries `sqlite_master WHERE type='index'`.)
- [x] `cargo test -p cognee-database --test test_session_lifecycle_schema` passes (6/6).
- [x] `scripts/check_all.sh` clean (Rust fmt/check/clippy + C API + Python; the pre-existing JS jest failure noted in `IMPLEMENTATION-PROMPT.md §0` is unrelated).
- [x] No regression in existing migrator tests.

### Conventions adopted during implementation

- **UUIDs persisted as 32-char hex `String`** (not `Uuid`) for `user_id` and `dataset_id`, matching the rest of `crates/database/src/entities/`. LIB-05's `SessionLifecycleDb` trait will convert `uuid::Uuid` ↔ `String` at the repository boundary.
- **Timestamps use application-side defaults** rather than database `DEFAULT now()` clauses — this is the faithful port of Python's `default=lambda: datetime.now(timezone.utc)` in `cognee/modules/session_lifecycle/models.py`. The repository's `ensure_and_touch_session` (LIB-05) sets `started_at` / `last_activity_at` explicitly on insert.
- **`status="abandoned"` is never written** to the row by this task — it is inferred at read time by LIB-05's `effective_status` SQL expression based on `last_activity_at` vs `SESSION_ABANDON_AFTER_SECONDS` (Decision 12). LIB-03 stores the four "real" statuses only (`running`, `ended`, `errored`, plus the implicit default `running`).
- **`to_dict()` field ordering matches Python byte-for-byte** thanks to enabling the `serde_json/preserve_order` feature on the `cognee-database` crate (the only consumer that needs ordered map keys today; future entities that mirror Python `to_dict()` shapes inherit this for free).

## 7. References

- [Python `models.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/session_lifecycle/models.py)
- [LIB-05 — repository trait + impl + tests](lib-05-session-records-repo.md)
- [E-09](e-09-sessions-list.md), [E-10](e-10-sessions-stats.md), [E-11](e-11-sessions-cost-by-model.md), [E-12](e-12-sessions-detail.md)
