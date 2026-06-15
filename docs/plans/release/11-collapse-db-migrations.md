# 11 — Collapse DB Migrations to a Single Baseline (per chain)

> Wave 3 · Priority P1 · Track A · Release-blocking: no · Effort: 0.5d ·
> Depends on: — · Source: [release-readiness-plan.md](../release-readiness-plan.md) §6 (T9.1–T9.4); also referenced in the 0.1.0 release gate ([index](00-INDEX.md), task 22 depends on this)

## Goal

`0.1.0` is the **first released version** — there is no prior on-disk schema in the
wild to upgrade from, so the incremental SeaORM migration history is dead weight.
Squash **each of the two independent migrator chains** into a single
`m<date>_000001_baseline` migration whose `up()` builds the complete *current* schema
and whose `down()` drops it. The resulting database must be **byte/structure-identical**
to the pre-squash database (same tables, columns, types, defaults, indexes, FKs, seed
rows), and must stay **schema-compatible with the Python cognee DB**.

End state:
- `crates/database/src/migrator/` contains exactly **one** migration file (the baseline) + `mod.rs` with a one-element `migrations()` vector.
- `crates/session/src/migrator/` contains exactly **one** migration file (the baseline) + `mod.rs` with a one-element `migrations()` vector.
- All existing schema-compat tests pass unchanged; a fresh DB bootstraps identically from the single baseline.

## Background & why

Two **independent** SeaORM migrator chains exist. They use **separate migration
tracking tables** so they can be collapsed independently:

| Chain | Migrator type | Tracking table | Invoked from |
|---|---|---|---|
| Relational | `Migrator` (`crates/database/src/migrator/mod.rs`) | default `seaql_migrations` | `crates/database/src/connection.rs:16` → `Migrator::up(db, None)` inside `initialize()` |
| Session | `SessionMigrator` (`crates/session/src/migrator/mod.rs`) | `seaql_session_migrations` (overridden via `migration_table_name()`) | `crates/session/src/sea_orm_store.rs:27` → `SessionMigrator::up(db.as_ref(), None)` |

### Relational chain — 14 migrations (current `mod.rs`, verbatim list)

```
m20250101_000001_initial_schema
m20250201_000001_acl_tables
m20250301_000001_add_importance_weight
m20250422_000001_user_tenant_role_tables
m20260424_000001_graph_sync_checkpoints
m20260427_000001_http_auth_columns
m20260428_000001_tenants_rbac
m20260429_000001_sync_operations
m20260501_000001_create_notebooks
m20260501_000002_pipeline_run_payload_fields
m20260501_000003_session_records
m20260512_000001_add_parent_user_id
m20260528_000001_create_dataset_configurations
m20260901_000003_pipeline_run_dataset_nullable
```

### Session chain — 3 migrations (current `mod.rs`, verbatim list)

```
m20250402_000001_session_qa_entries
m20250423_000002_session_qa_feedback_fields
m20260429_000003_session_trace_steps
```

**Why this is safe:** SeaORM applies each migration's `up()` in order, and several of
the later migrations are `ALTER TABLE`s that add columns to tables created earlier
(e.g. `add_importance_weight` adds `data.importance_weight`; `http_auth_columns` adds
`users.hashed_password`/`users.is_verified`; `add_parent_user_id` adds
`users.parent_user_id`; `session_qa_feedback_fields` adds 4 columns to
`session_qa_entries`). The baseline simply *creates the final tables with those columns
already present*, producing the same end-state schema in one step.

**Why "before tagging 0.1.0":** once 0.1.0 is tagged, the baseline becomes the frozen
starting point. Future schema changes are then added as *new* incremental migrations on
top of the baseline. Collapsing after release would orphan deployed DBs.

> This is purely Rust-side schema hygiene. It is **independent** of the Python alembic
> fix in the release plan's T3.1 (cross-SDK parity CI) even though both touch
> "migrations on a virgin DB."

## Prerequisites

```bash
git checkout -b task/11-collapse-db-migrations
```

