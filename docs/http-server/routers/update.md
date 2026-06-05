# Router: update

Multipart `PATCH` endpoint that replaces an existing document in a dataset by deleting it and re-adding the new payload, then re-running cognify on the affected dataset. Distinct from `/add` (which appends) and `/datasets/{id}/data/{did}` `DELETE` (which only deletes); `update` chains delete → add → cognify in a single call.

Companion docs: [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../tenants.md](../tenants.md), [../pipelines.md](../pipelines.md), [add.md](add.md).

## 1. Mount & file
- Mount prefix: `/api/v1/update`
- Router file: `crates/http-server/src/routers/update.rs`
- Python source: [`cognee/api/v1/update/routers/get_update_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/update/routers/get_update_router.py)
- Underlying SDK function: [`cognee/api/v1/update/update.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/update/update.py) — orchestrates `delete_data` + `add` + `cognify`.
- Rust delegation target: `cognee_lib::api::update::update(...)`. Phase-2 plan: implement as the same composition the Python SDK does (`delete_data → add → cognify`). A future single-shot atomic delete-then-add is out of scope.

## 2. Endpoints

### 2.1 `PATCH /api/v1/update` — Replace an existing document and re-cognify the dataset

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params**:

  | Name | Type | Required | Notes |
  |---|---|---|---|
  | `data_id` | `Uuid` | yes | UUID of the existing data row to replace. Python source: [`get_update_router.py:39`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/update/routers/get_update_router.py#L39). |
  | `dataset_id` | `Uuid` | yes | UUID of the dataset that owns `data_id`. |

  Note: Python declares both as ordinary function parameters with type `UUID`. FastAPI infers them as **query** parameters because the function also has multipart `File`/`Form` parameters (no `Body` annotation). The Rust handler exposes them as `Query<UpdateQuery>`. This is a quirk worth documenting — if you're tempted to put them in the multipart body for "consistency" with `/add`, don't: it breaks parity.
- **Request body**: `multipart/form-data`. See §2.1.1.
- **Response body** (`200 OK`, `application/json`): `Map<UUID, PipelineRunInfoDTO>` — one entry per dataset cognified. Because `update` always operates on exactly one `dataset_id`, the map has exactly one entry. Source: [`update.py:104`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/update/update.py#L104) (returns the result of `cognify(...)` which is a `{dataset_id: PipelineRunInfo}` dict). **Important**: unlike `/add`, this is **not** collapsed to a single `PipelineRunInfo` — Python returns the dict shape directly. Match.

  ```json
  {
    "0193b0f1-aaaa-7000-8000-000000000002": {
      "status": "PipelineRunCompleted",
      "pipeline_run_id": "0193b0f1-bbbb-...",
      "dataset_id":      "0193b0f1-aaaa-...",
      "dataset_name":    "main_dataset",
      "payload":         null,
      "data_ingestion_info": null
    }
  }
  ```
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | No valid credential. |
  | `403` | `{"error": "<msg>", "detail": null}` | Permission gate denies (`write` on dataset). |
  | `404` | `{"error": "<msg>", "detail": null}` (mapped from `DataNotFoundError` / `UnauthorizedDataAccessError`) | `data_id` not found in `dataset_id`. Python's `delete_data` raises `UnauthorizedDataAccessError` for both "not found" and "no permission" — match. |
  | `422` | `{"detail": [...], "body": ...}` | Missing/invalid `data_id` or `dataset_id` query params; multipart shape invalid. |
  | `500` | `{"error": "Pipeline run errored", "detail": "<inner>"}` | Any of the cognify runs (only one for `update`) returns `PipelineRunErrored`. Source: [`get_update_router.py:97-112`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/update/routers/get_update_router.py#L97-L112). |
  | `500` | `{"error": "Internal server error", "detail": "<exception>"}` | Any other exception. |

  Same `ErrorResponseDTO {error, detail}` envelope as `add` (see [add.md §3.1](add.md#31-error-envelope-deviation)).

#### 2.1.1 Multipart parts

| Part name | Required | Cardinality | Content type | Backing | Notes |
|---|---|---|---|---|---|
| `data` | No (Python defaults to `None`) | 0..N | `application/octet-stream` etc. | Streamed to temp file | Same handling as `/add`: each part is one new file (or URL/S3 string < 4 KiB). HTTP(S) URL strings are fetched by the add phase with the MIME routing, HTML two-file storage, and URL metadata described in [add.md §2.1.1](add.md#211-multipart-parts). The replacement may be **multiple** new documents — this is unusual but matches the Python signature (`List[UploadFile]`, `update.py:14`). |
| `node_set` | No | 0..N | `text/plain` | Form field, repeated | Same `[""]`-defaults-to-`None` normalization as `/add`. |

  No `datasetName` or `datasetId` parts — those live in the query string.

  **Streaming**: identical to `/add` (per-part spool to `tokio::fs::File`).

  **Body-size limit**: same per-route override as `/add` (`HTTP_BODY_LIMIT_BYTES_UPDATE`, default 1 GiB).

  **Max part count**: 256 (matches `/add`).

- **Side effects**:
  - **Relational DB**: deletes the `data` row for `data_id`; inserts new `data` rows for each multipart `data` part; updates `pipeline_runs` for `add_pipeline` and `cognify_pipeline`. This is a multi-step transaction at the SDK level — there is **no atomic rollback** if the cognify step fails. Document loudly.
  - **Graph DB**: removes nodes/edges associated with the old `data_id` (via `delete_data_nodes_and_edges`), then re-extracts them in the cognify step. Briefly graph state is missing the document; clients polling `/datasets/{id}/graph` during the window may see partial data.
  - **Vector DB**: removes vector points for the deleted `data_id`, then re-inserts new ones during cognify. Same partial-state window.
  - **File storage**: deletes the old raw file (`legacy_delete` + `delete_data`), stores new files via `LocalStorage::store_stream`.
    - If a replacement part is an HTTP(S) URL, the chained add step fetches and stores it exactly like `/add`; the chained cognify step can then rebuild `WebPage` / `WebSite` provenance for the replacement content.
  - **Channels**: none in phase 2 (no background mode exposed).
- **Delegation target**: `cognee_lib::api::update::update(data_id, files, dataset_id, user, node_set, ...)`. Internally chains:
  1. `cognee_lib::api::datasets::datasets::delete_data(dataset_id, data_id, user)` — [`update.py:80-84`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/update/update.py#L80-L84).
  2. `cognee_lib::api::add::add(files, dataset_id, user, node_set, ...)` — [`update.py:86-95`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/update/update.py#L86-L95).
  3. `cognee_lib::api::cognify::cognify(datasets=[dataset_id], user, ...)` — [`update.py:97-103`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/update/update.py#L97-L103).
- **Validation rules**:
  1. `data_id` must be a valid UUID v4. Otherwise 422 (axum's `Query` deserialize fails through the validation extractor).
  2. `dataset_id` same.
  3. `node_set` of `[""]` or `[]` normalizes to `None`.
  4. Filename traversal check on each `data` part (same as `/add`).
  5. The data row identified by `data_id` must currently belong to `dataset_id`; this is enforced by `datasets.delete_data` ([`datasets.py:159-160`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/datasets.py#L159-L160) — raises `UnauthorizedDataAccessError`).
- **Permission gate**: `write` AND `delete` on the target dataset, via `state.lib.permissions().user_can(user.id, dataset_id, "write")` and `..."delete"` (see [../tenants.md §9](../tenants.md#9-repository-surface)). The chained `delete_data` actually checks for `delete` permission (see [`datasets.py:137`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/datasets.py#L137)), so the **effective** required permission set is `{delete, write}`. Resolve both before kicking off the chain to fail fast.
- **Rate / size limits**: per-route body limit; no rate limit.
- **OpenAPI**:
  - Tag: `["update"]`
  - `parameters`: `data_id` and `dataset_id` as `Query` parameters (`required = true`).
  - `requestBody`: `multipart/form-data` with `data` (binary, repeating) and `node_set` (string, repeating).
  - Responses: `200: HashMap<Uuid, PipelineRunInfoDTO>`, `403/404/422/500: ErrorResponseDTO`.
  - Security: defaults to global `[BearerAuth, ApiKeyAuth]`.
- **Telemetry**:
  - Span name: `cognee.api.update`.
  - Attributes:
    - `cognee.api.endpoint = "PATCH /v1/update"` (Python parity — [`get_update_router.py:74-83`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/update/routers/get_update_router.py#L74-L83)).
    - `cognee.dataset.id`, `cognee.data.id`.
    - `cognee.update.file_count`.
    - `cognee.pipeline.name = "update"` (logical name); chained `add_pipeline` and `cognify_pipeline` runs each get their own child span with their own `cognee.pipeline.name`.
    - `cognee.user.id`.
- **Python parity notes**:
  - Returns `Dict[UUID, PipelineRunInfo]`, **not** a single `PipelineRunInfo` (unlike `/add`). [`update.py:104`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/update/update.py#L104).
  - Python catches *any* of the dict values being `PipelineRunErrored` and converts the whole response to `500` — [`get_update_router.py:97-112`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/update/routers/get_update_router.py#L97-L112). Match: even though `update` always has one dataset, the loop is preserved for parity.
  - Same `ErrorResponse {error, detail}` envelope quirk as `/add`.
  - The Python `update.py` does **not** propagate `dataset_id` validation back to the caller cleanly — if `data_id` doesn't belong to `dataset_id`, the chained `delete_data` raises `UnauthorizedDataAccessError`. We surface this as 404 with the underlying message string; do not invent a separate 400 for the mismatch.
  - The Python implementation has no atomic rollback; if `cognify` errors after `delete_data` + `add` succeed, the old document is gone and the new document is in place but unprocessed. Match. Document the failure-mode trade-off in the response payload for the client to act on.
  - Python's docstring claims the endpoint matches `/add`'s 400 behavior, but in practice the handler does not check whether `dataset_id` is missing (it's a required query param, so FastAPI's validation runs first — 422). We do the same.

## 3. Cross-cutting behavior

### 3.1 Same error envelope as `/add`

`update` shares the `ErrorResponseDTO {error, detail}` shape with `/add` (see [add.md §3.1](add.md#31-error-envelope-deviation)). Reuse the same `ApiError::WriteEndpointError` variant.

### 3.2 No background mode

Like `/add`, `update` does not expose `run_in_background`. The chained cognify call inside `cognee_lib::api::update::update(...)` is always blocking (`update.py:97-103` calls `cognify(...)` without the `run_in_background=True` kwarg).

### 3.3 No partial-success protocol

If the chained `delete_data` succeeds but `add` fails, the response is a 500 with the original document already gone. The client must re-issue an `add` to recover. There is no transactional wrapper at the HTTP layer; the SDK does not offer one either.

### 3.4 Replacing one document with multiple

The multipart form supports zero-or-more `data` parts. Python accepts this without complaint — uploading two new files to "replace" `data_id` produces two new `data` rows (and an empty replacement is a no-op for the new content but still triggers cognify). We document this as legitimate but unusual; clients typically post exactly one replacement file.

## 4. DTO definitions

Located in `crates/http-server/src/dto/update.rs`.

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::dto::add::{ErrorResponseDTO, PipelineRunInfoDTO};

/// Query string for `PATCH /api/v1/update`.
///
/// `data_id` and `dataset_id` are query params, not multipart fields,
/// because Python's FastAPI handler declares them as ordinary function
/// parameters with type `UUID` (no `Form()`/`Body()` wrapper) — FastAPI
/// then routes them as Query.
#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "snake_case")]
pub struct UpdateQuery {
    /// UUID of the existing data row to replace.
    pub data_id: Uuid,
    /// UUID of the dataset that owns `data_id`.
    pub dataset_id: Uuid,
}

/// Multipart form for `PATCH /api/v1/update`.
///
/// As with `/add`, this DTO is OpenAPI-only; the handler reads parts manually.
#[derive(Debug, ToSchema)]
#[allow(dead_code)]
pub struct UpdateMultipart {
    /// One or more replacement files. Zero parts is a degenerate but legal
    /// case (no new content; cognify still re-runs).
    #[schema(format = "binary")]
    pub data: Vec<Vec<u8>>,

    /// Repeated form field; each entry is one node-set tag. `[""]` and `[]`
    /// both normalize to absent.
    pub node_set: Option<Vec<String>>,
}

/// Internal post-parse representation; not on the wire.
pub struct UpdateRequest {
    pub data_id: Uuid,
    pub dataset_id: Uuid,
    pub files: Vec<crate::dto::add::UploadedPart>,
    pub node_set: Option<Vec<String>>,
}

/// Response shape — keyed by dataset UUID.
///
/// Even though `update` always touches exactly one dataset, the wire shape
/// is a map (matches Python's `cognify()` return type). Do not collapse.
pub type UpdateResponseDTO = HashMap<Uuid, PipelineRunInfoDTO>;
```

Field-level mapping vs Python:

| Python | Rust | Notes |
|---|---|---|
| `data_id: UUID` (query) | `UpdateQuery.data_id: Uuid` | Query param, not multipart. |
| `dataset_id: UUID` (query) | `UpdateQuery.dataset_id: Uuid` | Query param, not multipart. |
| `data: List[UploadFile]` | `UpdateRequest.files: Vec<UploadedPart>` | Internal; wire name is `data`. |
| `node_set: List[str]` | `node_set: Option<Vec<String>>` | Same normalization as `/add`. |
| `Dict[UUID, PipelineRunInfo]` | `HashMap<Uuid, PipelineRunInfoDTO>` | One-entry map. |

## 5. Implementation tasks

1. Reuse `UploadedPart`, `PipelineRunInfoDTO`, `ErrorResponseDTO` from `crates/http-server/src/dto/add.rs`. Add `UpdateQuery`, `UpdateMultipart`, `UpdateRequest`, `UpdateResponseDTO` in `crates/http-server/src/dto/update.rs`.
2. Reuse `parse_add_multipart`-style helper, parameterized for `update` (no `datasetName`/`datasetId` parts allowed): `parse_update_multipart(req: Multipart, cfg: &UpdateConfig) -> Result<(Vec<UploadedPart>, Option<Vec<String>>), ApiError>`.
3. Add `patch_update` handler in `crates/http-server/src/routers/update.rs`:
   - Inject `AuthenticatedUser`, `State<AppState>`, `Query<UpdateQuery>`, `Multipart`.
   - Permission check `write` and `delete` on `dataset_id` (fail fast with 403).
   - Parse multipart → validate filenames → call `state.lib.update(...)`.
   - Map `Map<Uuid, PipelineRunInfo>` → `UpdateResponseDTO`.
   - If any value is `PipelineRunErrored`, return 500 with the error envelope.
4. OpenAPI annotation `#[utoipa::path(patch, ...)]` declaring both query params and the multipart body.
5. Per-route body limit override (`HTTP_BODY_LIMIT_BYTES_UPDATE`, default 1 GiB).
6. Unit tests: query-string deserialization for `data_id`/`dataset_id`; multipart `node_set` normalization.
7. Integration tests in `crates/http-server/tests/test_update.rs`:
   - Add a document via `/add`, capture `data_id`/`dataset_id`. PATCH `/update` with new content. Fetch graph. Assert old node gone, new node present.
   - Missing `data_id` query param → 422.
   - Wrong `dataset_id` (not the owner of `data_id`) → 404.
   - Caller lacks `delete` on dataset → 403.
   - Cognify failure mid-flight → 500 with `{"error": "Pipeline run errored", ...}`.
8. Cross-SDK parity tests: PATCH same payload to Python and Rust; assert response key set is the same and `PipelineRunCompleted` status matches.

## 6. Open questions

1. **Atomic rollback**: do we want a transactional wrapper that, on cognify failure, restores the old document? Costly and complex; not in Python; punt to a follow-up.
2. **Permission semantics**: Python's `delete_data` requires `delete` permission, but the `/update` docstring says `write`. The effective requirement is `{delete, write}` (`delete` for the chained delete, `write` for the chained add). Should we document this or relax to `write` only by adding a special-cased internal call? Lean: document the union and leave behavior identical.
3. **Multipart-vs-query for IDs**: should the Rust handler accept `data_id`/`dataset_id` from either query or body for ergonomics? Lean: no — strict parity with Python's quirk.
4. **Empty `data` parts**: zero-file PATCH triggers cognify on an unchanged dataset. Useful as a "force re-cognify" trick, or a footgun? Document but allow.
5. **Concurrency with `/add` on the same dataset**: two clients PATCHing the same `data_id` simultaneously could leave the row gone with no replacement. The SDK lacks pessimistic locking; document or add an advisory lock keyed on `(dataset_id, data_id)`.

## 7. References

- Python router: [`cognee/api/v1/update/routers/get_update_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/update/routers/get_update_router.py) (lines 1-125).
- Python SDK function: [`cognee/api/v1/update/update.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/update/update.py) (lines 1-105).
- Chained `delete_data`: [`cognee/api/v1/datasets/datasets.py:124-175`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/datasets.py#L124-L175).
- Chained `add`: [`cognee/api/v1/add/add.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/add.py).
- Chained `cognify`: [`cognee/api/v1/cognify/cognify.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/cognify.py).
- Pipeline run shape: [`cognee/modules/pipelines/models/PipelineRunInfo.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRunInfo.py).
- Architecture: [../architecture.md §8](../architecture.md#8-middleware-stack), [../architecture.md §10](../architecture.md#10-request-validation).
- Sister write path: [add.md](add.md) (request shape, multipart streaming, error envelope).
