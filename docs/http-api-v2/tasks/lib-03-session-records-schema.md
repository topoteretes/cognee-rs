# LIB-03 — `cognee-database` `session_records` + `session_model_usage` schema and entities

| | |
|---|---|
| Scope | SeaORM entities + migration only. The repository trait + impl + tests live in **LIB-05**. |
| Status | **Not Started** |
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

- `crates/database/src/migrator/` has migrations for users, datasets, ACLs, etc. — **no** session_records table.
- `crates/database/src/lib.rs` exposes `IngestDb`, `SearchHistoryDb`, `DeleteDb` traits — none cover session lifecycle.
- `crates/session/src/sea_orm_backend/` has migrations for the QA cache table; the lifecycle table is a separate concern (lives in the **main** relational DB so the activity feed and ACL filtering can join against it).

## 4. Implementation steps

> **Decision (2026-04-29) — Decision 6**: this task does NOT introduce any wire-visible `DateTime<Utc>` fields — its outputs are SeaORM `Model` types in `cognee-database`, not HTTP DTOs. The downstream HTTP DTOs (`SessionRowDTO` in E-09, etc.) apply the `iso8601_offset` serde helper there. The helper itself is owned by E-03 (A-2 in the §0 order). Investigation agent: do not re-litigate.

1. **SeaORM entities** in `crates/database/src/entities/`:
   - `session_record.rs` — `Entity`, `Model`, `Column`, `PrimaryKey` (composite `(session_id, user_id)`). Match every column type, default, and index from §2.
   - `session_model_usage.rs` — same pattern, three-column composite PK `(session_id, user_id, model)`.
   - Both expose `Model::to_dict()`-equivalent helpers (or `Into<serde_json::Value>`) so LIB-05 can serialize without re-defining field lists.

2. **Migration** `crates/database/src/migrator/m_<timestamp>_session_records.rs`:
   - `CREATE TABLE session_records (...)` with all 12 columns.
   - `CREATE INDEX ix_session_records_user_id ON session_records(user_id);`
   - `CREATE INDEX ix_session_records_dataset_id ON session_records(dataset_id);`
   - `CREATE INDEX ix_session_records_last_activity_at ON session_records(last_activity_at);`
   - `CREATE INDEX ix_session_records_status ON session_records(status);`
   - `CREATE TABLE session_model_usage (...)` with 3-column PK + `tokens_in`/`tokens_out`/`cost_usd`/`updated_at`.
   - Add the migration to the migrator's `migrations()` Vec.

3. **Re-export the entities** from `crates/database/src/lib.rs` (LIB-05 imports them).

## 5. Tests

- `crates/database/tests/test_session_lifecycle_schema.rs` (new), in-memory SQLite:
  - `migration_creates_session_records_table` — apply migration, query `sqlite_master` for the table, assert all 12 columns exist with correct types.
  - `migration_creates_session_model_usage_table` — same, 3 + 4 columns.
  - `migration_creates_expected_indexes` — query `sqlite_master WHERE type='index'`, assert all 4 named indexes are present.
  - `migration_is_idempotent_under_repeat` — apply migration, then again with `up()` — should be a no-op (or a clean error caught and reported, matching the migrator's existing semantics for duplicate migrations).
  - `roundtrip_session_record_entity` — insert a `session_record::ActiveModel`, fetch back via `Entity::find_by_id`, assert all columns match.
  - `roundtrip_session_model_usage_entity` — same.

## 6. Acceptance criteria

- [ ] Migration applies cleanly to a fresh SQLite + Postgres DB.
- [ ] Both entities round-trip via SeaORM (insert → find → assert).
- [ ] All four indexes are present after migration.
- [ ] `cargo test -p cognee-database --test test_session_lifecycle_schema` passes.
- [ ] `scripts/check_all.sh` clean.
- [ ] No regression in existing migrator tests.

## 7. References

- [Python `models.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/session_lifecycle/models.py)
- [LIB-05 — repository trait + impl + tests](lib-05-session-records-repo.md)
- [E-09](e-09-sessions-list.md), [E-10](e-10-sessions-stats.md), [E-11](e-11-sessions-cost-by-model.md), [E-12](e-12-sessions-detail.md)
