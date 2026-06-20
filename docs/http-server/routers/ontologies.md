# Router: ontologies

Multipart endpoint for uploading and listing OWL/RDF ontology files. Ontologies are user-scoped and stored under a per-user directory; the upload validates the `.owl` extension and the user-provided `ontology_key` for shape, then writes the file plus a JSON metadata index. Cognify pipelines can later reference these files by key (the ontology integration is described in [`cognee-ontology`](../../../crates/ontology/) and is out of scope for this doc).

Companion docs: [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../tenants.md](../tenants.md), [../observability.md](../observability.md).

## 1. Mount & file
- Mount prefix: `/api/v1/ontologies`
- Router file: `crates/http-server/src/routers/ontologies.rs`
- Python source: [`cognee/api/v1/ontologies/routers/get_ontology_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/routers/get_ontology_router.py)
- Underlying SDK class: [`cognee/api/v1/ontologies/ontologies.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/ontologies.py) (`OntologyService`)
- Rust delegation target: the existing [`cognee_ontology::OntologyManager`](../../../crates/ontology/src/manager.rs) (re-exported as `cognee_lib::ontology::OntologyManager`). Methods: `OntologyManager::list`, `::upload`, `::get_contents` ([manager.rs:152, :249, :263](../../../crates/ontology/src/manager.rs)). The Python class is `OntologyService`; the Rust port reuses the existing `OntologyManager` rather than mirroring the Python class name.

## 2. Endpoints

