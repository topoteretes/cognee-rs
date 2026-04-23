# Gap 5: Dataset Management

> **Status:** Implemented
>
> **Implementation plan:** [impl/05-dataset-management-plan.md](impl/05-dataset-management-plan.md)

This document details the dataset management capabilities present in the Python SDK's `datasets` class that are absent from the Rust implementation.

---

## Python Dataset Management API

**File:** `cognee/api/v1/datasets/datasets.py`

The Python SDK provides a `datasets` class with 8 static methods for dataset CRUD operations:

| Method | Signature | Purpose |
|--------|-----------|---------|
| `list_datasets` | `(user=None) -> list[Dataset]` | List all datasets user has "read" permission on |
| `discover_datasets` | `(directory_path: str) -> list[str]` | Scan filesystem directory for dataset folders (sync) |
| `list_data` | `(dataset_id: UUID, user=None) -> list[Data]` | List all data items in a dataset |
| `has_data` | `(dataset_id: str, user=None) -> bool` | Check if dataset contains any data |
| `get_status` | `(dataset_ids: list[UUID]) -> dict` | Get cognify pipeline status for datasets |
| `empty_dataset` | `(dataset_id: UUID, user=None) -> None` | Delete all data and graph nodes, then the dataset record |
| `delete_data` | `(dataset_id, data_id, user, mode="soft", delete_dataset_if_empty=False)` | Delete a specific data item |
| `delete_all` | `(user=None) -> None` | Delete all user's datasets |

### Method Details

**`list_datasets(user=None)`** (line 48):
- Defaults to `get_default_user()` if user is None
- Calls `get_authorized_existing_datasets([], "read", user)` -- ACL-filtered query
- Returns datasets the user has "read" permission on

**`discover_datasets(directory_path)`** (line 55):
- Synchronous method (not async)
- Calls `discover_directory_datasets(directory_path)` which walks the directory tree
- Returns list of dataset folder names (dot-separated for nested dirs)

**`list_data(dataset_id, user=None)`** (line 59):
- Permission-checks via `get_authorized_dataset(user, dataset_id)`
- Returns all `Data` records linked to the dataset

**`has_data(dataset_id, user=None)`** (line 70):
- Note: `dataset_id` parameter is typed `str` in the Python source (not `UUID`)
- Returns `bool` -- whether the dataset has any associated data records

**`get_status(dataset_ids)`** (line 79):
- Queries `pipeline_runs` table for the latest run per dataset, filtered by `pipeline_name="cognify_pipeline"`
- Returns dict mapping `str(dataset_id) -> PipelineRunStatus` string value
- Pipeline states: `DATASET_PROCESSING_INITIATED`, `DATASET_PROCESSING_STARTED`, `DATASET_PROCESSING_COMPLETED`, `DATASET_PROCESSING_ERRORED`

**`empty_dataset(dataset_id, user=None)`** (line 83):
- Requires "delete" permission via `get_authorized_dataset(user, dataset_id, "delete")`
- Deletes graph nodes/edges for the dataset first
- Deletes the dataset record (while junction rows still exist for pipeline_status cleanup)
- Then deletes individual data records using `asyncio.gather()` with error tolerance

**`delete_data(dataset_id, data_id, user, mode, delete_dataset_if_empty)`** (line 124):
- `mode` parameter exists for backwards compatibility; "hard" is discouraged
- If the data item is not found in the system, assumes custom graph model and deletes nodes/edges directly
- Checks for related graph nodes before choosing delete strategy
- `delete_dataset_if_empty=True`: auto-deletes dataset if last item removed

**`delete_all(user=None)`** (line 178):
- Uses `get_authorized_existing_datasets([], "delete", user)` for ACL-filtered list
- Iterates all authorized datasets and calls `empty_dataset()` on each

---

## Rust Existing Capabilities

### IngestDb Trait

**File:** `crates/database/src/traits/ingest_db.rs`

```rust
pub trait IngestDb: Send + Sync {
    async fn get_dataset_by_name(&self, name: &str, owner_id: Uuid, tenant_id: Option<Uuid>) -> Result<Option<Dataset>>;
    async fn create_dataset(&self, dataset: Dataset) -> Result<Dataset>;
    async fn get_data(&self, id: Uuid) -> Result<Option<Data>>;
    async fn create_data(&self, d: Data) -> Result<Data>;
    async fn attach_data_to_dataset(&self, dataset_id: Uuid, data_id: Uuid) -> Result<()>;
    async fn update_last_accessed(&self, data_ids: &[Uuid], timestamp: DateTime<Utc>) -> Result<()>;
}
```

