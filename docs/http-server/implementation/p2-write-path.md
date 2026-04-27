# Implementation: P2 — Write path

## 1. Goal

Land the write-path routers — `/api/v1/add`, `/api/v1/update`, `/api/v1/datasets/*` (CRUD + raw download + graph + schema + status), `/api/v1/ontologies`, `/api/v1/delete` (deprecated), `/api/v1/forget` — together with the shared multipart streaming helper they all sit on top of. After this phase, a freshly-built `cognee-http-server` accepts file uploads, manages dataset metadata, exposes graph rendering, streams raw bytes back to clients, accepts ontology files, and supports both the deprecated single-data delete and the unified `/forget` endpoint with full Python parity. Permission gates are wired through the `PermissionsRepository` trait surface — the **real** SeaORM-backed implementation lands in [P5](p5-admin.md); P2 ships a placeholder impl that always returns `true`, gated by a single `// TODO(P5)` comment per call site (see §6).

## 2. References (read these before starting)

- [README.md](README.md) — this directory's invariants and the per-step verification rule.
- [../plan.md §4](../plan.md#4-implementation-phases) — phase-scope contract for P2.
- [../architecture.md §8](../architecture.md#8-middleware-stack), [§9](../architecture.md#9-error-handling), [§10](../architecture.md#10-request-validation) — middleware (multipart body limits), error model, validation extractor.
- [../auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution) — the `AuthenticatedUser` extractor every handler injects.
- [../tenants.md §5](../tenants.md#5-permission-resolution), [§9](../tenants.md#9-repository-surface) — permission resolution semantics and the `PermissionsRepository` trait.
- Per-router specs (read end-to-end before starting that step):
  - [../routers/add.md](../routers/add.md)
  - [../routers/update.md](../routers/update.md)
  - [../routers/datasets.md](../routers/datasets.md) (largest — 11 endpoints)
  - [../routers/ontologies.md](../routers/ontologies.md)
  - [../routers/delete.md](../routers/delete.md)
  - [../routers/forget.md](../routers/forget.md)
  - [../routers/README.md §3](../routers/README.md#3-cross-router-conventions) — cross-router conventions, especially [§3.7 multipart](../routers/README.md#37-multipart-endpoints) and [§3.1 error envelopes](../routers/README.md#31-error-envelope).

## 3. Prerequisites

- **P0 done** — crate skeleton, `AppState`, `ApiError`, OpenAPI bootstrap, `build_router` assembly point, `tests/support/` test harness.
- **P1 done** — `AuthenticatedUser` extractor, JWT/cookie/`X-Api-Key` resolution, login/logout/me, users CRUD. Every P2 handler injects `AuthenticatedUser`.
- The `cognee_lib::api::add::add(...)`, `cognee_lib::api::update::update(...)`, `cognee_lib::api::datasets::*`, `cognee_lib::api::forget::forget(...)`, and `cognee_lib::api::datasets::delete_data(...)` library entry points must exist (they do — see [crates/lib/src/api/](../../crates/lib/src/api/)). If a delegation target is missing on the library side, surface it as a blocking issue against the relevant lib crate; do **not** stub it inside `cognee-http-server`.
- `cognee_ontology::OntologyManager` exists at [crates/ontology/src/manager.rs](../../crates/ontology/src/manager.rs); confirm before step 4.10 that it is re-exported under `cognee_lib::ontology::OntologyManager` (if not, that re-export is a one-line addition to `crates/lib/src/lib.rs` and lands as part of step 4.10).

## 4. Step-by-step

Each step is one commit. `Verify:` lists the exact command(s) that must succeed.

### 4.1 Add the shared multipart helper

Create `crates/http-server/src/multipart.rs`. Provide:

- A `parse_multipart(req: Multipart, opts: &MultipartOpts) -> Result<ParsedForm, ApiError>` helper that drains an `axum::extract::Multipart`, classifies each part as **form-field** (text, in-memory, ≤4 KiB) or **file** (streamed to a temp file via `tokio::fs::File` + `tokio::io::copy`), and returns a `ParsedForm` with both maps.
- An `UploadGuard` RAII wrapper around the per-request temp directory; its `Drop` impl unlinks the directory recursively. Hand each handler an `UploadGuard` so failure paths (validation 400, downstream 500) clean up correctly.
- A `MultipartOpts { max_parts, form_field_max_bytes, file_max_bytes, spool_dir }` struct, populated from `HttpServerConfig` (with defaults `max_parts=256`, `form_field_max_bytes=4096`, `file_max_bytes=cfg.body_limit`, `spool_dir=std::env::temp_dir().join("cognee-uploads")`).
- A small `streamed_to_disk(field, dest_path) -> Result<u64, io::Error>` async fn that wraps `tokio::io::copy` and returns the byte count.

Do **not** put any business validation here (filename traversal, URL-string detection, `.owl` extension); those belong in the per-router parse adapters (steps 4.2, 4.3, 4.10) so this helper stays neutral.

Per [../architecture.md §8](../architecture.md#8-middleware-stack): the global `DefaultBodyLimit::max(cfg.body_limit)` applies; per-route overrides are added in steps 4.2 and 4.3.

**Verify**: `cargo check -p cognee-http-server`.

### 4.2 DTOs for `add` and the `add` router

Create `crates/http-server/src/dto/add.rs` and copy the DTO definitions from [../routers/add.md §4](../routers/add.md#4-dto-definitions) verbatim: `AddMultipart`, `AddRequest`, `UploadedPart`, `PipelineRunInfoDTO`, `DataIngestionInfoDTO`, `ErrorResponseDTO`. Field-level mapping vs Python is in the same section's table — preserve every `#[serde(rename = "...")]` and `#[schema(...)]` attribute.

Create `crates/http-server/src/routers/add.rs` per [../routers/add.md §5](../routers/add.md#5-implementation-tasks):

1. `parse_add_multipart(req: Multipart, opts: &AddOpts) -> Result<AddRequest, ApiError>` — wraps `crate::multipart::parse_multipart`, then per-part:
   - For `data` parts: spool to disk; if the spooled body is < 4 KiB and decodes as UTF-8 starting with `http://`, `https://`, or `s3://`, unlink and set `UploadedPart::url_payload`. Run filename traversal check (reject `../`, `..\\`, leading `/`).
   - For `datasetName` / `datasetId` form fields: trim, normalize empty string to `None`.
   - For `node_set`: collect repetitions; normalize `[""]` and `[]` to `None`.
2. `post_add` handler — inject `AuthenticatedUser`, `State<AppState>`, `Multipart`. Parse → cross-field validate (either `datasetName` or `datasetId` non-empty, else `400 ErrorResponseDTO {error: "Either datasetId or datasetName must be provided.", detail: null}`). Run the permission gate (see step 4.13). Call `state.lib.add(...)` (delegation target per [../routers/add.md §1](../routers/add.md#1-mount--file)). Map result to `PipelineRunInfoDTO`. On `PipelineRunErrored`, return `500 ErrorResponseDTO {error: "Pipeline run errored", detail: <inner>}`.
3. `pub fn router() -> Router<AppState>` — single `POST /` route, with per-route override `.layer(DefaultBodyLimit::max(cfg.add_body_limit))` reading `HTTP_BODY_LIMIT_BYTES_ADD` (default 1 GiB) per [../routers/add.md §2.1.1](../routers/add.md#211-multipart-parts).
4. Add the `#[utoipa::path(post, ...)]` annotation declaring `multipart/form-data` body and the response/error variants.

Wire the handler's error branches to the **deviated** envelope `ApiError::WriteEndpointError { error, detail, status }` (per [../routers/add.md §3.1](../routers/add.md#31-error-envelope-deviation)). Add this `ApiError` variant in `crates/http-server/src/error.rs` if P0/P1 didn't add it; its `IntoResponse` serializes `ErrorResponseDTO`, **not** the canonical `{detail}` shape.

**Verify**: `cargo check --all-targets -p cognee-http-server` and a manual `oneshot` against the router with three `data` parts succeeds.

### 4.3 DTOs for `update` and the `update` router

Create `crates/http-server/src/dto/update.rs` and copy the DTO definitions from [../routers/update.md §4](../routers/update.md#4-dto-definitions): `UpdateQuery`, `UpdateMultipart`, `UpdateRequest`, `UpdateResponseDTO` (`HashMap<Uuid, PipelineRunInfoDTO>`). Reuse `UploadedPart`, `PipelineRunInfoDTO`, `ErrorResponseDTO` from `dto::add`.

Create `crates/http-server/src/routers/update.rs`:

1. `parse_update_multipart` — like `parse_add_multipart` but rejects `datasetName`/`datasetId` parts with 400 (those live in the query string for `update`).
2. `patch_update` handler — inject `AuthenticatedUser`, `State<AppState>`, `Query<UpdateQuery>`, `Multipart`. Run **two** permission checks: `write` AND `delete` on `dataset_id` (the union of `add` + `delete_data`; see [../routers/update.md §4 permission gate](../routers/update.md#21-patch-apiv1update--replace-an-existing-document-and-re-cognify-the-dataset)). Call `state.lib.update(...)`. Map the returned `Map<Uuid, PipelineRunInfo>` to `UpdateResponseDTO`. If any value is `PipelineRunErrored`, return `500 ErrorResponseDTO {error: "Pipeline run errored", detail: <inner>}` per [../routers/update.md §2.1 error responses](../routers/update.md#21-patch-apiv1update--replace-an-existing-document-and-re-cognify-the-dataset). Map `DataNotFoundError` / `UnauthorizedDataAccessError` to `404 ErrorResponseDTO`.
3. `pub fn router() -> Router<AppState>` — single `PATCH /` route with `HTTP_BODY_LIMIT_BYTES_UPDATE` (default 1 GiB) override.
4. `#[utoipa::path(patch, ...)]` annotation with both query params and the multipart body.

The response is a one-entry `HashMap`; do **not** collapse to a single value (deliberate Python parity quirk — see [../routers/update.md §2.1 response body](../routers/update.md#21-patch-apiv1update--replace-an-existing-document-and-re-cognify-the-dataset)).

**Verify**: `cargo check --all-targets -p cognee-http-server`.

### 4.4 DTOs for `datasets`

Create `crates/http-server/src/dto/datasets.rs` and copy every DTO from [../routers/datasets.md §4](../routers/datasets.md#4-dto-definitions): `DatasetDTO`, `DataDTO`, `GraphDTO`, `GraphNodeDTO`, `GraphEdgeDTO`, `DatasetCreationPayload`, `DatasetSchemaPayloadDTO`, `DatasetSchemaResponseDTO`, `DatasetStatusQuery`, `PipelineRunStatus`, `DatasetStatusResponseDTO`, `ErrorMessageDTO`. Pay attention to the casing rules in [../routers/datasets.md §3.1](../routers/datasets.md#31-camelcase-wire-for-typed-responses): `OutDTO`-derived structs use `#[serde(rename_all = "camelCase")]`; the `/schema` GET response stays snake_case (it's a raw dict in Python).

Add three additional `ApiError` variants in `crates/http-server/src/error.rs` per [../routers/datasets.md §3.2](../routers/datasets.md#32-heterogeneous-error-envelopes):

- `ApiError::Teapot(String)` → `418 {"detail": "..."}` (used by 2.1, 2.7).
- `ApiError::WriteEnvelopeError(String, StatusCode)` → `<status> {"error": "..."}` (used by 2.2's 409 catch-all, 2.6's 404, 2.8's 404).
- `ApiError::ErrorMessageError(String, StatusCode)` → `<status> {"message": "..."}` (used by 2.3's 404).

Do **not** unify these — clients depend on the exact shape per endpoint.

**Verify**: `cargo check -p cognee-http-server`.

### 4.5 Datasets router skeleton + endpoint 2.1 (`GET /` list)

Create `crates/http-server/src/routers/datasets.rs`. Define `pub fn router() -> Router<AppState>` with empty body for now; add the `list_datasets` handler per [../routers/datasets.md §2.1](../routers/datasets.md#21-get-apiv1datasets--list-datasets-the-caller-can-read). Delegate to `cognee_lib::modules::users::permissions::get_all_user_permission_datasets(user, "read")`. Map `Vec<Dataset>` → `Vec<DatasetDTO>`. On any error, return `ApiError::Teapot(format!("Error retrieving datasets: {e}"))` (Python uses 418 here intentionally).

Wire the route `.route("/", get(list_datasets))` and the OpenAPI annotation.

**Verify**: `cargo check --all-targets -p cognee-http-server`. Add an inline unit test that a freshly-built `Router` accepts `GET /api/v1/datasets`.

### 4.6 Endpoint 2.2 (`GET /status`) and 2.3 (`GET /{id}/data`)

- `get_dataset_status` per [../routers/datasets.md §2.2](../routers/datasets.md#22-get-apiv1datasetsstatus--pipeline-status-for-one-or-more-datasets): inject `Query<DatasetStatusQuery>`. Empty `dataset` list returns `{}` (do not 422 — Python parity). Datasets the caller cannot `read` are silently dropped from the result map. On generic error, `ApiError::WriteEnvelopeError(<inner>, StatusCode::CONFLICT)` (the 409 catch-all is intentional).
- `get_dataset_data` per [../routers/datasets.md §2.3](../routers/datasets.md#23-get-apiv1datasetsdataset_iddata--list-data-items-in-a-dataset): inject `Path<Uuid>`. Permission gate `read` (step 4.13). Empty data list returns `[]` (not 404 — explicit branch in Python). On 404, return `ApiError::ErrorMessageError(format!("Dataset ({id}) not found."), StatusCode::NOT_FOUND)` — `{message}` envelope.

Wire `.route("/status", get(get_dataset_status))` and `.route("/{dataset_id}/data", get(get_dataset_data))`.

**Verify**: `cargo check --all-targets -p cognee-http-server`.

### 4.7 Endpoint 2.4 (`GET /{id}/data/{did}/raw`) — streaming download

Per [../routers/datasets.md §2.4](../routers/datasets.md#24-get-apiv1datasetsdataset_iddatadata_idraw--stream-the-original-raw-file). Add a new module `crates/http-server/src/responses/raw_file.rs` with two builders:

- `serve_local_file(path, name, mime) -> Response` — opens via `tokio::fs::File`, wraps in `tokio_util::io::ReaderStream`, builds `axum::body::Body::from_stream`, sets `Content-Type`, `Content-Disposition: attachment; filename="<name>"`, and `Content-Length` from `metadata().len()`.
- `serve_s3_stream(reader, name, mime, size) -> Response` — wraps an `AsyncRead` from `cognee_lib::infrastructure::files::open_data_file(uri, "rb")` in `tokio_util::io::ReaderStream`, sets the same headers; **omits** `Content-Length` if `size` is `None` (chunked transfer).

Add `get_raw_data` handler:

1. Permission gate `read` on `dataset_id`.
2. Fetch the data row via `get_data(user.id, data_id)`. Map `DataNotFoundError` to `404 {detail: ...}` (canonical envelope here, distinct from §2.3's `{message}` — see [../routers/datasets.md §3.2](../routers/datasets.md#32-heterogeneous-error-envelopes)).
3. Parse `data.raw_data_location` URI: `urlparse(...).scheme`. Empty scheme or single-letter scheme (Windows drive letter) → local. `file://` → local. `s3://` → S3. Anything else → `501 {detail: "Storage scheme '<scheme>' not supported for direct download."}`.
4. Compute `download_name`: for S3, `Path(parsed_uri.path).name or data.name`; for local, basename of the resolved path. Compute mime: `data.mime_type` if present, else `mime_guess::from_path(path).first_or_octet_stream()`.

S3 chunk size is 1 MiB ([../routers/datasets.md §2.4 parity notes](../routers/datasets.md#24-get-apiv1datasetsdataset_iddatadata_idraw--stream-the-original-raw-file)). Streaming uses `tokio::io::AsyncReadExt::read_buf` natively — no thread-pool hop.

**Verify**: `cargo check --all-targets -p cognee-http-server`. Smoke-test by streaming a 50 MiB file from a temp directory and assert the response byte count matches.

### 4.8 Endpoint 2.5 (`GET /{id}/graph`) and 2.6 (`GET /{id}/schema`)

- `get_dataset_graph` per [../routers/datasets.md §2.5](../routers/datasets.md#25-get-apiv1datasetsdataset_idgraph--rendered-knowledge-graph): permission gate is enforced inside `get_formatted_graph_data` — handler does not duplicate. Call `cognee_lib::modules::graph::methods::get_formatted_graph_data(dataset_id, user)`. Map `GraphData` → `GraphDTO` (camelCase wire).
- `get_dataset_schema` per [../routers/datasets.md §2.6](../routers/datasets.md#26-get-apiv1datasetsdataset_idschema--read-graph-schema--custom-prompt): permission gate `read`. Delegate to `cognee_lib::modules::data::methods::get_dataset_configuration(dataset_id)`. When no row exists, both fields are `null`. Response is `DatasetSchemaResponseDTO` with **snake_case** wire (raw dict in Python). On 404 (no `read` permission), return `ApiError::WriteEnvelopeError("Dataset not found".into(), StatusCode::NOT_FOUND)` — `{error}` envelope.

**Verify**: `cargo check --all-targets -p cognee-http-server`.

### 4.9 Endpoint 2.7 (`POST /` create) and 2.8 (`PUT /{id}/schema` upsert)

- `create_new_dataset` per [../routers/datasets.md §2.7](../routers/datasets.md#27-post-apiv1datasets--create-a-dataset-or-return-existing-by-name): inject `Json<DatasetCreationPayload>`. Idempotent — if a dataset with that name already exists for `user.id`, return the existing row. On creation, grant `read+write+share+delete` ACLs (loop x4 over `give_permission_on_dataset`). Status is **200** (not 201). On any error, `ApiError::Teapot(format!("Error creating dataset: {e}"))`.
- `update_dataset_schema` per [../routers/datasets.md §2.8](../routers/datasets.md#28-put-apiv1datasetsdataset_idschema--upsert-graph-schema--custom-prompt): permission gate `write`. Critical: distinguish "field omitted" (`Option::is_none()`) from "field present and null" (`Some(Value::Null)`). The DTO uses `#[serde(default, skip_serializing_if = "Option::is_none")]`; the handler must check `value.is_some()` before calling the upsert with that field. Response is `{"status": "ok"}`. On 404 (no `write`), `ApiError::WriteEnvelopeError("Dataset not found".into(), StatusCode::NOT_FOUND)`.

**Verify**: `cargo check --all-targets -p cognee-http-server`.

### 4.10 Endpoints 2.9–2.11 (delete trio)

- `delete_all_datasets` per [../routers/datasets.md §2.9](../routers/datasets.md#29-delete-apiv1datasets--delete-every-dataset-the-caller-owns): no path/query/body. Returns `null` (200, not 204). Delegate to `cognee_lib::api::datasets::delete_all(user)`.
- `delete_dataset` per [../routers/datasets.md §2.10](../routers/datasets.md#210-delete-apiv1datasetsdataset_id--empty-delete-one-dataset): permission gate `delete`. Delegate to `cognee_lib::api::datasets::empty_dataset(dataset_id, user)`. Map `UnauthorizedDataAccessError` to `404 {detail: "Dataset (<uuid>) not accessible."}` (canonical envelope).
- `delete_data` per [../routers/datasets.md §2.11](../routers/datasets.md#211-delete-apiv1datasetsdataset_iddatadata_id--delete-one-data-item): permission gate `delete`. Delegate to `cognee_lib::api::datasets::delete_data(...)`. Returns `{"status": "success"}`. The deprecated `/api/v1/delete` route (step 4.12) calls into the same SDK function.

After all 11 handlers are wired, fill `pub fn router() -> Router<AppState>` with all routes in **Python file order** so OpenAPI tag ordering is stable per [../routers/datasets.md §5](../routers/datasets.md#5-implementation-tasks) item 3.

**Verify**: `cargo check --all-targets -p cognee-http-server`.

### 4.11 DTOs and router for `ontologies`

Create `crates/http-server/src/dto/ontologies.rs` and copy DTOs from [../routers/ontologies.md §4](../routers/ontologies.md#4-dto-definitions): `OntologyUploadMultipart`, `OntologyMetadataDTO`, `OntologyUploadResponseDTO`, `OntologyListResponseDTO`, `OntologyListEntryDTO`, `OntologyErrorResponseDTO`.

If `cognee_lib::ontology::OntologyManager` is not yet re-exported, add a one-line re-export in `crates/lib/src/lib.rs`:

```rust
pub mod ontology { pub use cognee_ontology::OntologyManager; }
```

The Python class is `OntologyService`; the Rust port reuses the **existing** `cognee_ontology::OntologyManager` per [../routers/ontologies.md §1](../routers/ontologies.md#1-mount--file) and [crates/ontology/src/manager.rs](../../crates/ontology/src/manager.rs). Methods used: `OntologyManager::list(user_id)`, `OntologyManager::upload(user_id, key, reader, description)`.

Add a new `ApiError::OntologyEnvelope { error: String, status: StatusCode }` variant ([../routers/ontologies.md §3.1](../routers/ontologies.md#31-error-envelope)) — emits `{"error": "<msg>"}`.

Create `crates/http-server/src/routers/ontologies.rs`:

1. `get_list` — delegate to `OntologyManager::list(user.id)`. Map result to `OntologyListResponseDTO` (BTreeMap for deterministic ordering in tests; the wire is unordered JSON either way). Empty result → `{}` (200, not 404).
2. `post_upload` — parse multipart with the helper from step 4.1. Per-router validation rules ([../routers/ontologies.md §2.2.1](../routers/ontologies.md#221-multipart-parts)):
   - Exactly one `ontology_file` part. >1 → `400 OntologyErrorResponseDTO {error: "Only one ontology_file is allowed"}`.
   - `ontology_key.trim()` must not start with `[` or `{`. Same for `description.trim()`.
   - Filename present and ends in `.owl` (case-insensitive).
   - Buffer the file fully into memory before write (Python parity per [../routers/ontologies.md §3.4](../routers/ontologies.md#34-buffering-parity-with-python)).
3. Pass `description` to `OntologyManager::upload`. Wrap result in `OntologyUploadResponseDTO {uploaded_ontologies: vec![meta]}` — single-element list per Python parity.
4. Map `ValueError`-equivalent errors (collision, bad key) to `400 OntologyErrorResponseDTO`; all other errors to `500 OntologyErrorResponseDTO` per [../routers/ontologies.md §2.2 error responses](../routers/ontologies.md#22-post-apiv1ontologies--upload-one-ontology).
5. `pub fn router() -> Router<AppState>` with `GET /` and `POST /`.
6. OpenAPI annotations.

**Verify**: `cargo check --all-targets -p cognee-http-server`.

### 4.12 DTOs and router for the deprecated `/delete`

Create `crates/http-server/src/dto/delete.rs` and copy DTOs from [../routers/delete.md §4](../routers/delete.md#4-dto-definitions): `DeleteQuery` (with `default_mode` fn), `DeleteMode` enum (`Soft` | `Hard`), `DeleteSuccessResponseDTO`, `DeleteErrorResponseDTO`.

Per [../routers/delete.md §6 open question 3](../routers/delete.md#6-open-questions): Python accepts arbitrary strings for `mode`. The Rust port matches strict wire parity — accept the string, pass it through without enum validation if a mismatch would fail axum's deserialization. Use `String` directly on `DeleteQuery.mode` to avoid 422-on-typo divergence; add a `to_mode_enum() -> DeleteMode` helper that defaults unknowns to `Soft` (matches Python's `delete_data` behavior).

Create `crates/http-server/src/routers/delete.rs`:

1. `delete_data_deprecated` handler — inject `Query<DeleteQuery>`. Permission gate `delete` on `dataset_id`. Delegate to `cognee_lib::api::datasets::delete_data(dataset_id, data_id, user, mode, delete_dataset_if_empty)`. Return `DeleteSuccessResponseDTO::ok()` on success.
2. **Headers**: every response (success **and** error) sets:
   - `Deprecation: true`
   - `Sunset: <date>` from `COGNEE_DEPRECATED_SUNSET_DELETE` (default `2026-12-01`).
   - `Link: </api/v1/datasets/{dataset_id}/data/{data_id}>; rel="successor-version"`
3. On any other error, return `409 DeleteErrorResponseDTO {error: <inner>}` (Python catch-all). Add an `ApiError::DeprecatedConflict(String)` variant for this.
4. Emit `tracing::warn!(target: "deprecated", "DELETE /v1/delete invoked by user={}", user.id)` at handler entry.
5. OpenAPI: mark `deprecated = true` in `#[utoipa::path(...)]`. Tag `["delete (deprecated)"]` (separate from `["datasets"]`).

**Verify**: `cargo check --all-targets -p cognee-http-server`.

### 4.13 DTOs and router for `/forget`

Create `crates/http-server/src/dto/forget.rs` and copy DTOs from [../routers/forget.md §4](../routers/forget.md#4-dto-definitions): `ForgetPayloadDTO` (camelCase wire — `dataId`), `DatasetRef` enum with custom `Deserialize` (UUID-first fallback), `ForgetDataItemResponse`, `ForgetDatasetResponse`, `ForgetEverythingResponse`, `ForgetResponseDTO` (untagged enum), `ForgetErrorResponseDTO`, `ForgetMode` enum, `ForgetPayloadDTO::resolve_mode()`.

The cross-field truth table is in [../routers/forget.md §2.1.1](../routers/forget.md#21-post-apiv1forget--unified-delete) — implement `resolve_mode()` exactly as the table specifies, including the Python quirk that `everything=true` silently ignores `data_id` and `dataset` (no 422).

Create `crates/http-server/src/routers/forget.rs`:

1. `post_forget` handler — inject `AuthenticatedUser`, `State<AppState>`, `Json<ForgetPayloadDTO>`.
2. Call `payload.resolve_mode()`. On `Err(msg)`, return `422 ForgetErrorResponseDTO {error: <msg>}` (Python uses 422, not 400, for these — see the quirk note in [../routers/forget.md §2.1](../routers/forget.md#21-post-apiv1forget--unified-delete)).
3. Permission check, mode-dependent: mode 1/2 → `delete` on the resolved `dataset_id`; mode 3 → no upfront check (the SDK filters internally).
4. If `state.lib.cloud_client().is_some()`, proxy via the cloud client (per [../routers/forget.md §3.4](../routers/forget.md#34-remote-client-proxying)).
5. Otherwise call `state.lib.forget(data_id, dataset, everything, user)`. Map result to the appropriate `ForgetResponseDTO` variant.
6. **Quirk**: `DatasetNotFoundError` (raised by `_resolve_dataset_id` on missing UUID) maps to **422** with the canonical message `"Invalid request parameters. Specify dataset, data_id+dataset, or everything=True."`, **not** 404. Python collapses missing-dataset and cross-field-validation cases into one 422 — see [../routers/forget.md §2.1 quirk note](../routers/forget.md#21-post-apiv1forget--unified-delete).
7. All other errors → `500 ForgetErrorResponseDTO {error: "An error occurred during deletion."}` (terse for parity).

Reuse `ApiError::OntologyEnvelope` from step 4.11 for the `{error}` shape, or add a dedicated `ApiError::ErrorEnvelope { error: String, status: StatusCode }` variant if cleaner — both routers share the shape but `ontologies` and `forget` ship under different tags so a shared variant is fine.

**Verify**: `cargo check --all-targets -p cognee-http-server`.

### 4.14 Permission-gate plumbing (P5 placeholder)

Every dataset-touching endpoint must call `state.lib.permissions().user_can(user.id, dataset_id, "<perm>")` per [../tenants.md §9](../tenants.md#9-repository-surface). The trait signature lives in [../tenants.md §9](../tenants.md#9-repository-surface) — `PermissionsRepository::user_can(&self, user_id: Uuid, dataset_id: Uuid, perm: &str) -> Result<bool, DbError>`.

The **real** SeaORM-backed impl lands in [P5](p5-admin.md) as part of the `tenants_rbac` migration. P2 ships a placeholder:

1. Add `PermissionsRepository` as a trait re-export from `cognee-database` (or add it as a stub trait in `cognee-database` if the crate doesn't yet declare it — confirm via `grep -rn PermissionsRepository crates/`). The trait location is settled in [../plan.md §7 open question 1](../plan.md#7-open-questions) (lean: `cognee-database`).
2. Add a placeholder impl `AllowAllPermissions` in `crates/http-server/src/permissions_stub.rs` that returns `Ok(true)` from `user_can`, `Ok(vec![])` from `visible_datasets`, and `unimplemented!()` from the rest. Construct it inside `AppState::build` until P5 lands.
3. Every call site in steps 4.2, 4.3, 4.5–4.10, 4.12, 4.13 prefixes the `user_can` call with `// TODO(P5): wire real PermissionsRepository`.

This is documented as a known gap in §6 (acceptance) — `scripts/check_all.sh` must still pass with the stub in place.

**Verify**: `cargo check --all-targets -p cognee-http-server`.

### 4.15 Wire all P2 routers into `build_router`

In `crates/http-server/src/lib.rs` (or wherever `build_router` lives — defined in P0), nest the new routers per [../architecture.md §7](../architecture.md#7-router-composition) at these prefixes (matches Python order):

- `.nest("/add", add::router())`
- `.nest("/datasets", datasets::router())`
- `.nest("/ontologies", ontologies::router())`
- `.nest("/delete", delete::router())`
- `.nest("/update", update::router())`
- `.nest("/forget", forget::router())`

All under the `/api/v1` parent. Order matches [../architecture.md §7](../architecture.md#7-router-composition) so OpenAPI tag ordering is stable.

Register every handler's `#[utoipa::path(...)]` annotation in the OpenAPI root struct (`src/openapi.rs`) so `GET /openapi.json` enumerates the new endpoints.

**Verify**: `cargo check --all-targets -p cognee-http-server` and `curl /openapi.json | jq '.paths | keys'` shows every new path.

### 4.16 Update status tables

Flip the per-router status from **Draft** → **In Progress** in:

- [README.md](README.md) — P2 row.
- [../routers/README.md](../routers/README.md) — rows for `add`, `update`, `datasets`, `ontologies`, `delete`, `forget`.

Once P2 lands and tests pass, flip those rows to **Done** in the merge commit.

**Verify**: visual diff.

## 5. Tests

All under `crates/http-server/tests/`. Each file uses the `tower::ServiceExt::oneshot` pattern from [../architecture.md §18](../architecture.md#18-testing-architecture). Where multipart bodies are required, build them with the `multipart-stream` helper (or hand-roll boundary strings) — `tests/support/multipart.rs` ships in P0.

| File | Coverage |
|---|---|
| `test_add.rs` | Multipart upload of (a) one text file → 200 + `PipelineRunCompleted` + 1 `data_ingestion_info`; (b) three files → 3 rows; (c) a `data` part whose body is `https://example.com/foo` < 4 KiB → URL ingestion path (the temp file is unlinked, `url_payload` set); (d) both `datasetName` and `datasetId` missing → 400 with `{"error": "Either datasetId or datasetName must be provided.", "detail": null}`; (e) caller lacks `write` → 403; (f) 257-part request → 400; (g) `node_set=[""]` normalizes to `None`; (h) filename traversal (`../etc/passwd`) → 400. Assert `data_id`/`content_hash`/`dataset_id` shape per Python parity. |
| `test_update.rs` | Add a document via `/add`, capture `data_id`/`dataset_id`. PATCH `/update` with new content, fetch graph, assert old node gone + new node present. Missing `data_id` → 422. Wrong `dataset_id` (not the owner of `data_id`) → 404. Caller lacks `delete` on dataset → 403. Cognify failure mid-flight → 500 with `{"error": "Pipeline run errored", ...}`. |
| `test_datasets_crud.rs` | List/create/delete dataset round-trip. Empty `name` accepted (Python parity). Idempotent create (same name → same row). All-datasets delete → 200 `null`. Single-dataset delete → 200. |
| `test_datasets_graph.rs` | Add + cognify a small dataset, then `GET /{id}/graph` returns `GraphDTO` with non-empty nodes/edges. CamelCase wire keys (`createdAt`, etc.) verified. |
| `test_datasets_raw.rs` | Round-trip via `/raw` for both local FS and (if feature-gated) S3. Add → fetch the data row → `GET /raw` → assert byte-for-byte equality, `Content-Disposition: attachment; filename="..."`, `Content-Type` from `data.mime_type`, `Content-Length` for local. Unknown URI scheme → 501 with the canonical message. |
| `test_datasets_schema.rs` | `PUT /schema` then `GET /schema` round-trip for both `graph_schema` (Object) and `custom_prompt` (String). Partial update — sending only `graph_schema` leaves `custom_prompt` untouched. Field absent vs `null` distinction (only `null` clears; absent leaves untouched). Empty `DatasetConfiguration` → both fields `null`. |
| `test_datasets_status.rs` | Seed a `pipeline_runs` row for one dataset; `GET /status?dataset=<uuid>` returns `{<uuid>: "DATASET_PROCESSING_COMPLETED"}`. Empty `dataset` query → `{}` (not 422). Datasets the caller cannot read are silently dropped. Bad UUID → 422. |
| `test_ontologies.rs` | Multipart upload of a tiny `.owl` file → 200 with `uploaded_ontologies: [<one>]`; list → entry present. Upload twice with same key → second 400 `{"error": "Ontology key '<key>' already exists"}`. Upload with `.txt` filename → 400 `{"error": "File must be in .owl format"}`. Upload with `ontology_key="[evil]"` → 400. Two concurrent uploads with different keys → both succeed; metadata.json contains both. |
| `test_delete.rs` | Add a data row → DELETE via deprecated `/api/v1/delete?data_id=...&dataset_id=...` → 200 `{"status": "success"}`. Wrong `data_id` → 409 with `{error: ...}` envelope. **Assert `Deprecation: true` and `Sunset: <date>` headers on every response (success and error).** Compare deletion side effects to canonical `/datasets/{id}/data/{did}` route. |
| `test_forget.rs` | Three-mode truth table per [../routers/forget.md §2.1.1](../routers/forget.md#21-post-apiv1forget--unified-delete): (a) `data_id+dataset` → 200 `ForgetDataItemResponse`; (b) `dataset` only → 200 `ForgetDatasetResponse`; (c) `everything=true` → 200 `ForgetEverythingResponse` with `datasets_removed=N`. (d) Neither field → 422 with the canonical message. (e) `data_id` only → 422. (f) Wrong UUID for `dataset` → 422 with the canonical message (Python collapses missing-dataset into 422). (g) `everything=true` + extra fields → 200 (ignored). (h) No auth → 401. |

For each test, follow the P0 harness: `let app = test_router(state).await; let resp = app.oneshot(req).await.unwrap();`. Use `tests/support/auth.rs` to mint a bearer token for `AuthenticatedUser`.

**Verify per file**: `cargo test --test <file_name> -p cognee-http-server`.

## 6. Acceptance criteria

- [ ] `cargo check --all-targets -p cognee-http-server` succeeds.
- [ ] All P2 integration tests pass: `cargo test -p cognee-http-server --tests` runs every file in §5 green.
- [ ] `scripts/check_all.sh` passes (fmt + check + clippy + capi/python/js binding checks).
- [ ] **Multipart upload of a 50 MiB file via `/add` round-trips through `/datasets/{id}/data/{did}/raw` byte-for-byte.** Add this as a single explicit test in `test_datasets_raw.rs` (gated on `tempfile::tempdir`); compute SHA-256 of input and output, assert equality.
- [ ] Status table in [README.md](README.md) updated: P2 row flipped from **Draft** → **Done**.
- [ ] Per-router status table in [../routers/README.md](../routers/README.md) updated: rows for `add`, `update`, `datasets`, `ontologies`, `delete`, `forget` flipped to **Done**.
- [ ] Permission gates everywhere are stubbed via `AllowAllPermissions` returning `Ok(true)`, with one `// TODO(P5): wire real PermissionsRepository` comment per call site. P5 will replace the stub with the SeaORM impl per [../tenants.md §9](../tenants.md#9-repository-surface) — until then, **enforcement is bypassed**. Document this loudly in the PR description so reviewers know not to merge to a production branch without P5.
- [ ] OpenAPI: every new handler appears in `GET /openapi.json` with the right tag, params, request body, and response schema. `curl http://localhost:8000/openapi.json | jq '.paths | keys'` lists all 17 P2 paths (1 add + 1 update + 11 datasets + 2 ontologies + 1 delete + 1 forget).

## 7. Files touched

**New files** (created in P2):

- `crates/http-server/src/multipart.rs` — shared multipart helper (step 4.1).
- `crates/http-server/src/responses/raw_file.rs` — streaming local + S3 file responder (step 4.7).
- `crates/http-server/src/permissions_stub.rs` — `AllowAllPermissions` placeholder (step 4.14).
- `crates/http-server/src/dto/add.rs` — DTOs for `/add` (step 4.2).
- `crates/http-server/src/dto/update.rs` — DTOs for `/update` (step 4.3).
- `crates/http-server/src/dto/datasets.rs` — DTOs for `/datasets/*` (step 4.4).
- `crates/http-server/src/dto/ontologies.rs` — DTOs for `/ontologies` (step 4.11).
- `crates/http-server/src/dto/delete.rs` — DTOs for `/delete` (step 4.12).
- `crates/http-server/src/dto/forget.rs` — DTOs for `/forget` (step 4.13).
- `crates/http-server/src/routers/add.rs` — handler (step 4.2).
- `crates/http-server/src/routers/update.rs` — handler (step 4.3).
- `crates/http-server/src/routers/datasets.rs` — 11 handlers (steps 4.5–4.10).
- `crates/http-server/src/routers/ontologies.rs` — handlers (step 4.11).
- `crates/http-server/src/routers/delete.rs` — handler (step 4.12).
- `crates/http-server/src/routers/forget.rs` — handler (step 4.13).
- `crates/http-server/tests/test_add.rs`, `test_update.rs`, `test_datasets_crud.rs`, `test_datasets_graph.rs`, `test_datasets_raw.rs`, `test_datasets_schema.rs`, `test_datasets_status.rs`, `test_ontologies.rs`, `test_delete.rs`, `test_forget.rs` (step 5).

**Modified files**:

- `crates/http-server/src/error.rs` — new `ApiError` variants: `WriteEndpointError`, `Teapot`, `WriteEnvelopeError`, `ErrorMessageError`, `OntologyEnvelope` (or shared `ErrorEnvelope`), `DeprecatedConflict`. Each gets an `IntoResponse` arm emitting the right shape (steps 4.2, 4.4, 4.11, 4.12).
- `crates/http-server/src/lib.rs` (or `src/router.rs`) — `build_router` nests the six new routers (step 4.15).
- `crates/http-server/src/openapi.rs` — register every new handler under the OpenAPI root (step 4.15).
- `crates/http-server/src/state.rs` — `AppState::build` wires `AllowAllPermissions` (step 4.14).
- `crates/http-server/src/config.rs` — adds `add_body_limit`, `update_body_limit`, `deprecated_sunset_delete`, `ontology_dir` config fields (env: `HTTP_BODY_LIMIT_BYTES_ADD`, `HTTP_BODY_LIMIT_BYTES_UPDATE`, `COGNEE_DEPRECATED_SUNSET_DELETE`, `COGNEE_ONTOLOGY_DIR`).
- `crates/lib/src/lib.rs` — re-export `cognee_ontology::OntologyManager` under `cognee_lib::ontology` if not already (step 4.11).
- `docs/http-server/implementation/README.md` — flip P2 status to **Done** (step 4.16).
- `docs/http-server/routers/README.md` — flip rows for the six P2 routers to **Done** (step 4.16).
