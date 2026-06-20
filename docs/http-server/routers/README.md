# HTTP server — router reference

One reference doc per router, each covering its mount, endpoints, DTOs,
cross-cutting behavior, and Python-parity notes. All 31 routers are implemented
and shipped (cross-SDK parity verified by the `e2e-cross-sdk/harness/test_http_*.py`
suites). Section overview: [../README.md](../README.md).

Companion docs: [../architecture.md](../architecture.md) ·
[../auth.md](../auth.md) · [../pipelines.md](../pipelines.md) ·
[../websocket.md](../websocket.md) · [../observability.md](../observability.md) ·
[../tenants.md](../tenants.md).

## Routers

| Router | Mount prefix | Purpose |
|---|---|---|
| [health](health.md) | `/health` | Liveness/readiness probe. |
| [auth](auth.md) | `/api/v1/auth` | Login / logout / current-user (`/me`). |
| [auth-register](auth-register.md) | `/api/v1/auth` | User registration. |
| [auth-reset-password](auth-reset-password.md) | `/api/v1/auth` | Password reset flow. |
| [auth-verify](auth-verify.md) | `/api/v1/auth` | Email/token verification. |
| [api-keys](api-keys.md) | `/api/v1/auth/api-keys` | API-key issuance/management. |
| [users](users.md) | `/api/v1/users` | User CRUD. |
| [users-by-email](users-by-email.md) | `/api/v1/users/get-user-id` | Resolve a user id by email. |
| [add](add.md) | `/api/v1/add` | Ingest files/text into a dataset. |
| [update](update.md) | `/api/v1/update` | Re-ingest and re-cognify changed data. |
| [datasets](datasets.md) | `/api/v1/datasets` | Dataset listing/status/data. |
| [ontologies](ontologies.md) | `/api/v1/ontologies` | Ontology upload/management. |
| [cognify](cognify.md) | `/api/v1/cognify` | Build the knowledge graph (async job). |
| [memify](memify.md) | `/api/v1/memify` | Enrich the graph with triplet embeddings. |
| [remember](remember.md) | `/api/v1/remember` | Persist a memory/QA turn. |
| [improve](improve.md) | `/api/v1/improve` | Feedback-driven graph improvement. |
| [search](search.md) | `/api/v1/search` | Query across the 15 search types. |
| [recall](recall.md) | `/api/v1/recall` | Retrieve stored memories for a query. |
| [sessions](sessions.md) | `/api/v1/sessions` | Session listing / QA history. |
| [forget](forget.md) | `/api/v1/forget` | Remove specific memories/nodes. |
| [delete](delete.md) | `/api/v1/delete` | Delete data/datasets (deprecated alias). |
| [settings](settings.md) | `/api/v1/settings` | Read/update runtime settings. |
| [configuration](configuration.md) | `/api/v1/configuration` | Bulk configuration surface. |
| [permissions](permissions.md) | `/api/v1/permissions` | ACL / role / permission management. |
| [visualize](visualize.md) | `/api/v1/visualize` | Render the graph to HTML. |
| [activity](activity.md) | `/api/v1/activity` | Span/activity debug feed. |
| [sync](sync.md) | `/api/v1/sync` | Cloud sync (background job). |
| [llm](llm.md) | `/api/v1/llm` | Direct custom-prompt LLM calls. |
| [responses](responses.md) | `/api/v1/responses` | OpenAI Responses-API-shaped dispatch. |
| [notebooks](notebooks.md) | `/api/v1/notebooks` | Notebook CRUD + cell execution. |
| [checks](checks.md) | `/api/v1/checks` | Cloud connectivity checks. |

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
