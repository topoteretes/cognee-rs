# HTTP Server — Per-Router Specs

Each FastAPI router in [`cognee/api/v1/`](https://github.com/topoteretes/cognee/tree/main/cognee/api/v1) gets its own design document under this directory. This README is the **index, status table, and per-doc template** — write a new file per router as you take it on; do not lump multiple routers into one file.

Companion docs: [../plan.md](../plan.md), [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../pipelines.md](../pipelines.md), [../websocket.md](../websocket.md), [../observability.md](../observability.md), [../tenants.md](../tenants.md), [../e2e-parity.md](../e2e-parity.md).

## 1. Status table

One row per router. Update the row in the same PR that lands or changes the underlying spec.

### Legend

- **Draft** — written but not yet validated against code.
- **Approved** — reviewed; ready to implement against.
- **In Progress** — implementation underway.
- **Done** — implementation landed; cross-SDK parity tests pass.

| # | Router | Mount prefix | Doc | Status |
|---|---|---|---|---|
| 1 | health | `/health` | [health.md](health.md) | **Done** |
| 2 | auth (login/logout/me) | `/api/v1/auth` | [auth.md](auth.md) | **Draft** |
| 3 | auth — register | `/api/v1/auth` | [auth-register.md](auth-register.md) | **Draft** |
| 4 | auth — reset-password | `/api/v1/auth` | [auth-reset-password.md](auth-reset-password.md) | **Draft** |
| 5 | auth — verify | `/api/v1/auth` | [auth-verify.md](auth-verify.md) | **Draft** |
| 6 | api_keys | `/api/v1/auth/api-keys` | [api-keys.md](api-keys.md) | **Draft** |
| 7 | users | `/api/v1/users` | [users.md](users.md) | **Draft** |
| 8 | users — user_id_by_email | `/api/v1/users/get-user-id` | [users-by-email.md](users-by-email.md) | **Draft** |
| 9 | add | `/api/v1/add` | [add.md](add.md) | **Draft** |
| 10 | update | `/api/v1/update` | [update.md](update.md) | **Draft** |
| 11 | datasets | `/api/v1/datasets` | [datasets.md](datasets.md) | **Draft** |
| 12 | ontologies | `/api/v1/ontologies` | [ontologies.md](ontologies.md) | **Draft** |
| 13 | cognify | `/api/v1/cognify` | [cognify.md](cognify.md) | **Draft** |
| 14 | memify | `/api/v1/memify` | [memify.md](memify.md) | **Draft** |
| 15 | remember | `/api/v1/remember` | [remember.md](remember.md) | **Draft** |
| 16 | improve | `/api/v1/improve` | [improve.md](improve.md) | **Draft** |
| 17 | search | `/api/v1/search` | [search.md](search.md) | **Draft** |
| 18 | recall | `/api/v1/recall` | [recall.md](recall.md) | **Draft** |
| 19 | forget | `/api/v1/forget` | [forget.md](forget.md) | **Draft** |
| 20 | delete (deprecated) | `/api/v1/delete` | [delete.md](delete.md) | **Draft** |
| 21 | settings | `/api/v1/settings` | [settings.md](settings.md) | **Draft** |
| 22 | configuration | `/api/v1/configuration` | [configuration.md](configuration.md) | **Draft** |
| 23 | permissions | `/api/v1/permissions` | [permissions.md](permissions.md) | **Draft** |
| 24 | visualize | `/api/v1/visualize` | [visualize.md](visualize.md) | **Draft** |
| 25 | activity | `/api/v1/activity` | [activity.md](activity.md) | **Draft** |
| 26 | sync | `/api/v1/sync` | [sync.md](sync.md) | **Draft** |
| 27 | llm | `/api/v1/llm` | [llm.md](llm.md) | **Draft** |
| 28 | responses | `/api/v1/responses` | [responses.md](responses.md) | **Draft** |
| 29 | notebooks | `/api/v1/notebooks` | [notebooks.md](notebooks.md) | **Draft** |
| 30 | checks (cloud) | `/api/v1/checks` | [checks.md](checks.md) | **Draft** |

The implementation phases in [../plan.md](../plan.md#4-implementation-phases) drive the order in which these docs need to be ready.

## 2. Per-doc template

Every per-router doc must use the structure below. Fill all sections; if a section legitimately doesn't apply, write "Not applicable" and one sentence why. Don't drop sections silently.

```markdown
# Router: <name>

Brief one-paragraph summary: what the router does, who calls it, and the one or two sentences that
distinguish it from related routers (e.g. `/api/v1/recall` vs `/api/v1/search`).

Companion docs: [../plan.md](../plan.md), [../architecture.md](../architecture.md), [../auth.md](../auth.md),
and any sub-doc relevant to this router (e.g. [../pipelines.md](../pipelines.md) for routers that dispatch jobs).

## 1. Mount & file
- Mount prefix: `/api/v1/<name>`
- Router file: `crates/http-server/src/routers/<name>.rs`
- Python source: `cognee/api/v1/<name>/routers/get_<name>_router.py`

## 2. Endpoints

For each endpoint, one sub-section. Order by HTTP method (GET → POST → PATCH → PUT → DELETE) then path.

### 2.x `<METHOD> <path>` — short verb-phrase summary

- **Auth**: `required` | `optional` | `none`. (Cite the extractor used: `AuthenticatedUser`, `OptionalAuthenticatedUser`, etc.)
- **Path params**: list with types.
- **Query params**: list with types and defaults.
- **Request body**: media type + DTO struct name + field-level breakdown (Rust types, Python types, optional/default).
- **Response body**: media type + DTO struct name + field-level breakdown. Note the `200`/`201`/`202`/`204` choice.
- **Error responses**: a table of `status` × `body shape` × `condition`. Use the canonical `ApiError` variants from [../architecture.md §9](../architecture.md#9-error-handling).
- **Side effects**: writes to relational DB, graph DB, vector DB, file storage, broadcast channels, etc.
- **Delegation target**: which `cognee_lib::*` function the handler calls. The handler itself should be thin.
- **Validation rules**: cross-field rules that go beyond serde defaults (Pydantic `model_validator` analogs).
- **Rate / size limits**: body size, request rate, etc.
- **OpenAPI**: any non-default tags, security overrides, response schemas.
- **Telemetry**: span name (e.g. `cognee.api.<name>.<verb>`), important attributes from [../observability.md §3.3](../observability.md#33-span-instrumentation-conventions).
- **Python parity notes**: behaviors that look quirky but match Python on purpose, with a citation.

## 3. Cross-cutting behavior
Anything that applies to all endpoints in this router: shared input validation, shared error
mapping, shared authorization rule (e.g. "all routes require permission `X` on the target dataset").

## 4. DTO definitions
The DTO structs in full, in Rust, with `#[derive]` attributes and field comments where the type is
non-obvious. Map each Pydantic field name to the Rust field name. Note `serde(rename_all =
"snake_case")` if needed for compat.

## 5. Implementation tasks
Numbered list of subtasks for the implementor:
1. Add DTO structs in `crates/http-server/src/dto/<name>.rs`.
2. Add handler functions in `crates/http-server/src/routers/<name>.rs`.
3. Add OpenAPI annotations.
4. Add unit tests in the same file.
5. Add integration tests in `crates/http-server/tests/test_<name>.rs`.
6. Add cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_<name>.py`.

## 6. Open questions
Items the implementor should resolve before merging, with proposed answers when possible.

## 7. References
- Python source path(s).
- Any other doc that constrains this router (auth, pipelines, websocket, observability, tenants).
```

## 3. Cross-router conventions

These apply to **every** router doc and don't need to be repeated in each.

### 3.1 Error envelope

The Rust server returns the Python-shaped `ApiError` JSON described in [../architecture.md §9](../architecture.md#9-error-handling) and pinned in [../auth.md §8.1](../auth.md#81-apiv1authlogin--post). The canonical envelope is `{"detail": "..."}` (or `{"detail": [validation errors], "body": ...}` for 422s). Per-router specs call out *which* `ApiError` variant fires for *which* condition.

**Documented Python-parity envelope deviations** (these routers ship non-canonical shapes verbatim because Python does):

| Router | Endpoint(s) | Envelope shape | Reason |
|---|---|---|---|
| api-keys | `POST /api/v1/auth/api-keys`, `DELETE /api/v1/auth/api-keys/{id}` | `{"error": {"message": "..."}}` | Unique to this router; Python wraps API-key errors in a nested `error.message` object. Encoded via dedicated `ApiError::ApiKeyEnvelope(String)` variant. |
| sync | `POST /api/v1/sync`, `GET /api/v1/sync/status` | `{"error": "..."}` (top-level), plus `SyncConflictDTO { error, details }` for `409` | Python's sync router emits `{error}` for catch-all 409s and a richer `{error, details}` shape for in-progress conflicts. |
| checks | `POST /api/v1/checks/connection` | `{"detail": "...", "name": "..."}` | Cloud `CogneeApiError`-flavored envelope (typo `CloudConnnectionError` is preserved verbatim). |
| health | `GET /health`, `GET /health/detailed` | `{"status": "...", "reason": "..."}` (shallow) and per-component dicts (detailed) | Health endpoints predate the canonical envelope; their bodies are bespoke shapes. |
| forget, ontologies, datasets (raw download), recall, visualize, llm | various 4xx/5xx | mix of `{error}`, `{error, detail}`, `{error, hint}` | Inherited from Python; documented per-router. |

If you find yourself wanting to emit a non-canonical shape, first check whether the Python source for that endpoint already emits one. If yes, add it to this table; if no, use the canonical `{"detail": "..."}` shape.

### 3.2 Authentication declaration

Every endpoint declares one of three modes:

| Mode | Extractor | Semantics |
|---|---|---|
| `required` | `AuthenticatedUser` | 401 if no credential present (or default user when `REQUIRE_AUTHENTICATION=false`). |
| `optional` | `OptionalAuthenticatedUser` | Reads the credential if present; never errors. |
| `none` | (no extractor) | Public endpoint — `/health`, `/`, login routes. Document why. |

### 3.3 DTO naming

Rust DTO names match Python's Pydantic class names (e.g. `CognifyPayloadDTO`, `SearchPayloadDTO`) so cross-SDK code reviewers find their bearings instantly. Use `#[derive(Deserialize, Serialize, ToSchema)]` and `#[serde(rename_all = "snake_case")]` to match Python's snake_case wire format.

### 3.4 Pagination

Python's existing endpoints **do not paginate** — most return `LIMIT 50` or all rows. We replicate that for compat. New endpoints that need pagination should mention it explicitly and propose a shape (cursor vs offset) in their open questions.

### 3.5 ID parameters

UUIDs are accepted both in path (`/{id}`) and as query/body fields. Use `Path<Uuid>` extractors. Reject invalid UUIDs with `400 {"detail": "<error>"}`. Python's [`RequestValidationError`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L165-L176) handler does the same; our custom `Json` extractor (see [../architecture.md §10](../architecture.md#10-request-validation)) reproduces it.

### 3.6 Background-job endpoints

Endpoints that accept `run_in_background: bool` use the `cognee_core::PipelineRunRegistry` machinery from [../pipelines.md](../pipelines.md). Per-router docs should:

- State the `pipeline_name` value used (e.g. `"cognify_pipeline"`, `"memify_pipeline"`).
- State the response shape for both `run_in_background=true` and `=false`.
- State the pipeline-run telemetry attributes recorded.

### 3.7 Multipart endpoints

Endpoints that accept `multipart/form-data` (`/add`, `/update`, `/remember`, `/ontologies`) use `axum::extract::Multipart`. Per-router docs must:

- List every part name and content type.
- State the body-size limit if non-default (default 100 MiB, see [../architecture.md §8](../architecture.md#8-middleware-stack)).
- State whether parts stream to disk or buffer in memory.

### 3.8 Permission gates

Endpoints that touch a dataset typically check a permission via the `PermissionsRepository` from [../tenants.md §9](../tenants.md#9-repository-surface). Per-router docs state the required `(permission, dataset_id)` pair for each handler.

### 3.9 Telemetry

Every handler is wrapped in `#[tracing::instrument]` with a span name `cognee.api.<router>.<verb>`. See [../observability.md §3.4](../observability.md#34-span-name-conventions). Per-router docs only need to mention the *additional* attributes recorded beyond the defaults.

### 3.10 OpenAPI

Each endpoint gets a `#[utoipa::path(...)]` annotation declaring tags, parameters, request body, responses, and security. Tags match the Python `tags=[...]` lists in [`client.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py).

## 4. Suggested writing order

When creating per-router docs, prioritize as the implementation plan suggests ([../plan.md §4](../plan.md#4-implementation-phases)):

1. **P0** (foundation): `health`.
2. **P1** (auth): `auth`, `auth-register`, `auth-reset-password`, `auth-verify`, `api-keys`, `users`, `users-by-email`.
3. **P2** (write path): `add`, `datasets`, `ontologies`, `update`, `delete`, `forget`.
4. **P3** (pipelines + WS): `cognify`, `memify`, `remember`, `improve`.
5. **P4** (read path): `search`, `recall`, `llm`, `visualize`.
6. **P5** (admin): `permissions`, `settings`, `configuration`.
7. **P6** (observability): `activity`, `sync`, `checks`.
8. **P7** (advanced): `notebooks`, `responses`.

Each per-router doc lands as part of the PR that implements that router. The doc's status flips through `Draft → Approved → In Progress → Done` in step with the code.

## 5. References

- Python router files: [`cognee/api/v1/<name>/routers/`](https://github.com/topoteretes/cognee/tree/main/cognee/api/v1).
- Cross-SDK parity test files: `e2e-cross-sdk/harness/test_http_<name>.py` (see [../e2e-parity.md §5](../e2e-parity.md#5-test-inventory)).
- Implementation phases: [../plan.md §4](../plan.md#4-implementation-phases).
