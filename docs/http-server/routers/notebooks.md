# Router: notebooks

The `notebooks` router exposes a per-user, server-stored Jupyter-like notebook surface used by the cognee-frontend's "Notebooks" panel. Each notebook is a list of typed cells (`markdown` or `code`) persisted in the relational DB; users can list, create, update, and delete them. A separate `POST /{notebook_id}/{cell_id}/run` endpoint executes a single Python code cell inside a process-local sandbox — this is the only piece that's *not* a CRUD operation, and it is the only piece deferred to Stage B in the Rust port.

This router is intentionally split into two scopes:

- **CRUD scope (Stage A deliverable)** — `GET`, `POST`, `PUT`, `DELETE` over the `notebooks` table. Pure SQLite/Postgres I/O, identical wire shape to Python.
- **Sandbox scope (Stage B deliverable)** — `POST /{notebook_id}/{cell_id}/run`. Requires a Python execution environment we do not yet ship. Stage A returns `501 Not Implemented` with a documented JSON body.

Companion docs: [../plan.md](../plan.md), [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../observability.md](../observability.md), [../tenants.md](../tenants.md).

## 1. Mount & file

- Mount prefix: `/api/v1/notebooks`
- Router file: `crates/http-server/src/routers/notebooks.rs`
- Python source: [`cognee/api/v1/notebooks/routers/get_notebooks_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/notebooks/routers/get_notebooks_router.py)
- Python module backing the router (read these for parity): [`cognee/modules/notebooks/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/notebooks).

The router is registered in [`cognee/api/client.py:269-274`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L269-L274) with `tags=["notebooks"]`.

## 2. Endpoints

Five endpoints. Listed in HTTP-method order (`GET → POST → PUT → DELETE`); the nested `POST /{notebook_id}/{cell_id}/run` follows the top-level `POST` because it's still a `POST`.