Files & sources to read first (re-grep to confirm 2026-06-14 line numbers are current):
- `crates/database/src/migrator/mod.rs` (chain list + `migrations()` vector)
- All 14 relational migration files in `crates/database/src/migrator/`
- `crates/session/src/migrator/mod.rs` + its 3 files
- `crates/database/src/connection.rs:15-19` (`initialize()`)
- `crates/session/src/sea_orm_store.rs:27` (`SessionMigrator::up`)
- Tests: `crates/database/tests/migration_compat.rs`, `crates/database/tests/sync_operations_migration.rs`, and `crates/database/tests/test_session_lifecycle_schema.rs`

## Files to change

| Path | Change |
|---|---|
| `crates/database/src/migrator/m20260914_000001_baseline.rs` | **New.** Single baseline migration: `up()` builds the entire relational schema; `down()` drops it. |
| `crates/database/src/migrator/mod.rs` | Replace 14 `mod` decls + 14-element `migrations()` with the single baseline. |
| `crates/database/src/migrator/m20250101_000001_initial_schema.rs` … `m20260901_000003_pipeline_run_dataset_nullable.rs` | **Delete** all 14 old files. |
| `crates/session/src/migrator/m20260914_000001_baseline.rs` | **New.** Single baseline for the session chain. |
| `crates/session/src/migrator/mod.rs` | Replace 3 `mod` decls + 3-element `migrations()` with the single baseline. Keep `migration_table_name()` returning `seaql_session_migrations`. |
| `crates/session/src/migrator/m20250402_000001_session_qa_entries.rs` … `m20260429_000003_session_trace_steps.rs` | **Delete** all 3 old files. |

> Use the date `20260914` (or any date strictly **after** the latest existing migration,
> `20260901`) so the baseline sorts last — this matters only as a naming convention; on a
> fresh DB there is no pre-existing `seaql_migrations` row to conflict with.

## The complete target schema (authoritative reference)

The baseline `up()` must reproduce **exactly** this. Every table/column/index/FK/seed
below was extracted from the current migration chain. **Preserve every detail** —
column types, NOT NULL, DEFAULTs, the `#[sea_orm(iden = "type")]` overrides, index
names, FK ON DELETE actions, and the seed `INSERT`s — or cross-SDK / runtime parity
breaks.

### Relational chain tables (10 base + RBAC + feature tables)

