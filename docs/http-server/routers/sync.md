# Router: sync

Cloud-sync endpoints. Triggers a long-running, three-step idempotent file-sync between the local cognee instance and a Cognee Cloud tenant: hash-diff → upload missing → download missing → trigger remote and local cognify. Concurrency rule: **one running sync per user** — a second `POST` while another sync is in progress returns `409` with details about the running operation.

Companion docs: [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../pipelines.md](../pipelines.md), [../tenants.md](../tenants.md).

## 1. Mount & file
- Mount prefix: `/api/v1/sync`
- Router file: `crates/http-server/src/routers/sync.rs`
- Python source: [`cognee/api/v1/sync/routers/get_sync_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py).
- Underlying sync engine (Python): [`cognee/api/v1/sync/sync.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/sync.py).
- Persistence model (Python): [`cognee/modules/sync/models/SyncOperation.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/sync/models/SyncOperation.py).

## 2. Endpoints

### 2.1 `POST /api/v1/sync` — start a cloud sync

Initiates a sync of one or more datasets to Cognee Cloud. The server records a `sync_operations` row, spawns a background task, and returns immediately with a `run_id`.

- **Auth**: `required` (`AuthenticatedUser`). Python uses `Depends(get_authenticated_user)` ([Python L33](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py#L33)).
- **Path params**: none.
- **Query params**: none.
- **Request body**: `application/json`, `SyncRequestDTO`:
  - `dataset_ids: Option<Vec<Uuid>>` — explicit list of datasets to sync. `None` or `[]` means *"all datasets the user has `write` permission on"* (Python passes `None` through to `get_specific_user_permission_datasets`, [Python L136](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py#L136)).

  Empty body `{}` is valid and means "all".
- **Response body**: `application/json`, `200 OK`, `SyncResponseDTO`:
  - `run_id: String` — UUID v4 string. Note: Python types this as `str`, not `UUID` ([sync.py L131](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/sync.py#L131)). We match (`String`, not `Uuid`) so wire-format roundtrips work.
  - `status: String` — always literally `"started"` ([sync.py L91, L158](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/sync.py#L158)).
  - `dataset_ids: Vec<String>` — every dataset id, stringified.
  - `dataset_names: Vec<String>`
  - `message: String` — human-readable, e.g. `"Sync operation started in background for 3 datasets. Use run_id '...' to track progress."` ([sync.py L161](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/sync.py#L161)).
  - `timestamp: String` — ISO-8601, the time the request hit the handler. Python uses `datetime.now(timezone.utc).isoformat()` ([sync.py L134](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/sync.py#L134)).
  - `user_id: String` — `user.id.to_string()`.

  Python's response signature is `dict[str, SyncResponse]` ([Python L30](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py#L30)) but the underlying `sync()` function returns a bare `SyncResponse`, not a dict. The actual wire body **is** a flat object (the `dict[str, SyncResponse]` annotation is misleading and only used for OpenAPI). We match the actual wire shape: a flat `SyncResponseDTO`.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `400` | `{"error": "<msg>"}` | Empty `datasets` list (`ValueError` from `sync()` when no datasets resolved). [Python L147–L148](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py#L147-L148). |
  | `401` | `ApiError::Unauthorized` | Missing/invalid credential. |
  | `403` | `{"error": "<msg>"}` | `PermissionError` from the permissions gate. [Python L149–L150](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py#L149-L150). |
  | `409` | `SyncConflictDTO` (see below) | User already has a sync in `STARTED` or `IN_PROGRESS` state. [Python L114–L132](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py#L114-L132). |
  | `409` | `{"error": "Cloud service unavailable: <msg>"}` | `ConnectionError` from `_sync_to_cognee_cloud`. [Python L151–L154](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py#L151-L154). |
  | `409` | `{"error": "Cloud sync operation failed."}` | Any other exception during dispatch. [Python L155–L157](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py#L155-L157). |

  Note: Python flattens both "in-progress" and "cloud unavailable" onto `409`. We replicate. The `Conflict` variant of `ApiError` is reused; the body shape is variant-specific (see DTOs).

  `SyncConflictDTO` ([Python L120–L131](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py#L120-L131)):
  ```json
  {
    "error": "Sync operation already in progress",
    "details": {
      "run_id":              "<uuid>",
      "status":              "already_running",
      "dataset_ids":         ["<uuid>", ...],
      "dataset_names":       ["...", ...],
      "message":             "You have a sync operation already in progress with run_id '<uuid>'. ...",
      "timestamp":           "<iso8601>",
      "progress_percentage": 45
    }
  }
  ```

- **Side effects**:
  1. **Read** — `get_running_sync_operations_for_user(user.id)` against `sync_operations` ([sync `methods/get_sync_operation.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/sync/methods/get_sync_operation.py)). If any row is in `STARTED` or `IN_PROGRESS`, short-circuit with 409.
  2. **Read** — `get_specific_user_permission_datasets(user.id, "write", request.dataset_ids)` against `acls`. Filters/validates the dataset list.
  3. **Insert** — `INSERT INTO sync_operations (run_id, dataset_ids, dataset_names, user_id, status='started', progress_percentage=0, created_at=NOW())` ([create_sync_operation.py](https://github.com/topoteretes/cognee/blob/main/cognee/modules/sync/methods/create_sync_operation.py)).
  4. **Spawn** — `tokio::spawn` of `_perform_background_sync(run_id, datasets, user)`.
  5. **Register** — insert into the in-memory `SyncRegistry` ([../architecture.md §6](../architecture.md#6-application-state--dependency-injection); see §3.1 below).
- **Delegation target**:
  - Permission gate: `state.lib.permissions().datasets_for_user(user.id, Permission::Write, request.dataset_ids)` (mirror of `get_specific_user_permission_datasets`, see [../tenants.md §9](../tenants.md#9-repository-surface)).
  - DB row create: `state.lib.sync().create_operation(run_id, dataset_ids, dataset_names, user.id)` — wraps the new `SyncOperationRepository`.
  - Background work: `state.lib.sync().run_background(run_id, datasets, user)` — wraps the Rust port of `_perform_background_sync` from `crates/cloud/` (see Open Questions for whether this lives in `cognee-cloud` or a new `cognee-sync` crate).
  - Concurrency check: `state.sync.has_running(user.id)` against the in-memory `SyncRegistry`.
- **Validation rules**:
  - `dataset_ids`: every UUID must parse. Empty array == `None`.
  - **Cross-field**: at least one resolved dataset must come back from the permissions gate, else Python raises `ValueError("At least one dataset must be provided for sync operation")` from `sync()` itself ([sync.py L127–L128](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/sync.py#L127-L128)). Map to `400 {"error": "..."}`.
  - **Concurrency**: see §3.1.
- **Rate / size limits**: small JSON body; default 100 MiB body limit applies but is irrelevant.
- **Permission gate**: `write:dataset_id` for every requested dataset (or all writable datasets when `dataset_ids` is empty). Enforced by `get_specific_user_permission_datasets`, which silently filters non-permitted datasets. **Quirk**: if `dataset_ids = [a, b, c]` and the user has write access to only `[a]`, Python proceeds to sync `[a]` without telling the user about the dropped ones. We match.
- **OpenAPI**: tag `["Cloud Sync"]`. Request: `application/json` `SyncRequestDTO`. Responses: `200 SyncResponseDTO`, `400 ErrorDTO`, `403 ErrorDTO`, `409 SyncConflictDTO | ErrorDTO`. Security: `[BearerAuth, ApiKeyAuth, CookieAuth]`.
- **Telemetry**:
  - Span `cognee.api.sync.start`. Attributes: `cognee.sync.run_id`, `cognee.sync.dataset_count`, `cognee.sync.dataset_ids` (joined with `,`), `cognee.user.id`.
  - Python emits a `send_telemetry("Cloud Sync API Endpoint Invoked", ...)` PostHog event ([Python L98–L108](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py#L98-L108)). Rust does **not** port this PostHog call in phase 1 — telemetry stays inside the `tracing` ecosystem ([../observability.md §1](../observability.md#1-goals--non-goals)).
  - The background task itself runs under span `cognee.sync.background` with attributes `cognee.sync.records_uploaded`, `cognee.sync.records_downloaded`, `cognee.sync.bytes_uploaded`, `cognee.sync.bytes_downloaded`. These are durable on the `sync_operations` row, not just spans.
- **Python parity notes**:
  - The `dict[str, SyncResponse]` response-model annotation in Python is **not** what the wire returns; the actual wire shape is a flat `SyncResponse` object ([sync.py L156–L164](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/sync.py#L156-L164)). Cross-SDK parity tests must compare against the runtime body, not the OpenAPI schema. Open Question: do we replicate the misleading OpenAPI schema or fix it?
  - When the user has no running sync and no permitted datasets, Python's `sync()` raises `ValueError`. The router catches `ValueError` and returns `400`. We mirror.
  - `set_database_global_context_variables` is imported but unused in the router file. Ignore.
  - The body `dataset_ids` field must accept `null` as well as missing — Pydantic `Optional[List[UUID]] = None`. Rust: `#[serde(default)] dataset_ids: Option<Vec<Uuid>>`.

### 2.2 `GET /api/v1/sync/status` — overview of running syncs for the caller

Returns whether the caller has any sync currently running, plus a snippet about the latest one. Cheap polling endpoint for the frontend.

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params**: none.
- **Request body**: none.
- **Response body**: `application/json`, `200 OK`, `SyncStatusOverviewDTO`:
  - `has_running_sync: bool`
  - `running_sync_count: usize`
  - `latest_running_sync: Option<LatestRunningSyncDTO>` — present only when `running_sync_count > 0`. Fields ([Python L226–L233](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py#L226-L233)):
    - `run_id: String`
    - `dataset_ids: Vec<String>`
    - `dataset_names: Vec<String>`
    - `progress_percentage: u32`
    - `created_at: Option<String>` — ISO-8601.

  **Note**: Python returns `dataset_name` (singular) in the docstring example ([Python L196–L197](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py#L196-L197)) but the actual code returns `dataset_ids` and `dataset_names` (plural, with `s`). The docstring is stale. Actual wire shape uses the plural fields. We match the *code*, not the docstring.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `ApiError::Unauthorized` | Missing/invalid credential. |
  | `500` | `{"error": "Failed to get sync status overview"}` | Repository raised. **Python parity**: Python returns `JSONResponse(500, {"error": "..."})`, *not* the canonical `ApiError` envelope ([Python L240–L242](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py#L240-L242)). We match this specific envelope with a special `ApiError::Custom { status, body }` or by returning a tuple directly. |
- **Side effects**: read-only single query against `sync_operations`.
- **Delegation target**: `state.lib.sync().running_for_user(user.id) -> Vec<SyncOperationRow>` (port of `get_running_sync_operations_for_user`).
- **Validation rules**: none.
- **Rate / size limits**: small. The frontend polls this on a loop (e.g. every 2s during an active sync); response body is bounded.
- **Permission gate**: implicit — only operations owned by the caller are returned, by virtue of the `WHERE user_id = ?` filter.
- **OpenAPI**: tag `["Cloud Sync"]`. Response: `application/json` `SyncStatusOverviewDTO`. Security: same as 2.1.
- **Telemetry**: span `cognee.api.sync.status`. Attributes: `cognee.sync.running_count`. PostHog event `"Sync Status Overview API Endpoint Invoked"` ([Python L205–L212](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py#L205-L212)) is **not** ported.
- **Python parity notes**:
  - The router orders by `created_at DESC` (Python's `get_running_sync_operations_for_user` does this internally, [Python `methods/get_sync_operation.py:104`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/sync/methods/get_sync_operation.py#L104)).
  - Python casts `created_at` via `.isoformat()`; Rust uses `chrono::DateTime<Utc>::to_rfc3339()` which is wire-equivalent for the `+00:00` timezone format Python emits.
  - The 500 response body uses `{"error": ...}` — a plain JSON object, not the canonical `{"detail": ...}` envelope. This is a one-off shape that needs a special path through `ApiError`.

## 3. Cross-cutting behavior

### 3.1 Concurrency: one running sync per user

The "one running sync per user" rule is enforced at two layers:

1. **Authoritative — DB query**: `SELECT * FROM sync_operations WHERE user_id = ? AND status IN ('started', 'in_progress') ORDER BY created_at DESC` ([`get_running_sync_operations_for_user`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/sync/methods/get_sync_operation.py#L82-L107)). Every `POST /api/v1/sync` runs this first; if non-empty, return `409`.
2. **Optimistic — `SyncRegistry`**: an in-memory `Arc<SyncRegistry>` in `AppState` ([../architecture.md §6](../architecture.md#6-application-state--dependency-injection)) tracks running runs for the current process. Two concurrent `POST`s racing through step 1 (both find the DB empty, both proceed) collide on the `SyncRegistry::insert` step; the loser sees `RegistryError::AlreadyRunning` and returns `409`.

```rust
// crates/http-server/src/sync/registry.rs

#[derive(Clone, Default)]
pub struct SyncRegistry {
    inner: Arc<DashMap<Uuid, RunningSync>>,   // key = user_id
}

pub struct RunningSync {
    pub run_id:              String,           // matches Python's str type
    pub user_id:             Uuid,
    pub dataset_ids:         Vec<Uuid>,
    pub dataset_names:       Vec<String>,
    pub created_at:          DateTime<Utc>,
    pub progress_percentage: AtomicU32,        // updated by the background task
    pub abort:               AbortHandle,      // aborted on shutdown
}

impl SyncRegistry {
    pub fn try_register(&self, user_id: Uuid, run: RunningSync) -> Result<(), AlreadyRunning>;
    pub fn snapshot_for(&self, user_id: Uuid) -> Option<RunningSyncSnapshot>;
    pub fn complete(&self, user_id: Uuid);
    pub fn update_progress(&self, user_id: Uuid, pct: u32);
}
```

`try_register` uses `DashMap::entry(...).or_*` with a `match` so insertion is atomic. **Why one entry per user, not per run_id**: the rule is one *active* sync per user. Indexing by `user_id` makes the conflict check O(1).

The `SyncRegistry` is the in-memory mirror of the DB rows; on graceful shutdown each entry is aborted and the corresponding `sync_operations` row is updated to `failed` with `error_message = "server_shutdown"` (analogous to [../pipelines.md §12](../pipelines.md#12-crash--restart-recovery)).

### 3.2 Background task model

Sync uses its own background machinery (`SyncRegistry` + `sync_operations` table), not the `cognee_core::PipelineRunRegistry` from [../pipelines.md](../pipelines.md). Why: the `sync_operations` table is distinct from `pipeline_runs` (different schema, different TTL, different progress model); the WebSocket subscription protocol does not currently target sync. If a `/sync/subscribe/{run_id}` endpoint is added later, we may unify behind `PipelineRunRegistry` — see Open Questions.

The background task ([sync.py `_perform_background_sync`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/sync.py#L167-L229)) does roughly:
1. `mark_sync_started(run_id)` — sets `status = in_progress`, `started_at = NOW()`.
2. Loop up to 3 retries: `_sync_to_cognee_cloud(...)` → returns counts and per-dataset hashes.
3. On success: `mark_sync_completed(...)` with totals.
4. On final failure: `mark_sync_failed(run_id, error_message)`.

The `_sync_to_cognee_cloud` step itself ([sync.py L232–L387](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/sync.py#L232-L387)):
1. `_check_hashes_diff(dataset)` — POST to cloud's `/api/sync/{dataset_id}/diff` with local hashes; receives `{missing_on_remote, missing_on_local}`.
2. Concurrently per dataset: upload missing-on-remote, download missing-on-local.
3. Trigger remote cognify (`POST /api/cognify`) once for all uploaded datasets.
4. Trigger local cognify if any files were downloaded.
5. Update progress 0% → 80% → 90% → 95% → 100%.

The Rust port lives in `cognee-cloud` (or a new `cognee-sync` crate — Open Question). The handler in `crates/http-server/` only orchestrates: dispatch and DB row creation; everything else is delegated.

### 3.3 Persistence — `sync_operations` table

Schema ported from [`SyncOperation.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/sync/models/SyncOperation.py):

| Column | Type | Notes |
|---|---|---|
| `id` | UUID PK | Internal row id, separate from `run_id`. |
| `run_id` | TEXT UNIQUE NOT NULL INDEX | Public id, **string** UUID v4 — matches Python's `str` annotation. |
| `status` | TEXT NOT NULL DEFAULT `'started'` | Enum: `started \| in_progress \| completed \| failed \| cancelled`. |
| `progress_percentage` | INTEGER NOT NULL DEFAULT 0 | 0–100. |
| `dataset_ids` | JSONB | Array of UUID strings. |
| `dataset_names` | JSONB | Array of strings. |
| `user_id` | UUID INDEX | FK reference; not enforced as FK to allow user deletion without breaking sync history. |
| `created_at` | TIMESTAMPTZ NOT NULL DEFAULT NOW() | — |
| `started_at` | TIMESTAMPTZ NULL | Set by `mark_sync_started`. |
| `completed_at` | TIMESTAMPTZ NULL | Set by `mark_sync_completed` / `mark_sync_failed`. |
| `total_records_to_sync` | INTEGER NULL | Hint, set during run. |
| `total_records_to_download` | INTEGER NULL | — |
| `total_records_to_upload` | INTEGER NULL | — |
| `records_downloaded` | INTEGER DEFAULT 0 | — |
| `records_uploaded` | INTEGER DEFAULT 0 | — |
| `bytes_downloaded` | INTEGER DEFAULT 0 | — |
| `bytes_uploaded` | INTEGER DEFAULT 0 | — |
| `dataset_sync_hashes` | JSONB | `{dataset_id_str: {"uploaded": [hashes], "downloaded": [hashes]}}` — data lineage. |
| `error_message` | TEXT NULL | — |
| `retry_count` | INTEGER DEFAULT 0 | Bumped per retry. |

SeaORM migration in `crates/database/src/migrator/m_<timestamp>_sync_operations.rs`. Idempotent against a Python-seeded DB.

### 3.4 Error envelope quirks

Both endpoints use a `{"error": ...}` envelope on failure paths instead of the canonical `{"detail": ...}`. This is the `ApiError` exception case described in [../architecture.md §9](../architecture.md#9-error-handling) — Python's router uses `JSONResponse` directly, bypassing the global `ApiError` handler. Implement via a one-off `ApiError::CustomBody { status: u16, body: serde_json::Value }` variant or per-handler `Result<Json<T>, (StatusCode, Json<E>)>`. The cross-SDK parity tests will lock this down.

## 4. DTO definitions

```rust
// crates/http-server/src/dto/sync.rs

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// `POST /api/v1/sync` request body. Maps `cognee.api.v1.sync.routers.get_sync_router.SyncRequest`.
///
/// Pydantic: `dataset_ids: Optional[List[UUID]] = None`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct SyncRequestDTO {
    /// Datasets to sync. `None` or `[]` means "all datasets the caller has write access to".
    #[serde(default)]
    pub dataset_ids: Option<Vec<Uuid>>,
}

/// `POST /api/v1/sync` 200 response.
///
/// Maps `cognee.api.v1.sync.SyncResponse`. **Note**: `run_id` is `String`, not
/// `Uuid`, to match Python's `str` annotation
/// (https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/sync.py#L89).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct SyncResponseDTO {
    pub run_id:        String,           // always uuid4 string; preserve type for parity
    pub status:        String,           // literal "started"
    pub dataset_ids:   Vec<String>,      // stringified UUIDs (Python parity)
    pub dataset_names: Vec<String>,
    pub message:       String,
    pub timestamp:     String,           // ISO-8601 from the request handler
    pub user_id:       String,           // stringified user UUID
}

/// `POST /api/v1/sync` 409 body when another sync is already running for the user.
///
/// Mirrors the dict at
/// https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py#L120-L131
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct SyncConflictDTO {
    pub error:   String,                  // literal "Sync operation already in progress"
    pub details: SyncConflictDetailsDTO,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct SyncConflictDetailsDTO {
    pub run_id:              String,
    pub status:              String,      // literal "already_running"
    pub dataset_ids:         Vec<Uuid>,   // Python returns these as the JSONB array directly
    pub dataset_names:       Vec<String>,
    pub message:             String,
    pub timestamp:           String,      // ISO-8601, the existing run's created_at
    pub progress_percentage: u32,
}

/// `GET /api/v1/sync/status` 200 response. Maps the dict at
/// https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py#L218-L234
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct SyncStatusOverviewDTO {
    pub has_running_sync:    bool,
    pub running_sync_count:  usize,
    /// Present only when `running_sync_count > 0`. Serialized as omitted when None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_running_sync: Option<LatestRunningSyncDTO>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct LatestRunningSyncDTO {
    pub run_id:              String,
    pub dataset_ids:         Vec<Uuid>,
    pub dataset_names:       Vec<String>,
    pub progress_percentage: u32,
    pub created_at:          Option<String>,    // ISO-8601 or None
}

/// 4xx/5xx body shared by simpler error paths in this router.
/// Python's router uses `JSONResponse(status_code=X, content={"error": "..."})`
/// rather than the canonical `ApiError` envelope, so the body shape is
/// `{ "error": "..." }`, not `{ "detail": "..." }`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct SyncErrorDTO {
    pub error: String,
}
```

## 5. Implementation tasks

1. Add DTO structs in `crates/http-server/src/dto/sync.rs`.
2. Add `crates/http-server/src/sync/registry.rs` with `SyncRegistry` (DashMap-backed, `try_register` atomic conflict check, abort-on-shutdown).
3. Add SeaORM migration `crates/database/src/migrator/m_<ts>_sync_operations.rs` matching the Python schema; idempotent against an existing Python-seeded DB.
4. Add `SyncOperationRepository` in `cognee-database` with `create_operation`, `mark_started`, `mark_completed`, `mark_failed`, `update_progress`, `running_for_user`, `get_by_run_id`. Mirror Python's [`methods/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/sync/methods) module.
5. Port the cloud-side flow (`_sync_to_cognee_cloud`, `_check_hashes_diff`, `_upload_missing_files`, `_download_missing_files`, `_trigger_remote_cognify`) into `cognee-cloud` (extending `crates/cloud/src/`) or a new `cognee-sync` crate (Open Question).
6. Add `crates/http-server/src/routers/sync.rs` with two handlers `post_sync` and `get_status`.
7. Wire `state.sync = Arc<SyncRegistry>` in `AppState::build` ([../architecture.md §6](../architecture.md#6-application-state--dependency-injection)).
8. Add `#[utoipa::path(...)]` annotations.
9. Unit tests:
   - `SyncRegistry::try_register` is atomic under N concurrent threads (only one wins).
   - `SyncErrorDTO` round-trips with `{"error": "..."}` (no `detail`).
   - `SyncResponseDTO` preserves `run_id: String` shape across serialize/deserialize.
10. Integration tests in `crates/http-server/tests/test_sync.rs`:
    - Empty `dataset_ids` is treated as "all my datasets".
    - Running `POST /sync` twice returns 409 the second time, with `details.run_id` matching the first.
    - `GET /sync/status` reflects the running sync, then `false` after the background task completes (mocked cloud).
    - `dataset_ids` containing UUIDs the user lacks `write` permission on are silently filtered (Python parity).
    - 401 on missing auth.
11. Cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_sync.py`:
    - With a mocked cloud (`COGNEE_CLOUD_API_URL=http://mock`), POST to both Python and Rust, assert the response shapes match exactly except for `run_id` and `timestamp`.
    - Concurrency: race two POSTs on each backend, assert the same 200/409 split.

## 6. Open questions

1. **Where does the cloud-side flow live?** Options: (a) extend `cognee-cloud` (fits the existing serve/disconnect work), (b) new `cognee-sync` crate, (c) inline in `crates/http-server/src/sync/`. Lean: **(a) extend `cognee-cloud`** — it already owns the cloud HTTP client (`CloudClient`, [`crates/cloud/src/cloud_client.rs`](../../crates/cloud/src/cloud_client.rs)) and the auth/credentials story. Adding `cognee_cloud::sync::run_background` keeps the cloud-facing surface contiguous.
2. **PostHog telemetry**: Python emits `send_telemetry(...)` events in both endpoints. Rust phase 1 does not port these (out of scope per [../observability.md §1](../observability.md#1-goals--non-goals)). Should we add a feature-gated `posthog` shim later? Defer to a follow-up doc.
3. **`run_id: str` vs `Uuid`**: Python types `run_id` as `str`, not `UUID`, even though it is always a uuid4 string. Should Rust DTOs use `Uuid` (better type) or `String` (parity)? Lean: **String** for parity; convert internally to `Uuid` for indexing if needed.
4. **OpenAPI schema accuracy**: Python advertises `dict[str, SyncResponse]` in its `response_model` annotation but actually returns a single `SyncResponse` object on the wire. Rust's utoipa schema mirrors the **actual wire body** (`SyncResponse`), not Python's broken annotation — this is parity with Python's *observable* behavior. The OpenAPI annotation in Python is the deviation; the wire is the source of truth.
5. **`/sync/status` 500 envelope**: keep Python's `{"error": "..."}` shape or convert to canonical `{"detail": "..."}`? Lean: **keep Python's shape** for byte-for-byte parity; add a `ApiError::CustomBody` variant.
6. **Unify with `cognee_core::PipelineRunRegistry`**: when we add `/sync/subscribe/{run_id}` (not in scope yet), should we collapse `SyncRegistry` into `PipelineRunRegistry` so the WS protocol is uniform? Defer; revisit when WS subscriptions are requested.

## 7. References

- Python router: [`cognee/api/v1/sync/routers/get_sync_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py).
- Python sync engine: [`cognee/api/v1/sync/sync.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/sync.py).
- Python persistence model: [`cognee/modules/sync/models/SyncOperation.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/sync/models/SyncOperation.py).
- Python sync repository methods: [`cognee/modules/sync/methods/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/sync/methods).
- AppState `SyncRegistry` declaration: [../architecture.md §6](../architecture.md#6-application-state--dependency-injection).
- Pipeline registry analogue (different table, different lifecycle): [../pipelines.md](../pipelines.md).
- Cloud HTTP client: [`crates/cloud/src/cloud_client.rs`](../../crates/cloud/src/cloud_client.rs).
- Per-router README and template: [README.md](README.md).
