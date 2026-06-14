# 18 — `forget` memory_only + `DatasetManager.create_dataset`

> Wave 4 · Priority P1 (should-fix) · Track A · Release-blocking: no · Effort: 1d ·
> Depends on: [09 — lifecycle destructive ops](09-lifecycle-destructive-ops.md) ·
> Source: [cleanup-and-parity-audit.md](../cleanup-and-parity-audit.md) B6.2, B7.1 · [index](00-INDEX.md)

## Goal

Bring two missing Python facade capabilities to Rust:

1. **`forget` memory-only mode** — add `ForgetTarget::DatasetMemoryOnly` and
   `ForgetTarget::DataItemMemoryOnly` that wipe **graph + vector** for a dataset/data
   item but **preserve raw files + relational `Data`/`Dataset` rows**, and **reset the
   cognify pipeline status** so the data can be re-cognified with different settings.
2. **`DatasetManager.create_dataset` / `create_authorized_dataset`** — a facade that
   creates a dataset with a deterministic ID (matching Python's `uuid5` formula) and,
   for the authorized variant, grants the owner (and any `parent_user_id`) all four ACL
   permissions (`read`, `write`, `delete`, `share`).

End state: a Rust caller can re-cognify without losing ingested files, and can create a
dataset row + ACL grants from the SDK facade exactly as Python does.

## Background & why

Python's `forget()` has **6 targets** (audit B6.2). Rust only has 3
(`Item`/`Dataset`/`All`), all of which run a full cascading delete (graph → vector →
relational → files). There is **no way in Rust to drop only the derived memory** while
keeping the source data — which is the exact workflow for "re-cognify with a new prompt
or graph model." Python implements this as `*_memory_only` targets that skip relational
+ file deletion and reset the cognify pipeline status.

Separately (audit B7.1), Python exposes `create_dataset` / `create_authorized_dataset`
in `modules/data/methods/`, but Rust's `DatasetManager` only has list/query/delete
methods — no create. The HTTP/CLI layers in Rust create datasets implicitly during
`add`, but there is no explicit facade create with ACL granting, so an SDK consumer
can't pre-create an empty, permissioned dataset.

This task depends on **09** because 09 establishes the soft/hard delete + pipeline-reset
machinery this task reuses; do 09 first.

### Python vs Rust at a glance

| Concept | Python | Rust (current) |
|---|---|---|
| forget targets | `everything`, `dataset`, `data_item`, `dataset_memory_only`, `data_item_memory_only`, `unknown` | `All`, `Dataset`, `Item` only |
| memory-only delete | wipes graph+vector, keeps files+rows, resets cognify status | **absent** |
| dataset create facade | `create_dataset` + `create_authorized_dataset` | **absent** on `DatasetManager` |
| dataset ID formula | `uuid5(NAMESPACE_OID, f"{name}{user_id}{tenant_id}")` | `generate_dataset_id()` — **identical** ✓ |
| ACL permissions | `("read","write","delete","share")` | `grant_permission(principal, dataset, name)` ✓ (4 names match) |

## Prerequisites

```bash
git checkout -b task/18-forget-memoryonly-and-create-dataset
```

Read first (both sides):

- Rust: `crates/lib/src/api/forget.rs`, `crates/lib/src/api/datasets.rs`,
  `crates/delete/src/lib.rs` (DeleteScope, DeleteMode, `reset_dataset_pipeline_run_status`),
  `crates/database/src/traits/acl_db.rs` (`grant_permission`),
  `crates/ingestion/src/id_generation.rs` (`generate_dataset_id`),
  `crates/database/src/types.rs` (`PipelineRunStatus`).
- Python: `/tmp/cognee-python/cognee/api/v1/forget/forget.py`,
  `/tmp/cognee-python/cognee/modules/data/methods/create_dataset.py`,
  `/tmp/cognee-python/cognee/modules/data/methods/create_authorized_dataset.py`,
  `/tmp/cognee-python/cognee/modules/data/methods/get_unique_dataset_id.py`.

Re-grep to confirm current line numbers before editing:

```bash
grep -n "ForgetTarget\|DeleteMode::Hard\|enum ForgetTarget" crates/lib/src/api/forget.rs
grep -n "pub struct DatasetManager\|pub fn new\|with_acl\|acl_db\|fn list_data\|fn has_data" crates/lib/src/api/datasets.rs
grep -n "reset_dataset_pipeline_run_status\|enum DeleteScope\|enum DeleteMode" crates/delete/src/lib.rs
grep -n "fn grant_permission" crates/database/src/traits/acl_db.rs
grep -n "fn generate_dataset_id" crates/ingestion/src/id_generation.rs
```

## Files to change

| Path | Change |
|---|---|
| `crates/delete/src/lib.rs` | Add a `DeleteScope` "memory-only" disposition (or a `memory_only: bool` on existing scopes) that skips relational-row + file deletion and forces a pipeline-status reset. |
| `crates/lib/src/api/forget.rs` | Add `ForgetTarget::DatasetMemoryOnly` and `DataItemMemoryOnly`; map them to the memory-only delete; keep the `everything` label parity. |
| `crates/lib/src/api/datasets.rs` | Add `create_dataset(name, owner_id, tenant_id)` and `create_authorized_dataset(name, owner_id, tenant_id, parent_user_id)` to `DatasetManager`. |
| `crates/delete/src/lib.rs` (tests) / `crates/lib/tests/` | Add tests verifying files/rows survive and pipeline status resets; verify create + ACL grants. |

## Python reference

### forget memory-only — `/tmp/cognee-python/cognee/api/v1/forget/forget.py`

Target selection (the 6 targets), **lines 73–84**:

```python
if everything:
    target = "everything"
elif memory_only and data_id:
    target = "data_item_memory_only"
elif memory_only and dataset_ref:
    target = "dataset_memory_only"
elif data_id:
    target = "data_item"
elif dataset_ref:
    target = "dataset"
else:
    target = "unknown"
```

`_forget_dataset_memory` (**lines 231–301**) — docstring states the scope exactly:

```
Cleanup scope:
- Graph DB (nodes, edges): yes
- Vector DB (embeddings): yes
- Pipeline status: reset (so cognify re-processes all data)
- Relational DB (dataset, data records): preserved
- Raw files: preserved
```

It (a) deletes graph nodes/edges + vector embeddings for the dataset, then (b) resets
`pipeline_status` on the data records, then (c) resets the dataset-level
`cognify_pipeline` run status.

`_forget_data_memory` (**lines 304–363**) — same, scoped to one `data_id`, and it
resets **only `cognify_pipeline`** (not the `add` pipeline) — **lines 343–348**.

### create_dataset — `/tmp/cognee-python/cognee/modules/data/methods/create_dataset.py:14–38`

```python
@with_async_session
async def create_dataset(dataset_name, user, session):
    owner_id = user.id
    dataset = (await session.scalars(
        select(Dataset).filter(Dataset.name == dataset_name)
        .filter(Dataset.owner_id == owner_id)
        .filter(Dataset.tenant_id == user.tenant_id))).first()
    if dataset is None:
        dataset_id = await get_unique_dataset_id(dataset_name=dataset_name, user=user)
        dataset = Dataset(id=dataset_id, name=dataset_name, data=[],
                          owner_id=owner_id, tenant_id=user.tenant_id)
        session.add(dataset)
        await session.commit()
    return dataset
```

`get_unique_dataset_id` = `uuid5(NAMESPACE_OID, f"{dataset_name}{str(user.id)}{str(user.tenant_id)}")`
— **identical to Rust's `generate_dataset_id`** (`crates/ingestion/src/id_generation.rs:21–32`).

### create_authorized_dataset — `/tmp/cognee-python/cognee/modules/data/methods/create_authorized_dataset.py`

The 4 permissions (**line 13**): `_DATASET_PERMISSIONS = ("read", "write", "delete", "share")`.

Logic (**lines 16–54**): create the dataset, grant all 4 perms to the user; if
`user.parent_user_id` is set **and** differs from `user.id`, resolve the parent and grant
all 4 perms to them too (skip with a warning if the parent doesn't resolve).

## Implementation steps

### Part A — `forget` memory-only

1. **Confirm the current `DeleteScope`/`DeleteMode` shape.**

   ```bash
   grep -n "enum DeleteScope\|enum DeleteMode\|delete_dataset_if_empty\|reset_dataset_pipeline_run_status" crates/delete/src/lib.rs
   ```

   The current `forget()` (`crates/lib/src/api/forget.rs:~133–166`) builds a
   `DeleteScope::{Data,Dataset,User}` and a `DeleteRequest { scope, mode: DeleteMode::Hard }`.
   `reset_dataset_pipeline_run_status` already exists in `crates/delete/src/lib.rs`
   (currently around lines 629–671) and is invoked in delete phase 0.

2. **Add a memory-only disposition to the delete request.** Prefer a small, explicit
   field over a new scope variant so the existing cascade code stays one path. In
   `crates/delete/src/lib.rs`, add to `DeleteRequest`:

   ```rust
   pub struct DeleteRequest {
       pub scope: DeleteScope,
       pub mode: DeleteMode,
       /// When true: delete graph + vector only; preserve relational rows and raw
       /// files; force a cognify pipeline-status reset. Mirrors Python's
       /// `*_memory_only` forget targets.
       pub memory_only: bool,
   }
   ```

   Update all existing constructors of `DeleteRequest` to set `memory_only: false`
   (search the workspace: `grep -rn "DeleteRequest {" crates/`).

3. **Branch the cascade on `memory_only`.** In `DeleteService::execute` (the function that
   currently runs graph → vector → relational → files), when `request.memory_only` is
   `true`:
   - run the graph-node/edge deletion + vector deletion for the scope **as today**;
   - **skip** the relational-row deletion (`Data`/`Dataset` rows) and the **file** deletion;
   - **always** call `reset_dataset_pipeline_run_status` for the affected dataset(s)
     even though we are not removing junction rows.

   For the data-item scope, mirror Python and reset only the `cognify_pipeline` status
   (Python `_forget_data_memory`, lines 343–348). The existing
   `reset_dataset_pipeline_run_status` re-initiates **all** non-`Initiated` pipelines for
   the dataset; that is acceptable for the **dataset** memory-only case (Python resets
   the dataset-level cognify run + all data records), but for the **data-item** case
   prefer a narrower reset. If a per-pipeline reset helper does not exist, add one that
   only re-initiates the `cognify_pipeline` run for the dataset.

   > Determinism note: Do **not** delete or recreate the `Data`/`Dataset` rows here —
   > their `id`s are content-addressed and re-cognify must reuse them. Removing/recreating
   > would be a no-op for IDs but risks losing the `dataset↔data` junction.

4. **Add the two new `ForgetTarget` variants** in `crates/lib/src/api/forget.rs`:

   ```rust
   pub enum ForgetTarget {
       Item { data_id: Uuid, dataset: DatasetRef },
       Dataset { dataset: DatasetRef },
       All,
       /// Wipe graph+vector for a dataset, keep files + Data rows, reset cognify status.
       DatasetMemoryOnly { dataset: DatasetRef },
       /// Same, for a single data item.
       DataItemMemoryOnly { data_id: Uuid, dataset: DatasetRef },
   }
   ```

5. **Map the new variants** in `forget()`'s `match`. For each, resolve the dataset name
   exactly as the non-memory variants do, build the matching `DeleteScope::{Dataset,Data}`,
   and set `memory_only: true`. Keep `mode` as-is from task 09's outcome (09 switches
   `forget` to `DeleteMode::Soft`). The memory-only path overrides file/relational
   deletion regardless of `mode`.

   ```rust
   ForgetTarget::DatasetMemoryOnly { dataset } => {
       let dataset_name = dataset.to_name(owner_id, db).await?;
       let scope = DeleteScope::Dataset { owner_id, dataset_name: dataset_name.clone() };
       (scope, /* memory_only */ true, format!("dataset_memory_only:{dataset_name}"))
   }
   ```

   (Thread a `memory_only` bool out of the match and into the `DeleteRequest`.)

6. **Match Python's `everything` label** if you also relabel `All` — Python uses
   `"everything"` for the all-targets case. Optional; only do this if a parity test
   asserts the label string. Otherwise leave Rust's `"all"`.

### Part B — `DatasetManager.create_dataset` / `create_authorized_dataset`

7. **Confirm the `DatasetDb` trait can insert a dataset.**

   ```bash
   grep -n "fn create_dataset\|fn upsert_dataset\|fn insert_dataset\|fn get_dataset\b\|trait DatasetDb\|trait IngestDb" crates/database/src/**/*.rs
   ```

   If a create/upsert exists, reuse it. If only the ingestion path creates datasets,
   add a `create_dataset_row(id, name, owner_id, tenant_id)` to the `DatasetDb`/`IngestDb`
   trait and implement it on `DatabaseConnection` (idempotent: no-op if a row with that id
   exists — mirror Python's "if dataset is None" guard).

8. **Add `create_dataset` to `DatasetManager`** (`crates/lib/src/api/datasets.rs`):

   ```rust
   pub async fn create_dataset(
       &self,
       name: &str,
       owner_id: Uuid,
       tenant_id: Option<Uuid>,
   ) -> Result<Dataset, DatasetError> {
       let id = cognee_ingestion::generate_dataset_id(name, owner_id, tenant_id);
       // idempotent create; returns existing row if present
       let ds = self.db.create_dataset_row(id, name, owner_id, tenant_id).await?;
       Ok(ds)
   }
   ```

   Use `generate_dataset_id` (do **not** invent a new ID formula — it must match Python's
   `uuid5` byte-for-byte for cross-SDK reads).

9. **Add `create_authorized_dataset`** that grants ACL after create:

   ```rust
   const DATASET_PERMISSIONS: [&str; 4] = ["read", "write", "delete", "share"];

   pub async fn create_authorized_dataset(
       &self,
       name: &str,
       owner_id: Uuid,
       tenant_id: Option<Uuid>,
       parent_user_id: Option<Uuid>,
   ) -> Result<Dataset, DatasetError> {
       let ds = self.create_dataset(name, owner_id, tenant_id).await?;
       let Some(acl) = &self.acl_db else {
           return Err(DatasetError::AclNotConfigured); // or document a no-op; see Gotchas
       };
       for perm in DATASET_PERMISSIONS {
           acl.grant_permission(owner_id, ds.id, perm).await?;
       }
       if let Some(parent) = parent_user_id {
           if parent != owner_id {
               for perm in DATASET_PERMISSIONS {
                   acl.grant_permission(parent, ds.id, perm).await?;
               }
           }
       }
       Ok(ds)
   }
   ```

   `grant_permission` is idempotent (`crates/database/src/traits/acl_db.rs:32–40`), so
   re-creating an authorized dataset is safe. Decide the no-ACL behavior consistently with
   task 10 / B7.3 (this plan recommends erroring rather than silently skipping — see
   Gotchas).

10. **Surface the new methods** wherever the HTTP/CLI dataset surface lives if a route
    or subcommand is expected (check `crates/http-server/src/routers/datasets.rs` and the
    CLI). Out of strict scope for parity of the **facade**, but wire if a matching Python
    endpoint exists.

## Verification

```bash
# Compiles across all targets
cargo check --all-targets

# Lint clean (no new unwraps in non-test code)
cargo clippy --all-targets -- -D warnings

# Targeted tests (uses LLM/embedding harness if the test touches cognify)
bash scripts/run_tests_with_openai.sh forget_memory_only
bash scripts/run_tests_with_openai.sh create_authorized_dataset
```

### Tests to add

1. **`forget_memory_only_preserves_files_and_rows`** (delete crate, mocks OK):
   - Seed a dataset with one `Data` row, a stored file, graph nodes/edges, and vectors.
   - `forget(DatasetMemoryOnly { .. })`.
   - Assert: graph nodes/edges removed **and** vector points removed; **the `Data` row,
     `Dataset` row, and stored file still exist**; the dataset's `cognify_pipeline`
     latest status is `Initiated` (reset).

2. **`forget_data_item_memory_only_resets_only_cognify`**:
   - Same seed; `forget(DataItemMemoryOnly { data_id, .. })`.
   - Assert the data row + file survive; `cognify_pipeline` status reset; the `add`
     pipeline status **not** reset (parity with Python lines 343–348).

3. **`create_dataset_id_matches_generate_dataset_id`**:
   - `mgr.create_dataset("ds", owner, Some(tenant))`.
   - Assert returned `id == generate_dataset_id("ds", owner, Some(tenant))`.
   - Call twice → same id, single row (idempotent).

4. **`create_authorized_dataset_grants_four_permissions`**:
   - With an `AclDb` wired, create with `parent_user_id = Some(p)`.
   - Assert the owner has all 4 perms and the parent has all 4 perms on `ds.id`.
   - With `parent_user_id == owner_id`, assert no duplicate/error and grants applied once.

Expected: all four pass; existing forget/datasets tests unchanged.

## Acceptance criteria

- [ ] `ForgetTarget::{DatasetMemoryOnly, DataItemMemoryOnly}` exist and are dispatched.
- [ ] Memory-only forget removes graph + vector but **preserves raw files and
      `Data`/`Dataset` rows**.
- [ ] Memory-only forget resets the cognify pipeline status (dataset-wide for the dataset
      variant; cognify-only for the data-item variant).
- [ ] `DatasetManager::create_dataset` returns a row whose id equals
      `generate_dataset_id(name, owner, tenant)` and is idempotent.
- [ ] `DatasetManager::create_authorized_dataset` grants `read/write/delete/share` to the
      owner and, when set & distinct, to `parent_user_id`.
- [ ] `cargo check --all-targets` and `cargo clippy -- -D warnings` pass; all four new
      tests pass.

## Gotchas / do-not

- **Do NOT change the dataset ID formula.** `create_dataset` must use the existing
  `generate_dataset_id` (uuid5 of `name + user_id + tenant_id`, with `"None"` for a
  missing tenant). Any deviation breaks cross-SDK dataset reads.
- **Memory-only must not touch files or relational rows.** The whole point is re-cognify
  without re-ingest. Verify the file path on disk still exists in the test.
- **Pipeline-reset scope differs by target.** Dataset memory-only resets the dataset-level
  cognify run (Python resets all data records + dataset run); data-item memory-only resets
  **only** `cognify_pipeline`, never the `add` pipeline.
- **`grant_permission` is idempotent** — re-running `create_authorized_dataset` is safe;
  do not add manual existence checks that diverge from the trait contract.
- **ACL-not-configured behavior:** this plan recommends `create_authorized_dataset`
  **erroring** when `acl_db` is `None` (it cannot honor its name otherwise). This aligns
  with B7.3's push toward always-on enforcement. If task 10 / project decision keeps ACL
  opt-in, document the no-op explicitly in the rustdoc instead of silently succeeding.
- **Update every `DeleteRequest { .. }` construction** when adding the `memory_only`
  field, or the build breaks. Grep first.

## Rollback

Each part is independent. To revert: delete the two `ForgetTarget` variants and their
match arms; remove the `memory_only` field + branch in `crates/delete/src/lib.rs`
(restore the constructors); remove `create_dataset`/`create_authorized_dataset` and any
new `DatasetDb` trait method. `git checkout main -- crates/delete/src/lib.rs
crates/lib/src/api/forget.rs crates/lib/src/api/datasets.rs` restores the prior state.
