# Router: configuration

The `/api/v1/configuration` router stores **per-user named configuration blobs** — opaque JSON
documents that the calling user owns. The intended use is for the frontend to persist KG schemas,
ingestion templates, and LLM system-prompt presets without giving them their own first-class
schema. Three endpoints: store-or-update by name, fetch by config ID, list-all-mine. Distinct
from `/api/v1/settings` (which is global server config) and from the user's auth profile.

Companion docs: [../architecture.md](../architecture.md),
[../auth.md](../auth.md), [../tenants.md](../tenants.md), [../observability.md](../observability.md).

## 1. Mount & file
- Mount prefix: `/api/v1/configuration` (Python: [`client.py` L242-L246](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L242-L246)).
- OpenAPI tag: `configuration`.
- Router file: `crates/http-server/src/routers/configuration.rs`.
- Python source: [`cognee/api/v1/users/routers/get_configuration_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_configuration_router.py)
  (54 lines, 3 endpoints).
- Backing model: [`PrincipalConfiguration`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/PrincipalConfiguration.py) — table `principal_configuration` with columns `id`, `owner_id`, `name`, `configuration` (JSON), `created_at`, `updated_at`.

## 2. Endpoints

### 2.1 `GET /get_user_configuration/` — list all of caller's named configurations

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none. Note the **trailing slash is part of the path** — `GET /get_user_configuration` (no slash) is a different route in FastAPI's strict-slash mode and §2.2 below claims that path; do not collapse.
- **Query params**: none.
- **Request body**: none.
- **Response body**: `200 OK`, `application/json`, `Vec<PrincipalConfigurationDTO>`. Each item is the full row (`id`, `ownerId`, `name`, `configuration`, `createdAt`, `updatedAt`) — Python serializes via `PrincipalConfiguration.to_json()` ([model L26-L34](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/PrincipalConfiguration.py#L26-L34)) which **uses camelCase keys for `ownerId`, `createdAt`, `updatedAt`** while the body fields are snake_case at the column level. We must mirror the camelCase wire format.
- **Error responses**:
  | Status | Body | Condition |
  |---|---|---|
  | 401 | `ApiError` (`InvalidCredentials`) | Missing or invalid auth credential. |
  | 500 | `ApiError` (`Internal`) | DB error reading `principal_configuration`. |
- **Side effects**: read-only.
- **Delegation target**: `cognee_lib::users::get_principal_all_configuration(principal_id = caller.id)` ([Python: `get_principal_configuration.py` L28-L51](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/methods/get_principal_configuration.py#L28-L51)).
- **Validation rules**: none.
- **Authorization checks**: implicit — `WHERE owner_id = caller.id` is the isolation rule. There is no admin override; superuser-listing of other users' configurations is not exposed.
- **OpenAPI**: tag `configuration`. Response schema `Vec<PrincipalConfigurationDTO>`.
- **Telemetry**: span `cognee.api.configuration.list`. Attrs: `user.id`, `configurations.count` after fetch.
- **Python parity notes**:
  - The path **must end with a trailing slash**. axum routers default to strict-slash matching when `Router::route("/get_user_configuration/", …)` is used; configure accordingly.
  - The list comprehension at [`get_principal_configuration.py` L51`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/methods/get_principal_configuration.py#L51) reads `[config_records.to_json() for config_records in config_records]` (the loop variable shadows the outer name; cosmetic — output unaffected).

### 2.2 `GET /get_user_configuration/{config_id}` — fetch one configuration by ID

- **Auth**: `required`.
- **Path params**: `config_id: Uuid`.
- **Query params**: none.
- **Request body**: none.
- **Response body**: `200 OK`, `application/json`, the `configuration` column verbatim (a JSON object, type `dict`). Python returns `config_record.configuration` directly (not the wrapping `to_json()`); when the record does not exist, **Python returns `{}` and HTTP 200, not 404** ([`get_principal_configuration.py` L23-L25](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/methods/get_principal_configuration.py#L23-L25)). Replicate.
- **Error responses**:
  | Status | Body | Condition |
  |---|---|---|
  | 400 | `Validation` | `config_id` is not a UUID. |
  | 401 | `InvalidCredentials` | Unauthenticated. |
  | 500 | `Internal` | DB error. |
  | (no 404) | — | Missing config returns `200 {}`. |
- **Side effects**: read-only.
- **Delegation target**: `cognee_lib::users::get_principal_configuration(config_id)`.
- **Validation rules**: `config_id` parses as a UUID.
- **Authorization checks**: **None** — Python's [`get_principal_configuration.py` L7-L25](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/methods/get_principal_configuration.py#L7-L25) does **not** check `owner_id = caller.id`. Any authenticated user who knows another user's `config_id` UUID can read its contents. **This is a confidentiality bug in Python.** We must decide whether to replicate the bug for parity or fix it; see open question §6.1. *Proposed default*: replicate in v1 with a `TODO(security)` comment; gate behind a config flag in v1.1.
- **OpenAPI**: tag `configuration`. Response is `serde_json::Value` (an arbitrary JSON object). Document the "missing → `{}`" behavior in the description.
- **Telemetry**: span `cognee.api.configuration.get`. Attrs: `user.id`, `config.id`, `found` (bool).
- **Python parity notes**:
  - The handler is annotated `response_model=dict`, so FastAPI does not coerce `None` returns. The empty-dict fallback is the *only* safety net.
  - There is no `principal_id` filter at the SQL level; this is intentional in Python's design but worth flagging in the doc as a parity-vs-security tradeoff.

### 2.3 `POST /store_user_configuration` — upsert a named configuration for the caller

- **Auth**: `required`.
- **Path params**: none.
- **Query params**: none.
- **Request body**: `application/json`, `StorePrincipalConfigurationPayloadDTO { name: String, config: serde_json::Value }`.
  - `name`: a user-scoped, non-unique, non-globally-unique identifier. Two different users may both have a configuration named `"default_llm_settings"`; the uniqueness key is `(owner_id, name)`. Within one owner, an upsert by name overwrites the prior `configuration` and bumps `updated_at` ([`store_principal_configuration.py` L36-L40](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/methods/store_principal_configuration.py#L36-L40)).
  - `config`: arbitrary JSON object. Stored as the `configuration` JSONB column.
- **Response body**: **`200 OK`** with body `null`. Python's handler is annotated `response_model=None` and has no `return`; FastAPI emits status `200` with a `null` JSON body. Rust matches verbatim — return `200 OK` with body `null`, not `204 No Content`. Strict wire parity.
- **Error responses**:
  | Status | Body | Condition |
  |---|---|---|
  | 400 | `Validation` | Invalid JSON; `name` empty after trim; `config` not a JSON object. |
  | 401 | `InvalidCredentials` | Unauthenticated. |
  | 500 | `Internal` | DB error / commit failure. |
- **Side effects**: insert or update one row in `principal_configuration` with `owner_id = caller.id`, `name = payload.name`, `configuration = payload.config`. The row's `id` is auto-generated `uuid4` on insert; on update it stays the same. Python uses **two SELECTs + commit** ("check exists, then update or insert"); replicating semantics is sufficient — implementing as `INSERT … ON CONFLICT (owner_id, name) DO UPDATE` is acceptable provided a unique index exists (open question §6.2 — Python does **not** declare such an index).
- **Delegation target**: `cognee_lib::users::store_principal_configuration(principal_id, name, configuration)`.
- **Validation rules**:
  - `name` non-empty after trim.
  - `config` is a JSON object (not a scalar or array). Python's annotation is `dict`, which Pydantic validates strictly.
- **Authorization checks**: implicit — `owner_id` is *always* the caller. Cannot store on another user's behalf.
- **OpenAPI**: tag `configuration`. `200 OK` with body `null` (Python parity).
- **Telemetry**: span `cognee.api.configuration.store`. Attrs: `user.id`, `config.name` (after redaction — names are user-supplied and may contain PII; treat as a secret per [../observability.md §5](../observability.md#5-secret-redaction) by emitting only the SHA-256 first-8-hex prefix).
- **Python parity notes**:
  - **`Form(...)` in the DTO is a Pydantic typing artifact** ([`get_configuration_router.py` L21-L23](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_configuration_router.py#L21-L23)). The class uses `name: str = (Form(..., description=...),)` — note the trailing comma — which makes the *default value* a `(FieldInfo,)` tuple. FastAPI ignores the default when the field is required via Pydantic's `BaseModel`. Effective behavior: this is a JSON body, **not multipart**. We model as JSON.
  - `name` is **not URL-safe-restricted**. Spaces, slashes, unicode are all allowed.
  - There is no per-user limit on the number of stored configurations.
- **Concurrency**: the SELECT-then-UPDATE-or-INSERT in Python is racy — two concurrent stores with the same `(owner_id, name)` can both insert. Without a unique index this surfaces as duplicate rows that the GET endpoints return both of. Open question §6.2.

## 3. Cross-cutting behavior

- **Auth gate only**: every endpoint requires `AuthenticatedUser`. There is no permission-resolution call to `PermissionsRepository`; the table's `owner_id` column is the per-row isolation key.
- **`PrincipalConfiguration` is principal-keyed, not just user-keyed**: the FK is `owner_id → principals.id`, so in principle a tenant or role could own configurations. The router only ever uses `caller.id` (a user UUID), but the underlying table supports broader semantics for future use ([`PrincipalConfiguration.py` L13-L17](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/PrincipalConfiguration.py#L13-L17), and the polymorphic principal model in [../tenants.md §3.1](../tenants.md#31-principals-super-table)).
- **JSON column**: `configuration` is `JSONB` on Postgres / `JSON` on SQLite. SeaORM uses `serde_json::Value` for the column type; we re-serialize on read.
- **No per-tenant isolation** — configurations are owned by the user, not the tenant. Switching tenants does not change the visible configuration set.
- **camelCase vs snake_case**: the `to_json()` serializer mixes `id`, `name`, `configuration` (snake) with `ownerId`, `createdAt`, `updatedAt` (camel). Replicate exactly (use `#[serde(rename = "ownerId")]` etc.). Do **not** apply `rename_all = "camelCase"` globally — that would convert `name`/`configuration` too.
- **Error mapping**: standard ([../architecture.md §9](../architecture.md#9-error-handling)). UUID parse errors → `400`; auth errors → `401`; DB errors → `500`.
- **Telemetry span names**: `cognee.api.configuration.{list,get,store}`.

## 4. DTO definitions

```rust
// crates/http-server/src/dto/configuration.rs
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Body for `POST /store_user_configuration`. Mirrors Python's
/// `StorePrincipalConfigurationPayloadDTO`, which is a JSON body despite the
/// `Form(...)` typing artifact — see Python parity notes in §2.3.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct StorePrincipalConfigurationPayloadDTO {
    /// User-scoped name. Two users can each have a configuration named
    /// `"default"`; uniqueness is `(owner_id, name)`. Non-empty after trim.
    pub name: String,
    /// Opaque JSON object stored verbatim in the `configuration` column.
    pub config: serde_json::Value,
}

/// Response item for `GET /get_user_configuration/`. Mirrors
/// `PrincipalConfiguration.to_json()` — note the **mixed snake/camel keys**.
/// Python's serializer at
/// [`PrincipalConfiguration.py` L26-L34](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/PrincipalConfiguration.py#L26-L34)
/// emits `id`, `name`, `configuration` in snake_case but `ownerId`,
/// `createdAt`, `updatedAt` in camelCase. We replicate exactly.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PrincipalConfigurationDTO {
    pub id: Uuid,
    #[serde(rename = "ownerId")]
    pub owner_id: Uuid,
    pub name: String,
    pub configuration: serde_json::Value,
    #[serde(rename = "createdAt")]
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// `null` when the row has never been updated since creation.
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}
```

The `GET /get_user_configuration/{config_id}` response does **not** use `PrincipalConfigurationDTO`
— it returns the bare `configuration` JSON object (`serde_json::Value`). Document this asymmetry in
the OpenAPI spec.

## 5. Implementation tasks

1. Add DTO structs in `crates/http-server/src/dto/configuration.rs` per §4.
2. Add a SeaORM entity for `principal_configuration` in `crates/database/src/entities/principal_configuration.rs` if it does not yet exist.
3. Add `cognee_lib::users::{store_principal_configuration, get_principal_configuration, get_principal_all_configuration}` façades wrapping the SeaORM queries.
4. Add handlers in `crates/http-server/src/routers/configuration.rs`. All three are
   `#[tracing::instrument(skip(state, payload))]` (skip the body to avoid logging arbitrary user JSON).
5. OpenAPI annotations; document the `200 {}` no-row behavior on `GET /…/{config_id}` and the
   `200` body-`null` choice on `POST` explicitly (Python parity, not `204`).
6. Unit tests:
   - DTO serialization emits exactly `id`, `ownerId`, `name`, `configuration`, `createdAt`, `updatedAt` keys (camelCase preserved).
   - `name = ""` → `400`.
   - `config = "scalar"` → `400`.
7. Integration tests in `crates/http-server/tests/test_configuration.rs`:
   - Store → list returns one item; store same `name` again → list still returns one item with bumped `updatedAt`.
   - Two users each storing `"default"` → each user sees only their own row.
   - `GET /…/{nonexistent_uuid}` → `200` with body `{}`.
   - **Cross-user fetch**: user A's GET on user B's `config_id` returns user B's data (parity with Python; flag in the doc as a known issue).
8. Cross-SDK parity test in `e2e-cross-sdk/harness/test_http_configuration.py`: Python stores a
   configuration; Rust reads the list and individual fetch; assert byte-equal modulo timestamp
   round-trip.

## 6. Open questions

1. **Cross-user read on `GET /get_user_configuration/{config_id}`** — Python permits any authenticated user to fetch any configuration by ID (no `owner_id` check). Rust matches verbatim — same lack of authorization. The behavior is a Python confidentiality bug; we replicate it for strict wire parity. Operators wanting isolation must apply it at a reverse-proxy layer.
2. **Unique index on `(owner_id, name)`** — Python relies on a SELECT-then-UPDATE-or-INSERT pattern without a unique constraint, allowing race-condition duplicates. Rust matches: no unique constraint, same SELECT-then-UPSERT pattern, same race window. No additive schema change.
3. **Trailing-slash strictness** — Python (FastAPI) is strict by default. Rust enables axum strict mode for this router so `GET /get_user_configuration` (no slash) returns `404` matching Python.
4. **`POST` status code: 200 vs 204** — Python returns `200` with body `null`. Rust matches: `200` with body `null`, not `204 No Content`.
5. **Per-user limit** — Python has no cap on configurations per user. Rust matches: no application-level cap. The global body-size middleware cap ([../architecture.md §8](../architecture.md#8-middleware-stack)) applies to individual writes.
6. **`name` validation** — Python accepts arbitrary strings (including newlines, control chars, slashes). Rust matches: no validation beyond non-empty after trim. Strict wire parity.
7. **Configuration size limit** — Python has no per-route cap. Rust matches: only the global `100 MiB` middleware cap ([../architecture.md §8](../architecture.md#8-middleware-stack)) applies.

## 7. References

- Python router: [`get_configuration_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_configuration_router.py).
- Python implementations:
  [`store_principal_configuration.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/methods/store_principal_configuration.py),
  [`get_principal_configuration.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/methods/get_principal_configuration.py).
- Python model: [`PrincipalConfiguration.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/PrincipalConfiguration.py).
- Mount in Python: [`client.py` L242-L246](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L242-L246).
- Auth extractor: [../auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution).
- Polymorphic principal model: [../tenants.md §3.1](../tenants.md#31-principals-super-table).
- Error mapping: [../architecture.md §9](../architecture.md#9-error-handling).
- Telemetry conventions: [../observability.md §3.4](../observability.md#34-span-name-conventions), [§5 secret redaction](../observability.md#5-secret-redaction).
