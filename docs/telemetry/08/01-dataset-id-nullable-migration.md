# Task 08-01 — Make `dataset_id` nullable + drop FK

**Status**: implemented in commit 526c892 (delete-lib test renamed from `…cascades…` to `…preserves…` to match new behaviour)
**Owner**: _unassigned_
**Depends on**: —
**Blocks**:
- [Task 08-02 — `data_info` helper](02-data-info-helper.md) (`RunSpec.dataset_id` was already `Option<Uuid>`, but downstream consumers need the new domain shape).
- [Task 08-03 — `run_info` shape alignment](03-run-info-shape-alignment.md) (the watcher passes `dataset_id: Option<Uuid>` to `log_pipeline_run` after the silent-drop branch is removed).
- [Task 08-04 — INITIATED from executor](04-initiated-from-executor.md), [Task 08-05 — Reset helpers](05-reset-helpers.md), [Task 08-06 — Reader helpers](06-reader-helpers.md), [Task 08-07 — Library wiring](07-library-pipeline-wiring.md), [Task 08-08 — Qualification check](08-check-qualification.md), [Task 08-09 — Tests](09-tests.md) — every later task assumes `dataset_id: Option<Uuid>` on the domain `PipelineRun`.

**Parent doc**: [08 — Pipeline Run Status Persistence](../08-pipeline-run-status.md)
**Locked decisions**: 4 (`dataset_id` becomes nullable; FK dropped), 11 (`SeaOrmPipelineRunRepository` is the single point of truth).

---

## 1. Goal

Bring the `pipeline_runs.dataset_id` column to byte-equivalent Python parity:

1. Drop the `dataset_id NOT NULL` constraint and the `FK → datasets(id) ON DELETE CASCADE`.
2. Change the SeaORM entity field `Model.dataset_id` from `String` to `Option<String>`.
3. Change the domain type `PipelineRun.dataset_id` from `Uuid` to `Option<Uuid>`.
4. Remove the silent-drop branch in `SeaOrmPipelineRunRepository::log_pipeline_run` (currently returns a fake id when `dataset_id` is `None`).
5. Update every downstream consumer (`latest_status` map key, `list_recent_with_attribution` projection, conversions, http-server activity DTO) to handle the optional.

## 2. Rationale

Python's `pipeline_runs.dataset_id` is `Column(UUID, index=True)` — nullable, no FK ([Python schema](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRun.py)). Today the Rust schema declares `dataset_id text NOT NULL` and a `FK → datasets(id) ON DELETE CASCADE` ([initial schema lines 316-323](../../crates/database/src/migrator/m20250101_000001_initial_schema.rs#L316-L323)).

Consequences of the divergence today:

- **Silent data loss**: the repository's early-return at [`sea_orm_impl.rs:54-58`](../../crates/database/src/pipelines/sea_orm_impl.rs#L54-L58) returns a generated `row_id` *without persisting* when `dataset_id` is `None`. Callers receive a UUID that points at no row.
- **Cross-SDK ingest break**: a row written by Python with `dataset_id IS NULL` cannot be inserted by Rust (NOT NULL violation).
- **Cascade surprise**: deleting a dataset in Rust cascades into `pipeline_runs`, silently destroying the audit trail. Python does not do this.

The migration realigns Rust with Python and unblocks every later task in this gap.

## 3. Pre-conditions

- `cargo check --all-targets` on the current HEAD passes cleanly; the workspace compiles end-to-end with no warnings. (A prior unrelated blocker in `crates/database/tests/permissions_repository.rs` — missing `parent_user_id` field in the `user::ActiveModel` seed — has already been resolved upstream.)
- The most recent registered migration is `m20260512_000001_add_parent_user_id` ([`migrator/mod.rs`](../../crates/database/src/migrator/mod.rs) line 33); the new file slots after it (or after any migration that ships in the interim — re-check `Migrator::migrations` order before committing).
- No callers depend on `dataset_id` being non-null in the domain `PipelineRun` outside of the files listed in §6. Verify via `rg "PipelineRun \{|PipelineRun::|\.dataset_id" crates/`.

## 4. Step-by-step

### 4.1 New migration file

Create `crates/database/src/migrator/m20260901_000003_pipeline_run_dataset_nullable.rs`.