### 2.1 `GET /` — list notebooks for the authenticated user

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params**: none. (Python does not paginate; we replicate per [README §3.4](README.md#34-pagination).)
- **Request body**: none.
- **Response body**: `200 OK`, `application/json`, body is `Vec<NotebookDTO>` (see §4). On first call for a new user the server lazily creates two **tutorial notebooks** (UUID5-derived ids, `deletable=false`); the response includes them alongside any user-created notebooks.
- **Error responses**:

  | Status | Body shape | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | No credential and `REQUIRE_AUTHENTICATION=true`. |
  | `500` | `{"detail": "Internal server error"}` | DB error reading `notebooks` table. |

- **Side effects**: on first call per user, the server seeds two tutorial notebooks via [`create_tutorial_notebooks`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/notebooks/methods/create_tutorial_notebooks.py). The Rust port replicates this seed-on-first-list behavior so the frontend's onboarding works identically.
- **Delegation target**: `cognee_lib::notebooks::list_notebooks(user_id) -> Vec<Notebook>` — TBD; the `cognee-lib` facade does not currently expose notebook methods. Stage A implementation adds a thin `cognee_lib::notebooks` module wrapping a new `cognee-database` repository (see §5).
- **Validation rules**: none beyond auth.
- **Permission gate**: implicit "owner" filter — the SELECT is scoped to `WHERE owner_id = $1`. No explicit `PermissionsRepository` check.
- **OpenAPI**: tag `notebooks`. Response schema `NotebookDTO[]`.
- **Telemetry**: span name `cognee.api.notebooks.list`. Attributes: `cognee.user.id`.
- **Python parity notes**: Python returns the SQLAlchemy ORM objects through FastAPI's default JSON encoder, which serializes them as the `Notebook` table columns. Match the column-by-column shape in `NotebookDTO`. The `cells` field is JSON-encoded list of `NotebookCell` objects (see [`Notebook.NotebookCellList`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/notebooks/models/Notebook.py#L33-L48)).

### 2.2 `POST /` — create a notebook

- **Auth**: `required`.
- **Path params**: none.
- **Query params**: none.
- **Request body**: `application/json`, `NotebookDataDTO`:

  | Field | Rust type | Python type | Required | Notes |
  |---|---|---|---|---|
  | `name` | `Option<String>` | `Optional[str] = Field(...)` | Yes (Pydantic `Field(...)` makes it required despite `Optional`) | Display name. Empty string is accepted by Python (no length check). |
  | `cells` | `Vec<NotebookCellDTO>` | `Optional[List[NotebookCell]] = Field(default=[])` | No (default `[]`) | List of typed cells. |

- **Response body**: `200 OK`, `application/json`, `NotebookDTO`. Python returns the freshly inserted row (with the server-assigned `id`, `created_at`, `deletable=true`). Match this exactly — do not return `201`.
- **Error responses**:

  | Status | Body shape | Condition |
  |---|---|---|
  | `400` | `{"detail": [...], "body": ...}` | Pydantic validation error (missing `name`, malformed cell). Use the canonical `ApiError::Validation`. |
  | `401` | `{"detail": "Unauthorized"}` | Missing/invalid auth. |
  | `500` | `{"detail": "Internal server error"}` | DB write failed. |

- **Side effects**: inserts one row into `notebooks` with `owner_id = user.id`, `deletable=true`, `created_at = now()`. Generates a fresh `uuid4` id.
- **Delegation target**: `cognee_lib::notebooks::create_notebook(user_id, name, cells, deletable=true)`.
- **Validation rules**: per Python, `name` is `Optional` but uses Pydantic `Field(...)` which makes the field required at the schema level (a missing `name` triggers a validation error). The Rust DTO mirrors this with a `Option<String>` that the `Json` extractor accepts as `null` *only if explicitly provided* — practically, treat the field as required for parity. Cell ids are client-supplied UUIDs; if absent the server does **not** generate them (Python relies on the client). Document this: the frontend always sends ids.
- **Permission gate**: none beyond ownership (the new row is owned by the caller).
- **OpenAPI**: tag `notebooks`. Request body `NotebookDataDTO`, response `NotebookDTO`.
- **Telemetry**: span name `cognee.api.notebooks.create`. Attributes: `cognee.notebook.id`, `cognee.notebook.cell_count`.
- **Python parity notes**: Python's [`create_notebook`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/notebooks/methods/create_notebook.py) uses `deletable=deletable or True` — passing `False` explicitly *still* yields `True` (truthiness bug). We replicate the bug for byte-compat; the Python public route passes `deletable=True` literal so the bug is unobservable through the HTTP surface anyway.

### 2.3 `PUT /{notebook_id}` — replace a notebook's name and/or cells

- **Auth**: `required`.
- **Path params**: `notebook_id: Uuid`.
- **Query params**: none.
- **Request body**: `application/json`, `NotebookDataDTO` (same shape as create — see §2.2).
- **Response body**: `200 OK`, `application/json`, `NotebookDTO` (the updated row).
- **Error responses**:

  | Status | Body shape | Condition |
  |---|---|---|
  | `400` | `{"detail": [...], "body": ...}` | Validation error. |
  | `401` | `{"detail": "Unauthorized"}` | — |
  | `404` | `{"error": "Notebook not found"}` | Notebook id not owned by caller, or doesn't exist. **Note the unusual `error` envelope** — Python uses `{"error": "..."}` here, not `{"detail": "..."}`. Match exactly. |
  | `500` | `{"detail": "Internal server error"}` | DB write failed. |

- **Side effects**: updates the `name` and/or `cells` columns of the matching row when the new value differs. The Python implementation only assigns when `notebook_data.name and notebook_data.name != notebook.name` — falsy `name` (empty string, `None`) leaves the existing name unchanged. Same for `cells`: only overwritten when `notebook_data.cells` is *truthy* (non-empty list). An empty cells list **does not clear cells**. We replicate this exactly.
- **Delegation target**: `cognee_lib::notebooks::update_notebook(notebook_id, user_id, patch) -> Option<Notebook>`.
- **Validation rules**: `notebook_id` parsed as `Uuid` via `Path<Uuid>` extractor; malformed UUIDs trigger `400 {"detail": "..."}`.
- **Permission gate**: ownership check via `WHERE id = $1 AND owner_id = $2`. A row owned by another user returns `404` (not `403`) — Python does the same to avoid leaking the existence of foreign rows.
- **OpenAPI**: tag `notebooks`. Path param `notebook_id: uuid`. Response `NotebookDTO`.
- **Telemetry**: span name `cognee.api.notebooks.update`. Attributes: `cognee.notebook.id`.
- **Python parity notes**: the 404 envelope is `{"error": ...}`, *not* `{"detail": ...}`. This deviates from FastAPI's default and from every other 404 in the API. Matched intentionally for client compat. See [router source line 53](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/notebooks/routers/get_notebooks_router.py#L53).

### 2.4 `POST /{notebook_id}/{cell_id}/run` — execute a code cell

> **Stage A deliverable: stub returning `501 Not Implemented`.**
> **Stage B deliverable: real sandbox-backed execution.**

The endpoint is registered in Stage A so frontends and SDKs that probe its existence (or call it expecting future support) get a structured error. Implementation of the actual Python evaluator is gated on choosing a sandbox strategy (see §2.4.4 below).

#### 2.4.1 Wire contract (both phases)

- **Auth**: `required`.
- **Path params**: `notebook_id: Uuid`, `cell_id: Uuid`.
- **Query params**: none.
- **Request body**: `application/json`, `RunCodeDataDTO`:

  | Field | Rust type | Python type | Required | Notes |
  |---|---|---|---|---|
  | `content` | `String` | `str = Field(...)` | Yes | Python source code to execute. |

- **Response body** (target shape, both phases honor the wire envelope): `200 OK`, `application/json`:
  ```json
  {
    "result": [<json-encoded stdout chunks>],
    "error":  "<traceback string>" | null
  }
  ```
  `result` is a JSON array of the values passed to the sandbox-installed `print` shim (each element is whatever `jsonable_encoder` would produce — strings, numbers, dicts). `error` is `null` on success or the formatted Python traceback as a single string on failure. **Note**: even on Python execution error, the HTTP status is `200` — the error is in-band. The HTTP status is only non-200 when the *router* fails (auth, missing notebook, bad UUID).

- **Error responses**:

  | Status | Body shape | Condition |
  |---|---|---|
  | `400` | `{"detail": [...], "body": ...}` | Validation error on `content` field. |
  | `401` | `{"detail": "Unauthorized"}` | — |
  | `404` | `{"error": "Notebook not found"}` | Same `{"error": ...}` envelope as `PUT`/`DELETE`. |
  | `501` | `{"detail": "Notebook cell execution is not implemented in this build", "code": "NOTEBOOK_RUN_NOT_IMPLEMENTED"}` | **Stage A only**: returned for every authenticated, well-formed call — even when the notebook exists. Stage B removes this. |

- **Permission gate**: ownership check via `get_notebook(notebook_id, user_id)`. The `cell_id` is **not validated against the notebook's stored cells** in Python — the cell content comes from the request body, not the DB. We replicate this; the `cell_id` is purely an addressing/telemetry value. Document this in the OpenAPI description.
- **OpenAPI**: tag `notebooks`. Path params `notebook_id: uuid`, `cell_id: uuid`. Add `x-cognee-stub: true` extension on Stage A to flag stub status to introspection tools.
- **Telemetry**: span name `cognee.api.notebooks.run_cell`. Attributes: `cognee.notebook.id`, `cognee.notebook.cell_id`, `cognee.notebook.run_outcome` (`"stubbed"` in Stage A, `"success"` / `"user_error"` in Stage B).

#### 2.4.2 Stage A stub — exact behavior

```rust
async fn run_notebook_cell(
    State(state): State<AppState>,
    Path((notebook_id, cell_id)): Path<(Uuid, Uuid)>,
    user: AuthenticatedUser,
    Json(_body): Json<RunCodeDataDTO>,
) -> Result<Response, ApiError> {
    let notebook = state.lib.notebooks().get(notebook_id, user.id).await?;
    if notebook.is_none() {
        return Ok(notebook_not_found());            // 404 {"error": "Notebook not found"}
    }
    Ok((
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "detail": "Notebook cell execution is not implemented in this build",
            "code":   "NOTEBOOK_RUN_NOT_IMPLEMENTED",
        })),
    ).into_response())
}
```

The stub still validates auth, parses the path, looks up the notebook (so a missing notebook beats the stub status — `404` first, `501` second), and validates the body — so frontends that test their request shape against `/run` see real validation errors instead of accidentally false-positive `501`s.

#### 2.4.3 Stage B — real execution

When Stage B lands, the handler swaps in the chosen execution backend and returns the `{"result": [...], "error": null|str}` envelope. The 501 fallback is removed; nothing else in the wire contract changes.

Delegation target (Stage B): `cognee_lib::notebooks::run_cell(content: &str) -> CellRunOutcome`, where:

```rust
pub struct CellRunOutcome {
    pub stdout: Vec<serde_json::Value>,   // captured prints, JSON-encoded
    pub error:  Option<String>,           // formatted Python traceback or analog
}
```

#### 2.4.4 Sub-design — sandbox strategy options

Python's [`run_in_local_sandbox`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/notebooks/operations/run_in_local_sandbox.py) is **not a sandbox**. It is `exec()` against the host process with a captured `print`. Concretely:

1. The user's code is wrapped: an `async def __user_main__()` is generated, indented, and a `run_sync(__user_main__(), running_loop)` call is appended.
2. A custom dict `environment` is created with `print` replaced by `printOutput.append`, and `cognee` injected so the user can `await cognee.add(...)`.
3. `exec(wrapped_code, environment)` runs in-process; stdout/stderr are redirected to a `StringIO`. Tracebacks are captured into `error`.

This is *trust-on-first-use*. Anyone with valid credentials can call `os.system("rm -rf /")`. Cognee accepts this because the deployment is single-tenant developer-local — the same posture as a Jupyter kernel running on the user's laptop.

The Rust port does **not** have a host Python interpreter — porting `exec()` of arbitrary Python is the entire problem. Three candidate strategies:

| Strategy | Pros | Cons | Verdict |
|---|---|---|---|
| **A. Subprocess to host `python3`** | Trivially correct (it's actual CPython). Same `cognee` Python SDK already installed in dev environments. Trivial to add timeouts (`tokio::process::Command` + `Child::kill`). | Requires CPython at runtime — a footgun for the all-Rust deployment story (Android, embedded, distroless containers). Subprocess auth/state replication is non-trivial: the sandbox needs the same DB pool / config as the parent. Adds a full `pip install cognee` to the deployment. | **Phase-2 default for self-hosted dev deployments.** Document the Python dep clearly. |
| **B. Embedded RustPython** | Pure Rust; no external interpreter. Runs in-process. | RustPython does not implement the entire CPython stdlib, and notably has incomplete `asyncio` support — Python's wrapper code uses `asyncio.set_event_loop`/`asyncio.run`, which is exactly the path RustPython is weakest on. The user's notebook code typically does `await cognee.add(...)`, which requires both `asyncio` *and* a working `cognee` Python package — neither is shippable through RustPython. | **Rejected** for cell execution; possibly viable later if cells are restricted to non-async pure-Python expressions. |
| **C. Wasm sandbox (Pyodide / wasmtime)** | True sandbox; resource-limited; OS-isolated. Runs everywhere wasmtime runs. | Pyodide ships ~10 MiB of Python wasm + stdlib; `cognee` Python package would need a wasm build, which doesn't exist. Pyodide cannot call back into the host process to reach the live cognee DB / graph / vector store, defeating the whole purpose of running cells. | **Rejected** for cell execution in Stage B. Re-evaluate if the use case shifts to "demo/teaching mode" where cells run against a stubbed cognee. |

**Stage B plan**: default to **strategy A** (subprocess), feature-gated behind a `notebooks-sandbox-subprocess` cargo feature on `cognee-http-server`. The subprocess is invoked with `python3 -c <wrapper>`, the user's content is passed via stdin, and the parent waits with a configurable timeout. Stdout is parsed line-by-line into the `result` array; stderr is captured into `error`. Subprocess CWD and env are scoped to a per-cell tempdir. `RLIMIT_AS` (memory) and `RLIMIT_CPU` (CPU seconds) caps are set on Unix.

#### 2.4.5 Python parity notes for the run endpoint

- The `\xa0` (non-breaking space) → `\n` substitution in [`run_in_local_sandbox.py:23`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/notebooks/operations/run_in_local_sandbox.py#L23) accommodates code copy-pasted from web UIs that mangles spacing. The Rust port (Stage B) must do the same substitution before passing content to the subprocess.
- Python uses `loop.run_in_executor(...)` ([`run_async`](https://github.com/topoteretes/cognee/blob/main/cognee/infrastructure/utils/run_async.py)) so the sandbox runs on a worker thread, not the request thread. The Rust subprocess approach is naturally non-blocking.
- The `result` field elements are passed through `jsonable_encoder` — strings stay as JSON strings, dicts as JSON objects, etc. Match this in the Rust subprocess parser.

## 3. Cross-cutting behavior

- **Authentication mode**: every endpoint in this router is `required`. There is no public surface.
- **Ownership scoping**: every read/write filters on `owner_id = user.id`. There is no admin override; superusers see only their own notebooks. (Python is the same; the frontend uses the per-user notebook set as a private workspace.)
- **404 envelope**: this router uses `{"error": "Notebook not found"}` instead of the standard `{"detail": "..."}`. Reproduced for compat.
- **Tutorial seeding**: lazy on first `GET /` per user. The seeded notebooks have **deterministic UUID5 ids** derived from `(NAMESPACE_OID, "Cognee Basics - tutorial 🧠")` and `(NAMESPACE_OID, "Python Development with Cognee - tutorial 🧠")`. The Rust port reads the same tutorial source files and produces the same ids — verified by a parity test. See `crates/http-server/tests/test_notebooks.rs`.
- **Tenant scope**: notebooks have no `tenant_id` column. They are bound to `owner_id` only. Future multi-tenancy work needs to decide whether to add the column; for Stage A we do not.

## 4. DTO definitions

```rust
// crates/http-server/src/dto/notebooks.rs

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Mirrors `cognee.modules.notebooks.models.Notebook` (one row).
///
/// Wire format must match Python's default SQLAlchemy → JSON serialization:
/// every column is emitted, `cells` is a JSON array, `created_at` is ISO-8601.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NotebookDTO {
    pub id:         Uuid,
    pub owner_id:   Uuid,
    pub name:       String,
    pub cells:      Vec<NotebookCellDTO>,
    pub deletable:  bool,
    pub created_at: DateTime<Utc>,
}

/// Mirrors `cognee.modules.notebooks.models.NotebookCell`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NotebookCellDTO {
    pub id:      Uuid,
    /// `"markdown"` or `"code"`. We use a string for wire compat with Python's
    /// `Literal["markdown", "code"]`; a closed Rust enum is rejected at the DTO
    /// boundary because Python tolerates unknown values silently and we want to
    /// match that on read paths.
    #[serde(rename = "type")]
    pub kind:    String,
    pub name:    String,
    pub content: String,
}

/// Mirrors the inline `NotebookData(InDTO)` Pydantic class in
/// [`get_notebooks_router.py:24-26`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/notebooks/routers/get_notebooks_router.py#L24-L26).
///
/// Python uses `Optional[str] = Field(...)` to mean "required at validation time
/// but allowed to be `None`". For Rust parity we deserialize as `Option<String>`
/// and validate non-`None` in the handler when Python's `Field(...)` would fire.
#[derive(Debug, Deserialize, ToSchema)]
pub struct NotebookDataDTO {
    pub name:  Option<String>,
    #[serde(default)]
    pub cells: Vec<NotebookCellDTO>,
}

/// Mirrors `RunCodeData(InDTO)` defined inline in
/// [`get_notebooks_router.py:63-64`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/notebooks/routers/get_notebooks_router.py#L63-L64).
#[derive(Debug, Deserialize, ToSchema)]
pub struct RunCodeDataDTO {
    pub content: String,
}

/// Stage B outcome. Wire shape: `{"result": [...], "error": null|str}`.
#[derive(Debug, Serialize, ToSchema)]
pub struct RunCodeOutcomeDTO {
    pub result: Vec<serde_json::Value>,
    pub error:  Option<String>,
}
```

Pydantic-to-Rust field mapping table:

| Python field | Rust field | Type mapping | Notes |
|---|---|---|---|
| `Notebook.id` | `NotebookDTO.id` | `UUID` → `Uuid` | — |
| `Notebook.owner_id` | `NotebookDTO.owner_id` | `UUID` → `Uuid` | — |
| `Notebook.name` | `NotebookDTO.name` | `str` → `String` | — |
| `Notebook.cells` | `NotebookDTO.cells` | `List[NotebookCell]` → `Vec<NotebookCellDTO>` | JSON-encoded column. |
| `Notebook.deletable` | `NotebookDTO.deletable` | `bool` → `bool` | — |
| `Notebook.created_at` | `NotebookDTO.created_at` | `datetime` → `DateTime<Utc>` | UTC-tagged. |
| `NotebookCell.id` | `NotebookCellDTO.id` | `UUID` → `Uuid` | — |
| `NotebookCell.type` | `NotebookCellDTO.kind` | `Literal["markdown","code"]` → `String` | Renamed via `serde(rename = "type")`. |
| `NotebookCell.name` | `NotebookCellDTO.name` | `str` → `String` | — |
| `NotebookCell.content` | `NotebookCellDTO.content` | `str` → `String` | Cell source. |
| `NotebookData.name` | `NotebookDataDTO.name` | `Optional[str] = Field(...)` → `Option<String>` | Required at validation time per parity note. |
| `NotebookData.cells` | `NotebookDataDTO.cells` | `Optional[List[NotebookCell]] = []` → `Vec<NotebookCellDTO>` (default `[]`) | — |
| `RunCodeData.content` | `RunCodeDataDTO.content` | `str = Field(...)` → `String` | — |

## 5. Implementation tasks

Stage A (CRUD + stubbed `/run`):

1. **Migration** — add a SeaORM migration `crates/database/src/migrator/m_xxxx_create_notebooks.rs` matching Python's `notebooks` table:
   - `id UUID PK`
   - `owner_id UUID NOT NULL` (index)
   - `name TEXT NOT NULL`
   - `cells JSONB NOT NULL DEFAULT '[]'::jsonb` (or `JSON` in SQLite)
   - `deletable BOOLEAN NOT NULL DEFAULT TRUE`
   - `created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`
2. **Repository** — add `crates/database/src/repositories/notebook_repository.rs` exposing `list_by_owner`, `create`, `get_by_id_and_owner`, `update`, `delete`. Behind a new `NotebookDb` trait so cross-DB (SQLite/Postgres) testing works.
3. **Cognee-lib facade** — add `cognee_lib::notebooks` module with `list_notebooks`, `create_notebook`, `update_notebook`, `delete_notebook`, plus the lazy tutorial seeder.
4. **Tutorial seeder** — port [`create_tutorial_notebooks.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/notebooks/methods/create_tutorial_notebooks.py): walk `crates/http-server/assets/tutorials/` (Rust mirror of Python's `cognee/modules/notebooks/tutorials/`), parse `cell-N.md`/`cell-N.py` into `NotebookCell` rows, derive notebook id with `Uuid::new_v5(NAMESPACE_OID, name)` to match Python.
5. **DTOs** — `crates/http-server/src/dto/notebooks.rs` per §4.
6. **Handlers** — `crates/http-server/src/routers/notebooks.rs` with `list`, `create`, `update`, `delete`, and the **stub `/run`**.
7. **OpenAPI** — `#[utoipa::path(...)]` on each handler with the `notebooks` tag.
8. **Unit tests** — DTO round-trip, 404 envelope shape, stub 501 body.
9. **Integration tests** — `crates/http-server/tests/test_notebooks.rs`:
   - Create → list → update → delete round trip.
   - First-call list seeds two tutorial notebooks; their ids are deterministic across runs.
   - `/run` returns 501 with the documented body when the notebook exists; 404 when it doesn't (404 wins over 501).
   - Per-user isolation: user A cannot read/update/delete user B's notebook (404, not 403).
10. **Cross-SDK parity** — `e2e-cross-sdk/harness/test_http_notebooks.py`:
    - List response from Python and Rust produce the same tutorial notebook ids (UUID5 deterministic).
    - Create + read-back round-trips between SDKs.
    - The unusual `{"error": "..."}` 404 envelope is byte-identical.

Stage B (sandbox `/run`):

11. Add `notebooks-sandbox-subprocess` feature to `crates/http-server/Cargo.toml`.
12. Implement `cognee_lib::notebooks::run_cell` using `tokio::process::Command` to invoke `python3` with the user code wrapped in the same async harness Python uses.
13. Add resource limits (`RLIMIT_AS`, `RLIMIT_CPU`, wallclock timeout via `tokio::time::timeout`).
14. Add `NotebookSandboxConfig` (timeout, memory cap, python binary path) plumbed through `HttpServerConfig`.
15. Add tests that drive a real subprocess (gated behind `cfg(unix)` + `python3` availability — graceful skip on CI without Python installed).

## 6. Open questions

1. **Tutorial assets** — Python ships tutorial cells as files in the package data (`cognee/modules/notebooks/tutorials/`). Where do the equivalent files live in the Rust workspace? Options: (a) duplicate the files into `crates/http-server/assets/tutorials/` and `include_dir!` them; (b) read them from a configured filesystem path at runtime; (c) skip tutorial seeding in Stage A and revisit. **Proposed**: (a), so the binary is self-contained.
2. **Empty `cells` overwrite** — Python's `PUT` does not clear the cells list when the request body has `"cells": []`. Frontends therefore *cannot* delete all cells from a notebook via this endpoint. Should we keep the bug for parity, or fix it (and break parity)? **Proposed**: keep parity; document loudly. Frontend can work around by sending a single empty markdown cell.
3. **Sandbox strategy commitment** — Stage B defaults to subprocess CPython (strategy A in §2.4.4). Is that acceptable to the deployment story for self-hosted dev (Docker, brew)? Or do we want to push harder on a wasm-isolated path even at the cost of a stubbed cognee SDK inside the sandbox?
4. **Sandbox `cognee` package availability** — when Stage B ships, the subprocess needs `pip install cognee` available so user code can do `await cognee.add(...)`. Should the Rust HTTP server image bundle CPython + the cognee Python wheel, or document it as an operator-installed prerequisite? **Proposed**: optional Docker stage that adds Python + cognee, gated behind a build arg.
5. **Sandbox auth/state propagation** — when a notebook cell calls `cognee.add(...)`, which credentials does it use? Python relies on the global config + default user. The subprocess approach would need to inherit `OPENAI_API_KEY`, DB connection strings, and a service account / API key for the running user. How do we scope this safely so a notebook can't use the operator's keys against another tenant?
6. **Tenancy retrofit** — the table has no `tenant_id`. If we later add multi-tenant notebooks, do we migrate by deriving `tenant_id` from `owner_id`'s primary tenant, or do we leave the column null and treat notebooks as user-scoped forever?

## 7. References

- Python router: [`cognee/api/v1/notebooks/routers/get_notebooks_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/notebooks/routers/get_notebooks_router.py)
- Notebook ORM model: [`cognee/modules/notebooks/models/Notebook.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/notebooks/models/Notebook.py)
- DB methods: [`cognee/modules/notebooks/methods/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/notebooks/methods)
  - [`create_notebook.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/notebooks/methods/create_notebook.py)
  - [`get_notebooks.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/notebooks/methods/get_notebooks.py)
  - [`get_notebook.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/notebooks/methods/get_notebook.py)
  - [`update_notebook.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/notebooks/methods/update_notebook.py)
  - [`delete_notebook.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/notebooks/methods/delete_notebook.py)
  - [`create_tutorial_notebooks.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/notebooks/methods/create_tutorial_notebooks.py)
- Sandbox runner: [`cognee/modules/notebooks/operations/run_in_local_sandbox.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/notebooks/operations/run_in_local_sandbox.py)
- `run_async` helper: [`cognee/infrastructure/utils/run_async.py`](https://github.com/topoteretes/cognee/blob/main/cognee/infrastructure/utils/run_async.py)
- Mount registration: [`cognee/api/client.py:269-274`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L269-L274)
- Cross-router conventions: [README.md §3](README.md#3-cross-router-conventions)
- Auth extractor specification: [../auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution)
- Telemetry attribute conventions: [../observability.md §3.3](../observability.md#33-span-instrumentation-conventions)
- Plan reference (phase 7 / advanced): [../plan.md §7](../plan.md#7-implementation-phases)
