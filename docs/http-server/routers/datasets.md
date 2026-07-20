# Router: datasets

The CRUD-and-everything-else router for datasets and the `Data` rows inside them. Eleven endpoints: list/create/delete the dataset itself, list/delete data items inside, fetch the rendered knowledge graph, fetch and update the per-dataset graph schema, query pipeline status, and stream the original raw bytes back to the client. This is the biggest router in the API by surface area.

Companion docs: [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../tenants.md](../tenants.md), [../observability.md](../observability.md), [delete.md](delete.md), [add.md](add.md).

## 1. Mount & file
- Mount prefix: `/api/v1/datasets`
- Router file: `crates/http-server/src/routers/datasets.rs`
- Python source: [`cognee/api/v1/datasets/routers/get_datasets_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py)
- Underlying SDK class: [`cognee/api/v1/datasets/datasets.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/datasets.py) (the `datasets` namespace) plus `cognee/modules/data/methods/*` for direct data access and `cognee/modules/graph/methods/get_formatted_graph_data.py` for the graph rendering.
- Rust delegation surface: a mix of `cognee::api::datasets::*`, `cognee::modules::data::*`, and `cognee::modules::graph::*`. The handler keeps no business logic.

## 2. Endpoints

11 sub-sections, ordered by HTTP method (GET → POST → PUT → DELETE) then by path.

### 2.1 `GET /api/v1/datasets` — List datasets the caller can read

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params**: none.
- **Request body**: none.
- **Response body** (`200 OK`): `[DatasetDTO, ...]`. Each entry has `{id, name, created_at, updated_at, owner_id}` (camelCase on the wire — see §3.1).
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | No valid credential. |
  | `418` | `{"detail": "Error retrieving datasets: <inner>"}` | Generic catch-all from Python. Source: [`get_datasets_router.py:122-126`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L122-L126). The `418 I'm a teapot` is intentional — Python uses it as a "couldn't categorize" status. The Rust `ApiError::Teapot(String)` variant maps here. |