### 2.1 `GET /api/v1/ontologies` — List the caller's uploaded ontologies

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params**: none.
- **Request body**: none.
- **Response body** (`200 OK`): a free-form JSON object mapping `ontology_key` → metadata. Source: [`get_ontology_router.py:104-105`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/routers/get_ontology_router.py#L104-L105) returns the raw `metadata` dict from `OntologyService.list_ontologies(user)`. Each entry's metadata is the JSON written to `metadata.json` at upload time:
  ```json
  {
    "my_schema": {
      "filename":     "schema.owl",
      "size_bytes":   12345,
      "uploaded_at":  "2026-04-24T12:34:56.789012+00:00",
      "description":  "FOAF + DC core"
    },
    "another_key": { ... }
  }
  ```
  Wire keys are snake_case (the response is a plain dict, no `OutDTO` aliasing). Source: [`ontologies.py:68-73`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/ontologies.py#L68-L73).
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | No valid credential. |
  | `500` | `{"error": "<inner>"}` | Generic catch — Python returns `{"error": str(e)}`. Source: [`get_ontology_router.py:107`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/routers/get_ontology_router.py#L107). Note `{error}` envelope (not `{detail}`). |

- **Side effects**: reads `metadata.json` at `/tmp/ontologies/<user_id>/metadata.json`. Creates the user directory if missing (lazy).
- **Delegation target**: `cognee_ontology::OntologyManager::list(user) -> serde_json::Map<String, OntologyMetadata>` (re-exported as `cognee_lib::ontology::OntologyManager::list`).
- **Validation rules**: none.
- **Permission gate**: per-user (the directory is keyed by `user.id`); no per-dataset permission needed because ontologies are not currently associated with datasets at the storage layer.
- **OpenAPI**: tag `["ontologies"]`, response `200: object` (free-form). `additionalProperties: OntologyMetadataDTO`.
- **Telemetry**:
  - Span name: `cognee.api.ontologies.list`.
  - Attributes: `cognee.api.endpoint = "GET /api/v1/ontologies"`, `cognee.user.id`, `cognee.ontology.count`.
- **Python parity notes**: returns an empty object `{}` if the user has uploaded none — not 404. Match.

### 2.2 `POST /api/v1/ontologies` — Upload one ontology

- **Auth**: `required`.
- **Path params**: none.
- **Query params**: none.
- **Request body**: `multipart/form-data`. See §2.2.1.
- **Response body** (`200 OK`): a one-element list wrapped in `{"uploaded_ontologies": [...]}`:
  ```json
  {
    "uploaded_ontologies": [
      {
        "ontology_key": "my_schema",
        "filename":     "schema.owl",
        "size_bytes":   12345,
        "uploaded_at":  "2026-04-24T12:34:56.789012+00:00",
        "description":  "FOAF + DC core"
      }
    ]
  }
  ```
  Source: [`get_ontology_router.py:67-77`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/routers/get_ontology_router.py#L67-L77). Wire keys: snake_case (raw dict). The list always has length 1 because the endpoint accepts exactly one file.

- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `400` | `{"error": "Only one ontology_file is allowed"}` | More than one file uploaded with the `ontology_file` part name. Source: [`get_ontology_router.py:50-53`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/routers/get_ontology_router.py#L50-L53). |
  | `400` | `{"error": "ontology_key must be a string"}` | `ontology_key` looks like JSON (starts with `[` or `{`). Source: [`:55-56`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/routers/get_ontology_router.py#L55-L56). |
  | `400` | `{"error": "description must be a string"}` | `description` looks like JSON. Source: [`:57-58`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/routers/get_ontology_router.py#L57-L58). |
  | `400` | `{"error": "File must have a filename"}` | The upload part has no filename. Source: [`ontologies.py:51-52`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/ontologies.py#L51-L52). |
  | `400` | `{"error": "File must be in .owl format"}` | Filename does not end in `.owl` (case-insensitive). Source: [`ontologies.py:53-54`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/ontologies.py#L53-L54). |
  | `400` | `{"error": "Ontology key '<key>' already exists"}` | Key collision in the per-user metadata. Source: [`ontologies.py:59-60`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/ontologies.py#L59-L60). |
  | `401` | `{"detail": "Unauthorized"}` | — |
  | `422` | `{"detail": [...], "body": ...}` | Missing required parts (`ontology_key`, `ontology_file`). |
  | `500` | `{"error": "<inner>"}` | Disk I/O / JSON write failure. Source: [`get_ontology_router.py:80-81`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/routers/get_ontology_router.py#L80-L81). |

  Note: Python catches `ValueError` separately from generic `Exception`, mapping the former to 400 and the latter to 500. Both use `{error: str}`. The Rust port mirrors with two distinct `ApiError::OntologyBadRequest(String)` and `ApiError::OntologyInternal(String)` variants serializing the same `{error}` envelope.

#### 2.2.1 Multipart parts

| Part name | Required | Cardinality | Content type | Backing | Notes |
|---|---|---|---|---|---|
| `ontology_key` | yes | 1 | `text/plain` | Form field | User-defined identifier. Must not start with `[` or `{` (Python's anti-JSON guard, [`get_ontology_router.py:55-56`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/routers/get_ontology_router.py#L55-L56)). Whitespace is stripped before the check, but stored verbatim. |
| `ontology_file` | yes | exactly 1 | `application/rdf+xml` (typical), `text/xml`, `application/octet-stream` (no validation on content type — only filename extension) | Buffered in memory **before write** | The Python SDK uses `await file.read()` ([`ontologies.py:62`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/ontologies.py#L62)) — full buffer. Filename must end in `.owl` (case-insensitive). The handler verifies `len(form.getlist("ontology_file")) == 1` to reject multi-file uploads even though the parameter type suggests one. |
| `description` | no | 0..1 | `text/plain` | Form field | If present, must not start with `[` or `{`. |

  **Buffering**: Python buffers the full file in memory before writing to disk ([`ontologies.py:62-66`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/ontologies.py#L62-L66)). The Rust port matches verbatim — read the multipart part fully into memory, then write to disk in one shot. Streaming to disk would be a memory-consumption improvement but introduces partial-write failure modes that don't exist in Python; strict parity says match Python's read-then-write pattern.

  **Body-size limit**: ontologies are typically small (≤ 50 MB); the global 100 MiB cap suffices. No per-route override needed.

  **Max part count**: 3 logical parts (`ontology_key`, `ontology_file`, optional `description`). The handler explicitly rejects extra `ontology_file` parts. Other unrecognized parts are silently ignored to match Python's permissive `Form()` semantics.

- **Side effects**:
  - **File storage**: writes `<file_path> = $TMPDIR/ontologies/<user_id>/<ontology_key>.owl` and rewrites `$TMPDIR/ontologies/<user_id>/metadata.json` with the new key. **No cognee storage layer is used** — the OntologyService writes directly to the OS temp dir. This is a known limitation: ontologies are lost on container restart in ephemeral filesystem deployments. Document loudly. The Rust port preserves this behavior for parity but **adds** a configurable base dir via `COGNEE_ONTOLOGY_DIR` env var (falls back to `std::env::temp_dir().join("ontologies")` when unset). Python honors `tempfile.gettempdir()` ([`ontologies.py:26`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/ontologies.py#L26)).
  - **Relational DB**: none — ontology metadata is **not** persisted in the relational DB. (This is a real gap; tracked as open question.)
  - **Graph / Vector DB**: none.
- **Delegation target**: `cognee_ontology::OntologyManager::upload(user, ontology_key, file: impl AsyncRead, description: Option<String>) -> OntologyMetadata` (re-exported as `cognee_lib::ontology::OntologyManager::upload`).
- **Validation rules**:
  1. Exactly one `ontology_file` part. >1 → 400.
  2. `ontology_key.trim()` must not start with `[` or `{`.
  3. `description.trim()` (when present) must not start with `[` or `{`.
  4. Filename present.
  5. Filename ends in `.owl` (case-insensitive).
  6. `ontology_key` not already present in the user's metadata.
- **Permission gate**: per-user (the user's directory). No dataset-level permission check.
- **Rate / size limits**: global 100 MiB body limit (sufficient for OWL files).
- **OpenAPI**:
  - Tag: `["ontologies"]`
  - `requestBody`: `multipart/form-data` with the three parts above.
  - Responses: `200: OntologyUploadResponseDTO`, `400/422/500: OntologyErrorResponseDTO`.
  - Security: defaults to global `[BearerAuth, ApiKeyAuth]`.
- **Telemetry**:
  - Span name: `cognee.api.ontologies.upload`.
  - Attributes: `cognee.api.endpoint = "POST /api/v1/ontologies"`, `cognee.user.id`, `cognee.ontology.key`, `cognee.ontology.size_bytes`, `cognee.ontology.filename`.
- **Python parity notes**:
  - The `[` / `{` anti-JSON guard on `ontology_key` and `description` is a defensive check against clients that accidentally JSON-encode form fields ([`get_ontology_router.py:55-58`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/routers/get_ontology_router.py#L55-L58)). Reproduce.
  - The metadata file (`metadata.json`) is rewritten in full on each upload — there is no atomic update primitive. Concurrent uploads could clobber each other. Match for parity, but note the race in §6.
  - The response wraps a single result in `{"uploaded_ontologies": [<one>]}` even though only one upload is supported. The list shape is preserved for forward compatibility with a hypothetical multi-upload endpoint (`OntologyService.upload_ontologies` exists but is not wired to a route — [`ontologies.py:85-124`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/ontologies.py#L85-L124)).
  - Python uses `datetime.now(timezone.utc).isoformat()` for `uploaded_at`. Rust must match — `chrono::Utc::now().to_rfc3339()` produces a different sub-second precision; use `format!("{}", now.format("%Y-%m-%dT%H:%M:%S%.6f%:z"))` to match Python's microsecond precision exactly.
  - File is written with `wb` (binary). Rust uses `tokio::fs::OpenOptions::new().write(true).create(true).truncate(true).open(...)`.

## 3. Cross-cutting behavior

### 3.1 `{error}` envelope

This router uses `{"error": "<msg>"}` for both 400 and 500. **Distinct** from the canonical `{"detail": "..."}` shape and from the `{error, detail}` shape used by add/update. Define a dedicated `ApiError::OntologyEnvelope { error: String, status: StatusCode }` variant.

### 3.2 Per-user storage isolation

Every operation is scoped by `user.id`. The directory tree is `<base>/<user_id>/{<key>.owl, metadata.json}`. There is no cross-tenant visibility; superusers do not see other users' ontologies via this router. (For admin reads, a separate endpoint would be needed; not in scope.)

### 3.3 No association with datasets at upload

The upload does not bind the ontology to any dataset. Cognify operations later reference ontologies by key via the `ontology_keys` field in `CognifyPayloadDTO` (covered in `routers/cognify.md`). This is intentional but worth noting — clients shipping an ontology must remember to plumb the `ontology_key` through to subsequent calls.

### 3.4 Buffering parity with Python

Python's [`ontologies.py:62`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/ontologies.py#L62) does `await file.read()` — a full-buffer read before write. Rust matches verbatim: read the multipart part into a `Vec<u8>` and write in one shot. Operators ingesting files larger than process memory should rely on the multipart body-size limit ([../architecture.md §8](../architecture.md#8-middleware-stack)) to reject them; the same constraint applies to Python.

## 4. DTO definitions

Located in `crates/http-server/src/dto/ontologies.rs`.

```rust
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;

/// Multipart form for `POST /api/v1/ontologies`. OpenAPI-only DTO; the
/// handler reads parts manually.
#[derive(Debug, ToSchema)]
#[allow(dead_code)]
pub struct OntologyUploadMultipart {
    /// User-defined identifier; must not start with `[` or `{`.
    pub ontology_key: String,

    /// The OWL file. Filename must end in `.owl` (case-insensitive).
    /// Exactly one allowed.
    #[schema(format = "binary")]
    pub ontology_file: Vec<u8>,

    /// Optional human-readable description; must not start with `[` or `{`.
    pub description: Option<String>,
}

/// Wire shape of a single uploaded-ontology entry. Snake_case (raw dict in
/// Python, not `OutDTO`).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct OntologyMetadataDTO {
    pub ontology_key: String,
    pub filename: String,
    pub size_bytes: u64,
    /// ISO-8601 UTC timestamp with microsecond precision (Python's
    /// `datetime.now(timezone.utc).isoformat()` shape).
    pub uploaded_at: String,
    pub description: Option<String>,
}

/// Response body for `POST /api/v1/ontologies` — always one entry in `uploaded_ontologies`.
#[derive(Debug, Serialize, ToSchema)]
pub struct OntologyUploadResponseDTO {
    pub uploaded_ontologies: Vec<OntologyMetadataDTO>,
}

/// Response body for `GET /api/v1/ontologies` — map of key → metadata.
/// We use `BTreeMap` for deterministic ordering in tests, even though
/// Python's `dict` is insertion-ordered. The wire shape is identical
/// either way (JSON objects are unordered in spec).
pub type OntologyListResponseDTO = BTreeMap<String, OntologyListEntryDTO>;

/// Per-entry metadata as written into `metadata.json`. Distinct from
/// `OntologyMetadataDTO` because the listing entry omits `ontology_key`
/// (it's the map key) — Python writes a 4-field dict.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct OntologyListEntryDTO {
    pub filename: String,
    pub size_bytes: u64,
    pub uploaded_at: String,
    pub description: Option<String>,
}

/// `{error: String}` envelope. Distinct from the canonical `ApiError`
/// `{detail: ...}` and from the `add`/`update` `{error, detail}`.
#[derive(Debug, Serialize, ToSchema)]
pub struct OntologyErrorResponseDTO {
    pub error: String,
}
```

Field-level mapping vs Python:

| Python | Rust | Wire | Notes |
|---|---|---|---|
| `ontology_key: str` (Form) | `ontology_key: String` | `ontology_key` | snake_case form field. |
| `ontology_file: UploadFile` (File) | streamed `Vec<u8>` | `ontology_file` | Required exactly one. |
| `description: Optional[str]` (Form) | `Option<String>` | `description` | Optional. |
| `OntologyMetadata` (dataclass — [`ontologies.py:11-17`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/ontologies.py#L11-L17)) | `OntologyMetadataDTO` | snake_case | All fields. |
| `metadata.json` per-entry dict | `OntologyListEntryDTO` | snake_case | Drops `ontology_key` — it's the map key. |
| `{"error": str}` | `OntologyErrorResponseDTO` | `error` | One-field envelope. |

## 5. Implementation tasks

1. Add DTOs in `crates/http-server/src/dto/ontologies.rs` (all of §4).
2. The existing [`cognee_ontology::OntologyManager`](../../../crates/ontology/src/manager.rs) (`list` / `upload` / `get_contents`) is the delegation target — re-export it under `cognee_lib::ontology::OntologyManager` if not already exposed. The on-disk format (`<base>/<user_id>/<key>.owl` + `metadata.json`) is handled by the manager.
3. Add the `get_list` and `post_upload` handlers in `crates/http-server/src/routers/ontologies.rs`:
   - `get_list`: simple delegation; map service result to `OntologyListResponseDTO`.
   - `post_upload`: parse multipart, validate (5 rules), stream to disk, update metadata, return `OntologyUploadResponseDTO`.
4. OpenAPI annotations declaring tags, request body, responses.
5. Per-tenant base dir override via `COGNEE_ONTOLOGY_DIR`.
6. Unit tests: anti-JSON guard for `ontology_key` and `description`; `.owl` extension validation (case-insensitive); duplicate-key rejection; multi-file rejection.
7. Integration tests in `crates/http-server/tests/test_ontologies.rs`:
   - Upload a `.owl` → 200 with `uploaded_ontologies` entry.
   - List → entry present.
   - Upload twice with same key → second returns 400.
   - Upload with `.txt` filename → 400.
   - Upload with `ontology_key="[evil]"` → 400.
   - Upload concurrent (two threads, different keys) → both succeed; metadata.json contains both.
8. Cross-SDK parity tests:
   - Upload identical file via Python and Rust; assert identical metadata shape and on-disk filename layout.
   - List on a Python-seeded directory from Rust; assert keys/values match.

## 6. Open questions

1. **Persistent ontology storage**: Python uses ephemeral `tempfile.gettempdir()` — uploads are lost across restarts. Rust matches verbatim: same temp-dir-based storage, no `COGNEE_ONTOLOGY_DIR` env var, no relational-DB metadata. Operators wanting durable storage must mount `/tmp` from a persistent volume (the same workaround Python deployments use).
2. **Concurrent metadata.json writes**: read-modify-write race when two uploads land at once. Python has the same race; Rust matches. Operators wanting safety must serialize uploads at a reverse-proxy / queue layer.
3. **OWL content validation**: Python only checks the file extension. Rust matches — only the `.owl` extension is validated; content parsing happens at cognify time (where it would also happen in Python).
4. **Response shape for `GET`**: free-form object (matches Python). No upgrade to `[OntologyMetadataDTO, ...]`.
5. **Cross-tenant ontology sharing**: per-user (matches Python). No tenant scoping.
6. **Microsecond timestamp precision**: Python's `isoformat()` produces e.g. `2026-04-24T12:34:56.789012+00:00` (6 decimals, `+00:00` offset). Rust must use `format!("%Y-%m-%dT%H:%M:%S%.6f%:z")` (not `to_rfc3339_opts(SecondsFormat::Micros, true)` which emits `Z`) to match Python's wire format byte-for-byte. Confirm with cross-SDK timestamp-equality tests.

## 7. References

- Python router: [`cognee/api/v1/ontologies/routers/get_ontology_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/routers/get_ontology_router.py) (lines 1-109).
- Python SDK service: [`cognee/api/v1/ontologies/ontologies.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/ontologies.py) (lines 1-159).
- Rust ontology crate: [`crates/ontology/`](../../../crates/ontology/) — RDF/JSON-LD/Turtle loader, ontology resolver trait.
- Cognify integration (where ontology keys are consumed): `routers/cognify.md` (TBD in P3).
- Architecture: [../architecture.md §8 multipart](../architecture.md#8-middleware-stack).
- Cross-router conventions: [README.md §3](README.md#3-cross-router-conventions).
