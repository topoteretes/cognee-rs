# API v2: `forget()`

**Python source:** `cognee/api/v1/forget/forget.py` (198 lines)
**Rust status:** **Implemented** (with session cleanup for `everything` mode)
**Implementation plan:** [impl/forget-plan.md](impl/forget-plan.md)

---

## 1. What it does

`forget()` is a unified deletion facade that replaces separate prune/delete/empty_dataset APIs with a single mental model. Three deletion modes:

### Mode 1: Forget a Single Data Item
```python
await forget(data_id=item_id, dataset=dataset_id)
```
- **Inputs:** `data_id` (UUID), `dataset` (UUID or name)
- **Validation:** `data_id` without `dataset` raises `ValueError`
- **Cleanup scope:**
  - Relational DB: Detach data from dataset in `dataset_data` junction; cascade-delete if referenced nowhere else
  - Graph DB: Delete all nodes/edges bearing this data's provenance ID
  - Vector DB: Delete all points (embeddings) tied to this data
  - File storage: Delete the raw data file (`text_<md5>.txt`) if data has no remaining dataset links
- **Session cache:** Not cleaned (sessions are keyed by user_id+session_id, not dataset—targeted cleanup requires future tagging enhancement)
- **Returns:** `{"data_id": str(id), "dataset_id": str(ds_id), "status": "success"}`

### Mode 2: Forget an Entire Dataset
```python
await forget(dataset="scientists")
```
- **Inputs:** `dataset` (name or UUID)
- **Resolution:** Dataset name → UUID via `get_authorized_dataset_by_name()` or by UUID via `get_authorized_dataset()`
- **Cleanup scope:** Same as Mode 1, but for ALL data in the dataset
  - Relational DB: Delete all rows in `dataset` table; detach all `dataset_data` rows; cascade-delete `Data` records with no other datasets
  - Graph DB: Delete all nodes/edges for this dataset (via `delete_graph()` filtered by dataset_id)
  - Vector DB: Delete all collections tied to this dataset
  - File storage: All raw data files
- **Session cache:** Not cleaned (same limitation)
- **Returns:** `{"dataset_id": str(ds_id), "status": "success"}`

### Mode 3: Forget Everything (User Scoped)
```python
await forget(everything=True)
```
- **Inputs:** No `data_id` or `dataset`; optional `user` (resolved to default if None)
- **Cleanup scope (exhaustive):**
  - **Relational DB:** Delete all user-owned datasets and associated data records
  - **Graph DB:** Wipe entire graph (all nodes/edges)
  - **Vector DB:** Delete all vector collections
  - **File storage:** All raw data files
  - **Session cache (critical difference from Modes 1–2):** Full `cache_engine.prune()` call clears ALL session Q&A entries, feedback scores, graph context snapshots, and session metadata
- **Returns:** `{"datasets_removed": count, "status": "success"}`

### Observability & Telemetry

All modes:
1. Log telemetry event `"cognee.forget"` with target (`"everything"` | `"data_item"` | `"dataset"`), dataset/data IDs, cognee version
2. Create OTEL span `"cognee.api.forget"` with attributes:
   - `COGNEE_FORGET_TARGET` (target type)
   - `COGNEE_DATASET_NAME` (if dataset provided)
   - `COGNEE_RESULT_COUNT` (count deleted; for `everything`, datasets_removed)

### Permissioning

All three modes enforce user authorization:
- `get_authorized_dataset()` or `get_authorized_dataset_by_name()` with `"delete"` permission required
- Raises `ValueError` if dataset not found or user lacks permission

### User Resolution

If `user=None`, resolves to default user via `get_default_user()` from `cognee.modules.users.methods`.

---

## 2. Building blocks (Python)

### Core deletion logic (`cognee/api/v1/datasets/datasets.py`)

| Function | Purpose | File | Lines |
|----------|---------|------|-------|
| `datasets.delete_data()` | Detach single data item from dataset; cascade graph/vector cleanup | `cognee/api/v1/datasets/datasets.py` | 124–175 |
| `datasets.empty_dataset()` | Delete entire dataset + all its data + cascade cleanup | `cognee/api/v1/datasets/datasets.py` | 83–121 |
| `datasets.delete_all()` | Iterate all user datasets and call `empty_dataset()` on each | `cognee/api/v1/datasets/datasets.py` | 178–187 |