SQLite cannot `DROP FOREIGN KEY` or `ALTER COLUMN ... DROP NOT NULL` in place — the migration must rebuild the table. Postgres can use plain `ALTER TABLE`. The SeaORM `manager.get_database_backend()` discriminator lets the migration branch.

```rust
//! Gap 08-01: drop the `dataset_id NOT NULL` constraint and the
//! `FK → datasets(id) ON DELETE CASCADE` from `pipeline_runs`, for Python parity.
//!
//! SQLite cannot drop foreign keys in place; the migration rebuilds the table.
//! Postgres uses ALTER TABLE.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let backend = manager.get_database_backend();
        match backend {
            sea_orm::DatabaseBackend::Sqlite => sqlite_rebuild(manager).await,
            sea_orm::DatabaseBackend::Postgres => postgres_alter(manager).await,
            sea_orm::DatabaseBackend::MySql => Err(DbErr::Migration(
                "MySQL backend not supported".into(),
            )),
        }
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Best-effort: this migration is *de-restrictive*; reversing it would
        // require backfilling NULLs and re-introducing the FK, which is unsafe
        // if the user has ad-hoc rows. We refuse the down-migration.
        let _ = manager;
        Err(DbErr::Migration(
            "08-01 cannot be reversed — NULL dataset_id rows may exist".into(),
        ))
    }
}

async fn sqlite_rebuild(manager: &SchemaManager) -> Result<(), DbErr> {
    let conn = manager.get_connection();
    // 1) Create new table without FK / NOT NULL on dataset_id.
    conn.execute_unprepared(
        r#"
        CREATE TABLE pipeline_runs_new (
            id              TEXT PRIMARY KEY NOT NULL,
            created_at      TIMESTAMP WITH TIME ZONE NOT NULL,
            status          TEXT NOT NULL,
            pipeline_run_id TEXT NOT NULL,
            pipeline_name   TEXT NOT NULL,
            pipeline_id     TEXT NOT NULL,
            dataset_id      TEXT,
            run_info        JSON
        );
        "#,
    )
    .await?;

    // 2) Copy data.
    conn.execute_unprepared(
        r#"
        INSERT INTO pipeline_runs_new (
            id, created_at, status, pipeline_run_id,
            pipeline_name, pipeline_id, dataset_id, run_info
        )
        SELECT id, created_at, status, pipeline_run_id,
               pipeline_name, pipeline_id, dataset_id, run_info
        FROM pipeline_runs;
        "#,
    )
    .await?;

    // 3) Drop old table, rename new.
    conn.execute_unprepared("DROP TABLE pipeline_runs;").await?;
    conn.execute_unprepared("ALTER TABLE pipeline_runs_new RENAME TO pipeline_runs;")
        .await?;

    // 4) Recreate indexes (matches initial schema).
    conn.execute_unprepared(
        "CREATE INDEX idx_pipeline_runs_pipeline_run_id ON pipeline_runs(pipeline_run_id);",
    )
    .await?;
    conn.execute_unprepared(
        "CREATE INDEX idx_pipeline_runs_pipeline_id ON pipeline_runs(pipeline_id);",
    )
    .await?;
    conn.execute_unprepared(
        "CREATE INDEX idx_pipeline_runs_dataset_id ON pipeline_runs(dataset_id);",
    )
    .await?;
    Ok(())
}

async fn postgres_alter(manager: &SchemaManager) -> Result<(), DbErr> {
    let conn = manager.get_connection();
    // FK name is auto-generated by SeaORM's create_table; query information_schema
    // to find it. SeaORM does not expose a stable name. Use a DO block to drop
    // whichever FK constraint references datasets(id) from pipeline_runs.
    conn.execute_unprepared(
        r#"
        DO $$
        DECLARE
            fk_name text;
        BEGIN
            SELECT conname INTO fk_name
            FROM pg_constraint
            WHERE conrelid = 'pipeline_runs'::regclass
              AND contype = 'f'
              AND confrelid = 'datasets'::regclass;
            IF fk_name IS NOT NULL THEN
                EXECUTE format('ALTER TABLE pipeline_runs DROP CONSTRAINT %I', fk_name);
            END IF;
        END$$;
        "#,
    )
    .await?;

    conn.execute_unprepared("ALTER TABLE pipeline_runs ALTER COLUMN dataset_id DROP NOT NULL;")
        .await?;
    Ok(())
}
```

Register in [`crates/database/src/migrator/mod.rs`](../../crates/database/src/migrator/mod.rs):

