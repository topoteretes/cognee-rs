# Implementation Plan: Dataset Management (Gap 05)

Reference: [../05-dataset-management.md](../05-dataset-management.md)

---

## Overview

Create a `DatasetManager` facade in `cognee-lib` that composes the existing
`IngestDb`, `DeleteDb`, and `AclDb` traits into a high-level dataset management
API matching the Python SDK's `datasets` class.

Most of the underlying infrastructure already exists:
- `DeleteDb` has `list_datasets_by_owner`, `get_dataset_data`, `list_datasets`,
  `delete_dataset`, `delete_data`, `detach_data_from_dataset`
- `AclDb` has `has_permission`, `authorized_dataset_ids`
- `AuthorizedDeleteService` already wraps `DeleteService` with ACL enforcement
- `PipelineRunStatus` enum and `pipeline_runs` table/ops already exist
- `get_dataset(id)` function exists in `ops/datasets.rs` but is not exposed via
  any trait
- `get_latest_pipeline_status(pipeline_name, dataset_id)` function exists in
  `ops/pipeline_runs.rs` but is not exposed via any trait

---

## Step 1: Expose missing methods on DB traits

### 1a. Add `get_dataset(id)` to `IngestDb`

File: `crates/database/src/traits/ingest_db.rs`

```rust
async fn get_dataset(&self, id: Uuid) -> Result<Option<Dataset>, DatabaseError>;
```

The implementation already exists in `ops/datasets::get_dataset`. Wire it in
the `impl IngestDb for DatabaseConnection` block.

### 1b. Add `list_datasets_by_owner` to `IngestDb`

File: `crates/database/src/traits/ingest_db.rs`

```rust
async fn list_datasets_by_owner(
    &self,
    owner_id: Uuid,
) -> Result<Vec<Dataset>, DatabaseError>;
```

Already implemented in `ops/datasets::list_datasets_by_owner` and exposed on
`DeleteDb`. Adding it to `IngestDb` avoids forcing callers to depend on the
delete trait just to list datasets.

### 1c. Add `count_dataset_data` to `DeleteDb`

File: `crates/database/src/traits/delete_db.rs`

```rust
async fn count_dataset_data(&self, dataset_id: Uuid) -> Result<usize, DatabaseError>;
```

New implementation in `ops/datasets.rs` using
`SELECT COUNT(*) FROM dataset_data WHERE dataset_id = ?`. Avoids loading all
`Data` records for the `has_data` check.

### 1d. Expose `get_latest_pipeline_status` on a DB trait

Option A (preferred): Add to `IngestDb` since pipeline status is a read-path
concern, not delete-specific.

```rust
async fn get_latest_pipeline_status(
    &self,
    pipeline_name: &str,
    dataset_id: Uuid,
) -> Result<Option<PipelineRunStatus>, DatabaseError>;
```

The implementation already exists in `ops/pipeline_runs::get_latest_pipeline_status`.

---

## Step 2: Create `DatasetManager` facade

### File: `crates/lib/src/api/datasets.rs`

```rust
use std::collections::HashMap;
use std::sync::Arc;

use cognee_database::{AclDb, DatabaseError, DeleteDb, IngestDb, PipelineRunStatus};
use cognee_models::{Data, Dataset};
use uuid::Uuid;

use super::error::DatasetError;

pub struct DatasetManager {
    db: Arc<dyn DatasetDb>,
    acl_db: Option<Arc<dyn AclDb>>,
}
```

Where `DatasetDb` is a convenience super-trait:

```rust
/// Combined trait for dataset operations.
/// Any `DatabaseConnection` implements all three component traits,
/// so it automatically satisfies this super-trait.
pub trait DatasetDb: IngestDb + DeleteDb + Send + Sync {}
impl<T: IngestDb + DeleteDb + Send + Sync> DatasetDb for T {}
```

### Constructor

```rust
impl DatasetManager {
    pub fn new(db: Arc<dyn DatasetDb>) -> Self {
        Self { db, acl_db: None }
    }

    pub fn with_acl(mut self, acl_db: Arc<dyn AclDb>) -> Self {
        self.acl_db = Some(acl_db);
        self
    }
}
```