### Data/dataset resolution (`cognee/modules/data/methods/`)

| Function | Purpose | Module |
|----------|---------|--------|
| `get_authorized_dataset()` | Resolve UUID + check `"delete"` ACL | `cognee/modules/data/methods/get_authorized_dataset.py` |
| `get_authorized_dataset_by_name()` | Resolve name → UUID + check `"delete"` ACL | `cognee/modules/data/methods/get_authorized_dataset_by_name.py` |
| `get_default_user()` | Fetch the default user context | `cognee/modules/users/methods/__init__.py` |

### Session/cache cleanup (`cognee/infrastructure/databases/cache/`)

| Function | Purpose | Module |
|----------|---------|--------|
| `get_cache_config()` | Read cache engine config (Redis vs. FS vs. disabled) | `cognee/infrastructure/databases/cache/__init__.py` |
| `get_cache_engine()` | Factory to instantiate the configured cache engine | `cognee/infrastructure/databases/cache/get_cache_engine.py` |
| `cache_engine.prune()` | Wipe all session entries (Redis FLUSHDB, FS rm, etc.) | Varies by backend |

### Observability (`cognee/modules/observability/`)

| Function | Purpose |
|----------|---------|
| `new_span()` | OTEL span context manager |
| Constants: `COGNEE_FORGET_TARGET`, `COGNEE_DATASET_NAME`, `COGNEE_RESULT_COUNT` | OTEL attribute keys |
| `send_telemetry()` | Event logging to external telemetry system |

### Server-mode remote delegation (`cognee/api/v1/serve/state.py`)

| Function | Purpose |
|----------|---------|
| `get_remote_client()` | If in server mode, return remote API client; else None. If not None, `client.forget()` is called instead of local logic |

---

## 3. Rust status per building block

