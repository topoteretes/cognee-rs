# Router: forget

Single-endpoint router that exposes the unified deletion command — the v2 replacement for the older `prune` / `empty_dataset` / `delete_data` triplet. The body has three modes (data item, whole dataset, or everything) selected by which fields are populated. The cross-field rule is: **exactly one of `data_id` / `dataset` / `everything=true` must indicate a target**, with two exceptions documented below.

Companion docs: [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../tenants.md](../tenants.md), [delete.md](delete.md), [datasets.md](datasets.md).

## 1. Mount & file
- Mount prefix: `/api/v1/forget`
- Router file: `crates/http-server/src/routers/forget.rs`
- Python source: [`cognee/api/v1/forget/routers/get_forget_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/routers/get_forget_router.py)
- Underlying SDK function: [`cognee/api/v1/forget/forget.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py)
- Rust delegation target: `cognee::api::forget::forget(data_id, dataset, everything, user) -> Result<ForgetResultDTO, _>`. The SDK already exists (see [project guide](../../../.claude/CLAUDE.md), Memory API v2 section).

## 2. Endpoints

### 2.1 `POST /api/v1/forget` — Unified delete

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params**: none.
- **Request body** (`application/json`): `ForgetPayloadDTO`. Wire is camelCase (Python `InDTO`):
  ```json
  // Mode 1 — single data item:
  {"dataId": "...", "dataset": "main_dataset"}

  // Mode 2 — entire dataset:
  {"dataset": "main_dataset"}

  // Mode 3 — everything the user owns:
  {"everything": true}
  ```
  Source: [`get_forget_router.py:16-19`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/routers/get_forget_router.py#L16-L19).

  Field shapes:
  - `data_id: Optional[UUID]` (camelCase wire `dataId`)
  - `dataset: Optional[Union[str, UUID]]` (string OR UUID — accepts a dataset name or a UUID literal)
  - `everything: bool = false`

- **Response body** (`200 OK`): one of three shapes depending on mode (Python returns whatever `forget()` returns; the dict shapes are listed in source):
  - **Mode 1** (`data_id` + `dataset`): `{"data_id": "<uuid>", "dataset_id": "<uuid>", "status": "success"}`. Source: [`forget.py:182`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L182).
  - **Mode 2** (`dataset` only): `{"dataset_id": "<uuid>", "status": "success"}`. Source: [`forget.py:160`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L160).
  - **Mode 3** (`everything=true`): `{"datasets_removed": <int>, "status": "success"}`. Source: [`forget.py:139`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L139).

  Wire keys are snake_case in the response (Python returns plain dicts, not `OutDTO`). The Rust DTO must use `#[serde(rename_all = "snake_case")]` on the response variants — distinct from the camelCase `InDTO` request shape.

- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | No valid credential. |
  | `422` | `{"error": "Invalid request parameters. Specify dataset, data_id+dataset, or everything=True."}` | Cross-field validation failed **OR** the supplied UUID dataset cannot be resolved. Source: [`get_forget_router.py:55-61`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/routers/get_forget_router.py#L55-L61). Note: status code is **422** (not 400 or 404), and the envelope is `{"error": ...}`. See "Quirk" note below. |
  | `500` | `{"error": "An error occurred during deletion."}` | Generic catch — Python deliberately strips the inner error message to avoid leaking implementation details. Source: [`get_forget_router.py:62-68`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/routers/get_forget_router.py#L62-L68). The Rust port logs the inner error at `error!` level but keeps the wire body terse. |

  **Quirk**: Python catches `ValueError` for the cross-field validation case (422), but `_resolve_dataset_id` also raises `ValueError` when the dataset is missing — so a missing dataset returns 422 in Python with the same generic "Invalid request parameters" body, even though semantically it would be a 404. **Rust matches Python exactly: every `ValueError` (cross-field violation OR missing UUID dataset) returns 422 with the canonical "Invalid request parameters..." message.** Strict wire parity; do not split into 404 + 422.

- **Side effects** (mode-dependent):
  - **Mode 1** (data item):
    - Relational DB: deletes one `data` row.
    - Graph DB: removes the data's subgraph.
    - Vector DB: removes vector points.
    - File storage: removes the raw file.
  - **Mode 2** (dataset):
    - Relational DB: deletes the `dataset` row + all `data` rows under it.
    - Graph DB: removes the dataset's full subgraph.
    - Vector DB: removes all vector points under the dataset.
    - File storage: removes all raw files.
    - **Sessions**: not cleaned up — sessions are keyed by `(user_id, session_id)` not by `dataset_id`. Documented in [`forget.py:148-152`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L148-L152).
  - **Mode 3** (everything):
    - All of mode 2 for every dataset the user owns.
    - **Plus** session cache prune: clears Redis or filesystem session store via `cache_engine.prune()`. Source: [`forget.py:124-138`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L124-L138).
    - Cache cleanup is wrapped in a try/except — if it fails, logs a warning and returns success anyway (non-fatal). Match.
- **Delegation target**: `cognee::api::forget::forget(data_id, dataset, everything, user)`. The handler is a thin wrapper that:
  1. Validates cross-field rules (§2.1.1).
  2. Calls the SDK.
  3. Maps the result dict to `ForgetResultDTO` (a tagged enum).
- **Validation rules** (§2.1.1 Cross-field validation):

  Python's `forget()` SDK function applies the following rules ([`forget.py:92-106`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L92-L106)):

  | `everything` | `dataset` | `data_id` | Mode | Behavior |
  |---|---|---|---|---|
  | `true` | any | any | **Mode 3** | `data_id` and `dataset` are **ignored**. Match: do not raise even if both are set. Source: [`forget.py:44-46`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L44-L46) (docstring) and [`:92-94`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L92-L94). |
  | `false` | set | set | **Mode 1** | Delete one item. |
  | `false` | set | unset | **Mode 2** | Delete dataset. |
  | `false` | unset | set | **error** | Raises `ValueError("data_id requires dataset to be specified.")`. Source: [`forget.py:103-104`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L103-L104). Returns 422 in HTTP layer. |
  | `false` | unset | unset | **error** | Raises `ValueError("Specify dataset, data_id+dataset, or everything=True.")`. Source: [`forget.py:106`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L106). Returns 422. |

  Rust handler must reproduce this matrix exactly. Note that `everything=true` does **not** require `dataset` and `data_id` to be unset — they're silently ignored. Add a unit test to lock this down.

- **Permission gate** (mode-dependent):
  - **Mode 1**: `delete` on the resolved `dataset_id`. The SDK's `_resolve_dataset_id` calls `get_authorized_dataset(user, dataset_ref, "delete")` ([`forget.py:189-197`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L189-L197)) — failures raise `ValueError`.
  - **Mode 2**: `delete` on the resolved `dataset_id` (same flow).
  - **Mode 3**: `delete` on each dataset the user owns (filtered inside `datasets.delete_all` — failures on individual datasets are logged but not surfaced). The endpoint does not refuse for users with no `delete` ACLs; it simply finds zero datasets and returns `{"datasets_removed": 0, "status": "success"}`.

  Cite [../tenants.md §5](../tenants.md#5-permission-resolution).

- **Rate / size limits**: none specifically; the global JSON body limit (100 MiB) is far more than needed.
- **OpenAPI**:
  - Tag: `["forget"]`
  - Request body: `ForgetPayloadDTO` with `oneOf` discrimination on the three modes (utoipa schema attribute).
  - Responses: `200: ForgetResponseDTO` (untagged enum across the three shapes), `404/422/500: ForgetErrorResponseDTO`.
  - Security: defaults to global `[BearerAuth, ApiKeyAuth]`.
- **Telemetry**:
  - Span name: `cognee.api.forget`.
  - Attributes (per [../observability.md §3.3](../observability.md#33-span-instrumentation-conventions)):
    - `cognee.api.endpoint = "POST /v1/forget"` (Python parity — [`get_forget_router.py:36-43`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/routers/get_forget_router.py#L36-L43)).
    - `cognee.forget.target` = `"data_item" | "dataset" | "everything"` (matches Python's [`forget.py:54-58`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L54-L58)).
    - `cognee.dataset.name` (when `dataset` is set; the resolved name, not the input string).
    - `cognee.dataset.id` (after resolution).
    - `cognee.data.id` (mode 1 only).
    - `cognee.user.id`.
    - `cognee.result.count` = `datasets_removed` (mode 3) or `1` (modes 1 & 2). Matches Python's [`forget.py:81-83`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L81-L83) and [`:94`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L94).
- **Python parity notes**:
  - **`InDTO` aliasing**: the request body uses camelCase wire keys (`dataId`) because of `alias_generator=to_camel`. But the **response** uses snake_case (`data_id`, `dataset_id`, `datasets_removed`) because Python returns plain dicts, not `OutDTO`s. The Rust DTO split must respect this.
  - **`dataset: Optional[Union[str, UUID]]`**: the field accepts either a dataset name or a UUID literal. Python's Pydantic resolves the union at deserialize time (UUID first, fallback to str). The Rust DTO uses an internal `DatasetRef` enum that implements `Deserialize` with try-UUID-first semantics:
    ```rust
    #[derive(Debug, Clone)]
    pub enum DatasetRef { Id(Uuid), Name(String) }
    impl<'de> Deserialize<'de> for DatasetRef { /* try Uuid, fall back to String */ }
    ```
  - **Cache prune side-effect**: silently swallows errors from `cache_engine.prune()` and logs a warning. The Rust port should do the same with `tracing::warn!` and not propagate the error.
  - **Span name**: Python uses `new_span("cognee.api.forget")` ([`forget.py:71`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L71)), so our `cognee.api.forget` span name matches exactly — important for cross-SDK trace correlation.
  - **Telemetry-event name parity**: Python sends `"cognee.forget"` (lowercase, dot) ([`forget.py:61`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L61)), distinct from the API-endpoint event `"Forget API Endpoint Invoked"` ([`get_forget_router.py:37`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/routers/get_forget_router.py#L37)). The Rust handler emits both.
  - **Remote client passthrough**: Python's `forget()` checks `get_remote_client()` and proxies to the cloud ([`forget.py:76-85`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py#L76-L85)). The Rust port preserves this for parity via an injectable seam: `Components` carries an `Option<Arc<dyn CloudDeleteClient>>` field (`components.cloud_client`, defined in `src/cloud_client.rs`), defaulting to `None` in the OSS wiring. When it is `Some`, the handler proxies the request by calling `forward_forget(&payload, &user) -> Result<ForgetResponseDTO, CloudClientError>` instead of executing locally; the closed cloud build supplies the concrete implementation.

## 3. Cross-cutting behavior

### 3.1 `{error}` envelope on 4xx/5xx

This router uses `{"error": "<msg>"}` for all error responses. Reuse the `ApiError::ErrorEnvelope { error: String, status: StatusCode }` variant defined for [ontologies.md](ontologies.md). Do not unify with the canonical `{detail}`.

### 3.2 Three response variants

The 200 response is one of three shapes. The Rust DTO uses an untagged enum:

```rust
#[derive(Debug, Serialize, ToSchema)]
#[serde(untagged)]
pub enum ForgetResponseDTO {
    DataItem(ForgetDataItemResponse),
    Dataset(ForgetDatasetResponse),
    Everything(ForgetEverythingResponse),
}
```

OpenAPI consumers see `oneOf: [ForgetDataItemResponse, ForgetDatasetResponse, ForgetEverythingResponse]`. Python doesn't formally declare a response model (just `dict`); the Rust schema mirrors the *actual wire shapes* Python emits via this `oneOf`, with no behavioral divergence — only the OpenAPI annotation is more specific. The actual response bodies match Python verbatim.

### 3.3 Mode silently overrides on `everything=true`

When `everything=true`, the `data_id` and `dataset` fields are **ignored**. We do not 422 even if both are set with `everything=true`. Document this in the OpenAPI description so clients don't get tripped up.

### 3.4 Remote-client proxying

When the server is configured with a cloud client, `/forget` proxies upstream rather than executing locally. The handler checks `components.cloud_client` (an `Option<Arc<dyn CloudDeleteClient>>`) first and, when it is `Some`, calls `forward_forget(&payload, &user)` — returning `Result<ForgetResponseDTO, CloudClientError>` — instead of running the local delete path. The trait lives in `src/cloud_client.rs`; OSS wiring leaves the field `None`, and the closed cloud build injects the concrete client. Telemetry attribute `cognee.forget.proxied = true` should be set on this branch.

## 4. DTO definitions

Located in `crates/http-server/src/dto/forget.rs`.

```rust
use serde::{Deserialize, Serialize, Deserializer};
use utoipa::ToSchema;
use uuid::Uuid;

/// Request body for `POST /api/v1/forget`. Python `InDTO` (camelCase wire).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ForgetPayloadDTO {
    /// UUID of a specific data item to remove. Requires `dataset` to be set
    /// when used (mode 1). Ignored when `everything=true`.
    #[serde(default)]
    pub data_id: Option<Uuid>,

    /// Dataset name OR UUID. Set alone (mode 2) deletes the whole dataset.
    /// Set with `data_id` (mode 1) deletes one item. Ignored when
    /// `everything=true`.
    #[serde(default)]
    pub dataset: Option<DatasetRef>,

    /// If true, delete everything the user owns (mode 3). Other fields ignored.
    #[serde(default)]
    pub everything: bool,
}

/// Accept either a UUID or a free-form name.
#[derive(Debug, Clone, ToSchema)]
#[schema(value_type = String)]
pub enum DatasetRef {
    Id(Uuid),
    Name(String),
}

impl<'de> Deserialize<'de> for DatasetRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Try as UUID first; if parsing fails treat as a free-form name.
        let s = String::deserialize(d)?;
        match Uuid::parse_str(&s) {
            Ok(u) => Ok(DatasetRef::Id(u)),
            Err(_) => Ok(DatasetRef::Name(s)),
        }
    }
}

/// Response variants. Wire is snake_case (Python returns plain dicts, not
/// `OutDTO`).
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct ForgetDataItemResponse {
    pub data_id: Uuid,
    pub dataset_id: Uuid,
    pub status: String,  // "success"
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct ForgetDatasetResponse {
    pub dataset_id: Uuid,
    pub status: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct ForgetEverythingResponse {
    pub datasets_removed: usize,
    pub status: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(untagged)]
pub enum ForgetResponseDTO {
    DataItem(ForgetDataItemResponse),
    Dataset(ForgetDatasetResponse),
    Everything(ForgetEverythingResponse),
}

/// `{error}` envelope for 422 / 404 / 500.
#[derive(Debug, Serialize, ToSchema)]
pub struct ForgetErrorResponseDTO {
    pub error: String,
}

impl ForgetPayloadDTO {
    /// Cross-field validation. Returns the resolved mode or an error suitable
    /// for 422 mapping.
    pub fn resolve_mode(&self) -> Result<ForgetMode, &'static str> {
        if self.everything {
            return Ok(ForgetMode::Everything);
        }
        match (&self.data_id, &self.dataset) {
            (Some(_), Some(_))     => Ok(ForgetMode::DataItem),
            (None,    Some(_))     => Ok(ForgetMode::Dataset),
            (Some(_), None)        => Err("data_id requires dataset to be specified."),
            (None,    None)        => Err("Specify dataset, data_id+dataset, or everything=True."),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ForgetMode { DataItem, Dataset, Everything }
```

Field-level mapping vs Python:

| Python | Rust | Wire (request) | Wire (response) | Notes |
|---|---|---|---|---|
| `ForgetPayloadDTO.data_id: Optional[UUID]` | `data_id: Option<Uuid>` | `dataId` | — | `InDTO` camelCase. |
| `ForgetPayloadDTO.dataset: Optional[Union[str, UUID]]` | `dataset: Option<DatasetRef>` | `dataset` | — | Custom Deserialize tries UUID first. |
| `ForgetPayloadDTO.everything: bool` | `everything: bool` | `everything` | — | Default `false`. |
| `dict {data_id, dataset_id, status}` | `ForgetDataItemResponse` | — | `data_id`, `dataset_id`, `status` | snake_case. |
| `dict {dataset_id, status}` | `ForgetDatasetResponse` | — | `dataset_id`, `status` | — |
| `dict {datasets_removed, status}` | `ForgetEverythingResponse` | — | `datasets_removed`, `status` | — |
| `{"error": <str>}` | `ForgetErrorResponseDTO` | — | `error` | One-field envelope. |

## 5. Implementation tasks

1. Add DTOs in `crates/http-server/src/dto/forget.rs` (all of §4).
2. Implement `DatasetRef` custom `Deserialize` and document the UUID-first fallback.
3. Add `post_forget` handler in `crates/http-server/src/routers/forget.rs`:
   - Inject `AuthenticatedUser`, `State<AppState>`, `Json<ForgetPayloadDTO>`.
   - Call `payload.resolve_mode()`; on `Err`, return 422 with `{"error": <msg>}`.
   - Permission check (depends on mode).
   - If `components.cloud_client.is_some()`, dispatch to it via `forward_forget(&payload, &user)` and return its `ForgetResponseDTO`.
   - Otherwise call the local delegation target.
   - Map result to `ForgetResponseDTO` variant.
   - Map `DatasetNotFoundError` to **422** with the canonical `{"error": "Invalid request parameters. Specify dataset, data_id+dataset, or everything=True."}` body — Python collapses missing-dataset and cross-field-validation cases into one 422; we match.
   - Map all other errors to 500 with `{"error": "An error occurred during deletion."}` (terse for parity).
4. OpenAPI annotation `#[utoipa::path(post, ...)]` with `oneOf` for response.
5. Unit tests:
   - `ForgetPayloadDTO::resolve_mode()` for every row of the §2.1.1 truth table.
   - `DatasetRef` deserialization for valid UUID, invalid UUID (becomes Name), empty string.
   - `everything=true` ignores `data_id` and `dataset` (no 422).
6. Integration tests in `crates/http-server/tests/test_forget.rs`:
   - Add data + cognify; POST with `data_id+dataset` → 200 with `ForgetDataItemResponse`; data row gone.
   - Add data + cognify; POST with `dataset` → 200 with `ForgetDatasetResponse`; dataset gone.
   - Add data across 3 datasets; POST with `everything=true` → 200 with `datasets_removed=3`.
   - POST with neither field → 422 with the canonical message.
   - POST with `data_id` only (no `dataset`) → 422 with the canonical message.
   - Wrong UUID for `dataset` → 422 with the canonical "Invalid request parameters..." body (Python parity — missing-dataset collapses into the cross-field-validation envelope; see §2.1 quirk note).
   - No auth → 401.
   - `everything=true` + extra fields set → 200 (ignored), `datasets_removed` reflects real count.
7. Cross-SDK parity tests:
   - Send identical `{"everything": true}` payload to Python and Rust; assert response shape match.
   - Send mode-1 and mode-2 payloads; assert response match.
   - Send invalid payload; assert both return 422 with the same `error` body.

## 6. Open questions

1. **Multi-dataset mode-1 / mode-2**: Python does not accept `data_id` as a list. Rust matches; no batch-delete shape on the HTTP DTO. Library callers can still loop.
2. **Idempotency**: deleting a non-existent `data_id` mid-mode-1 returns 500 in Python (the SDK raises and the catch-all collapses it). Rust matches — same 500 with the canonical terse body.
3. **Cache prune scope on `everything=true`**: keyed by `user_id` only. Matches Python; no cross-tenant prune.
4. **Dry-run preview**: Python does not offer `?preview=true`. Rust matches: no preview query parameter.
5. **Remote-client proxy auth**: when proxying to the cloud, forward the user's bearer token (matches the existing cloud-proxy pattern in [../auth.md](../auth.md)). Document the proxy header set explicitly when implementing.

## 7. References

- Python router: [`cognee/api/v1/forget/routers/get_forget_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/routers/get_forget_router.py) (lines 1-71).
- Python SDK function: [`cognee/api/v1/forget/forget.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/forget/forget.py) (lines 1-198).
- Underlying `delete_data` / `empty_dataset` / `delete_all`: [`cognee/api/v1/datasets/datasets.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/datasets.py).
- Permission helpers: [`cognee/modules/data/methods/get_authorized_dataset.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/data/methods/get_authorized_dataset.py), [`get_authorized_dataset_by_name`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/data/methods/).
- Observability constants: [`cognee/modules/observability/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/observability) — `COGNEE_FORGET_TARGET`, `COGNEE_DATASET_NAME`, `COGNEE_RESULT_COUNT`.
- Sister deletion routes: [delete.md](delete.md) (deprecated single-data delete), [datasets.md §2.9–2.11](datasets.md#29-delete-apiv1datasets--delete-every-dataset-the-caller-owns) (canonical CRUD).
- Cross-router conventions: [README.md §3](README.md#3-cross-router-conventions).
- Permission resolution: [../tenants.md §5](../tenants.md#5-permission-resolution).
