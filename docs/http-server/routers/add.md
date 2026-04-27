# Router: add

Multipart ingest endpoint that takes a list of files (or URL/file-path strings packed as parts) and adds them to a dataset, kicking off the `add_pipeline`. This is the front door of the cognee write path — it is *not* responsible for knowledge-graph extraction (that is `cognify`); it stores raw bytes, hashes them, and registers `Data` rows under the target dataset.

Companion docs: [../plan.md](../plan.md), [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../tenants.md](../tenants.md), [../pipelines.md](../pipelines.md), [../observability.md](../observability.md).

## 1. Mount & file
- Mount prefix: `/api/v1/add`
- Router file: `crates/http-server/src/routers/add.rs`
- Python source: [`cognee/api/v1/add/routers/get_add_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/routers/get_add_router.py)
- Underlying SDK function: [`cognee/api/v1/add/add.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/add.py)
- Rust delegation target: `cognee_lib::api::add::add(...)` (re-exports `cognee_ingestion::AddPipeline`).

## 2. Endpoints

### 2.1 `POST /api/v1/add` — Ingest one or more files into a dataset

- **Auth**: `required` (`AuthenticatedUser`). When `REQUIRE_AUTHENTICATION=false` the default user is substituted (see [../auth.md §10](../auth.md#10-require_authentication-semantics)).
- **Path params**: none.
- **Query params**: none.
- **Request body**: `multipart/form-data`. See §2.1.1 for parts.
- **Response body** (`200 OK`, `application/json`): A single `PipelineRunInfoDTO` (the result of `add_pipeline` over the one target dataset). Python collapses `{dataset_id: PipelineRunInfo}` to a single value when only one dataset is processed (which is always for `/add`); we replicate this. Source: [`add.py:243-246`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/add.py#L243-L246).

  ```json
  {
    "status": "PipelineRunCompleted",
    "pipeline_run_id": "0193b0f1-ea2c-7000-8000-000000000001",
    "dataset_id":      "0193b0f1-aaaa-7000-8000-000000000002",
    "dataset_name":    "main_dataset",
    "payload":         null,
    "data_ingestion_info": [
      {
        "data_id":      "0193b0f1-bbbb-...",
        "content_hash": "d41d8cd98f00b204e9800998ecf8427e",
        "name":         "doc1.txt",
        "extension":    "txt",
        "mime_type":    "text/plain",
        "raw_data_location": "file:///var/cognee/data/text_d41d8cd9....txt"
      }
    ]
  }
  ```

  `status` is the discriminator: one of `PipelineRunStarted | PipelineRunYield | PipelineRunCompleted | PipelineRunAlreadyCompleted | PipelineRunErrored`. Source: [`PipelineRunInfo.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRunInfo.py).

  Note: when `run_in_background=true` (Python supports this via the SDK, but the **HTTP** endpoint does not surface it — there is no form field for it — see §3.4), the response is `PipelineRunStarted` with `data_ingestion_info=null`. We follow Python and **omit** the `run_in_background` knob from the HTTP API for parity.

- **Error responses**:

  | Status | Body shape | Condition |
  |---|---|---|
  | `400` | `{"error": "Either datasetId or datasetName must be provided.", "detail": null}` | Both `datasetId` and `datasetName` are missing/empty. Python source: [`get_add_router.py:93-99`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/routers/get_add_router.py#L93-L99). |
  | `401` | `{"detail": "Unauthorized"}` | No valid credential. |
  | `403` | `{"error": "<msg>", "detail": null}` | Permission gate denies (`write` on target dataset). Mapped from `PermissionDeniedError` raised by `resolve_authorized_user_dataset`. |
  | `422` | `{"detail": [...], "body": <echo>}` | Multipart shape invalid (axum's serde extractor failure routed through the custom `Json`-style mapper — see [../architecture.md §10](../architecture.md#10-request-validation)). |
  | `500` | `{"error": "Pipeline run errored", "detail": "<inner>"}` | `add_pipeline` returned a `PipelineRunErrored`. Source: [`get_add_router.py:112-119`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/routers/get_add_router.py#L112-L119). |
  | `500` | `{"error": "Internal server error", "detail": "<exception>"}` | Any other exception bubbling out of `cognee_add`. Source: [`get_add_router.py:121-129`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/routers/get_add_router.py#L121-L129). |

  The error envelope is `ErrorResponse {error, detail}` (note: **`error`**, not `detail` as the top-level key — `add` and `update` predate the standard `ApiError` shape and use a `{error, detail}` envelope from [`cognee/api/DTO.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/DTO.py)). Per-router cross-cutting note: this is one of the few routers that does **not** use the canonical `{detail: "..."}` envelope. See §3.1 below.

#### 2.1.1 Multipart parts

| Part name | Required | Cardinality | Content type | Backing | Notes |
|---|---|---|---|---|---|
| `data` | No (but the call is meaningless without it; Python defaults to `None`) | 0..N | `application/octet-stream` (or `text/*`, `application/pdf`, ...) | Streamed to a per-request temp file via `tokio::fs::File`. Each part is independently spooled. | Each `data` part is one file. URLs and S3 paths are passed as **text parts whose body is the URL string** (Python's `UploadFile` accepts a `BinaryIO` and the SDK pattern-matches on the value to detect a URL string — see [`add.py:84-89`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/add.py#L84-L89) and `resolve_data_directories` / `resolve_dlt_sources`). Rust will convert each part to a `DataInput` variant: `FilePath` for spooled files, `Url` if the part body deserializes to a `http(s)?://` or `s3://` string and is shorter than 4 KiB. |
| `datasetName` | Conditional | 0..1 | `text/plain` | Form field | Either `datasetName` or `datasetId` must be present (validated post-parse). Defaults to the literal `"main_dataset"` in the SDK if neither is given, but the **HTTP layer rejects** the missing-both case with 400. |
| `datasetId` | Conditional | 0..1 | `text/plain` | Form field | UUID v4. Empty string `""` is treated as "absent" (Python uses `Union[UUID, Literal[""], None]` to make Swagger happy — [`get_add_router.py:43`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/routers/get_add_router.py#L43)). When set, it MUST refer to an existing dataset; otherwise 404. |
| `node_set` | No | 0..N | `text/plain` | Form field, repeated | Each repetition is one tag. Python defaults to `[""]` and substitutes `None` when only the empty default is sent (`get_add_router.py:108-109`). The Rust handler reproduces: an empty list, a single empty string, or absence all map to `None`. |

  **Streaming behavior**: each `data` part is read with `axum::extract::Multipart::next_field()`, then spooled to disk with `tokio::io::copy(field, &mut tokio::fs::File::create(temp_path))`. **Nothing buffers in memory** beyond the 16 KiB chunk axum reads at a time. Temp files live under `<COGNEE_UPLOAD_SPOOL_DIR or std::env::temp_dir()>/cognee-uploads/<request_id>/<part_index>-<sanitized-filename>` and are unlinked after `add_pipeline` returns (success or failure). Sanitization replaces path separators with `_` and truncates names to 200 bytes.

  Form-field parts (`datasetName`, `datasetId`, `node_set`) are buffered in memory with a 4 KiB cap each — they are explicitly *not* spooled to disk. A non-`data` part exceeding 4 KiB returns `400 {"error": "Form field <name> exceeds 4 KiB"}`.

  Order of parts is **not** required to be `data → datasetName → datasetId → node_set`; axum reads parts in the order they appear in the body. The handler accumulates parts in a struct and only triggers validation/dispatch after the multipart stream is exhausted.

  **Body-size limit**: the global `DefaultBodyLimit::max(100 * 1024 * 1024)` from the middleware stack ([../architecture.md §8](../architecture.md#8-middleware-stack)) applies. For very large uploads, `/add` overrides per-route: `Router::route("/", post(post_add).layer(DefaultBodyLimit::max(MAX_ADD_BYTES)))` where `MAX_ADD_BYTES` reads `HTTP_BODY_LIMIT_BYTES_ADD` (default 1 GiB). Rationale: documents up to a few hundred MiB are realistic; the global cap is for JSON endpoints.

  **Max part count**: 256 parts per request (a sanity limit; `axum::extract::Multipart` does not enforce one). 257th part returns `400 {"error": "Too many parts (max 256)"}`. Independent of `data`-part-count vs total-part-count: enforced at the level of `Multipart::next_field` calls.

  **Backpressure**: when the per-route body limit is exceeded mid-stream, axum aborts the underlying `hyper::body::Body::next()` and the partial spool file is unlinked in the handler's `Drop` impl (an `UploadGuard` wrapping the temp dir).

  **URL/file-path part disambiguation** (Python parity — [`add.py:84-89`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/add.py#L84-L89)):
  - A `data` part whose body is < 4 KiB AND whose decoded UTF-8 starts with `http://`, `https://`, or `s3://` is treated as a URL/S3 reference rather than raw bytes. The temp file is unlinked and the `UploadedPart::url_payload` field is set.
  - All other `data` parts are treated as file uploads.
  - There is no out-of-band signal (no `X-Url-Part: true` header) — clients communicate intent purely through body size + scheme prefix.
  - Filename presence does **not** disambiguate. A part named `link.txt` whose body is `https://example.com/foo` is a URL.

- **Side effects**:
  - **File storage**: each spooled file is hashed (MD5 by default, matching Python — see [`cognee-ingestion`'s `ContentHasher`](../../crates/ingestion/src/)) and stored at `LocalStorage::store_stream(...)` under `text_<md5>.<ext>` (text content) or `<filename>` (binary). Streaming, no buffer.
  - **Relational DB**: inserts/updates one `data` row per file with `(id, name, extension, mime_type, content_hash, raw_data_location, owner_id, tenant_id, dataset_id)`; updates `pipeline_runs` (`add_pipeline` start + completion); resets prior `add_pipeline` and `cognify_pipeline` runs for this dataset (`reset_dataset_pipeline_run_status`, `add.py:221-223`).
  - **Graph DB**: none — `add` does not write to graph.
  - **Vector DB**: none.
  - **Channels**: none on `run_in_background=false`. (Background path is not exposed via HTTP; see §3.4.)
- **Delegation target**: `cognee_lib::api::add::add(data, dataset_name, dataset_id, user, node_set, …)` — single async call. Wraps `AddPipeline` from `cognee-ingestion`. The handler must avoid putting any business logic between the multipart parse and this call.
- **Validation rules** (cross-field, beyond serde):
  1. Either `datasetName` or `datasetId` must be present and non-empty. Otherwise `400`.
  2. `datasetId` must be a valid UUID when present. Empty string is normalized to absent.
  3. `node_set` of `[""]` (single empty repetition) is normalized to `None` (matches Python's quirky default-handling, [`get_add_router.py:107-109`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/routers/get_add_router.py#L107-L109)). Empty list also normalizes to `None`. A list with at least one non-empty entry is preserved.
  4. Each `data` part's filename, if present, must not contain path traversal sequences (`../`, `..\\`, leading `/`). On violation, return `400` with `{"error": "Invalid filename: <name>"}`.
  5. `data` parts whose body is < 4 KiB and whose decoded text starts with `http://`, `https://`, or `s3://` are interpreted as URL/S3 strings rather than raw bytes (Python parity — `resolve_dlt_sources`, [`add.py:214-219`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/add.py#L214-L219)).
- **Permission gate**: `write` on the target dataset (cite [../tenants.md §5](../tenants.md#5-permission-resolution) for the resolution algorithm and [../tenants.md §9](../tenants.md#9-repository-surface) for the trait surface). Concretely: `state.lib.permissions().user_can(user.id, dataset_id, "write")`. The internal `resolve_authorized_user_datasets` helper in `cognee-lib` wraps this for batch flows. If `dataset_id` refers to an existing dataset the user lacks `write` on, returns 403. If `dataset_name` is given and no dataset of that name exists for the user, **a new dataset is created** and the user is granted `read+write+share+delete` on it (matches Python's `create_dataset` flow and the parallel handler in `datasets.create_new_dataset`).
- **Rate / size limits**: per-route body limit overrides global; no rate limit in phase 1.
- **OpenAPI**:
  - Tag: `["add"]`
  - `requestBody`: `multipart/form-data` with the four parts described in §2.1.1. Use `utoipa`'s `#[utoipa::path(post, request_body(content_type = "multipart/form-data", content = AddMultipart))]`.
  - Responses: `200: PipelineRunInfoDTO`, `400/401/403/422/500: ErrorResponseDTO`.
  - Security: defaults to global `[BearerAuth, ApiKeyAuth]`.
- **Telemetry**:
  - Span name: `cognee.api.add` (matches [../observability.md §3.4](../observability.md#34-span-name-conventions)).
  - Attributes (record on the span):
    - `cognee.api.endpoint = "POST /v1/add"` (Python parity — [`get_add_router.py:81-89`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/routers/get_add_router.py#L81-L89)).
    - `cognee.dataset.name` (when `datasetName` is set).
    - `cognee.dataset.id` (when `datasetId` is set, or after resolution).
    - `cognee.add.file_count` (number of `data` parts).
    - `cognee.add.bytes_in` (total bytes streamed).
    - `cognee.pipeline.name = "add_pipeline"`.
    - `cognee.pipeline.run_id` (after pipeline starts).
    - `cognee.user.id`.
  - Event: `add.completed` with `dataset_id`, `pipeline_run_id`, `data_ingestion_info_count`.
- **Python parity notes**:
  - Returning a single `PipelineRunInfo` rather than a `{dataset_id: PipelineRunInfo}` map is **deliberate** — Python collapses the dict when there's exactly one dataset ([`add.py:243-246`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/add.py#L243-L246)). Always one for `/add`. Match this byte-for-byte.
  - The 400 path uses `JSONResponse` with `ErrorResponse(error="...").model_dump()` rather than `HTTPException`, so the wire shape is `{"error": "...", "detail": null}` *not* `{"detail": "..."}`. The Rust `ApiError::AddBadRequest(String)` variant emits the matching shape; do **not** use `ApiError::BadRequest` (which emits `{"detail": ...}`). See §3.1.
  - The Python handler swallows every exception and returns 500 with `{"error": "Internal server error", "detail": str(error)}`. We mirror this *only for parity*; in dev mode (`ENV=dev`), the handler additionally records the trace into the span buffer and emits `error!`-level logs with the full backtrace.
  - `node_set=[""]` is the FastAPI default placeholder so Swagger renders an empty array; it must be normalized to `None` before passing to the SDK ([`get_add_router.py:107-109`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/routers/get_add_router.py#L107-L109)).
  - Python uses `WithJsonSchema({"type": "string", "format": "binary"})` to coerce Swagger's rendering ([`get_add_router.py:22`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/routers/get_add_router.py#L22)). `utoipa`'s `format = "binary"` annotation produces the same OpenAPI surface.
  - `dataset_name` in the SDK defaults to `"main_dataset"`, but the HTTP handler does **not** apply that fallback — both fields being missing is a 400 (`get_add_router.py:93-99`). Match.

## 3. Cross-cutting behavior

### 3.1 Error envelope deviation

`add` and `update` use `ErrorResponse {error, detail}` — `error` is a short label, `detail` is the long string. This is **not** the canonical `{"detail": "..."}` shape used by every other router. The Rust server reproduces this for parity; introduce a dedicated `ApiError::WriteEndpointError { error: String, detail: Option<String>, status: StatusCode }` variant with a custom `IntoResponse` that serializes `ErrorResponseDTO`, *not* the standard `{detail}` shape.

### 3.2 Dataset auto-creation

When `datasetName` is given and no dataset of that name exists for the user, the SDK silently creates one and grants the user full ACLs. The HTTP handler does **not** advertise this distinction (no `Location` header, no 201). It is a side effect of `resolve_authorized_user_dataset` and matches Python.

### 3.3 No `cognify` triggered

`/add` does **not** invoke the cognify pipeline. The pipeline runs the `add_pipeline` only — see [`add.py:233`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/add.py#L233). Knowledge-graph extraction is a separate `POST /api/v1/cognify` call.

### 3.4 `run_in_background` not exposed

The Python SDK function exposes `run_in_background: bool` ([`add.py:42`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/add.py#L42)), but the HTTP router **does not** surface it. The Rust handler hardcodes `run_in_background=false` to match. If we ever want background ingestion via HTTP, we add a new field; do not silently flip it.

### 3.5 Session fingerprinting

`/add` does not interact with the session store. Session affinity is a `search`/`recall` concern.

## 4. DTO definitions

Located in `crates/http-server/src/dto/add.rs`.

```rust
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Multipart form for `POST /api/v1/add`.
///
/// `axum::extract::Multipart` does not derive into a struct directly; this DTO
/// exists primarily for OpenAPI documentation. The handler reads parts
/// explicitly, populating an internal `AddRequest` that mirrors this shape.
#[derive(Debug, ToSchema)]
#[allow(dead_code)]  // OpenAPI-only
pub struct AddMultipart {
    /// One or more files. Empty (zero parts) is allowed but is then a no-op.
    #[schema(format = "binary")]
    pub data: Vec<Vec<u8>>,

    /// Dataset name. Either this or `dataset_id` is required.
    #[schema(example = "research_papers")]
    #[serde(rename = "datasetName")]
    pub dataset_name: Option<String>,

    /// Dataset UUID. Either this or `dataset_name` is required. Empty string
    /// is treated as absent.
    #[schema(example = "")]
    #[serde(rename = "datasetId")]
    pub dataset_id: Option<String>,

    /// Repeated form field; each entry is one node-set tag.
    #[schema(example = json!([""]))]
    pub node_set: Option<Vec<String>>,
}

/// Internal post-parse representation; not on the wire.
pub struct AddRequest {
    pub files: Vec<UploadedPart>,
    pub dataset_name: Option<String>,
    pub dataset_id: Option<Uuid>,
    pub node_set: Option<Vec<String>>,
}

pub struct UploadedPart {
    pub file_name: Option<String>,
    pub content_type: Option<String>,
    pub temp_path: std::path::PathBuf,   // spooled file
    pub byte_count: u64,
    pub url_payload: Option<String>,     // populated when the part body is a URL/S3 string
}

/// Response shape — matches Python's `PipelineRunInfo.model_dump()`.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct PipelineRunInfoDTO {
    /// "PipelineRunStarted" | "PipelineRunYield" | "PipelineRunCompleted"
    /// | "PipelineRunAlreadyCompleted" | "PipelineRunErrored". String, not enum,
    /// to keep wire compatibility with Python's str status field.
    pub status: String,
    pub pipeline_run_id: Uuid,
    pub dataset_id: Uuid,
    pub dataset_name: String,
    /// Free-form. Cognify yields a `GraphDTO` here; add yields `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Per-data-item rows from `add_pipeline`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_ingestion_info: Option<Vec<DataIngestionInfoDTO>>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct DataIngestionInfoDTO {
    pub data_id: Uuid,
    pub content_hash: String,
    pub name: String,
    pub extension: String,
    pub mime_type: String,
    pub raw_data_location: String,
}

/// `add`/`update`-specific error envelope. Keep separate from `ApiError`'s
/// canonical `{detail: "..."}` shape for byte-for-byte Python parity.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct ErrorResponseDTO {
    pub error: String,
    pub detail: Option<String>,
}
```

Field-level mapping vs Python:

| Python (Pydantic) | Rust | Notes |
|---|---|---|
| `data: List[UploadFile]` | `files: Vec<UploadedPart>` | Internal name; wire name is `data`. |
| `datasetName: str` | `dataset_name: Option<String>` | Wire is camelCase; serde `rename = "datasetName"`. |
| `datasetId: UUID|""|None` | `dataset_id: Option<Uuid>` | Empty string normalizes to `None` in handler. |
| `node_set: List[str]` | `node_set: Option<Vec<String>>` | `[""]` and `[]` both normalize to `None`. |
| `PipelineRunInfo.status` | `PipelineRunInfoDTO.status: String` | Discriminator-as-string. |
| `PipelineRunInfo.pipeline_run_id` | `pipeline_run_id: Uuid` | Same. |
| `PipelineRunInfo.dataset_id` | `dataset_id: Uuid` | Same. |
| `PipelineRunInfo.dataset_name` | `dataset_name: String` | Same. |
| `PipelineRunInfo.payload` | `payload: Option<Value>` | `null` for add. |
| `PipelineRunInfo.data_ingestion_info` | `data_ingestion_info: Option<Vec<...>>` | Per-data-item info. |
| `ErrorResponse.error / .detail` | `ErrorResponseDTO.error / .detail` | Same. |

## 5. Implementation tasks

1. Add `AddMultipart`, `AddRequest`, `UploadedPart`, `PipelineRunInfoDTO`, `DataIngestionInfoDTO`, `ErrorResponseDTO` in `crates/http-server/src/dto/add.rs`.
2. Add a multipart decode helper `parse_add_multipart(req: Multipart, cfg: &AddConfig) -> Result<AddRequest, ApiError>` that:
   - Streams every `data` part to a temp file.
   - Detects URL/S3 string bodies (< 4 KiB, valid scheme).
   - Validates filename, normalizes `node_set` and `datasetId`.
3. Add the `post_add` handler in `crates/http-server/src/routers/add.rs`:
   - Inject `AuthenticatedUser`, `State<AppState>`, `Multipart`.
   - Parse → validate → build `AddRequest` → call `state.lib.add(...)`.
   - Map the `PipelineRunInfo` result to `PipelineRunInfoDTO`.
   - Map `PipelineRunErrored` to `500 ErrorResponseDTO`.
4. Add OpenAPI annotation `#[utoipa::path(post, ...)]`.
5. Per-route `DefaultBodyLimit::max(...)` override using `HTTP_BODY_LIMIT_BYTES_ADD` env var (default 1 GiB).
6. Unit tests in the same file: filename traversal rejection; `node_set` normalization; `datasetId=""` normalization; URL-string detection.
7. Integration tests in `crates/http-server/tests/test_add.rs`:
   - POST a single text file → 200 + `PipelineRunCompleted` + 1 `data_ingestion_info` row.
   - POST 3 files → 3 rows.
   - POST a `data` part whose body is `https://example.com/foo` < 4 KiB → URL ingestion path.
   - Both `datasetName` and `datasetId` missing → 400 with `{"error": "Either datasetId or datasetName must be provided.", "detail": null}`.
   - Unauthenticated → 401.
   - Caller lacks `write` on existing `datasetId` → 403.
   - 257-part request → 400.
8. Cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_add.py`:
   - POST identical multipart to Python uvicorn and Rust binary; assert same `data_id`, `content_hash`, `dataset_id` (UUID5 deterministic over content+owner — see [project guide "Deterministic IDs"](../../.claude/CLAUDE.md)).
   - Assert response JSON shape equality modulo `pipeline_run_id` (random per run).

## 6. Open questions

1. **Per-route body limit**: Python applies no per-route limit (only the global FastAPI/uvicorn limit). Rust matches: only the global `100 MiB` middleware cap ([../architecture.md §8](../architecture.md#8-middleware-stack)) applies. No `HTTP_BODY_LIMIT_BYTES_ADD` env var.
2. **Spool location**: Python writes to `tempfile.NamedTemporaryFile()` which honors `TMPDIR` / `TEMP` / `TMP`. Rust matches via `std::env::temp_dir()` — same env-var precedence. No `COGNEE_UPLOAD_SPOOL_DIR` Rust-only addition.
3. **URL detection threshold**: Python uses `len(value) < 4096` to decide whether a multipart form value is a URL ([`get_add_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/routers/get_add_router.py)). Rust matches: 4 KiB threshold, no configuration knob.
4. **Background ingestion via HTTP**: Python does not expose `run_in_background` on `/add`. Rust matches — no `?run_in_background=true` query parameter. Library callers can still drive background ingestion via `cognee_lib::add` directly.
5. **Streaming hash vs spool-then-hash**: implementation detail invisible at the wire layer. Spool-then-hash is simpler; either approach yields identical observable behavior. No wire impact.
6. **Concurrency cap on `data` parts**: Python has none. Rust matches — no per-route concurrency limit beyond the global middleware default.

## 7. References

- Python router: [`cognee/api/v1/add/routers/get_add_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/routers/get_add_router.py) (lines 1-131).
- Python SDK function: [`cognee/api/v1/add/add.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/add/add.py) (lines 22-246).
- Pipeline run shapes: [`cognee/modules/pipelines/models/PipelineRunInfo.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRunInfo.py).
- Error envelope: [`cognee/api/DTO.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/DTO.py) (`ErrorResponse`).
- Permission resolution: [`get_authorized_existing_datasets`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/data/methods/get_authorized_existing_datasets.py), [../tenants.md §5](../tenants.md#5-permission-resolution).
- Architecture: [../architecture.md §8 middleware](../architecture.md#8-middleware-stack), [../architecture.md §10 validation](../architecture.md#10-request-validation).
- Cross-router conventions: [README.md §3](README.md#3-cross-router-conventions).
- Companion router (sister write path): [update.md](update.md).