| Building Block | Python Path | Rust Path | Status | Notes |
|---|---|---|---|---|
| **Single data item deletion** | `datasets.delete_data()` | `crates/delete/src/lib.rs:350–362` | ✅ Implemented | Handled via `DeleteScope::Data { owner_id, data_id, dataset_name, delete_dataset_if_empty }` |
| **Dataset deletion** | `datasets.empty_dataset()` | `crates/delete/src/lib.rs:336–343` | ✅ Implemented | Handled via `DeleteScope::Dataset { owner_id, dataset_name }` |
| **Everything deletion** | `datasets.delete_all()` | `crates/delete/src/lib.rs:324–333` | ✅ Implemented | Handled via `DeleteScope::User { owner_id }` |
| **Dataset name→ID resolution** | `get_authorized_dataset_by_name()` | `crates/delete/src/authorized.rs:164–184` | ✅ Implemented | `AuthorizedDeleteService::resolve_dataset_id()` |
| **Dataset UUID validation** | `get_authorized_dataset()` | `crates/delete/src/authorized.rs:164–184` | ✅ Implemented | Same as above, wraps `database.get_dataset_by_name()` |
| **Relational DB deletion** | `delete_data()`, `delete_dataset()` in `cognee/modules/data/methods/` | `crates/delete/src/lib.rs` (Phase 2: lines 374–472) | ✅ Implemented | Detach links, delete datasets, cascade delete data |
| **Graph DB cleanup** | `delete_dataset_nodes_and_edges()`, `delete_data_nodes_and_edges()` | `crates/delete/src/lib.rs` (Phase 1: lines 321–372) | ✅ Implemented | Delegated to `GraphDBTrait` via `self.cleanup_dataset()`, `self.cleanup_data()`, `self.cleanup_all()` |
| **Vector DB cleanup** | `vector_engine.delete_collection()` for each collection tied to data/dataset | `crates/delete/src/lib.rs` (Phase 1: lines 321–372) | ✅ Implemented | Via `self.cleanup_dataset()`, `self.cleanup_data()`, `self.cleanup_all()` |
| **File storage cleanup** | `storage.delete(file_path)` | `crates/delete/src/lib.rs:444–466` | ✅ Implemented | Called for orphaned data records with no remaining dataset links |
| **Session cache prune** | `cache_engine.prune()` (only for `everything=True`) | `crates/delete/src/lib.rs:531–540` | ✅ Implemented | Phase 5: Calls `self.session_store.prune()` ONLY when `DeleteScope::All` |
| **Search history cleanup** | Not explicit in Python `forget()` but in `prune_system()` | `crates/delete/src/lib.rs:506–525` (Phase 4) | ✅ Implemented | Deletes search_history rows for User/All scopes |
| **Pipeline status cleanup** | Not explicit in Python `forget()` | `crates/delete/src/lib.rs:272–318` (Phase 0) | ✅ Implemented | Clears pipeline_status JSON before junction detach |
| **Orphan sweep (hard mode)** | Not explicit in Python `forget()` | `crates/delete/src/lib.rs:483–499` (Phase 3) | ✅ Implemented | Hard mode: sweep degree-1 Entity/EntityType/EdgeType nodes |
| **Dry-run preview** | Not in Python `forget()`, but `DeleteService.preview()` supports it | `crates/delete/src/lib.rs:183–239` | ✅ Implemented | Counts deletion targets without executing |
| **ACL enforcement** | `get_authorized_dataset()` permission check | `crates/delete/src/authorized.rs` (entire module) | ✅ Implemented | `AuthorizedDeleteService` wrapper enforces "delete" permission before execution |
| **User resolution** | `get_default_user()` | Via `owner_id` parameter (caller responsibility) | ⚠️ Partial | Rust SDK passes `owner_id` directly; no automatic resolution (caller must provide) |
| **Telemetry/OTEL** | `send_telemetry()`, `new_span()`, span attributes | `crates/delete/src/lib.rs:175–249` | ✅ Implemented | Instrumented with `#[tracing::instrument]` macros; scope_label and mode_label helpers match Python OTEL attributes |
| **Remote delegation** | `get_remote_client()` check in Python | Not applicable | ✅ N/A | Rust SDK does not have server-mode delegation; each caller instantiates DeleteService directly |
| **API facade** | `forget()` function signature + dispatch | `crates/lib/src/api/forget.rs:44–99` | ✅ Implemented | `pub async fn forget(target: ForgetTarget, owner_id: Uuid, delete_service: &DeleteService, db: Option<&dyn IngestDb>)` |

---

## 4. Gaps — what Rust needs

### Critical Gap: Session Cleanup for Dataset/Data Modes

**Impact:** Medium — affects only `forget(everything=True)`, not the common item/dataset deletion patterns

**Detail:**
- Python's `_forget_everything()` (line 109) calls `cache_engine.prune()` to wipe ALL session cache
- Rust's `DeleteService::execute()` **only** calls `session_store.prune()` when `DeleteScope::All` (line 532–540)
- For `DeleteScope::Data` and `DeleteScope::Dataset`, session cache is NOT cleaned
- **Python justification** (lines 148–151): Session keys are `user_id+session_id` (not dataset-scoped), so deleting a dataset does not automatically invalidate sessions. Targeted cleanup would require tagging sessions with dataset IDs—a future enhancement

**Verdict:** This is NOT a bug; it matches Python's design limitation. No action needed.

### Medium Gap: Dataset Name-to-UUID Resolution in `forget()` API

**Impact:** Low — mostly an API convenience issue

**Detail:**
- Python `forget()` accepts `dataset: Union[str, UUID]` and transparently resolves names via `_resolve_dataset_id()` (lines 185–198)
- Rust's `forget()` in `crates/lib/src/api/forget.rs` already handles this:
  - `ForgetTarget::Dataset { dataset_name }` takes a String (name)
  - If `db` is provided, resolves to UUID before constructing `DeleteScope`
  - If `db` is None, passes name directly to `DeleteScope::Dataset` (relies on underlying `DeleteService`)
- However, `ForgetTarget` does NOT support UUID; it only takes names
- **Rust design:** Caller must convert UUID to name before calling `forget()`, OR pass `db` for validation

