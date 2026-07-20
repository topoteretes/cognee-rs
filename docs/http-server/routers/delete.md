# Router: delete (deprecated)

A deprecated single-endpoint router that aliases the canonical `DELETE /api/v1/datasets/{dataset_id}/data/{data_id}` (see [datasets.md §2.11](datasets.md#211-delete-apiv1datasetsdataset_iddatadata_id--delete-one-data-item)). Kept for backwards compatibility with clients pinned to cognee ≤ 0.3.8; new clients should use the canonical route.

Companion docs: [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../tenants.md](../tenants.md), [datasets.md](datasets.md), [forget.md](forget.md).

## 1. Mount & file
- Mount prefix: `/api/v1/delete`
- Router file: `crates/http-server/src/routers/delete.rs`
- Python source: [`cognee/api/v1/delete/routers/get_delete_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/delete/routers/get_delete_router.py)
- Underlying SDK: chains directly into `cognee::api::datasets::delete_data` — no new business logic.
- Rust delegation target: `cognee::api::datasets::delete_data(dataset_id, data_id, user, mode, delete_dataset_if_empty) -> Result<DeleteResult, _>`.

## 2. Endpoints

### 2.1 `DELETE /api/v1/delete` — Deprecated single-data delete

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params**:

  | Name | Type | Required | Default | Notes |
  |---|---|---|---|---|
  | `data_id` | `Uuid` | yes | — | UUID of the data row to delete. Source: [`get_delete_router.py:25`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/delete/routers/get_delete_router.py#L25). |
  | `dataset_id` | `Uuid` | yes | — | UUID of the dataset that owns `data_id`. Source: [`:26`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/delete/routers/get_delete_router.py#L26). |
  | `mode` | `String` | no | `"soft"` | `"soft"` (default) or `"hard"`. `"hard"` mode also removes degree-one entity nodes. Python warns: don't use `"hard"` — it's dangerous. Source: [`:27`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/delete/routers/get_delete_router.py#L27), [`datasets.py:128`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/datasets.py#L128). |
  | `delete_dataset_if_empty` | `bool` | no | `false` | If true and removing this data leaves the dataset empty, delete the dataset row too. Source: [`:29`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/delete/routers/get_delete_router.py#L29). |

  As with `update`, the params are query, not body, because Python types them as plain function parameters.

- **Request body**: none.
- **Response body** (`200 OK`): `{"status": "success"}`. Source: [`datasets.py:157`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/datasets.py#L157), [`:175`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/datasets.py#L175). Identical to `DELETE /api/v1/datasets/{id}/data/{did}`.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | No valid credential. |
  | `409` | `{"error": "<inner>"}` | Generic catch — Python wraps every exception in 409. Source: [`get_delete_router.py:67-69`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/delete/routers/get_delete_router.py#L67-L69). **Note**: 409 (`Conflict`) is unusual for a delete — it's not a write conflict. Reproduced for parity. |
  | `422` | `{"detail": [...], "body": ...}` | Missing or invalid `data_id`/`dataset_id`. |

  Notably, Python does **not** distinguish 404 (not found) from 403 (no permission) here — both bubble through as 409 with the inner error stringified. The canonical route ([datasets.md §2.11](datasets.md#211-delete-apiv1datasetsdataset_iddatadata_id--delete-one-data-item)) does distinguish; this is one of the reasons the alias is deprecated.

- **Side effects**: identical to `DELETE /api/v1/datasets/{dataset_id}/data/{data_id}` (see [datasets.md §2.11](datasets.md#211-delete-apiv1datasetsdataset_iddatadata_id--delete-one-data-item)):
  - **Relational DB**: removes the `data` row; optionally removes the `dataset` row if `delete_dataset_if_empty=true` and the dataset is now empty.
  - **Graph DB**: removes the data's subgraph (`delete_data_nodes_and_edges` for normal data, `legacy_delete` for orphan data with no graph nodes — see [`datasets.py:163-168`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/datasets.py#L163-L168)).
  - **Vector DB**: removes vector points associated with the data.
  - **File storage**: removes the raw file at `data.raw_data_location`.
- **Delegation target**: `cognee::api::datasets::delete_data(dataset_id, data_id, user, mode, delete_dataset_if_empty)`. The handler is essentially a one-liner.
- **Validation rules**: `data_id` and `dataset_id` are valid UUIDs; `mode` is `"soft"` or `"hard"` (Python doesn't enforce this — any string passes through, and the SDK's `delete_data` doesn't read the param past line 128. Rust SHOULD validate and reject unknown values with 422 to avoid silent typos).
- **Permission gate**: `delete` on `dataset_id` (cite [../tenants.md §5](../tenants.md#5-permission-resolution)). Same as canonical route.
- **Rate / size limits**: none.
- **OpenAPI**:
  - Tag: `["delete (deprecated)"]` — keep separate from `["datasets"]` so OpenAPI consumers can spot the deprecation.
  - `deprecated: true` on the operation (Python: [`get_delete_router.py:19-22`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/delete/routers/get_delete_router.py#L19-L22)).
  - Response: `200`, `409`, `422`. Hide from default Swagger UI exploration but keep discoverable.
  - Description text: include the migration note `"Use DELETE /v1/datasets/{dataset_id}/data/{data_id} instead. Removed in 0.4.0."`
- **Telemetry**:
  - Span name: `cognee.api.delete` (note: not `cognee.api.delete.data` — keep the deprecated route's span distinct from the canonical route's `cognee.api.datasets.data.delete` so traces can show how often the deprecated path is exercised).
  - Attributes: `cognee.api.endpoint = "DELETE /v1/delete"`, `cognee.dataset.id`, `cognee.data.id`, `cognee.delete.mode`, `cognee.delete.delete_dataset_if_empty`, `cognee.user.id`, `cognee.deprecated = true`.
  - Recommend an `info!` log at handler entry: `"Deprecated route DELETE /v1/delete invoked by user=<id>"` so deployments can monitor migration progress.
- **Python parity notes**:
  - The `@deprecated` decorator emits a runtime warning and (in OpenAPI) marks the operation deprecated. We replicate at the OpenAPI layer (`#[utoipa::path(... deprecated)]`) and also via `Sunset` / `Deprecation` HTTP headers per RFC 8594:
    - `Deprecation: true`
    - `Sunset: <date when removed>` (target: 2026-12-01, configurable via `COGNEE_DEPRECATED_SUNSET_DELETE`).
    - `Link: </api/v1/datasets/{dataset_id}/data/{data_id}>; rel="successor-version"`
    Python doesn't send these headers; this is a Rust-side enhancement that doesn't break parity.
  - 409 catch-all is intentional. Match.
  - The `mode` parameter has no effect inside `delete_data` for `"soft"` (the default) — it's only checked in `legacy_delete` for backward compatibility. The Rust port should accept the param, plumb it through, and not error on `"hard"` even though it's discouraged.

## 3. Cross-cutting behavior

### 3.1 Deprecation handling

This router is the only one in Phase 2 marked deprecated. The Rust implementation:

1. Adds `deprecated = true` in OpenAPI.
2. Sends `Deprecation: true` + `Sunset: <date>` + `Link: <successor>` headers.
3. Tags traces with `cognee.deprecated = true`.
4. Logs a one-line deprecation warning per invocation at `info` level (use `tracing::warn!` to make the line stand out).

### 3.2 Error envelope inconsistency

The 409 path uses `{error: ...}` while the canonical successor uses `{detail: ...}`. Reproduced for parity — clients writing migration tooling need to handle both shapes. The Rust `ApiError::DeprecatedConflict(String)` variant emits the `{error}` shape; the canonical route uses `ApiError::Internal` / `ApiError::NotFound` which emit `{detail}`.

### 3.3 No new permission checks

Same `delete` permission as the canonical route; no shortcuts or extra gates introduced by the deprecation alias.

### 3.4 Single-handler router

This router has exactly one endpoint. The router file is intentionally minimal — about 40 lines once we factor out the trait derives. Future maintainers should resist adding new endpoints under `/api/v1/delete`; new functionality goes under `/api/v1/datasets/...`.

## 4. DTO definitions

Located in `crates/http-server/src/dto/delete.rs`. All DTOs are tiny.

```rust
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// Query string for `DELETE /api/v1/delete` (deprecated).
#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "snake_case")]
pub struct DeleteQuery {
    /// UUID of the data row to delete.
    pub data_id: Uuid,

    /// UUID of the dataset that owns `data_id`.
    pub dataset_id: Uuid,

    /// `"soft"` (default) or `"hard"`. Hard mode also removes degree-one
    /// entity nodes; Python documents it as dangerous.
    #[serde(default = "default_mode")]
    pub mode: DeleteMode,

    /// If true and the dataset becomes empty after deletion, also delete
    /// the dataset row.
    #[serde(default)]
    pub delete_dataset_if_empty: bool,
}

fn default_mode() -> DeleteMode {
    DeleteMode::Soft
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DeleteMode {
    Soft,
    Hard,
}

/// Response body. Python returns `{"status": "success"}`.
#[derive(Debug, Serialize, ToSchema)]
pub struct DeleteSuccessResponseDTO {
    pub status: String,
}

impl DeleteSuccessResponseDTO {
    pub fn ok() -> Self {
        Self { status: "success".to_owned() }
    }
}

/// `{error}` envelope for 409.
#[derive(Debug, Serialize, ToSchema)]
pub struct DeleteErrorResponseDTO {
    pub error: String,
}
```

Field-level mapping vs Python:

| Python | Rust | Wire | Notes |
|---|---|---|---|
| `data_id: UUID` (query) | `DeleteQuery.data_id: Uuid` | `data_id` | snake_case query string. |
| `dataset_id: UUID` (query) | `DeleteQuery.dataset_id: Uuid` | `dataset_id` | — |
| `mode: str = "soft"` | `DeleteQuery.mode: DeleteMode` | `mode` | Defaults to `Soft`; Rust validates the enum, Python passes any string. |
| `delete_dataset_if_empty: bool = False` | `delete_dataset_if_empty: bool` | `delete_dataset_if_empty` | snake_case. |
| `{"status": "success"}` | `DeleteSuccessResponseDTO` | `{"status": "success"}` | snake_case. |
| `{"error": <str>}` | `DeleteErrorResponseDTO` | `{"error": <str>}` | snake_case. |

## 5. Implementation tasks

1. Add DTOs in `crates/http-server/src/dto/delete.rs` (all of §4).
2. Add `delete_data` handler in `crates/http-server/src/routers/delete.rs`:
   - Inject `AuthenticatedUser`, `State<AppState>`, `Query<DeleteQuery>`.
   - Permission check `delete` on `dataset_id`.
   - Delegate to `state.lib.datasets.delete_data(...)`.
   - Map exceptions to `409 DeleteErrorResponseDTO`.
   - Emit `Deprecation` / `Sunset` / `Link` headers on every response.
3. Mark the operation `deprecated = true` in `#[utoipa::path(...)]`.
4. Add a tracing event/log at handler entry tagged with `cognee.deprecated = true`.
5. Unit tests: query deserialization (default `mode=Soft`); enum rejects invalid values with 422; success response shape; error response shape.
6. Integration tests in `crates/http-server/tests/test_delete_deprecated.rs`:
   - Add a data row → DELETE via deprecated route → 200 + `{"status": "success"}`.
   - Wrong `data_id` → 409 with `{error}` envelope.
   - Without auth → 401.
   - Verify `Deprecation: true` header on every response (success and error).
   - Compare deletion side effects to the canonical route (round-trip parity).
7. Cross-SDK parity tests: same params hit Python uvicorn and Rust binary; assert identical response bodies and side effects.
8. Add a deprecation tracker: a single `AtomicU64` counter incremented per call, surfaced via the activity router (or as a metric in a follow-up).

## 6. Open questions

1. **Removal target version**: Python's `@deprecated` annotation says removed in 0.4.0 ([`get_delete_router.py:21`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/delete/routers/get_delete_router.py#L21)). The Rust port targets cognee 0.3.x at the moment — when do we actually drop the route? Lean: align with Python's removal cadence; ship a config flag `COGNEE_DISABLE_DEPRECATED_DELETE=true` for early opt-out.
2. **Sunset date**: 2026-12-01 is a guess. Confirm with the maintainers and align with the next semver-major release.
3. **`DeleteMode` strictness**: Python accepts a free-form string (no enum validation; passes through to the deletion service which interprets `"soft"` / `"hard"` and silently ignores anything else). Rust matches verbatim — accept the string, pass it through, no `422` for unknown values. Strict wire parity.
4. **Telemetry on deprecated calls**: emit the `cognee.deprecated = true` span attribute as an internal observability hook (does not change the wire). The "deprecation calls per minute" metric is a future internal observability concern only.
5. **Headers vs body for deprecation**: bodies are byte-identical to Python for client compat. Deprecation lives only in the `Sunset` / `Deprecation` HTTP headers (matching Python's behavior).

## 7. References

- Python router: [`cognee/api/v1/delete/routers/get_delete_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/delete/routers/get_delete_router.py) (lines 1-71).
- Underlying SDK: [`cognee/api/v1/datasets/datasets.py:124-175`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/datasets.py#L124-L175) — `datasets.delete_data`.
- Canonical successor route: [datasets.md §2.11](datasets.md#211-delete-apiv1datasetsdataset_iddatadata_id--delete-one-data-item).
- Multi-mode (everything / dataset / data_id) deletion: [forget.md](forget.md).
- Deprecation header RFC: [RFC 8594](https://datatracker.ietf.org/doc/html/rfc8594).
- Architecture: [../architecture.md §9](../architecture.md#9-error-handling).
- Cross-router conventions: [README.md §3](README.md#3-cross-router-conventions).
