# Task 23: Align Query Log Table Name -- `queries` Instead of `query_log`

## Summary

The Rust codebase already uses the correct table name `queries` in the migration and SeaORM entity definition, matching the Python `Query` model's `__tablename__ = "queries"`. However, the Rust module filename and internal naming convention uses `query_log` (e.g., `entities/query_log.rs`, `crate::entities::query_log`), which creates a misleading inconsistency when reading the code. This task renames the Rust module and internal references from `query_log` to `query` (singular, matching the Python class name `Query`) to align the codebase naming with the Python convention.

## Current Rust Behavior

### Table name (already correct)

**File:** `crates/database/src/entities/query_log.rs` (line 5)

```rust
#[sea_orm(table_name = "queries")]
pub struct Model { ... }
```

The `table_name` attribute is `"queries"`, which already matches Python's `__tablename__ = "queries"`.

### Module name (inconsistent)

**File:** `crates/database/src/entities/mod.rs` (line 7)

```rust
pub mod query_log;
```

The module is called `query_log`, but the Python class is `Query` and the table is `queries`. There is no `query_log` table in either Python or Rust.

### References using `query_log` module path

**File:** `crates/database/src/conversions.rs` (line 3, 158)

```rust
use crate::entities::{query_log, result_log, task_run};
// ...
pub(crate) fn query_model_to_history(m: query_log::Model) -> SearchHistoryEntry { ... }
```

**File:** `crates/database/src/entities/result_log.rs` (lines 19-28)

```rust
belongs_to = "super::query_log::Entity",
to = "super::query_log::Column::Id",
// ...
impl Related<super::query_log::Entity> for Entity { ... }
```

**File:** `crates/database/src/ops/search_history.rs` (line 8, 19, 53, 55)

```rust
use crate::entities::{query_log, result_log};
let model = query_log::ActiveModel { ... };
let mut q_query = query_log::Entity::find();
q_query = q_query.filter(query_log::Column::UserId.eq(...));
```

## Required Python Behavior

**File:** `/tmp/cognee-python/cognee/modules/search/models/Query.py`

```python
class Query(Base):
    __tablename__ = "queries"

    id = Column(UUID, primary_key=True, default=uuid4)
    text = Column(String)
    query_type = Column(String)
    user_id = Column(UUID)
    created_at = Column(DateTime(timezone=True), default=lambda: datetime.now(timezone.utc))
    updated_at = Column(DateTime(timezone=True), onupdate=lambda: datetime.now(timezone.utc))
```

The Python model is named `Query` (singular), stored in `models/Query.py`, with table name `queries`.

### Column name difference

Python uses `text` as the column name, while Rust uses `query_text`. This is a minor deviation but worth noting. The actual SQLite table is created by the Rust migration with `query_text`, so renaming the column would require a migration. This task focuses only on the module naming; a future task can address column name alignment if needed for cross-SDK compatibility.

## Step-by-Step Changes

### Step 1: Rename the entity module file

Rename `crates/database/src/entities/query_log.rs` to `crates/database/src/entities/query.rs`. No changes to the file contents are needed -- the `#[sea_orm(table_name = "queries")]` annotation already maps to the correct table.

### Step 2: Update `entities/mod.rs`

In `crates/database/src/entities/mod.rs`, change the module declaration:

```rust
// Before
pub mod query_log;

// After
pub mod query;
```

### Step 3: Update `entities/result_log.rs`

In `crates/database/src/entities/result_log.rs`, update the `belongs_to` and `Related` references:

```rust
// Before
belongs_to = "super::query_log::Entity",
to = "super::query_log::Column::Id",
// ...
impl Related<super::query_log::Entity> for Entity {

// After
belongs_to = "super::query::Entity",
to = "super::query::Column::Id",
// ...
impl Related<super::query::Entity> for Entity {
```

### Step 4: Update `conversions.rs`

In `crates/database/src/conversions.rs`, update the import and function parameter type:

```rust
// Before
use crate::entities::{query_log, result_log, task_run};
pub(crate) fn query_model_to_history(m: query_log::Model) -> SearchHistoryEntry {

// After
use crate::entities::{query, result_log, task_run};
pub(crate) fn query_model_to_history(m: query::Model) -> SearchHistoryEntry {
```

### Step 5: Update `ops/search_history.rs`

In `crates/database/src/ops/search_history.rs`, update all `query_log` references:

```rust
// Before
use crate::entities::{query_log, result_log};
let model = query_log::ActiveModel { ... };
let mut q_query = query_log::Entity::find();
q_query = q_query.filter(query_log::Column::UserId.eq(...));

// After
use crate::entities::{query, result_log};
let model = query::ActiveModel { ... };
let mut q_query = query::Entity::find();
q_query = q_query.filter(query::Column::UserId.eq(...));
```

### Step 6: Search for any other references

Run a workspace-wide search for `query_log` (as a Rust module path) and update any remaining references. The migration file (`m20250101_000001_initial_schema.rs`) uses `Queries` enum identifiers, not the `query_log` module, so no changes are needed there.

## Test Verification

1. **Compilation check:** `cargo check --all-targets` -- ensures all module path references resolve correctly after renaming.

2. **Existing test `persists_query_and_result_when_save_interaction_enabled`** in `crates/search/src/orchestration/search_orchestrator.rs` (line 574): This test exercises `log_query` and `get_history` via the `SearchHistoryDb` trait, which calls into the renamed module. Verify it still passes.

3. **Existing migration compat test** in `crates/database/tests/migration_compat.rs` (line 90): Verifies the `queries` table exists. No changes needed since the table name is unchanged.

4. Run `scripts/check_all.sh` to verify formatting, clippy, and all checks pass.

## Dependencies

- No new crate dependencies.
- No migration changes -- the `queries` table name is already correct.
- This is a pure rename/refactor with no runtime behavior change.
