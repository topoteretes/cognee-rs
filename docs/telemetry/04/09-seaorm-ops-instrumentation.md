# Task 04-09 — Instrument `crates/database/src/ops/*.rs` (ops-level)

**Status**: ✅ implemented in commit 176301b (~93 `#[instrument]` annotations across 13 ops files plus the `database_system_label` helper in `lib.rs`; `mod.rs` is not an ops file, so the actual file count is 13 rather than the 14 estimated above)
**Owner**: _unassigned_
**Depends on**:
- [Task 04-02](02-tracing-constants-dedupe.md) — `cognee_utils::tracing_keys::COGNEE_DB_SYSTEM`.
**Blocks**:
- [Task 04-10 — Tests](10-tests.md) — relational-side test cases.

**Parent doc**: [04 — DB-Adapter Span Instrumentation](../04-db-adapter-instrumentation.md)
**Locked decisions**: #1 (ops-level only, NOT per-query), #5 (INFO level), #8 (no feature gate).

---

## 1. Goal

Add `cognee.db.relational.<op>` spans to every public function in
[`crates/database/src/ops/`](../../crates/database/src/ops/), one
span per public op. The span name is `cognee.db.relational.<file>.<fn>`
(see [§4.1](#41-naming-convention) for the exact convention).

Files in scope (15 total):

| File | Public ops to instrument |
|---|---|
| [`acl.rs`](../../crates/database/src/ops/acl.rs) | every `pub async fn` |
| [`checkpoint.rs`](../../crates/database/src/ops/checkpoint.rs) | every `pub async fn` |
| [`data.rs`](../../crates/database/src/ops/data.rs) | `create_data`, `get_data`, `delete_data`, `update_data`, `count_data_dataset_links`, `update_data_token_count`, `update_last_accessed`, `clear_pipeline_status_for_dataset`, `list_datasets_for_data`, … (whatever `pub async fn` exist) |
| [`datasets.rs`](../../crates/database/src/ops/datasets.rs) | `create_dataset`, `get_dataset`, `get_dataset_by_name`, `list_datasets_by_owner`, `list_datasets`, `delete_dataset`, `attach_data_to_dataset`, `detach_data_from_dataset`, `count_dataset_data`, `get_dataset_data`, … |
| [`graph_storage.rs`](../../crates/database/src/ops/graph_storage.rs) | every `pub async fn` |
| [`notebooks.rs`](../../crates/database/src/ops/notebooks.rs) | every `pub async fn` |
| [`pipeline_runs.rs`](../../crates/database/src/ops/pipeline_runs.rs) | `create_pipeline_run`, `update_pipeline_run_status`, `get_pipeline_run`, `delete_pipeline_runs_by_dataset`, `get_latest_pipeline_status` |
| [`role.rs`](../../crates/database/src/ops/role.rs) | every `pub async fn` |
| [`search_history.rs`](../../crates/database/src/ops/search_history.rs) | every `pub async fn` |
| [`session_lifecycle.rs`](../../crates/database/src/ops/session_lifecycle.rs) | every `pub async fn` |
| [`task_runs.rs`](../../crates/database/src/ops/task_runs.rs) | every `pub async fn` |
| [`tenant.rs`](../../crates/database/src/ops/tenant.rs) | every `pub async fn` |
| [`tutorial_seeder.rs`](../../crates/database/src/ops/tutorial_seeder.rs) | every `pub async fn` |
| [`user.rs`](../../crates/database/src/ops/user.rs) | every `pub async fn` |

`mod.rs` only re-exports; no instrumentation there.

Required attributes per span:

| Attribute | Value |
|---|---|
| `cognee.db.system` | `"sqlite"` or `"postgres"` — derived at task time, see [§4.2](#42-system-attribute) |
| `cognee.db.row_count` | only when the function returns a `Vec<_>` or a `count`-style number |

`cognee.db.query` is **not** set on relational ops — locked decision
1 keeps this at op-level; we don't see the SQL string at this layer.

## 2. Rationale

Locked decision 1 picked ops-level only. Adding spans here gives
OTLP consumers a high-level picture (`pipeline_runs::create_pipeline_run`,
`datasets::list_datasets_by_owner`) without the noise of per-`sqlx`
spans (which SeaORM already emits via the `sqlx::query` log target,
already routed through tracing).

Naming convention: `cognee.db.relational.<file_stem>.<fn_name>`
(e.g. `cognee.db.relational.datasets.list_datasets_by_owner`). This
mirrors how Python would name them if it instrumented its
`sqlalchemy/operations/*.py`. Python doesn't, but the precedent is
unambiguous.

## 3. Pre-conditions

- Task 04-02 (constants dedupe) is complete.
- `cognee-database` does **not** currently depend on `cognee-utils`.
  This task adds that edge.
- A clean `cargo check --all-targets` on `main`.
- Tasks 04-04 and 04-05 are landed (the implementor copies their
  patterns).

## 4. Step-by-step

### 4.1 Naming convention

Use `name = "cognee.db.relational.<file_stem>.<fn>"` for every span.
Example: `pipeline_runs.rs::create_pipeline_run` →
`name = "cognee.db.relational.pipeline_runs.create_pipeline_run"`.

The dotted name is a single literal in the `#[instrument]` macro and
does **not** create nested spans — the dots are just separators in
the recorded span name string.

### 4.2 `cognee.db.system` attribute

The relational layer is shared between SQLite and Postgres backends
selected at runtime via the `DatabaseConnection` URL. Reading the
URL inside every op to set the right system value is fiddly and
slow.

**Two options:**

- **A**. Hard-code `cognee.db.system = "sqlite"` for all ops, with a
  documentation note that operators on Postgres see the same string.
  Wrong but simple.
- **B**. Read `db.get_database_backend()` (SeaORM provides this) at
  the top of each op and record dynamically.

**Pick option B**. SeaORM's `DatabaseBackend::name()` (or
`Debug` representation) maps cleanly: `Sqlite` → `"sqlite"`,
`Postgres` → `"postgres"`, `MySql` → `"mysql"`. Add a small helper
in `cognee-database`:

```rust
// crates/database/src/lib.rs (or a new helper module)

/// Map the active SeaORM backend to a `cognee.db.system` string
/// matching the values used by the vector / graph adapters.
pub fn database_system_label(db: &DatabaseConnection) -> &'static str {
    use sea_orm::DatabaseBackend::*;
    match db.get_database_backend() {
        Sqlite => "sqlite",
        Postgres => "postgres",
        MySql => "mysql",
    }
}
```

Then in each op:

```rust
let system = database_system_label(db);
tracing::Span::current().record(COGNEE_DB_SYSTEM, system);
```

`cognee.db.system` is declared as `tracing::field::Empty` in the
macro and recorded inside the function body. This costs one
enum-match per op call, which is negligible.

### 4.3 The instrumentation pattern

Pick `crates/database/src/ops/data.rs::create_data` (representative)
as the template:

```rust
// Before:
pub async fn create_data(db: &DatabaseConnection, d: Data) -> Result<Data, DatabaseError> {
    // body
}

// After:
use cognee_utils::tracing_keys::COGNEE_DB_SYSTEM;
use tracing::{Span, instrument};

#[instrument(
    name = "cognee.db.relational.data.create_data",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
    ),
    err,
)]
pub async fn create_data(db: &DatabaseConnection, d: Data) -> Result<Data, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    // body
}
```

For ops that return `Vec<T>` or a count, add `cognee.db.row_count`
to the field list and record the length at the end:

```rust
#[instrument(
    name = "cognee.db.relational.datasets.list_datasets_by_owner",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
pub async fn list_datasets_by_owner(
    db: &DatabaseConnection,
    owner_id: Uuid,
) -> Result<Vec<Dataset>, DatabaseError> {
    Span::current().record(COGNEE_DB_SYSTEM, database_system_label(db));
    let rows = /* existing body */;
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}
```

Do **not** annotate the input arguments. `skip_all` is the right
default — most arguments are entity IDs (UUIDs) that already appear
in higher-level spans, and a few are full `Data` / `Dataset`
structs (large, possibly sensitive).

### 4.4 Adding the dep

Edit [`crates/database/Cargo.toml`](../../crates/database/Cargo.toml).
Confirm `tracing = { workspace = true }` is present (the crate
already uses tracing via SeaORM). Add:

```toml
[dependencies]
# ... existing ...
cognee-utils = { path = "../utils" }
```

### 4.5 The helper function

Add `database_system_label` to
[`crates/database/src/lib.rs`](../../crates/database/src/lib.rs)
(or a new `crates/database/src/system_label.rs` module — implementor's
choice; the function is tiny). Re-export from `lib.rs`:

```rust
pub use system_label::database_system_label;
```

Or, if inlined into `lib.rs`, simply make it `pub fn`.

### 4.6 Rolling pattern

Apply the pattern in [§4.3](#43-the-instrumentation-pattern) to every
public function in the 14 op files (skip `mod.rs`). Sub-agent B
should:

1. Open each file.
2. List `pub async fn` (and `pub fn`) signatures.
3. Add the `#[instrument]` block with the file-stem-based name.
4. Add the `Span::current().record(COGNEE_DB_SYSTEM, …)` line as the
   first statement.
5. If the function returns a `Vec<T>` or a count, add
   `cognee.db.row_count` to the field list and record at the end.
6. Add the imports once per file.

Per-file diff example (skeleton):

```rust
// Top of file
use cognee_utils::tracing_keys::{COGNEE_DB_ROW_COUNT, COGNEE_DB_SYSTEM};
use tracing::{Span, instrument};

use crate::database_system_label;

// Each pub async fn gets the macro + first-line record.
```

The mechanical bulk is high (~50–100 functions across 14 files),
but each diff hunk is identical in shape. A scripted regex pass
could be used, but a careful manual sweep is safer because some ops
return tuples or option types that need the row_count case decided
case-by-case.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. cognee-database compiles in isolation.
cargo check -p cognee-database

# 3. Existing relational tests still pass.
cargo test -p cognee-database

# 4. Smoke test against in-memory SQLite.
cargo test -p cognee-database --test datasets

# 5. Clippy.
cargo clippy --all-targets -- -D warnings

# 6. Full check.
scripts/check_all.sh
```

Real attribute coverage tests land in [task 04-10](10-tests.md):
one representative test per op file, asserting the span name and
`cognee.db.system="sqlite"` (in-memory).

## 6. Files modified

- [`crates/database/Cargo.toml`](../../crates/database/Cargo.toml) —
  add `cognee-utils = { path = "../utils" }`.
- [`crates/database/src/lib.rs`](../../crates/database/src/lib.rs) —
  add `database_system_label` helper (or `pub use` from a new module).
- [`crates/database/src/ops/acl.rs`](../../crates/database/src/ops/acl.rs)
- [`crates/database/src/ops/checkpoint.rs`](../../crates/database/src/ops/checkpoint.rs)
- [`crates/database/src/ops/data.rs`](../../crates/database/src/ops/data.rs)
- [`crates/database/src/ops/datasets.rs`](../../crates/database/src/ops/datasets.rs)
- [`crates/database/src/ops/graph_storage.rs`](../../crates/database/src/ops/graph_storage.rs)
- [`crates/database/src/ops/notebooks.rs`](../../crates/database/src/ops/notebooks.rs)
- [`crates/database/src/ops/pipeline_runs.rs`](../../crates/database/src/ops/pipeline_runs.rs)
- [`crates/database/src/ops/role.rs`](../../crates/database/src/ops/role.rs)
- [`crates/database/src/ops/search_history.rs`](../../crates/database/src/ops/search_history.rs)
- [`crates/database/src/ops/session_lifecycle.rs`](../../crates/database/src/ops/session_lifecycle.rs)
- [`crates/database/src/ops/task_runs.rs`](../../crates/database/src/ops/task_runs.rs)
- [`crates/database/src/ops/tenant.rs`](../../crates/database/src/ops/tenant.rs)
- [`crates/database/src/ops/tutorial_seeder.rs`](../../crates/database/src/ops/tutorial_seeder.rs)
- [`crates/database/src/ops/user.rs`](../../crates/database/src/ops/user.rs)

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Mass-edit introduces a compile break in one file | Real — 14 files, ~80 functions. | Sub-agent C runs `cargo check --all-targets` after each task; sub-agent B should do per-file commits in their working tree (still one task-level commit at the end via sub-agent D — but the implementor can stage progress as they go). Or run `cargo check -p cognee-database` after each file. |
| `database_system_label` returns the wrong system in tests because in-memory SQLite is named differently by SeaORM | Low — SeaORM's `DatabaseBackend::Sqlite` is the same enum variant for any SQLite URL. | Verify with the inline test in 4.5. |
| Span volume is too high (every op call produces a span) | Real for high-throughput paths like `update_last_accessed` if called per-row. | Subscribers can filter by name; operators can set `RUST_LOG=info,cognee.db.relational=warn` to suppress. Locked decision 5 keeps INFO for parity; if volume becomes a problem, individual ops can be down-leveled to DEBUG case-by-case in a follow-up. |
| `mod.rs` re-exports rust-doc through unannotated paths and surfaces the macro as part of the public API | Acceptable — `#[instrument]` is a no-op visible attribute; the function signature is unchanged. | n/a |
| `Result<Option<T>, DatabaseError>` returns: do we record `row_count = 1` for `Some` and `0` for `None`? | Implementor's call. Recommend: yes, record. | Document in the implementor's commit message. |

## 8. Out of scope

- Instrumenting individual SeaORM `Statement` calls. SeaORM already
  emits `sqlx::query` events through tracing's log target.
- Adding a `cognee.db.query` field to relational ops. Locked
  decision 1 — ops-level only, no SQL text.
- Refactoring the ops modules into a trait-based abstraction.
- Touching `migrations/`. Migrations run once at startup and are
  not on the hot path.
- Touching `crates/database/src/connection.rs` (`connect`,
  `initialize`). Setup paths; instrumentation cost-to-value is low.