| # | Table | Notes (defaults / FKs / special columns) |
|---|---|---|
| 1 | `datasets` | cols: `id` PK, `name` NN, `owner_id` NN, `tenant_id`, `created_at` NN, `updated_at`. Indexes: `idx_datasets_owner_id`, `idx_datasets_tenant_id`. |
| 2 | `data` | 22+ cols incl. Python-compat: `id` PK, `name` NN, `raw_data_location` NN, `original_data_location` NN, `extension` NN, `mime_type` NN, `content_hash` NN, `owner_id` NN, `created_at` NN, `updated_at`, `label`, `original_extension`, `original_mime_type`, `loader_engine`, `raw_content_hash`, `tenant_id`, `external_metadata`, `node_set`, `pipeline_status`, `token_count` BIGINT NN DEFAULT `-1`, `data_size` BIGINT NN DEFAULT `-1`, `last_accessed`, **`importance_weight` DOUBLE NULL** (was added by `add_importance_weight`). Indexes: `idx_data_owner_id`, `idx_data_tenant_id`. |
| 3 | `dataset_data` | junction, composite PK `(dataset_id, data_id)`, `created_at` NN. FK `dataset_id→datasets.id` CASCADE, FK `data_id→data.id` CASCADE. |
| 4 | `queries` | `id` PK, `query_text` NN, `query_type` NN, `user_id`, `created_at` NN. |
| 5 | `results` | `id` PK, `query_id` NN, `serialized_result` NN, `user_id`, `created_at` NN. FK `query_id→queries.id` CASCADE. |
| 6 | `nodes` | `id` PK, `slug` NN, `user_id` NN, `data_id` NN, `dataset_id` NN, `label`, **`type` NN** (`#[sea_orm(iden = "type")]` override on `NodeType` — NOT `node_type`), `indexed_fields` JSON NN, `attributes` JSON, `created_at` NN. FK `dataset_id→datasets.id` CASCADE only (**no FK on `data_id`**). Indexes: `idx_nodes_dataset_slug` (dataset_id, slug), `idx_nodes_dataset_data` (dataset_id, data_id). |
| 7 | `edges` | `id` PK, `slug` NN, `user_id` NN, `data_id` NN, `dataset_id` NN, `source_node_id` NN, `destination_node_id` NN, `relationship_name` NN, `label`, `attributes` JSON, `created_at` NN. FK `dataset_id→datasets.id` CASCADE only (**no FK on `data_id`**). Indexes: `idx_edges_data_id`, `idx_edges_dataset_id`. |
| 8 | `pipeline_runs` | `id` PK, `created_at` NN, `status` NN, `pipeline_run_id` NN, `pipeline_name` NN, `pipeline_id` NN, **`dataset_id` NULLABLE, NO FK** (this is the final state after `pipeline_run_dataset_nullable` — see below), `run_info` JSON. Indexes: `idx_pipeline_runs_pipeline_run_id`, `idx_pipeline_runs_pipeline_id`, `idx_pipeline_runs_dataset_id`. |
| 9 | `task_runs` | `id` PK, `task_name` NN, `created_at` NN, `status` NN, `run_info` JSON. |
| 10 | `graph_metrics` | `id` PK + 12 metric cols (`num_tokens`, `num_nodes`, `num_edges` INT; `mean_degree`, `edge_density`, `avg_shortest_path_length`, `avg_clustering` DOUBLE; `num_connected_components` INT, `sizes_of_connected_components` JSON, `num_selfloops` INT, `diameter` INT), `created_at` NN, `updated_at`. |
| 11 | `principals` | `id` PK, **`type` NN** (`#[sea_orm(iden = "type")]` override on `PrincipalType`), `created_at` NN, `updated_at`. |
| 12 | `permissions` | `id` PK, `name` NN UNIQUE, `created_at` NN, `updated_at`. Index `idx_permissions_name`. **Seed 4 rows** (see below). |
| 13 | `acls` | `id` PK, `principal_id` NN, `permission_id` NN, `dataset_id` NN, `created_at` NN, `updated_at`. FKs: `principal_id→principals.id`, `permission_id→permissions.id`, `dataset_id→datasets.id` CASCADE. Indexes: `idx_acls_unique_grant` UNIQUE (principal_id, permission_id, dataset_id); `ix_acls_principal_dataset` (principal_id, dataset_id); `ix_acls_dataset` (dataset_id). |
| 14 | `tenants` | `id` PK, `name` NN UNIQUE, `owner_id` NN, `created_at` NN, `updated_at`. FK `id→principals.id`. |
| 15 | `users` | `id` PK, `email` NN UNIQUE, `is_active` BOOL NN DEFAULT true, `is_superuser` BOOL NN DEFAULT false, `tenant_id`, `created_at` NN, `updated_at`, **`hashed_password` TEXT NN DEFAULT `''`**, **`is_verified` BOOL NN DEFAULT true**, **`parent_user_id` TEXT NULL**. FKs: `id→principals.id`, `tenant_id→tenants.id`. **Seed 1 default user** (see below). |
| 16 | `roles` | `id` PK, `name` NN, `tenant_id` NN, `created_at` NN, `updated_at`. FKs: `id→principals.id`, `tenant_id→tenants.id`. Indexes: `idx_roles_tenant_name` UNIQUE (tenant_id, name); `ix_roles_tenant` (tenant_id). |
| 17 | `user_tenants` | junction, composite PK `(user_id, tenant_id)`, `created_at` NN. FKs to users/tenants. Index `ix_user_tenants_user` (user_id). |
| 18 | `user_roles` | junction, composite PK `(user_id, role_id)`, `created_at` NN. FKs to users/roles. Index `ix_user_roles_user` (user_id). |
| 19 | `graph_sync_checkpoints` | `key` PK (TEXT), `ts` timestamptz NN. |
| 20 | `user_api_key` | `id` PK, `user_id` NN, `api_key` NN (**NOT unique** — Python parity), `label`, `name`, `created_at` NN, `expires_at`. FK `user_id→principals.id` CASCADE. Index `idx_user_api_key_user_id`. |
| 21 | `role_default_permissions` | composite PK `(role_id, permission_id)`, `created_at` NN. FKs to roles/permissions CASCADE. |
| 22 | `user_default_permissions` | composite PK `(user_id, permission_id)`, `created_at` NN. FKs to users/permissions CASCADE. |
| 23 | `tenant_default_permissions` | composite PK `(tenant_id, permission_id)`, `created_at` NN. FK `tenant_id→tenants.id`, FK `permission_id→permissions.id` CASCADE. |
| 24 | `principal_configuration` | `id` PK, `owner_id` NN, `name` NN, `configuration` JSON NN, `created_at` NN, `updated_at`. FK `owner_id→principals.id`. |
| 25 | `sync_operations` | `id` PK, `run_id` NN, `status` NN DEFAULT `"started"`, `progress_percentage` INT NN DEFAULT 0, `dataset_ids` JSON, `dataset_names` JSON, `user_id` NN, `created_at` NN, `started_at`, `completed_at`, `total_records_to_sync` INT, `total_records_to_download` INT, `total_records_to_upload` INT, `records_downloaded` INT NN DEFAULT 0, `records_uploaded` INT NN DEFAULT 0, `bytes_downloaded` BIGINT NN DEFAULT 0, `bytes_uploaded` BIGINT NN DEFAULT 0, `dataset_sync_hashes` JSON, `error_message`, `retry_count` INT NN DEFAULT 0. Indexes: `idx_sync_operations_run_id` UNIQUE (run_id); `idx_sync_operations_user_id` (user_id). |
| 26 | `notebooks` | `id` PK, `owner_id` NN, `name` NN, `cells` JSON NN DEFAULT `"[]"`, `deletable` BOOL NN DEFAULT true, `created_at` NN. Index `idx_notebooks_owner_id`. |
| 27 | `pipeline_run_payload_fields` | composite PK `(pipeline_run_id, key)`, `value` JSON NN, `created_at` NN, `updated_at` NN (**both NN**). Index `idx_pipeline_run_payload_fields_run_id`. **No FK.** |
| 28 | `session_records` | composite PK `(session_id, user_id)`, `dataset_id`, `status` NN DEFAULT `"running"`, `started_at` NN, `last_activity_at` NN, `ended_at`, `tokens_in` INT NN DEFAULT 0, `tokens_out` INT NN DEFAULT 0, `cost_usd` DOUBLE NN DEFAULT 0.0, `error_count` INT NN DEFAULT 0, `last_model`. Indexes: `ix_session_records_user_id`, `ix_session_records_dataset_id`, `ix_session_records_last_activity_at`, `ix_session_records_status`. **No FKs.** |
| 29 | `session_model_usage` | composite PK `(session_id, user_id, model)`, `tokens_in` INT NN DEFAULT 0, `tokens_out` INT NN DEFAULT 0, `cost_usd` DOUBLE NN DEFAULT 0.0, `updated_at` NN. **No extra indexes, no FKs.** |
| 30 | `dataset_configurations` | `id` PK, `dataset_id` NN, `graph_schema` JSON, `custom_prompt` TEXT, `created_at` NN, `updated_at`. FK `fk_dataset_configurations_dataset_id` `dataset_id→datasets.id` CASCADE. Index `uq_dataset_configurations_dataset_id` UNIQUE (dataset_id). |