```rust
mod m20260901_000003_pipeline_run_dataset_nullable;
// ...
fn migrations() -> Vec<Box<dyn MigrationTrait>> {
    vec![
        // ... existing ...
        Box::new(m20260512_000001_add_parent_user_id::Migration),
        Box::new(m20260901_000003_pipeline_run_dataset_nullable::Migration),
    ]
}
```

### 4.2 Entity

Edit [`crates/database/src/entities/pipeline_run.rs`](../../crates/database/src/entities/pipeline_run.rs) line 30:

```rust
// before
pub dataset_id: String,
// after
pub dataset_id: Option<String>,
```

### 4.3 Domain type

Edit [`crates/database/src/types.rs`](../../crates/database/src/types.rs) line 66:

```rust
// before
pub dataset_id: Uuid,
// after
pub dataset_id: Option<Uuid>,
```

### 4.4 Conversions

Edit [`crates/database/src/conversions.rs`](../../crates/database/src/conversions.rs):

```rust
// line 301 — entity → domain
dataset_id: m.dataset_id
    .as_deref()
    .map(uuid_hex::from_hex)
    .transpose()
    .expect("DB stores only valid UUID hex strings; corruption indicates data integrity failure"),

// line 316 — domain → entity
dataset_id: Set(r.dataset_id.map(uuid_hex::to_hex)),
```

### 4.5 Repository

Edit [`crates/database/src/pipelines/sea_orm_impl.rs`](../../crates/database/src/pipelines/sea_orm_impl.rs):

**Remove the silent-drop early return** (lines 50-58):

```rust
// DELETE this block entirely:
// let dataset_id_val = match dataset_id {
//     Some(id) => id,
//     None => return Ok(row_id),
// };
```

Replace the `dataset_id` ActiveValue write (line 66):

```rust
// before
dataset_id: sea_orm::ActiveValue::Set(uuid_hex::to_hex(dataset_id_val)),
// after
dataset_id: sea_orm::ActiveValue::Set(dataset_id.map(uuid_hex::to_hex)),
```

Update `latest_status`'s `HashMap<Uuid, PipelineRunStatus>` key handling (~line 102). The map key is the dataset id; rows whose `dataset_id` is `None` are skipped (they don't belong to any dataset bucket):

```rust
for row in rows {
    let run: PipelineRun = row.into();
    if let Some(did) = run.dataset_id {
        result.entry(did).or_insert(run.status);
    }
    // Ad-hoc rows (dataset_id = None) are not surfaced by latest_status — they
    // are not associated with any dataset bucket the caller asked about.
}
```

Update `reset_orphans` ActiveModel construction (~line 300) — `orphan.dataset_id` is now `Option<String>`; pass it through directly.

### 4.6 HTTP-server activity router

Edit [`crates/http-server/src/routers/activity.rs`](../../crates/http-server/src/routers/activity.rs) around line 81:

```rust
// `r.dataset_id` is now Option<Uuid> at the source, but
// list_recent_with_attribution still emits the projection's own field —
// confirm whether the projection's row already uses Option (it does — see
// `sea_orm_impl.rs:203-206` which already maps zero-UUID to None for the
// projection). After 4.5, the projection should pass through the column
// nullability directly without the zero-UUID workaround.
```

Specifically, `list_recent_with_attribution`'s projection rebuild (~line 200) currently treats `dataset_id_hex.parse()` failures as `None`. After this task, the LEFT JOIN can yield a genuine NULL `dataset_id`; update the projection to read `dataset_id: Option<String>` from the entity row and `transpose` to `Option<Uuid>`:

```rust
// before (line 197 area)
dataset_id_hex,
// after
dataset_id_hex: Option<String>,
// and at line 206
let dataset_uuid = dataset_id_hex.as_deref().and_then(|s| uuid_hex::from_hex(s).ok());
```

### 4.7 Anywhere else that constructs `PipelineRun` or reads `.dataset_id`

Run:

```bash
rg "PipelineRun\s*\{|PipelineRunRow\s*\{|PipelineRunWithAttributionRow\s*\{" crates/
rg "\.dataset_id" crates/ | grep -v test | grep -v Cargo
```