- **Side effects**: none (read-only).
- **Delegation target**: `cognee::modules::users::permissions::get_all_user_permission_datasets(user, "read") -> Vec<Dataset>`. Source: [`get_datasets_router.py:118`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L118).
- **Validation rules**: none.
- **Permission gate**: `read` on each candidate dataset (the SDK call already filters; the handler does not re-check). [../tenants.md §5](../tenants.md#5-permission-resolution).
- **OpenAPI**: tag `["datasets"]`, response `200: list[DatasetDTO]`.
- **Telemetry**:
  - Span name: `cognee.api.datasets.list`.
  - Attributes: `cognee.api.endpoint = "GET /v1/datasets"`, `cognee.dataset.count` (after fetch), `cognee.user.id`.
- **Python parity notes**: 418 fallback for any error. Mirrored exactly.

### 2.2 `GET /api/v1/datasets/status` — Pipeline status for one or more datasets

- **Auth**: `required`.
- **Path params**: none.
- **Query params**:

  | Name | Type | Required | Notes |
  |---|---|---|---|
  | `dataset` | `Vec<Uuid>` (repeated) | yes (>= 1) | Each repetition adds one UUID. Python uses `Annotated[List[UUID], Query(alias="dataset")]` ([`get_datasets_router.py:368-370`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L368-L370)). The query string looks like `?dataset=...&dataset=...`. |
- **Request body**: none.
- **Response body** (`200 OK`): `{ "<uuid>": "DATASET_PROCESSING_INITIATED" | "DATASET_PROCESSING_STARTED" | "DATASET_PROCESSING_COMPLETED" | "DATASET_PROCESSING_ERRORED", ... }`. Source: [`PipelineRun.py:8-12`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRun.py#L8-L12). Datasets the user cannot read are silently dropped from the result map (because `get_authorized_existing_datasets` filters them).
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | No valid credential. |
  | `409` | `{"error": "<inner>"}` | Generic catch. Source: [`get_datasets_router.py:413-414`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L413-L414). Note the `{error}` envelope (not `{detail}`) — same quirk as add/update. |
  | `422` | `{"detail": [...], "body": ...}` | Bad UUID in query. |
- **Side effects**: none.
- **Delegation target**: `cognee::api::datasets::get_status(dataset_ids: &[Uuid]) -> HashMap<Uuid, PipelineRunStatus>`. Internal: queries the `pipeline_runs` table for the latest row per dataset where `pipeline_name = "cognify_pipeline"`.
- **Validation rules**: an empty `dataset` query list is accepted and returns `{}` (Python parity — empty input → empty result, no `422`). Strict wire parity; do not impose a `>= 1` requirement.
- **Permission gate**: `read` on each dataset. Datasets the caller can't read are silently filtered.
- **OpenAPI**: tag `["datasets"]`, parameter `dataset: array<Uuid>`, response `200: dict<UUID, PipelineRunStatus>`.
- **Telemetry**: span `cognee.api.datasets.status`; attributes `cognee.dataset.count`, `cognee.dataset.ids`.
- **Python parity notes**: 409 (`Conflict`) on generic error is unusual; match. The dropping of unauthorized datasets is silent — clients can detect by comparing requested vs returned key set.

### 2.3 `GET /api/v1/datasets/{dataset_id}/data` — List data items in a dataset

- **Auth**: `required`.
- **Path params**: `dataset_id: Uuid`.
- **Query params**: none.
- **Request body**: none.
- **Response body** (`200 OK`): `[DataDTO, ...]`. Each `{id, name, created_at, updated_at, extension, mime_type, raw_data_location, dataset_id}` (camelCase wire keys — `createdAt`, `updatedAt`, `mimeType`, `rawDataLocation`, `datasetId`). Source: [`get_datasets_router.py:301-365`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L301-L365).
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | — |
  | `404` | `{"message": "Dataset (<uuid>) not found."}` | Caller lacks `read` permission OR dataset doesn't exist. **Note**: Python returns `ErrorResponseDTO("Dataset ... not found.")` which has a single `message` field. Source: [`get_datasets_router.py:35-36`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L35-L36) (the model definition) and [`:347-350`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L347-L350) (the response). This is **distinct** from the `{error, detail}` envelope used by add/update. Document loudly. |
  | `422` | `{"detail": [...], "body": ...}` | Invalid UUID. |
- **Side effects**: none.
- **Delegation target**: `cognee::modules::data::methods::get_dataset_data(dataset_id: Uuid) -> Vec<Data>`. Source: [`get_datasets_router.py:341`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L341).
- **Validation rules**: `dataset_id` must be a valid UUID.
- **Permission gate**: `read` on `dataset_id` (cite [../tenants.md §5](../tenants.md#5-permission-resolution)).
- **OpenAPI**: tag `["datasets"]`, response `200: list[DataDTO]`, `404: ErrorMessageDTO`.
- **Telemetry**: span `cognee.api.datasets.data.list`; attributes `cognee.dataset.id`, `cognee.data.count`.
- **Python parity notes**: returns `[]` (empty list) not 404 if the dataset exists but has no data — explicit branch ([`get_datasets_router.py:356-357`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L356-L357)). Mirror.

### 2.4 `GET /api/v1/datasets/{dataset_id}/data/{data_id}/raw` — Stream the original raw file

This is the most operationally-loaded endpoint: it returns `FileResponse` (local files) or `StreamingResponse` (S3 / remote) depending on the `raw_data_location` URI scheme.

- **Auth**: `required`.
- **Path params**: `dataset_id: Uuid`, `data_id: Uuid`.
- **Query params**: none.
- **Request body**: none.
- **Response body** (`200 OK`): the raw file bytes.
  - `Content-Type`: `data.mime_type` if known; otherwise `application/octet-stream`.
  - `Content-Disposition`: `attachment; filename="<download_name>"`. For S3-backed files, `download_name` is `Path(parsed_uri.path).name or data.name` ([`get_datasets_router.py:487`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L487)). For local files, FastAPI's `FileResponse` derives the filename from the path.
  - `Content-Length`: set when the file size is known (always for local; conditional for S3).
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | — |
  | `404` | `{"detail": "Dataset (<uuid>) not found."}` | Dataset not accessible. Source: [`get_datasets_router.py:455-458`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L455-L458). Note: `{detail}` envelope (canonical) here — different from §2.3's `{message}`. **Inconsistency in Python**; we replicate. |
  | `404` | `{"detail": "<DataNotFoundError message>"}` | Data row not found in dataset, or raw file missing on disk. Source: `DataNotFoundError` mapped to `404` ([`exceptions.py:36-43`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/exceptions/exceptions.py#L36-L43)). |
  | `501` | `{"detail": "Storage scheme '<scheme>' not supported for direct download."}` | Unknown URI scheme on `raw_data_location`. Source: [`get_datasets_router.py:517-520`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L517-L520). |
- **Side effects**: opens the file or S3 stream; closes on response completion.
- **Delegation target**:
  1. `get_authorized_existing_datasets([dataset_id], "read", user)` — permission check.
  2. `get_dataset_data(dataset_id)` — list data rows.
  3. `get_data(user.id, data_id)` — fetch the row.
  4. URI-scheme dispatch:
     - `s3://...` → `cognee::infrastructure::files::open_data_file(uri, "rb")` → wrap in async iterator → `axum::body::Body::from_stream`.
     - `file://...` or empty scheme or `<single-letter>:` (Windows path) → `axum::extract::Path(local_path)` → `tokio::fs::File::open` → wrap in `tokio_util::io::ReaderStream` → `axum::body::Body::from_stream`. We do **not** use a built-in `FileResponse` analog — `tower-http::services::ServeFile` doesn't fit handler ergonomics; build the response manually with explicit `Content-Disposition`.
     - Anything else → 501.
- **Validation rules**: both UUIDs valid.
- **Permission gate**: `read` on `dataset_id`.
- **OpenAPI**: tag `["datasets"]`, response `200: binary` (`application/octet-stream`), `404 / 501: ErrorEnvelope`. Note: utoipa's binary response support uses `content_type = "application/octet-stream"` with `Vec<u8>` schema; the actual content type is dynamic and set per-response.
- **Telemetry**:
  - Span name: `cognee.api.datasets.data.raw`.
  - Attributes: `cognee.dataset.id`, `cognee.data.id`, `cognee.raw.scheme` (`file` / `s3` / other), `cognee.raw.bytes_out`, `cognee.raw.mime_type`.
- **Python parity notes**:
  - Python uses `urlparse(...).scheme` to dispatch and treats the empty scheme + single-letter scheme (Windows drive letters like `c:`) as local. Mirror exactly: [`get_datasets_router.py:504-506`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L504-L506).
  - `raw_location` may be a relative path (no scheme); the SDK's `get_data_file_path(raw_location)` resolves it against the configured storage root. Match.
  - The 1 MiB chunk size for S3 streaming ([`get_datasets_router.py:490`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L490)) is preserved — it's a reasonable default for high-throughput S3.
  - Python uses `run_async(file.read, chunk_size)` to wrap a sync read in a thread pool. Rust uses `tokio::io::AsyncReadExt::read_buf` natively — no thread pool needed.
  - Python's `FileResponse(path=path)` automatically infers `Content-Type` from the file extension. The Rust handler reads `data.mime_type` from the `Data` row first; falls back to `mime_guess::from_path(path).first_or_octet_stream()` if unset.

### 2.5 `GET /api/v1/datasets/{dataset_id}/graph` — Rendered knowledge graph

- **Auth**: `required`.
- **Path params**: `dataset_id: Uuid`.
- **Query params**: none.
- **Request body**: none.
- **Response body** (`200 OK`): `GraphDTO { nodes: [GraphNodeDTO], edges: [GraphEdgeDTO] }`. Wire is camelCase (`OutDTO` base). Source: [`get_datasets_router.py:71-73`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L71-L73).
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | — |
  | `404` | `{"detail": "..."}` | `DataNotFoundError` from the graph layer. |
  | `500` | `{"detail": "..."}` | Generic catch (no explicit Python handling — bubbles up as 500). |
- **Side effects**: read-only.
- **Delegation target**: `cognee::modules::graph::methods::get_formatted_graph_data(dataset_id, user) -> GraphData`. Source: [`get_datasets_router.py:296-299`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L296-L299).
- **Validation rules**: valid UUID.
- **Permission gate**: `read` on `dataset_id`. Enforced inside `get_formatted_graph_data` (the SDK) — handler does not duplicate.
- **OpenAPI**: tag `["datasets"]`, response `200: GraphDTO`.
- **Telemetry**: span `cognee.api.datasets.graph`; attributes `cognee.dataset.id`, `cognee.graph.node_count`, `cognee.graph.edge_count`.
- **Python parity notes**: graphs can be very large; payload size is unbounded by the API. Document as a known scaling concern.

### 2.6 `GET /api/v1/datasets/{dataset_id}/schema` — Read graph schema + custom prompt

- **Auth**: `required`.
- **Path params**: `dataset_id: Uuid`.
- **Query params**: none.
- **Request body**: none.
- **Response body** (`200 OK`):
  ```json
  { "graph_schema": <object|null>, "custom_prompt": <string|null> }
  ```
  Source: [`get_datasets_router.py:522-542`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L522-L542).
  Wire keys are snake_case here (the inner dict isn't a Pydantic model — it's a raw `dict` returned by the handler). When no `DatasetConfiguration` row exists, both fields are `null`.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | — |
  | `404` | `{"error": "Dataset not found"}` | Caller lacks `read` permission. Source: [`get_datasets_router.py:529-530`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L529-L530). Note `{error}` envelope. |
- **Side effects**: read-only.
- **Delegation target**: `cognee::modules::data::methods::get_dataset_configuration(dataset_id) -> Option<DatasetConfiguration>` — a new helper to add.
- **Validation rules**: valid UUID.
- **Permission gate**: `read`.
- **OpenAPI**: tag `["datasets"]`, response `200: DatasetSchemaResponseDTO`.
- **Telemetry**: span `cognee.api.datasets.schema.read`; attribute `cognee.dataset.id`.
- **Python parity notes**: the response is a plain dict — no `OutDTO` aliasing — so keys stay snake_case. Don't accidentally apply camelCase. The empty case returns `null` for both fields, not an empty object.

### 2.7 `POST /api/v1/datasets` — Create a dataset (or return existing by name)

- **Auth**: `required`.
- **Path params**: none.
- **Query params**: none.
- **Request body** (`application/json`): `DatasetCreationPayload {name: String}`. Wire field is camelCase (`InDTO`), so on the wire it's `{"name": "..."}` (single field — same shape either way).
- **Response body** (`200 OK`): `DatasetDTO`. **Note**: 200, not 201, even on creation — Python uses default status. Match.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | — |
  | `418` | `{"detail": "Error creating dataset: <inner>"}` | Generic catch. Source: [`get_datasets_router.py:184-188`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L184-L188). |
  | `422` | `{"detail": [...], "body": ...}` | Missing/invalid `name`. |
- **Side effects**:
  - **Relational DB**: insert one row in `datasets` if no dataset of that name exists for `user.id`. Otherwise return the existing row (idempotent — Python: [`get_datasets_router.py:166-169`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L166-L169)).
  - **ACLs**: on creation, grant the user `read+write+share+delete` permissions on the new dataset. Source: [`get_datasets_router.py:177-180`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L177-L180).
- **Delegation target**: `cognee::modules::data::methods::{get_datasets_by_name, create_dataset}` + `cognee::modules::users::permissions::give_permission_on_dataset` (loop x4).
- **Validation rules**: `name` non-empty (Python doesn't enforce this; we should — empty-name datasets are pathological. Open question §6).
- **Permission gate**: none — anyone authenticated may create datasets within their tenant.
- **OpenAPI**: tag `["datasets"]`, request body `DatasetCreationPayload`, response `200: DatasetDTO`.
- **Telemetry**: span `cognee.api.datasets.create`; attributes `cognee.dataset.name`, `cognee.dataset.id` (after creation), `cognee.dataset.was_existing` (boolean — true if returned a pre-existing row).
- **Python parity notes**: 418 envelope is intentional. Granting all four ACLs synchronously is critical — if any of the four `give_permission_on_dataset` calls fails, Python catches the exception and returns 418, leaving partial ACLs. The Rust handler should run them in a single transaction or document the partial-failure mode.

### 2.8 `PUT /api/v1/datasets/{dataset_id}/schema` — Upsert graph schema + custom prompt

- **Auth**: `required`.
- **Path params**: `dataset_id: Uuid`.
- **Query params**: none.
- **Request body** (`application/json`): `DatasetSchemaPayloadDTO { graph_schema: Option<Value>, custom_prompt: Option<String> }`. Both optional (camelCase wire because `InDTO`: `graphSchema` / `customPrompt`). Source: [`get_datasets_router.py:80-83`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L80-L83).
- **Response body** (`200 OK`): `{"status": "ok"}`. Plain dict (snake_case).
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | — |
  | `404` | `{"error": "Dataset not found"}` | Caller lacks `write`. Source: [`get_datasets_router.py:555-556`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L555-L556). |
  | `422` | `{"detail": [...], "body": ...}` | Body shape invalid. |
- **Side effects**: upsert one row in the `dataset_configurations` table (or whatever the Rust equivalent is — see [project guide](../../../.claude/CLAUDE.md) for migration status).
- **Delegation target**: a new `cognee::modules::data::methods::upsert_dataset_configuration(dataset_id, graph_schema, custom_prompt)` helper.
- **Validation rules**: valid UUID; `graph_schema` if present must be a JSON object (not a primitive); `custom_prompt` if present must be a string.
- **Permission gate**: `write` on `dataset_id`. Source: [`get_datasets_router.py:554`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L554).
- **OpenAPI**: tag `["datasets"]`, request body `DatasetSchemaPayloadDTO`, response `200: dict`.
- **Telemetry**: span `cognee.api.datasets.schema.write`; attributes `cognee.dataset.id`, `cognee.schema.has_graph_schema`, `cognee.schema.has_custom_prompt`.
- **Python parity notes**: partial updates are supported — sending only `graph_schema` leaves `custom_prompt` untouched ([`get_datasets_router.py:563-567`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py#L563-L567)). Behavior on `null` values: Python distinguishes "field omitted" (no change) from "field present and null" (sets to NULL) using `is not None`. The Rust handler must match — `serde_json::Value::Null` ≠ field absent. Use `serde(default, skip_serializing_if = "Option::is_none")` on the DTO and check `value.is_some()`.

### 2.9 `DELETE /api/v1/datasets` — Delete every dataset the caller owns

- **Auth**: `required`.
- **Path params**: none.
- **Query params**: none.
- **Request body**: none.
- **Response body** (`200 OK`, empty body — Python returns `None`): `null` (FastAPI serializes `None` as `null`). Match. Note: not 204 — Python uses default 200.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | — |
  | `500` | `{"detail": "..."}` | Bubbles from the SDK; Python doesn't wrap. |
- **Side effects**: deletes every dataset the user has `delete` on, plus all data rows, graph nodes, vector embeddings.
- **Delegation target**: `cognee::api::datasets::delete_all(user)`. Source: [`datasets.py:177-187`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/datasets.py#L177-L187).
- **Validation rules**: none.
- **Permission gate**: `delete` on each affected dataset (filtered inside the SDK).
- **OpenAPI**: tag `["datasets"]`, response `200: null`.
- **Telemetry**: span `cognee.api.datasets.delete_all`; attributes `cognee.dataset.count_deleted`.
- **Python parity notes**: silent no-op when no datasets exist. The handler must NOT 404 in that case — Python returns `null`.

### 2.10 `DELETE /api/v1/datasets/{dataset_id}` — Empty (delete) one dataset

- **Auth**: `required`.
- **Path params**: `dataset_id: Uuid`.
- **Query params**: none.
- **Request body**: none.
- **Response body** (`200 OK`): the return value of `datasets.empty_dataset(...)` — typically `None` → `null`, but may be an inner result object. Source: [`datasets.py:82-121`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/datasets.py#L82-L121).
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | — |
  | `404` | `{"detail": "Dataset (<uuid>) not accessible."}` | Caller lacks `delete` OR dataset doesn't exist. Source: `UnauthorizedDataAccessError` raised by `empty_dataset`. |
  | `500` | `{"detail": "..."}` | Other DB errors. |
- **Side effects**: cascading delete: graph nodes/edges → dataset metadata → individual data records → vector points.
- **Delegation target**: `cognee::api::datasets::empty_dataset(dataset_id, user)`.
- **Validation rules**: valid UUID.
- **Permission gate**: `delete` on `dataset_id`.
- **OpenAPI**: tag `["datasets"]`, response `200`, `404: ErrorMessageDTO`.
- **Telemetry**: span `cognee.api.datasets.delete`; attributes `cognee.dataset.id`, `cognee.dataset.data_count_deleted`.
- **Python parity notes**: name confusion in Python — `empty_dataset` actually deletes the dataset itself, not just empties it. Mirror.

### 2.11 `DELETE /api/v1/datasets/{dataset_id}/data/{data_id}` — Delete one data item

- **Auth**: `required`.
- **Path params**: `dataset_id: Uuid`, `data_id: Uuid`.
- **Query params**: none.
- **Request body**: none.
- **Response body** (`200 OK`): `{"status": "success"}`. Source: [`datasets.py:157`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/datasets.py#L157), [`:175`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/datasets.py#L175).
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | — |
  | `404` | `{"detail": "Dataset/Data (<uuid>) not accessible."}` | `UnauthorizedDataAccessError`. |
  | `500` | `{"detail": "..."}` | DB error. |
- **Side effects**: removes the data row, its graph subgraph, and its vector points. Does not delete the dataset.
- **Delegation target**: `cognee::api::datasets::delete_data(dataset_id, data_id, user)`.
- **Validation rules**: both UUIDs valid.
- **Permission gate**: `delete` on `dataset_id`. The deprecated `/api/v1/delete` route delegates here — see [delete.md](delete.md).
- **OpenAPI**: tag `["datasets"]`, response `200`, `404: ErrorMessageDTO`.
- **Telemetry**: span `cognee.api.datasets.data.delete`; attributes `cognee.dataset.id`, `cognee.data.id`.
- **Python parity notes**: when a custom graph model produces nodes that don't exist in the relational DB, Python takes a separate code path that still returns `{"status": "success"}` ([`datasets.py:148-158`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/datasets.py#L148-L158)). The Rust port must reproduce this.

## 3. Cross-cutting behavior

### 3.1 camelCase wire for typed responses

`DatasetDTO`, `DataDTO`, `GraphDTO`, `GraphNodeDTO`, `GraphEdgeDTO` all derive from Python's `OutDTO` ([`cognee/api/DTO.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/DTO.py)) which uses `alias_generator=to_camel`. Wire keys are camelCase: `createdAt`, `updatedAt`, `ownerId`, `mimeType`, `rawDataLocation`, `datasetId`, `nodeCount`. The Rust DTOs use `#[serde(rename_all = "camelCase")]` to match. **Exception**: the `/schema` endpoints return raw dicts (no `OutDTO`), keys stay snake_case (`graph_schema`, `custom_prompt`).

### 3.2 Heterogeneous error envelopes

This router uses three different error envelope shapes:

| Endpoint(s) | Envelope |
|---|---|
| 2.1, 2.7 | `{"detail": "..."}` (canonical) but emitted with status 418 |
| 2.2 (catch-all), 2.6 (404), 2.8 (404) | `{"error": "..."}` |
| 2.3 (404) | `{"message": "..."}` |
| 2.4 (404), 2.5, 2.9, 2.10, 2.11 | `{"detail": "..."}` |

The Rust port preserves all three. Define three `ApiError` variants — `Teapot`, `WriteEnvelopeError`, `ErrorMessageError` — and route each handler explicitly. Do not unify; clients depend on the specific shape.

### 3.3 Permission-by-route

| Endpoint | Permission |
|---|---|
| 2.1 list | filtered by `read` |
| 2.2 status | per-dataset `read` |
| 2.3 data | `read` |
| 2.4 raw | `read` |
| 2.5 graph | `read` |
| 2.6 schema GET | `read` |
| 2.7 create | none (any authenticated user) |
| 2.8 schema PUT | `write` |
| 2.9 delete-all | `delete` (filtered) |
| 2.10 delete | `delete` |
| 2.11 delete data | `delete` |

### 3.4 Async streaming responses

§2.4 (raw download) is the only endpoint that emits a streaming body. It uses `axum::body::Body::from_stream(...)` over either `tokio_util::io::ReaderStream` (local) or a custom `Stream<Item = Result<Bytes, std::io::Error>>` adapter wrapping the S3 file handle. Set `Content-Length` only when known (local files always; S3 conditional on `HEAD` metadata). For S3 streams without known size, omit the header — clients use `Transfer-Encoding: chunked`.

### 3.5 No background mode

None of the dataset endpoints accept `run_in_background`. Status polling is via §2.2 (`/datasets/status`) which reads from the `pipeline_runs` table.

## 4. DTO definitions

Located in `crates/http-server/src/dto/datasets.rs`.

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// `DatasetDTO` — Python `OutDTO` (camelCase wire).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DatasetDTO {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
    pub owner_id: Uuid,
}

/// `DataDTO` — Python `OutDTO` (camelCase wire).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DataDTO {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: Option<DateTime<Utc>>,
    pub extension: String,
    pub mime_type: String,
    pub raw_data_location: String,
    pub dataset_id: Uuid,
}

/// `GraphDTO` — Python `OutDTO` (camelCase wire).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GraphDTO {
    pub nodes: Vec<GraphNodeDTO>,
    pub edges: Vec<GraphEdgeDTO>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GraphNodeDTO {
    pub id: Uuid,
    pub label: String,
    pub r#type: String,
    pub properties: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GraphEdgeDTO {
    pub source: Uuid,
    pub target: Uuid,
    pub label: String,
}

/// Request body for `POST /api/v1/datasets`.
/// Python `InDTO` — camelCase aliases. Single-field DTO so the wire is `{"name": "..."}`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DatasetCreationPayload {
    pub name: String,
}

/// Request body for `PUT /api/v1/datasets/{id}/schema`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DatasetSchemaPayloadDTO {
    /// Free-form JSON object describing the dataset's graph schema. `null`
    /// vs absent are distinct: `null` clears the field, absent leaves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_prompt: Option<String>,
}

/// Response body for `GET /api/v1/datasets/{id}/schema`.
/// **snake_case** wire — Python returns a raw dict, not an `OutDTO`.
#[derive(Debug, Serialize, ToSchema)]
pub struct DatasetSchemaResponseDTO {
    pub graph_schema: Option<Value>,
    pub custom_prompt: Option<String>,
}

/// Query string for `GET /api/v1/datasets/status`.
#[derive(Debug, Deserialize, IntoParams)]
pub struct DatasetStatusQuery {
    /// Repeated query param `?dataset=<uuid>&dataset=<uuid>`.
    #[serde(rename = "dataset")]
    pub dataset: Vec<Uuid>,
}

/// `PipelineRunStatus` enum — wire is the raw string discriminator.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub enum PipelineRunStatus {
    #[serde(rename = "DATASET_PROCESSING_INITIATED")]
    DatasetProcessingInitiated,
    #[serde(rename = "DATASET_PROCESSING_STARTED")]
    DatasetProcessingStarted,
    #[serde(rename = "DATASET_PROCESSING_COMPLETED")]
    DatasetProcessingCompleted,
    #[serde(rename = "DATASET_PROCESSING_ERRORED")]
    DatasetProcessingErrored,
}

/// Response body for `GET /api/v1/datasets/status`.
pub type DatasetStatusResponseDTO = HashMap<Uuid, PipelineRunStatus>;

/// `ErrorResponseDTO` — `{message: String}` envelope used by 2.3's 404.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorMessageDTO {
    pub message: String,
}
```

Field-level mapping vs Python:

| Python | Rust | Wire key | Notes |
|---|---|---|---|
| `DatasetDTO.id/name/created_at/updated_at/owner_id` | `DatasetDTO.{id, name, created_at, updated_at, owner_id}` | `id`, `name`, `createdAt`, `updatedAt`, `ownerId` | camelCase via `OutDTO`. |
| `DataDTO.*` | `DataDTO.*` | camelCase | `mimeType`, `rawDataLocation`, `datasetId`. |
| `GraphDTO/GraphNodeDTO/GraphEdgeDTO` | same | camelCase | `properties` is a free dict. |
| `DatasetCreationPayload.name` | `name` | `name` | Single-field; either casing works. |
| `DatasetSchemaPayloadDTO.graph_schema/custom_prompt` | same | `graphSchema`, `customPrompt` | InDTO → camelCase. |
| `dict {graph_schema, custom_prompt}` (response) | `DatasetSchemaResponseDTO` | snake_case | Plain dict. |
| `dict[str, PipelineRunStatus]` | `HashMap<Uuid, PipelineRunStatus>` | uuid keys | — |
| `ErrorResponseDTO.message` | `ErrorMessageDTO.message` | `message` | One of three error envelopes. |

## 5. Implementation tasks

1. Add DTOs in `crates/http-server/src/dto/datasets.rs` (all of §4).
2. Add handlers in `crates/http-server/src/routers/datasets.rs`:
   - `list_datasets` (2.1)
   - `get_dataset_status` (2.2)
   - `get_dataset_data` (2.3)
   - `get_raw_data` (2.4) — streaming response
   - `get_dataset_graph` (2.5)
   - `get_dataset_schema` (2.6)
   - `create_new_dataset` (2.7)
   - `update_dataset_schema` (2.8)
   - `delete_all_datasets` (2.9)
   - `delete_dataset` (2.10)
   - `delete_data` (2.11)
3. Wire all 11 routes in `pub fn router() -> Router<AppState>`. Order matches Python file order so OpenAPI tag ordering is stable.
4. OpenAPI annotations on every handler with the right tag, params, request body, response shape, and 4xx variants.
5. Streaming response helper `crates/http-server/src/responses/raw_file.rs` with two impls (local `tokio::fs::File` + S3 wrapper) sharing a `Stream<Item = Result<Bytes, _>>` interface.
6. Unit tests for DTO serialization (camelCase ↔ snake_case boundaries).
7. Integration tests in `crates/http-server/tests/test_datasets.rs`:
   - 11 happy-path tests, one per endpoint.
   - 5 permission-gate tests (each gated endpoint with a 403).
   - Streaming raw download with `Content-Length`, `Content-Disposition`, range-not-supported.
   - S3 download via `MockStorage` + `LocalStorage` round-trip.
8. Cross-SDK parity tests: list / create / fetch graph / schema round-trip / delete data; assert response shape parity (modulo timestamps).

## 6. Open questions

1. **`POST /` empty-name validation**: Python accepts `{"name": ""}` and creates a dataset with empty name. Rust matches — no application-level rejection of empty names.
2. **`/status` empty list**: Python silently returns `{}` for an empty `dataset` list. Rust matches: empty query → `{}`, no 422.
3. **Heterogeneous error envelopes** (§3.2): three different shapes (`{detail}`, `{error}`, `{message}`) is intentional Python behavior. Rust matches all three; no unification.
4. **Range requests on `/raw`**: FastAPI's `FileResponse` handles `Range` headers natively for local files; the equivalent Rust path uses `tower_http::services::ServeFile` (or similar) which also supports Range. For S3-backed downloads, Python uses `StreamingResponse` which does not support Range; Rust matches verbatim — no Range support on the S3 path.
5. **Schema upsert race**: §2.8 reads then writes without a transaction — Python has the same race. Rust matches: no `SELECT ... FOR UPDATE`, no transactional wrap.
6. **Dataset deletion soft-fail**: §2.9 uses `asyncio.gather(return_exceptions=True)` ([`datasets.py:107-110`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/datasets.py#L107-L110)) so partial failures are logged but not surfaced. Rust matches — `futures::future::join_all` with logging on error rows; no aggregation of partial failures into the response.

## 7. References

- Python router: [`cognee/api/v1/datasets/routers/get_datasets_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/routers/get_datasets_router.py) (lines 1-578).
- Python SDK class: [`cognee/api/v1/datasets/datasets.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/datasets/datasets.py).
- Pipeline status enum: [`cognee/modules/pipelines/models/PipelineRun.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRun.py).
- Graph rendering: [`cognee/modules/graph/methods/get_formatted_graph_data.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/graph/methods/get_formatted_graph_data.py).
- Permission resolution: [../tenants.md §5](../tenants.md#5-permission-resolution).
- Architecture: [../architecture.md §8 streaming bodies](../architecture.md#8-middleware-stack), [../architecture.md §9 errors](../architecture.md#9-error-handling).
- Cross-router conventions: [README.md §3](README.md#3-cross-router-conventions).
- Sister routers: [add.md](add.md), [update.md](update.md), [delete.md](delete.md), [forget.md](forget.md), [ontologies.md](ontologies.md).