> **`pipeline_runs` critical nuance:** the original `initial_schema` created
> `pipeline_runs.dataset_id` as `NOT NULL` with a CASCADE FK, then
> `m20260901_000003_pipeline_run_dataset_nullable` rebuilt the table to make
> `dataset_id` **nullable and drop the FK** (Python parity). The baseline must create
> `pipeline_runs` in its **final** form: `dataset_id` **nullable, no FK** — do NOT
> re-add the FK.

### Seed rows (must be reproduced verbatim in the baseline `up()`)

These are emitted by the original `acl_tables` and `user_tenant_role_tables` migrations.
Use the **same SQLite-flavored `INSERT … ON CONFLICT … DO NOTHING`** statements via
`manager.get_connection().execute_unprepared(...)`. The "retroactive grant"
back-fill statements (which grant permissions to *existing* datasets' owners) are
**no-ops on a fresh DB** but should be kept for parity when bootstrapping a partially
Python-seeded DB. Keep all of them.

1. **4 permission rows** (`acl_tables.rs:120-128`):
   ```sql
   INSERT INTO permissions (id, name, created_at) VALUES
       ('00000000000000000000000000000001', 'read',   datetime('now')),
       ('00000000000000000000000000000002', 'write',  datetime('now')),
       ('00000000000000000000000000000003', 'delete', datetime('now')),
       ('00000000000000000000000000000004', 'share',  datetime('now'))
   ON CONFLICT (name) DO NOTHING
   ```
2. **Retroactive principal back-fill** (`acl_tables.rs:132-139`) — keep verbatim.
3. **Retroactive ACL grant** (`acl_tables.rs:141-159`) — keep verbatim.
4. **Default principal + default user** (`user_tenant_role_tables.rs:180-192`):
   ```sql
   INSERT INTO principals (id, type, created_at)
     VALUES ('00000000000000000000000000000000', 'user', datetime('now'))
     ON CONFLICT (id) DO NOTHING;
   INSERT INTO users (id, email, is_active, is_superuser, tenant_id, created_at)
     VALUES ('00000000000000000000000000000000', 'default_user@example.com', 1, 1, NULL, datetime('now'))
     ON CONFLICT (id) DO NOTHING;
   ```

> **Ordering constraint:** the seed `INSERT`s must run **after** the `permissions`,
> `principals`, `datasets`, and `users` tables exist. Place all `execute_unprepared`
> seed statements at the **end** of `up()`, after every `create_table`.

> **Postgres caveat:** the seed statements use SQLite's `datetime('now')`. The original
> migrations only seed on SQLite-flavored SQL too (they were authored SQLite-first).
> If the Postgres test lane (`DB_PROVIDER=postgres`) is exercised, verify these
> `execute_unprepared` calls still succeed on Postgres — `datetime('now')` is not valid
> Postgres. **Mitigation:** branch on `manager.get_database_backend()` for the seed SQL
> (use `now()` / `CURRENT_TIMESTAMP` on Postgres), OR keep the SeaORM builder-based
> table creation (backend-portable) and only special-case the raw seed SQL. Check
> whether the pre-squash chain actually ran these seeds on Postgres before assuming
> parity (grep the originals — they used `execute_unprepared` with SQLite SQL, so the
> Postgres lane may already tolerate/skip them; preserve **whatever the current behavior
> is**, do not "improve" it here).

### Session chain tables

| # | Table | Notes |
|---|---|---|
| 1 | `session_qa_entries` | `id` PK, `session_id` NN, `user_id`, `question` NN, `answer` NN, `context`, `created_at` NN, **plus** (from feedback migration) `feedback_text` TEXT, `feedback_score` INT, `used_graph_element_ids` TEXT, `memify_metadata` TEXT. Indexes: `idx_session_qa_session_id`, `idx_session_qa_session_user` (session_id, user_id). |
| 2 | `session_graph_context` | `id` PK, `session_id` NN, `user_id`, `context` TEXT NN, `updated_at` NN. Index `idx_session_graph_ctx_session_user` (session_id, user_id). |
| 3 | `session_trace_steps` | `trace_id` PK, `user_id` NN, `session_id` NN, `seq` BIGINT NN, `created_at` NN, `origin_function` NN, `status` NN, `memory_query` NN, `memory_context` NN, `method_params` NN, `method_return_value` (nullable), `error_message` NN, `session_feedback` NN. Index `idx_session_trace_steps_user_session_seq` (user_id, session_id, seq). |

## Implementation steps

### Part 1 — Capture the authoritative current schema (T9.1)

1. On `main` (before any deletion), apply all migrations to a fresh SQLite DB and dump
   the DDL as the golden reference:
   ```bash
   cargo test -p cognee-database --test migration_compat migration_from_empty_db_sqlite -- --nocapture
   ```
   Then capture the live DDL into a file you keep **outside** the commit (e.g. `/tmp/baseline_before.sql`):
   ```bash
   # Build a throwaway DB via the CLI/test, then dump:
   #   sqlite3 <path-to-fresh-db> '.schema' > /tmp/baseline_before.sql
   #   sqlite3 <path-to-fresh-db> 'SELECT * FROM permissions; SELECT * FROM users;' >> /tmp/baseline_before.sql
   ```
   Keep `/tmp/baseline_before.sql` (relational) and a session equivalent for the
   structural diff in Part 4. (If sqlite3 CLI is unavailable, add a temporary
   `--nocapture` test that runs the `table_names` + `PRAGMA table_info(...)` queries for
   every table and prints them; revert it after.)

### Part 2 — Write the relational baseline (T9.2)

2. Create `crates/database/src/migrator/m20260914_000001_baseline.rs`. Model it on the
   existing `m20250101_000001_initial_schema.rs` structure: `#[derive(DeriveMigrationName)] pub struct Migration;`
   `impl MigrationTrait` with `up()` / `down()`, and `#[derive(DeriveIden)]` enums for
   every table at the bottom.
3. In `up()`, create **all 30 relational tables** in **dependency order** so FK targets
   exist first. A safe order:
   ```
   datasets → data → dataset_data → queries → results → nodes → edges
   → pipeline_runs → task_runs → graph_metrics
   → principals → permissions → acls
   → tenants → users → roles → user_tenants → user_roles
   → graph_sync_checkpoints → user_api_key
   → role_default_permissions → user_default_permissions → tenant_default_permissions
   → principal_configuration → sync_operations → notebooks
   → pipeline_run_payload_fields → session_records → session_model_usage
   → dataset_configurations
   ```
   Copy each `create_table` / `create_index` block **verbatim** from the corresponding
   original migration, merging the later `ALTER`s directly into the `CREATE`:
   - `data`: add `.col(ColumnDef::new(Data::ImportanceWeight).double().null())` (from `add_importance_weight`).
   - `users`: add `hashed_password` (TEXT NN DEFAULT `""`), `is_verified` (BOOL NN DEFAULT true), `parent_user_id` (TEXT null) directly into the `CREATE`.
   - `session_qa_entries` (session chain — handled in Part 3): add the 4 feedback columns into the `CREATE`.
   - `pipeline_runs`: create with `dataset_id` **nullable, no FK** (final state).
4. Preserve the two `#[sea_orm(iden = "type")]` overrides on `Nodes::NodeType` and
   `Principals::PrincipalType`.
5. Append the seed `execute_unprepared` statements (the 4 permissions, the back-fills,
   and the default principal/user) **at the end** of `up()`, after all `create_table`s.
   See the Postgres caveat above — replicate the existing behavior exactly.
6. Write `down()` to drop every table in **reverse** dependency order (mirror the
   reverse-order drop in `initial_schema.rs:410-443`, extended to all 30 tables).

### Part 3 — Write the session baseline (T9.2)

7. Create `crates/session/src/migrator/m20260914_000001_baseline.rs` creating the 3
   session tables (`session_qa_entries` **with** the 4 feedback columns merged in,
   `session_graph_context`, `session_trace_steps`) and all their indexes. `down()` drops
   all three.

### Part 4 — Reduce both `mod.rs` files (T9.2)

8. Replace `crates/database/src/migrator/mod.rs` with:
   ```rust
   use sea_orm_migration::prelude::*;

   mod m20260914_000001_baseline;

   pub struct Migrator;

   #[async_trait::async_trait]
   impl MigratorTrait for Migrator {
       fn migrations() -> Vec<Box<dyn MigrationTrait>> {
           vec![Box::new(m20260914_000001_baseline::Migration)]
       }
   }
   ```
9. Replace `crates/session/src/migrator/mod.rs` similarly, **keeping**
   `migration_table_name()` → `seaql_session_migrations`:
   ```rust
   use sea_orm_migration::prelude::*;

   mod m20260914_000001_baseline;

   pub struct SessionMigrator;

   #[async_trait::async_trait]
   impl MigratorTrait for SessionMigrator {
       fn migration_table_name() -> DynIden {
           Alias::new("seaql_session_migrations").into_iden()
       }
       fn migrations() -> Vec<Box<dyn MigrationTrait>> {
           vec![Box::new(m20260914_000001_baseline::Migration)]
       }
   }
   ```

### Part 5 — Delete old files & update assumptions (T9.2, T9.4)

10. Delete all 14 old relational migration files and all 3 old session migration files.
11. Grep for any code/tests/docs that assume a specific migration **count** or reference
    deleted migration file/struct names:
    ```bash
    grep -rn "initial_schema\|acl_tables\|migrations().len\|14 migration\|m2025\|m2026" crates/ docs/ | grep -v "docs/plans"
    ```
    Update any such assertions **and** any doc-comment or `//!` header lines that cite
    old migration filenames (e.g. `crates/database/src/permissions/sea_orm_impl.rs:4-6`
    references `m20250201_000001_acl_tables.rs`, `m20250422_000001_user_tenant_role_tables.rs`,
    and `m20260428_000001_tenants_rbac.rs` — update those comments to reference the
    new baseline instead). The schema-compat tests assert *table presence*, not
    migration count — confirm by re-reading them.

### Part 6 — Verify (T9.3, T9.4)

12. Run the structural diff: bootstrap a fresh DB from the squashed baseline, dump its
    DDL, and diff against `/tmp/baseline_before.sql` from Part 1. They must be
    structurally identical (column order may differ harmlessly; types, NN, defaults,
    indexes, FKs, and seed rows must match). Normalize before diffing if needed
    (sort lines / strip auto-generated FK/index names).

## Verification

```bash
# 1. Compiles, no warnings.
cargo check -p cognee-database -p cognee-session --all-targets

# 2. Relational schema-compat tests (idempotency, all tables present, data preserved).
cargo test -p cognee-database --test migration_compat
# Expected: migration_is_idempotent_sqlite, migration_from_empty_db_sqlite,
# migration_preserves_existing_data_sqlite all PASS. PG variants skip without DB_PROVIDER.

# 3. Sync-operations table + repo round-trip from a fresh in-memory DB.
cargo test -p cognee-database --test sync_operations_migration
# Expected: migration_creates_table_idempotently, mark_failed_records_error_message PASS.

# 4. Session lifecycle schema test (uses initialize()).
cargo test -p cognee-database --test test_session_lifecycle_schema

# 5. Session store builds its schema from the single baseline.
cargo test -p cognee-session

# 6. Fresh-DB bootstrap via CLI (no migration error on a virgin file).
cargo run -p cognee-cli -- config 2>&1 | head   # any command that opens/initializes the DB

# 7. Full gate.
scripts/check_all.sh
```

Add a regression test to `crates/database/tests/migration_compat.rs` that asserts the
**full table set** (all 30 relational tables) is present and that the seed rows landed:

```rust
#[tokio::test]
async fn baseline_creates_full_table_set_sqlite() {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("initialize");
    let tables = table_names(&db).await;
    for t in [
        "datasets","data","dataset_data","queries","results","nodes","edges",
        "pipeline_runs","task_runs","graph_metrics","principals","permissions","acls",
        "tenants","users","roles","user_tenants","user_roles","graph_sync_checkpoints",
        "user_api_key","role_default_permissions","user_default_permissions",
        "tenant_default_permissions","principal_configuration","sync_operations",
        "notebooks","pipeline_run_payload_fields","session_records","session_model_usage",
        "dataset_configurations",
    ] {
        assert!(tables.iter().any(|x| x == t), "missing table {t}: {tables:?}");
    }
    // Seed parity: 4 permissions + the default user must exist.
    let perms = db.query_all(sea_orm::Statement::from_string(
        db.get_database_backend(), "SELECT name FROM permissions".into())).await.unwrap();
    assert_eq!(perms.len(), 4, "expected 4 seeded permissions");
}
```

Also confirm the single-migration invariant:

```rust
#[test]
fn relational_chain_has_one_migration() {
    use sea_orm_migration::MigratorTrait;
    assert_eq!(cognee_database::migrator::Migrator::migrations().len(), 1);
}
```
(Adjust the path if `Migrator` is not re-exported; otherwise add the test inside the crate.)

## Acceptance criteria

- [ ] `crates/database/src/migrator/` contains exactly one migration file + `mod.rs`; `migrations()` returns a 1-element vector.
- [ ] `crates/session/src/migrator/` contains exactly one migration file + `mod.rs`; `migrations()` returns a 1-element vector; `migration_table_name()` still returns `seaql_session_migrations`.
- [ ] All 14 old relational + 3 old session migration files deleted.
- [ ] All existing schema-compat tests (`migration_compat`, `sync_operations_migration`, `test_session_lifecycle_schema`) pass **unchanged**.
- [ ] New `baseline_creates_full_table_set_sqlite` test passes (30 tables + 4 seeded permissions + default user).
- [ ] Structural DDL diff (pre-squash vs squashed) shows no schema differences.
- [ ] Fresh-DB bootstrap via CLI and HTTP server succeeds with no migration error.
- [ ] `scripts/check_all.sh` passes.

## Gotchas / do-not

- **Cross-SDK schema parity is sacred.** The Rust schema must stay byte-compatible with
  the Python cognee DB. Do **not** rename columns, change the `type` column overrides on
  `nodes`/`principals`, add a UNIQUE on `user_api_key.api_key`, or re-add the
  `pipeline_runs.dataset_id` FK/NOT-NULL. Reproduce the **final** state, not the
  intermediate states.
- **Preserve `token_count` / `data_size` defaults of `-1`** and the importance/weight,
  auth, and parent-user columns — they were added incrementally and are easy to miss.
- **Seed rows are part of the schema contract.** The 4 permission rows and the default
  user (`00000000000000000000000000000000`) are relied on by ACL checks and the default
  user flow. Reproduce the exact IDs and the `ON CONFLICT DO NOTHING` clauses.
- **Two separate tracking tables.** Do not merge the session chain into the relational
  `Migrator` — they intentionally use different `seaql_*` tables and are run from
  different call sites (`connection.rs` vs `sea_orm_store.rs`).
- **Postgres seed SQL.** `datetime('now')` is SQLite-only. Replicate exactly what the
  current chain does on Postgres (branch on backend if the originals did, or leave as-is
  if the PG lane never ran the seeds). Do not silently change PG behavior.
- **Don't "improve" the schema.** This task is a pure squash — zero behavioral schema
  change. Any improvement belongs in a *new* incremental migration after 0.1.0.
- **Date ordering only matters as convention.** Pick a date after `20260901`; on a fresh
  DB there is no conflicting `seaql_migrations` row.

## Rollback

```bash
git checkout main -- crates/database/src/migrator crates/session/src/migrator
```
This restores all 17 original migration files and both `mod.rs` files. No data migration
is needed because this is pre-release (no deployed DBs exist).