Update every construction site to `Option<Uuid>`. Common sites:
- `crates/core/src/pipeline_run_registry/scoped_watcher.rs` — `log_pipeline_run` call sites pass `dataset_id: Option<Uuid>` from `RunSpec` already; the silent-drop removal in §4.5 makes those persist now. No code change needed here, but verify behaviour with the new tests in task 09.
- `crates/database/src/pipelines/repository.rs` — `latest_status`'s return signature `HashMap<Uuid, PipelineRunStatus>` stays unchanged (rows without a dataset are filtered out per §4.5).

### 4.8 Lockfile / build

```bash
cargo check --all-targets
```

No new deps; the diff is pure source.

## 5. Verification

```bash
# 1. Workspace compiles end-to-end.
cargo check --all-targets

# 2. Run the existing pipeline-run-repository tests against the new schema.
cargo test -p cognee-database --test pipeline_run_repository -- --nocapture

# 3. Verify the migration applies cleanly against the existing fixture DB.
cargo test -p cognee-database -- migration_smoke

# 4. The repo no longer silently drops rows when dataset_id is None.
#    Add a quick inline check (task 09 covers this; this command just sanity-runs):
cargo test -p cognee-database --test pipeline_run_repository \
    -- log_pipeline_run_persists_with_none_dataset_id

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/database/src/migrator/m20260901_000003_pipeline_run_dataset_nullable.rs`](../../crates/database/src/migrator/m20260901_000003_pipeline_run_dataset_nullable.rs) — **NEW**.
- [`crates/database/src/migrator/mod.rs`](../../crates/database/src/migrator/mod.rs) — register migration.
- [`crates/database/src/entities/pipeline_run.rs`](../../crates/database/src/entities/pipeline_run.rs) — `dataset_id: Option<String>`.
- [`crates/database/src/types.rs`](../../crates/database/src/types.rs) — `dataset_id: Option<Uuid>`.
- [`crates/database/src/conversions.rs`](../../crates/database/src/conversions.rs) — bidirectional `Option<String>` ↔ `Option<Uuid>`.
- [`crates/database/src/pipelines/sea_orm_impl.rs`](../../crates/database/src/pipelines/sea_orm_impl.rs) — remove silent-drop branch; pass through `Option<String>` on insert; filter `None` rows in `latest_status`; update `reset_orphans`.
- [`crates/http-server/src/routers/activity.rs`](../../crates/http-server/src/routers/activity.rs) — projection now accepts genuine NULL dataset_id.
- (Possibly) other crates that construct `PipelineRun` or `PipelineRunRow` directly — discover via `rg`.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| SQLite table-rebuild migration on a live deployment loses data if interrupted | Low — SeaORM migrations run in a transaction, and the rebuild is INSERT-SELECT inside that transaction. | Add a sanity check in the migration: `SELECT COUNT(*) FROM pipeline_runs` before drop, then after rename — fail-fast on mismatch. |
| Postgres FK name lookup fails on databases where the constraint was renamed manually | Low | The `DO` block uses `pg_constraint` introspection rather than hard-coding the name. |
| Downstream consumers that destructure `PipelineRun { dataset_id, .. }` break at compile time | Medium — desired outcome of the change. | The compiler enumerates every site; fix each in §4.6 / §4.7. |
| Cross-SDK harness fails because Python's existing rows have non-null dataset_id (unchanged) but Rust now writes NULL on ad-hoc paths | Low — Python's column accepts NULL. | Task 09 cross-SDK test covers both directions. |
| Down migration is intentionally `Err` — embedders running `MigratorTrait::down()` in test fixtures break | Low | Document in commit body. Test fixtures should use fresh in-memory DBs anyway. |
| `latest_status` semantics change: rows with `dataset_id IS NULL` no longer surface | Medium — desired behaviour (matches Python, which keys by dataset). | Document in the trait doc-comment; add a test in task 09 that asserts `None`-dataset rows are silently filtered. |
| LIB-06 `pipeline_run_payload_fields` sidecar references `pipeline_run_id` (not `dataset_id`), so it is unaffected | Low | No change needed. |

## 8. Out of scope

- Adding `dataset_id` back to ad-hoc HTTP runs (none exist in P3). The change is purely schema-level.
- Renaming the column or adding a per-tenant dataset id. Out of scope.
- Migrating `task_runs.pipeline_run_id` to nullable. Different table; different gap.
- Updating the cross-SDK schema-parity test (`e2e-cross-sdk/test_pipeline_runs_parity.py` does not exist yet; lands in task 08-09).