**Verdict:** **Minor limitation** — UUID support would be a nice-to-have but is not critical. Add to roadmap if Python API parity is strict.

### Low Gap: Default User Resolution

**Impact:** Low — SDK design choice

**Detail:**
- Python `forget(user=None)` auto-resolves to default user via `get_default_user()` (line 89)
- Rust `forget()` signature requires explicit `owner_id: Uuid` — no automatic resolution
- **Justification:** Rust SDK is more explicit; caller is responsible for user context

**Verdict:** **By design**, not a bug. If auto-resolution is desired, wrap `forget()` in a higher-level function in the SDK's CLI or bindings layer.

### Low Gap: Telemetry/Observability Parity

**Impact:** Very Low — structured logging already in place

**Detail:**
- Rust uses `#[tracing::instrument]` macros (lines 175, 241) with OpenTelemetry-compatible span attributes
- Python uses `send_telemetry()` for external event logging
- Rust `DeleteService` does not call an external telemetry endpoint; it only logs via `tracing`

**Verdict:** **Acceptable trade-off.** OTEL is available; external telemetry would be a feature addition, not a gap.

---

## 5. Effort estimate

**Status:** **Already Implemented**

### What Exists (Ready to Use)

1. **`DeleteService`** (`crates/delete/src/lib.rs`) — production-grade with:
   - Soft/hard mode deletion
   - Dry-run preview
   - ACL enforcement via `AuthorizedDeleteService`
   - Session cache pruning (for `everything` mode)
   - 5-phase execution: pipeline status → graph/vector → relational → orphan sweep → search history → session prune
   - Comprehensive error handling and telemetry

2. **`forget()` API function** (`crates/lib/src/api/forget.rs`) — thin wrapper:
   - Maps `ForgetTarget` enum to `DeleteScope`
   - Resolves dataset names to IDs
   - Executes via `DeleteService`
   - Returns structured `ForgetResult`

3. **CLI command** (`crates/cli/src/commands/delete.rs`) — user-facing:
   - `--data-id`, `--dataset-name`, `--user-id`, `--all` flags
   - `--dry-run`, `--force` options
   - ACL enforcement toggle
   - Preview + confirmation flow

### Complexity Assessment

| Aspect | S/M/L/XL |
|--------|----------|
| **API function** | ✅ **Already done** (S) |
| **Core deletion logic** | ✅ **Already done** (M – but implemented as `DeleteService`) |
| **Session cleanup** | ✅ **Already done** (S – integrated into Phase 5) |
| **ACL enforcement** | ✅ **Already done** (M – `AuthorizedDeleteService`) |
| **Testing** | ✅ **Already done** (3 integration tests + CLI E2E) |
| **Documentation** | ⚠️ **Partial** (this report) |

**Overall:** **Implemented with 90%+ Python parity.** The only gap is optional UUID support in the API enum; Python's design limitations (session cleanup scope) are replicated faithfully.

---

## 6. Rust Implementation Details

### API Entry Point (`crates/lib/src/api/forget.rs`)

```rust
pub async fn forget(
    target: ForgetTarget,
    owner_id: Uuid,
    delete_service: &DeleteService,
    db: Option<&dyn IngestDb>,
) -> Result<ForgetResult, ApiError>
```

**Key design:**
- `target: ForgetTarget` — enum with three variants: `Item { data_id, dataset_name }`, `Dataset { dataset_name }`, `All`
- Resolves dataset names to IDs if `db` provided (validation-only; not required)
- Constructs `DeleteRequest { scope: DeleteScope, mode: DeleteMode::Hard }`
- Delegates to `delete_service.execute()` for all heavy lifting
- Returns `ForgetResult { target: String, delete_result: DeleteResult }`

### DeleteService Core (`crates/delete/src/lib.rs:250–578`)

**Five-phase execution in `execute()` method:**

1. **Phase 0: Pipeline Status Cleanup** (lines 272–318)
   - Clears `pipeline_status` JSON entries before detaching junction rows
   - Ensures re-running cognify after deletion will reprocess the dataset

