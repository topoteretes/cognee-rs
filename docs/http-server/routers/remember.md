# Router: remember

The remember router is the one-shot "ingest + cognify" endpoint: it accepts a multipart upload of files, persists them as data, and immediately runs the cognify pipeline against the resulting dataset. It is the highest-level write endpoint Cognee exposes, designed for clients that want a single round-trip from raw bytes to a queryable knowledge graph.

It distinguishes itself from `/api/v1/add` (multipart upload only — no graph extraction) and `/api/v1/cognify` (graph extraction only — no upload). Internally it calls both in sequence; the response shape comes from `cognee.api.v1.remember.remember.RememberResult.to_dict()`.

Companion docs: [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../pipelines.md](../pipelines.md), [../tenants.md](../tenants.md), [../observability.md](../observability.md).

## 1. Mount & file
- Mount prefix: `/api/v1/remember`
- Router file: `crates/http-server/src/routers/remember.rs`
- Python source: [`cognee/api/v1/remember/routers/get_remember_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py)
- Mounted in [Python `client.py:295`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L295) with `tags=["remember"]`.

## 2. Endpoints

### 2.1 `POST /api/v1/remember` — ingest + cognify in one call

- **Auth**: `required` (`AuthenticatedUser`). Python uses `Depends(get_authenticated_user)` at [line 37](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L37).
- **Path params**: none.
- **Query params**: none.
- **Request body**: `multipart/form-data`. Field-by-field mapping:

  | Python part name | Python field type | Rust field | Rust type | Default | Notes |
  |---|---|---|---|---|---|
  | `data` | `List[UploadFile]` | `data` | `Vec<UploadedFilePart>` | empty | One or more files. Streamed to disk via `axum::extract::Multipart`. |
  | `datasetName` | `Form[Optional[str]]` | `dataset_name` | `Option<String>` | `None` | Note Python's camelCase part name — kept verbatim for wire compat. |
  | `datasetId` | `Form[Union[UUID, Literal[""], None]]` | `dataset_id` | `Option<DatasetIdRef>` | `None` | Same Python `Union` quirk as memify; reuse `DatasetIdRef` deserializer. |
  | `node_set` | `Form[Optional[List[str]]]` | `node_set` | `Option<Vec<String>>` | `Some(vec![""])` | Python defaults to `[""]` and the handler treats `[""]` as `None` ([line 84](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L84)). Rust must mirror. |
  | `run_in_background` | `Form[Optional[bool]]` | `run_in_background` | `Option<bool>` | `Some(false)` | See [pipelines.md §9](../pipelines.md#9-sync-vs-background-dispatch-http-wire-shapes). |
  | `custom_prompt` | `Form[Optional[str]]` | `custom_prompt` | `Option<String>` | `Some("")` | Forwarded to the cognify step. |
  | `chunks_per_batch` | `Form[Optional[int]]` | `chunks_per_batch` | `Option<u32>` | `Some(10)` | Note: Python defaults to `10` here (not `None` like cognify). Match. |

- **Response body**:

  - **Success — blocking (`run_in_background=false`)** — `200 OK`, `application/json`. Body shape: `RememberResult.to_dict()`. Python's [`RememberResult`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py) returns:

    ```json
    {
      "dataset_id":      "<uuid>",
      "dataset_name":    "<str>",
      "data_ids":        ["<uuid>", ...],
      "pipeline_run_id": "<uuid>",
      "status":          "PipelineRunCompleted",
      "duration_ms":     1234,
      "session_id":      "<uuid|null>",
      "data_size_bytes": 12345,
      "data_item_count": 3
    }
    ```

    The shape is whatever `RememberResult.to_dict()` produces; the handler returns `jsonable_encoder(result.to_dict())` ([line 90](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L90)). The Rust port mirrors the keys verbatim.

  - **Success — background (`run_in_background=true`)** — `200 OK`. Same `RememberResult.to_dict()` shape, but with `status="PipelineRunStarted"` and `duration_ms` measuring only the *add* portion plus the kickoff. The cognify portion runs in the background; subscribers can attach to `/api/v1/cognify/subscribe/{pipeline_run_id}` to follow it (the deterministic `pipeline_run_id` is the cognify run's id, since memify is not invoked here).

- **Error responses**:

  | Status | Body | Condition | Source |
  |---|---|---|---|
  | `400` | `{"detail": "Either datasetId or datasetName must be provided."}` | Both `datasetId` and `datasetName` empty/missing. Python raises `HTTPException(400, ...)` so the body shape is `{"detail": "..."}` (not `{"error": "..."}`). | [Python lines 70–74](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L70-L74) |
  | `400` | `{"detail": [{...}]}` | Multipart parse failure or bad form fields. | Custom multipart extractor |
  | `401` | `{"detail": "Unauthorized"}` | No JWT/cookie/API key. | `AuthenticatedUser` |
  | `403` | `{"detail": "..."}` | User lacks `write` permission on the target dataset. | `cognee_lib::permissions` |
  | `409` | `{"error": "An error occurred during remember."}` | Any exception during processing. **Note**: Python returns `409`, not `500`, for catch-all errors here ([line 91–96](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L91-L96)). The `detail` field is **not** populated — only the literal `error` string. | Python parity |
  | `413` | `{"detail": "request body too large"}` | Aggregate multipart body exceeds the limit (default 100 MiB). | `tower_http::limit::RequestBodyLimitLayer` |
  | `422` | `{"detail": [...]}` | Pydantic-level type errors on form fields. | |

  Note: remember is **not** a `PipelineRunErrored` → `500` endpoint like memify/cognify. **All** errors funnel through `409`. This is a Python parity quirk worth flagging — if `cognee_remember` raises because of a `PipelineRunErrored` inside the cognify step, the surface is `409` not `500`. Reproduce.

- **Side effects**:
  - Writes the uploaded files to file storage (`LocalStorage` by default) under the user's tenant.
  - Computes content hashes and inserts `Data` rows into the relational DB.
  - Inserts a `Dataset` row (creating it if `dataset_name` doesn't exist).
  - Writes `pipeline_runs` rows for the cognify portion (`DATASET_PROCESSING_INITIATED → STARTED → COMPLETED|ERRORED`).
  - Emits `RunEvent`s on the broadcast channel for the cognify run; subscribers can attach to the cognify WS endpoint using the returned `pipeline_run_id`.
  - Writes graph + vector data exactly as `/api/v1/cognify` does — see [cognify.md §2.1 side effects](cognify.md#21-post-apiv1cognify--run-the-cognify-pipeline).
  - **Does not** invoke memify; the resulting graph is queryable via `/api/v1/search` but not yet enriched with `Triplet:text` embeddings unless the caller follows up with `/api/v1/memify`.

- **Delegation target**: `cognee_lib::api::remember::remember(files, RememberConfig { dataset_name, dataset_id, node_set, custom_prompt, chunks_per_batch, user })`. **Note: no `run_in_background` field on `RememberConfig`** — the library function is synchronous after the §2 prerequisite refactor. Background dispatch is the HTTP handler's responsibility (it wraps this call in `state.pipelines.register_inline` or `register_background`). The Rust function mirrors Python's `cognee.api.v1.remember.remember`: stream files through `AddPipeline::run`, then invoke `cognify()` on the resulting dataset, then return a `RememberResult`. The router itself does no business logic beyond the dispatcher choice.

- **Validation rules**:
  - At least one of `datasetName` / `datasetId` must be set (Python rejects via `HTTPException(400)`).
  - `node_set=[""]` is treated as `None` (Python parity). Empty list `[]` is also treated as `None` to be defensive.
  - `dataset_id=""` → `None` via the `DatasetIdRef` deserializer (see [memify.md §3](memify.md#3-cross-cutting-behavior)).
  - At least one file must be present in `data` — Python's `default=None` makes the parameter optional; the inner `cognee_remember` raises if `data` is `None`. Surface as `409` (matches Python catch-all).
  - `chunks_per_batch > 0` — Rust additional guard, returns `400`. Python doesn't validate.

- **Permission gate**: `write` on the target dataset *if `dataset_id` is supplied* (cross-tenant); for `dataset_name` the dataset is created if missing under the user's tenant. The check lives inside `cognee_lib::api::remember::remember`.

- **Rate / size limits**:
  - **Aggregate body limit**: 100 MiB by default (`HTTP_BODY_LIMIT_BYTES`, see [architecture.md §11](../architecture.md#11-configuration)). Configurable per deployment.
  - **Streaming**: each part streams to a tempfile in `dirs::cache_dir()/cognee/uploads/<request-id>/<part-name>` — no in-memory buffering. See [architecture.md §8](../architecture.md#8-middleware-stack).

- **OpenAPI**:
  - `tags = ["remember"]`.
  - `security = [{BearerAuth: []}, {ApiKeyAuth: []}]`.
  - `requestBody.content` declared as `multipart/form-data` with `data` typed `array of binary` (per Python's `Annotated[UF, WithJsonSchema({"type": "string", "format": "binary"})]` workaround at [line 21](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L21)).
  - Documented responses: 200, 400, 403, 409, 413, 422.

- **Telemetry**:
  - Span name: `cognee.api.remember.post`.
  - Attributes: `user.id`, `dataset.ref`, `data.file_count`, `data.bytes_total`, `node_set.count`, `run_in_background`, `custom_prompt.present`, `chunks_per_batch`.
  - Sub-spans inherited from `cognee_remember` (`cognee.add.run`, `cognee.cognify.run`).
  - Emits the Python-equivalent telemetry event `"Remember API Endpoint Invoked"` via `send_telemetry` once per request, with `additional_properties.node_set` set ([Python lines 60–68](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L60-L68)).

- **Python parity notes**:
  - **Catch-all is `409`, not `500`** — unique to this router. The body is `{"error": "An error occurred during remember."}` literally, with no `detail`. Reproduce (do not enrich the body).
  - **`HTTPException` shape vs `JSONResponse` shape**: the 400 path uses `HTTPException(400, detail="...")` which produces `{"detail": "..."}`; the 409 path uses `JSONResponse(...)` with `{"error": "..."}`. Different keys for different errors — known Python inconsistency. Reproduce both.
  - **camelCase form field names**: `datasetName` and `datasetId` use camelCase, unlike most other endpoints that use snake_case. This is a wire-format quirk that frontend clients depend on. Use `#[serde(rename = "datasetName")]` etc. on the form-extractor struct (see §4).
  - **`node_set=[""]`** is the **default**, not `None` — Python's `Form(default=[""])`. The handler then translates `[""]` to `None` ([line 84](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L84)). Rust must apply the same translation *after* extraction.
  - The `chunks_per_batch=10` default differs from cognify's `None` default. Honour the literal default.
  - Python returns `jsonable_encoder(result.to_dict())` — UUIDs become strings, datetimes become ISO 8601 strings. Rust `serde_json::to_value(result)` with proper `chrono::DateTime<Utc>` serialization yields the same output.

### 2.2 `POST /api/v1/remember/entry` — store a typed memory entry

A JSON (not multipart) sibling endpoint that writes a single typed memory entry — a QA pair, an agent trace step, or feedback on a prior QA — into the session cache for a given `session_id`, rather than ingesting + cognifying files. It mirrors Python's `RememberEntryRequest` route at [`get_remember_router.py:101-113`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py#L101-L113).

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params**: none.
- **Request body**: `application/json`, DTO `RememberEntryRequestDTO` (`crates/http-server/src/dto/remember_entry.rs`). Wire is **camelCase** (Decision 10); snake_case forms are also accepted via per-field `serde(alias = ...)` for Python `populate_by_name=True` parity.

  | Field | Rust type | Required | Notes |
  |---|---|---|---|
  | `entry` | `cognee_models::memory::MemoryEntry` | Yes | Discriminated union tagged by `type`: `"qa"` / `"trace"` / `"feedback"`. Unknown `type` values fail to parse (`400`). |
  | `datasetName` (alias `dataset_name`) | `String` | No (default `"main_dataset"`) | Target dataset name. |
  | `sessionId` (alias `session_id`) | `String` | Yes | Session to attach the entry to. Empty/whitespace-only values are rejected with the Pydantic-style validation envelope (`400`). |

  The `entry` union variants (see `cognee_models::memory`): `qa` (`question`, `answer`, optional `context`, `feedbackText`, `feedbackScore`, `usedGraphElementIds`), `trace` (agent trace step), `feedback` (references a prior `qaId`).

- **Response body**: `200 OK`, `application/json`, `RememberResultDTO` (same struct as §2.1). `status` is `SessionStored` for the typed-entry path. The stored entry id is returned in the result.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `400` | `{"detail": [{"loc": ["body", "session_id"], "msg": "session_id is required for typed memory entries", "type": "value_error"}]}` | Missing/empty `session_id`. |
  | `400` | `{"detail": [...], "body": ...}` | Malformed body / unknown `entry.type`. Canonical `ApiError::Validation`. |
  | `401` | `{"detail": "Unauthorized"}` | Missing/invalid auth. |
  | `409` | `{"error": "An error occurred during remember."}` | Catch-all (e.g. components not wired). Same `{error}` envelope and `409` status as the multipart `POST /` path. |
  | `503` | `{"detail": "Session cache is not configured."}` | The session manager / session cache is not wired into `ComponentHandles`. |

- **Side effects**: best-effort upsert of the `session_records` row for `session_id` (log-and-swallow on failure), then writes the entry to the session cache via `SessionManager` (e.g. `save_qa` for the `qa` variant). Does **not** run add/cognify and does **not** write graph/vector data.
- **Delegation target**: `SessionManager` (from `ComponentHandles`, augmented with the configured LLM when present) plus `SessionLifecycleDb::ensure_and_touch_session`. Mirrors `crates/lib/src/api/remember.rs:625-769`.
- **Validation rules**: `session_id` must be non-empty after trimming (handler-level check). `entry` must deserialize into a known `MemoryEntry` variant.
- **Permission gate**: none beyond auth — the entry is scoped to the caller's `user_id`.
- **OpenAPI**: tag `remember`. Request body `RememberEntryRequestDTO`; the `entry` field is schema-typed as `serde_json::Value` (the `MemoryEntry` union lives in `cognee-models` without `ToSchema`). Responses `200`/`400`/`401`/`409`/`503`.
- **Telemetry**: span name `cognee.api.remember_entry`. Attributes: `endpoint = "POST /v1/remember/entry"`, `cognee.user_id`, `entry_type` (the union discriminator, recorded after parse).
- **Python parity notes**: camelCase wire with snake_case aliases (`populate_by_name=True`). `datasetName` defaults to `"main_dataset"`. The catch-all reuses the same `409 {"error": ...}` envelope as the multipart endpoint.

## 3. Cross-cutting behavior

- **Pipeline name**: the cognify portion uses `"cognify_pipeline"` (the same name `/api/v1/cognify` uses). The deterministic `pipeline_run_id` returned in the response collides intentionally — calling `/api/v1/remember` then `/api/v1/cognify` on the same dataset returns `PipelineRunAlreadyCompleted` for the second call.

- **`cognee_core::PipelineRunRegistry` methods called**:
  - `register_inline(spec, work)` for blocking.
  - `register_background(spec, work)` for background.
  - `RunSpec { run_id: Some(prid), pipeline_name: "cognify_pipeline", user_id: Some(user.id), dataset_id }`.
  - The `add` step does **not** register a separate `pipeline_runs` row in Python (add is not pipeline-tracked); Rust matches.

- **Library API note**: `cognee_lib::api::remember::remember()` no longer accepts a `run_in_background` parameter. The current library implementation has a bespoke `RememberResult` + `JoinHandle` shared-state machinery for in-process background mode ([crates/lib/src/api/remember.rs:75-107](../../../crates/lib/src/api/remember.rs#L75-L107), [:236-336](../../../crates/lib/src/api/remember.rs#L236-L336)) — that path is being removed as a prerequisite of this router landing. After the refactor, the function returns a synchronous `Result<RememberResult, Error>` whose fields reflect the completed-or-errored run; the HTTP layer wraps it via the registry as it does for cognify/memify/improve. See [pipelines.md §2](../pipelines.md#2-library-refactor-prerequisite).

- **Multipart streaming**: `axum::extract::Multipart` gives us a stream of parts. The handler iterates parts, dispatches:
  - String-typed parts (`datasetName`, `datasetId`, `run_in_background`, etc.) collected into a `RememberFormDTO`.
  - File-typed parts (`data`) streamed to tempfiles via `tokio::io::copy`.
  - Multiple `data` parts accumulate in a `Vec<UploadedFilePart>`.

- **Auth**: per [auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution); cookie / bearer / API key all accepted on multipart uploads.

- **Tenant scope**: dataset name resolves only against the user's tenant. `datasetId` permits cross-tenant access subject to the permission check.

## 4. DTO definitions

```rust
// crates/http-server/src/dto/remember.rs
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;
use chrono::{DateTime, Utc};

use crate::dto::util::DatasetIdRef;

/// Multipart form fields. Files are extracted separately as a `Vec<UploadedFilePart>`.
/// Source: cognee/api/v1/remember/routers/get_remember_router.py:29-38
#[derive(Debug, Clone, Default)]
pub struct RememberFormDTO {
    /// Python: `datasetName` — note camelCase wire name.
    pub dataset_name: Option<String>,

    /// Python: `datasetId` — accepts `""` as "no value".
    pub dataset_id: Option<DatasetIdRef>,

    /// Python default `[""]`; the handler treats `[""]` as `None`.
    pub node_set: Option<Vec<String>>,

    /// Python default `False`.
    pub run_in_background: Option<bool>,

    /// Python default `""`; `Some("")` means "use the cognify default prompt".
    pub custom_prompt: Option<String>,

    /// Python default `10`. Forwarded to cognify as-is.
    pub chunks_per_batch: Option<u32>,
}

/// One uploaded file from the `data` parts.
#[derive(Debug, Clone)]
pub struct UploadedFilePart {
    pub filename: String,
    pub content_type: Option<String>,
    pub temp_path: std::path::PathBuf,
    pub byte_count: u64,
}

/// Mirrors Python's `RememberResult.to_dict()` output.
/// Source: cognee/api/v1/remember/remember.py
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct RememberResultDTO {
    pub dataset_id: Uuid,
    pub dataset_name: String,
    pub data_ids: Vec<Uuid>,
    pub pipeline_run_id: Uuid,
    /// "PipelineRunStarted" | "PipelineRunCompleted" | "PipelineRunErrored"
    pub status: String,
    pub duration_ms: u64,
    pub session_id: Option<Uuid>,
    pub data_size_bytes: u64,
    pub data_item_count: u64,
    /// Present only on `PipelineRunErrored`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Optional ISO-8601 timestamp; only emitted by Python on background runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
}
```

The `DatasetIdRef` helper is shared with `/memify` and `/improve`; see [memify.md §4](memify.md#4-dto-definitions).

## 5. Implementation tasks

1. Add DTOs in `crates/http-server/src/dto/remember.rs` (`RememberFormDTO`, `UploadedFilePart`, `RememberResultDTO`).
2. Add multipart extractor logic in `crates/http-server/src/middleware/multipart.rs` (or extend the one used by `/add`):
   - Stream file parts to a per-request tempdir (`dirs::cache_dir()/cognee/uploads/<request-id>/`).
   - Collect string parts via `serde_urlencoded::from_str` into `RememberFormDTO`.
   - Apply the `[""]` → `None` translation for `node_set` after extraction.
3. Add the handler `post_remember` in `crates/http-server/src/routers/remember.rs`:
   - Parse multipart into `(RememberFormDTO, Vec<UploadedFilePart>)`.
   - Validate at least one of `dataset_name` / `dataset_id` (return `400` via `ApiError::BadRequest("Either datasetId or datasetName must be provided.")` — note the message goes to `detail`, not `error`).
   - Call `cognee_lib::api::remember::remember(files, RememberConfig { ... }, user)`.
   - On error, return `409 {"error": "An error occurred during remember."}` (no `detail`).
   - On success, serialise the `RememberResult` to JSON and return `200`.
4. Wire the router into `build_router`:

   ```rust
   .nest("/remember", remember::router())
   ```

5. Add OpenAPI annotations: `#[utoipa::path(post, path = "/api/v1/remember", tag = "remember", request_body(content_type = "multipart/form-data"), ...)]`.
6. Ensure the body limit middleware is mounted on this router (`tower_http::limit::RequestBodyLimitLayer::new(state.config.body_limit)`).
7. Add unit tests in the same file:
   - Both `datasetId` and `datasetName` empty → `400 {"detail": "Either datasetId or datasetName must be provided."}`.
   - Single file upload with `datasetName` → 200 with the response shape from §2.1.
   - `run_in_background=true` → 200 with `status="PipelineRunStarted"`.
   - Inner exception in `cognee_remember` → 409 with the literal error body.
   - `node_set=[""]` → translated to `None` before delegating.
   - `dataset_id=""` → translated to `None`.
8. Add integration tests in `crates/http-server/tests/test_remember.rs`:
   - End-to-end multipart upload with two files (gated behind `OPENAI_URL` for the cognify step).
   - Background dispatch attaches a WS to the returned `pipeline_run_id` and observes the cognify run completing.
   - 413 path: deliberately exceed the body limit.
9. Add cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_remember.py`:
   - Same multipart payload, same response keys (`dataset_id`, `dataset_name`, `data_ids`, `pipeline_run_id`, `status`, ...).
   - Same deterministic `pipeline_run_id` for the same `(user, dataset)` pair across both SDKs.

## 6. Open questions

1. **`datasetName`/`datasetId` casing**: Python uses camelCase for these two form parts, snake_case everywhere else. Should Rust offer both casings (accept `dataset_name` and `datasetName`) for SDK ergonomics, or strict-match Python? Proposal: strict-match for Phase 3; revisit in Phase 4 if SDK users complain.

2. **Catch-all `409`**: every error funnels into `409 {"error": "An error occurred during remember."}` with no detail. This makes operator debugging hard. Should Rust enrich the response with a server-side `detail` (e.g. `{"error": "...", "detail": "<msg>"}`) while staying compatible with the Python literal? Proposal: keep parity; surface details via `tracing` only.

3. **`PipelineRunErrored` status code**: Python's catch-all `409` swallows even `PipelineRunErrored` from the inner cognify. Should Rust treat `PipelineRunErrored` as `500` (matching cognify's behavior) and only fallback to `409` for other exceptions? Proposal: keep parity (always 409); document the divergence so Phase-4 hardening can revisit.

4. **Multipart tempfile lifetime**: tempfiles are deleted when the handler returns — but a slow client could disconnect mid-upload, leaving orphaned tempfiles. Add a periodic sweep of `cache_dir()/cognee/uploads/` for files older than 1h? Proposal: yes; track in [observability.md](../observability.md).

5. **Streaming vs buffered ingestion**: `cognee_remember` currently expects file paths. We stream to disk then pass the path. For very large files (≥1 GiB) we want true streaming through `add()`. Track as a Phase 4 follow-up.

6. **Empty `data` array**: Python's `data: List[UploadFile] = File(default=None)` allows zero files. The downstream `cognee_remember` raises in that case, which surfaces as 409. Should Rust pre-validate and return `400`? Proposal: yes — return `400 {"detail": "data: at least one file is required"}`. Document as a Rust-side correctness improvement.

7. **`chunks_per_batch=0`**: Python passes through; Rust returns `400` for safety. Same open question as cognify §6.2.

## 7. References

- Python router: [`cognee/api/v1/remember/routers/get_remember_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/routers/get_remember_router.py).
- Python core function: [`cognee/api/v1/remember/remember.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/remember/remember.py).
- Python `RememberResult`: defined in `cognee/api/v1/remember/remember.py` (search for `class RememberResult`).
- Pipeline registry & event channel: [pipelines.md](../pipelines.md).
- WebSocket protocol (the cognify portion's progress feed is reachable via the cognify WS endpoint using the returned id): [websocket.md](../websocket.md).
- Auth extractors: [auth.md §5](../auth.md#2-three-auth-mechanisms--precedence-and-resolution).
- Tenant resolution: [tenants.md §5](../tenants.md#5-permission-resolution).
- Multipart conventions: [routers/README.md §3.7](README.md#37-multipart-endpoints).