### Methods

#### `list_datasets`

```rust
pub async fn list_datasets(&self, owner_id: Uuid) -> Result<Vec<Dataset>, DatasetError> {
    if let Some(acl) = &self.acl_db {
        let authorized_ids = acl.authorized_dataset_ids(owner_id, "read").await?;
        let mut datasets = Vec::with_capacity(authorized_ids.len());
        for id in authorized_ids {
            if let Some(ds) = self.db.get_dataset(id).await? {
                datasets.push(ds);
            }
        }
        Ok(datasets)
    } else {
        Ok(self.db.list_datasets_by_owner(owner_id).await?)
    }
}
```

#### `list_data`

```rust
pub async fn list_data(
    &self,
    dataset_id: Uuid,
    owner_id: Uuid,
) -> Result<Vec<Data>, DatasetError> {
    self.check_read_permission(owner_id, dataset_id).await?;
    Ok(self.db.get_dataset_data(dataset_id).await?)
}
```

#### `has_data`

```rust
pub async fn has_data(&self, dataset_id: Uuid) -> Result<bool, DatasetError> {
    let count = self.db.count_dataset_data(dataset_id).await?;
    Ok(count > 0)
}
```

#### `get_status`

```rust
pub async fn get_status(
    &self,
    dataset_ids: &[Uuid],
) -> Result<HashMap<Uuid, PipelineRunStatus>, DatasetError> {
    let mut statuses = HashMap::with_capacity(dataset_ids.len());
    for &id in dataset_ids {
        if let Some(status) = self
            .db
            .get_latest_pipeline_status("cognify_pipeline", id)
            .await?
        {
            statuses.insert(id, status);
        }
    }
    Ok(statuses)
}
```