No `get_dataset(id)` or `list_datasets_by_owner()` on this trait (those methods exist in `ops/datasets.rs` and on `DeleteDb`, but not on `IngestDb`).

### DeleteDb Trait

**File:** `crates/database/src/traits/delete_db.rs`

Has dataset-related methods:
- `list_datasets_by_owner(owner_id)` -- lists all datasets for an owner (no ACL)
- `list_datasets()` -- lists all datasets globally
- `get_dataset_data(dataset_id)` -- returns all `Data` records for a dataset
- `delete_dataset(id)` -- deletes a dataset record
- `delete_data(id)` -- deletes a data record
- `detach_data_from_dataset(dataset_id, data_id)` -- removes junction row
- Pipeline cleanup: `delete_pipeline_runs_by_dataset`, `clear_pipeline_status_for_dataset`
- Graph provenance: `get_nodes_by_dataset`, `get_edges_by_dataset`, delete methods
- Search history cleanup methods

### AclDb Trait

**File:** `crates/database/src/traits/acl_db.rs`

Full ACL infrastructure exists:
- `has_permission(principal_id, dataset_id, permission_name)` -- check single permission
- `authorized_dataset_ids(principal_id, permission_name)` -- list authorized datasets
- `grant_permission`, `revoke_permission`, `ensure_principal`

### DeleteService and AuthorizedDeleteService

**File:** `crates/delete/src/lib.rs`, `crates/delete/src/authorized.rs`

- `DeleteService` with `DeleteScope::{Data, Dataset, User, All}` and `DeleteMode::{Soft, Hard}`
- `AuthorizedDeleteService` wraps `DeleteService` with ACL enforcement
- `DeleteScope::Data` supports `delete_dataset_if_empty` flag (matching Python)
- Full cascade: relational DB -> graph DB -> vector DB -> file storage

### Pipeline Status Infrastructure

**File:** `crates/database/src/ops/pipeline_runs.rs`, `crates/database/src/types.rs`

- `PipelineRunStatus` enum: `Initiated`, `Started`, `Completed`, `Errored`
- `pipeline_runs` table with `dataset_id`, `pipeline_name`, `status`, `created_at`
- `get_latest_pipeline_status(pipeline_name, dataset_id)` function exists in ops
  but is NOT exposed on any DB trait
- `create_pipeline_run`, `update_pipeline_run_status` functions also exist

### Existing but Unexposed Functions

In `crates/database/src/ops/datasets.rs`:
- `get_dataset(db, id)` -- get dataset by UUID (not on any trait)

In `crates/database/src/ops/pipeline_runs.rs`:
- `get_latest_pipeline_status(db, pipeline_name, dataset_id)` -- not on any trait

---

## Gap Analysis

| Operation | Python | Rust | Gap |
|-----------|--------|------|-----|
| **List datasets (ACL-filtered)** | `list_datasets(user)` | `DeleteDb::list_datasets_by_owner(owner_id)` + `AclDb::authorized_dataset_ids` (separate calls, no facade) | No unified facade |
| **List dataset data** | `list_data(dataset_id, user)` | `DeleteDb::get_dataset_data(dataset_id)` (no ACL) | No ACL check |
| **Check has data** | `has_data(dataset_id, user)` | Not exposed (would require loading all records via `get_dataset_data`) | Missing + no efficient count query |
| **Get pipeline status** | `get_status(dataset_ids)` | `get_latest_pipeline_status` exists in ops but not on any trait | Not exposed as trait method |
| **Discover datasets on disk** | `discover_datasets(path)` | Not implemented | Missing |
| **Get dataset by ID** | Used internally | `ops::datasets::get_dataset(id)` exists but not on `IngestDb` trait | Not on trait |
| **Empty dataset (cascade)** | `empty_dataset(dataset_id, user)` | Via `DeleteService` with `DeleteScope::Dataset` (requires building request manually) | No convenience method |
| **Delete data item** | `delete_data(dataset_id, data_id, user, mode, ...)` | Via `DeleteService` with `DeleteScope::Data` | No convenience method |
| **Delete all user data** | `delete_all(user)` | Via `DeleteService` with `DeleteScope::User` | No convenience method |

### Summary

The underlying infrastructure (DB operations, ACL, pipeline status, delete cascade) is largely in place. The primary gap is the absence of a **high-level `DatasetManager` facade** that:

1. Composes `IngestDb` + `DeleteDb` + `AclDb` into a single API
2. Applies ACL checks transparently
3. Exposes `has_data` with an efficient count query
4. Exposes pipeline status querying via a trait method
5. Provides `discover_datasets` filesystem scanning
6. Wraps `DeleteService` calls with a dataset-centric API