2. **Phase 1: Graph/Vector Cleanup** (lines 321–372)
   - Fast-path for `DeleteScope::All`: wipes entire graph/collections
   - Dataset-scoped: calls `cleanup_dataset(dataset_id)` per dataset
   - Data-scoped: calls `cleanup_data(data_id, dataset_id)` per affected link
   - Counts and collects warnings

3. **Phase 2: Relational Cleanup** (lines 374–472)
   - Detaches data from datasets (`dataset_data` junction rows)
   - Invalidates pipeline runs for affected datasets (for data-scoped deletions)
   - Deletes dataset records
   - Deletes orphaned data records (those with no remaining dataset links)
   - Deletes associated storage files

4. **Phase 3: Hard-Mode Orphan Sweep** (lines 483–499)
   - If `DeleteMode::Hard`: removes degree-1 Entity/EntityType/EdgeType nodes
   - Soft mode leaves orphans; hard mode cleans them up

5. **Phase 4: Search History Cleanup** (lines 506–525)
   - Deletes search_history rows for User/All scopes
   - No-op for Data/Dataset scopes (no dataset_id on query table)

6. **Phase 5: Session Cache Pruning** (lines 531–540)
   - **Only for `DeleteScope::All`** (matches Python's behavior)
   - Calls `session_store.prune()` if provided
   - Sets `pruned_sessions: true` in result

### ACL Enforcement (`crates/delete/src/authorized.rs`)

`AuthorizedDeleteService` wrapper enforces "delete" permission on all affected datasets before delegating to `DeleteService`:
- Checks `principal_id` has "delete" on each target dataset
- Supports per-scope validation: Data (check target dataset), Dataset (check target), User (check all user datasets), All (check all system datasets)
- Returns `PermissionDenied` error if any check fails

### Session Store Trait (`crates/session/src/session_store.rs:72–74`)

```rust
async fn prune(&self) -> Result<(), SessionError>;
```

Implemented by:
- `FsSessionStore` — deletes all files under session root
- `RedisSessionStore` — calls Redis FLUSHDB
- `SeaOrmSessionStore` — deletes all rows from session tables

---

## 7. Test Coverage

### Integration Tests

**File:** `crates/delete/tests/`

1. `authorized_delete_integration.rs` — ACL enforcement
2. `hard_mode_orphan_sweep.rs` — Orphan cleanup behavior
3. `delete_error_paths.rs` — Error scenarios

### Inline Tests

**In `crates/delete/src/lib.rs`:**

- `delete_all_prunes_session_store()` (line 3639) — Verifies session prune is called for `DeleteScope::All`
- Dataset deletion tests
- Data-scoped deletion tests
- Preview vs. execute consistency tests

### CLI E2E

**File:** `crates/cli/src/commands/delete.rs` + acceptance tests

- `--dry-run` flag preserves state
- `--force` skips confirmation
- `--enforce-acl` enables authorization checking
- Mode selection validation

---

## 8. How to Use

### Direct API

```rust
use cognee_lib::api::forget;
use cognee_lib::delete::DeleteService;

let service = DeleteService::new(storage, database)
    .with_graph_db(graph)
    .with_vector_db(vector)
    .with_session_store(session);

let result = forget(
    ForgetTarget::All,
    user_id,
    &service,
    Some(&database),
).await?;

println!("Deleted: {:?}", result.delete_result);
```

### CLI

```bash
# Forget a single data item
cargo run --bin cognee -- delete \
  --data-id <uuid> \
  --dataset-name <name> \
  --user-id <owner_uuid> \
  --dry-run

# Forget an entire dataset
cargo run --bin cognee -- delete \
  --dataset-name scientists \
  --user-id <owner_uuid>

# Forget everything
cargo run --bin cognee -- delete \
  --all \
  --user-id <owner_uuid> \
  --force
```

---

## Conclusion

**`forget()` is production-ready in Rust with feature parity to Python.** The implementation covers all three deletion modes, includes session cleanup for the `everything` scope, enforces ACL, and provides dry-run preview. No blocking gaps remain; only minor enhancements (UUID API support, telemetry export) are opportunities for future work.