Note: Datasets not present in the result map have no pipeline runs (equivalent
to Python's "not started" behavior where the key simply does not appear).

#### `discover_datasets`

```rust
pub fn discover_datasets(directory_path: &std::path::Path) -> Result<Vec<String>, DatasetError> {
    let mut datasets = Vec::new();
    for entry in std::fs::read_dir(directory_path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                datasets.push(name.to_owned());
            }
        }
    }
    Ok(datasets)
}
```

This is a sync utility (matching the Python version which is also sync).

#### `empty_dataset` / `delete_data` / `delete_all`

These delegate to the existing `DeleteService` / `AuthorizedDeleteService`.
Rather than duplicating cascade logic, the facade constructs the appropriate
`DeleteRequest` and forwards:

```rust
pub async fn empty_dataset(
    &self,
    dataset_id: Uuid,
    owner_id: Uuid,
    delete_service: &DeleteService,
) -> Result<DeleteResult, DatasetError> {
    let dataset = self.require_dataset(dataset_id).await?;
    self.check_delete_permission(owner_id, dataset_id).await?;
    let request = DeleteRequest {
        scope: DeleteScope::Dataset {
            owner_id,
            dataset_name: dataset.name,
        },
        mode: DeleteMode::Hard,
    };
    Ok(delete_service.execute(&request).await?)
}

pub async fn delete_data(
    &self,
    dataset_id: Uuid,
    data_id: Uuid,
    owner_id: Uuid,
    mode: DeleteMode,
    delete_dataset_if_empty: bool,
    delete_service: &DeleteService,
) -> Result<DeleteResult, DatasetError> {
    let dataset = self.require_dataset(dataset_id).await?;
    self.check_delete_permission(owner_id, dataset_id).await?;
    let request = DeleteRequest {
        scope: DeleteScope::Data {
            owner_id,
            data_id,
            dataset_name: Some(dataset.name),
            delete_dataset_if_empty,
        },
        mode,
    };
    Ok(delete_service.execute(&request).await?)
}

pub async fn delete_all(
    &self,
    owner_id: Uuid,
    delete_service: &DeleteService,
) -> Result<Vec<DeleteResult>, DatasetError> {
    let datasets = self.list_datasets(owner_id).await?;
    let mut results = Vec::with_capacity(datasets.len());
    for ds in datasets {
        let request = DeleteRequest {
            scope: DeleteScope::Dataset {
                owner_id,
                dataset_name: ds.name,
            },
            mode: DeleteMode::Hard,
        };
        results.push(delete_service.execute(&request).await?);
    }
    Ok(results)
}
```

### Helper methods

```rust
async fn check_read_permission(
    &self,
    owner_id: Uuid,
    dataset_id: Uuid,
) -> Result<(), DatasetError> {
    if let Some(acl) = &self.acl_db {
        if !acl.has_permission(owner_id, dataset_id, "read").await? {
            return Err(DatasetError::PermissionDenied);
        }
    }
    Ok(())
}

async fn check_delete_permission(
    &self,
    owner_id: Uuid,
    dataset_id: Uuid,
) -> Result<(), DatasetError> {
    if let Some(acl) = &self.acl_db {
        if !acl.has_permission(owner_id, dataset_id, "delete").await? {
            return Err(DatasetError::PermissionDenied);
        }
    }
    Ok(())
}

async fn require_dataset(&self, id: Uuid) -> Result<Dataset, DatasetError> {
    self.db
        .get_dataset(id)
        .await?
        .ok_or(DatasetError::NotFound)
}
```

---

## Step 3: Create `DatasetError` type

### File: `crates/lib/src/api/error.rs`

```rust
use cognee_database::DatabaseError;
use cognee_delete::DeleteError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DatasetError {
    #[error("permission denied")]
    PermissionDenied,

    #[error("dataset not found")]
    NotFound,

    #[error("database error: {0}")]
    Database(#[from] DatabaseError),

    #[error("delete error: {0}")]
    Delete(#[from] DeleteError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
```

---

## Step 4: Wire into `cognee-lib`

### File: `crates/lib/src/api/mod.rs` (new)

```rust
pub mod datasets;
pub mod error;

pub use datasets::DatasetManager;
pub use error::DatasetError;
```

### File: `crates/lib/src/lib.rs` (modify)

Add:
```rust
pub mod api;
pub use api::{DatasetManager, DatasetError};
```

And in the `prelude` module:
```rust
pub use crate::api::DatasetManager;
```

---

## Step 5: Add CLI subcommand (optional follow-up)

Extend `crates/cli/src/commands/` with a `datasets` subcommand group:

```
cognee datasets list [--owner-id UUID]
cognee datasets list-data --dataset-id UUID [--owner-id UUID]
cognee datasets has-data --dataset-id UUID
cognee datasets status --dataset-id UUID [--dataset-id UUID ...]
cognee datasets discover --path DIR
```

This is a convenience enhancement and can be deferred.

---

## Files to Create

| File | Purpose |
|------|---------|
| `crates/lib/src/api/mod.rs` | API module declaration |
| `crates/lib/src/api/datasets.rs` | `DatasetManager` facade |
| `crates/lib/src/api/error.rs` | `DatasetError` enum |

## Files to Modify

| File | Change |
|------|--------|
| `crates/database/src/traits/ingest_db.rs` | Add `get_dataset`, `list_datasets_by_owner`, `get_latest_pipeline_status` |
| `crates/database/src/traits/delete_db.rs` | Add `count_dataset_data` |
| `crates/database/src/ops/datasets.rs` | Add `count_dataset_data` implementation |
| `crates/lib/src/lib.rs` | Add `pub mod api` and re-exports |

## No New Tables Required

The `pipeline_runs` table and `PipelineRunStatus` enum already exist. The
`get_latest_pipeline_status` function is already implemented in
`ops/pipeline_runs.rs` -- it just needs to be exposed on a trait.

---

## Testing Strategy

1. **Unit tests** in `crates/lib/src/api/datasets.rs`:
   - `DatasetManager` with mock DB (in-memory SQLite) and no ACL
   - `DatasetManager` with ACL enforcement (mock `AclDb`)
   - `has_data` returns correct bool
   - `get_status` returns correct map
   - `discover_datasets` finds directories

2. **Integration tests** in `crates/lib/tests/`:
   - Full flow: add data via `AddPipeline`, then use `DatasetManager` to list,
     check status, delete
   - ACL enforcement: verify permission denied errors

3. **CLI E2E tests** (if CLI subcommand is added):
   - `cognee datasets list` after adding data
   - `cognee datasets status` after cognify
