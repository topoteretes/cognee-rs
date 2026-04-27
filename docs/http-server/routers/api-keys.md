# Router: api-keys

Per-user API-key management. Lets an authenticated user list, create, and delete the keys they use to authenticate against cognee from CLIs, scripts, and SDKs. Keys grant the same authority as the owning user and are bounded to 10 per user. The companion auth doc ([../auth.md §5](../auth.md#5-api-keys)) covers the at-rest storage model, hash policy, lookup semantics, and limits; this doc covers the management endpoints.

The router is mounted alongside `/api/v1/auth` (despite being a different Python source file) because it shares the auth wire surface; users see it as the "API key" tab on the auth UI.

Companion docs: [../plan.md](../plan.md), [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../tenants.md](../tenants.md), [auth.md](auth.md).

## 1. Mount & file

- Mount prefix: `/api/v1/auth/api-keys` (full URL).
- Router file: `crates/http-server/src/routers/api_keys.rs`.
- Python source: [`cognee/api/v1/api_keys/routers/get_api_key_management_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/api_keys/routers/get_api_key_management_router.py).
- Mounted in: [`cognee/api/client.py:218`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L218) — `app.include_router(get_api_key_management_router(), prefix="/api/v1/auth", tags=["auth"])`. The router defines its routes with the `/api-keys` prefix internally, so the full path is `/api/v1/auth/api-keys`.
- Helper modules:
  - [`cognee/modules/users/api_key/create_api_key.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/api_key/create_api_key.py) — `secrets.token_hex(32)` + 10-key limit.
  - [`cognee/modules/users/api_key/delete_api_key.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/api_key/delete_api_key.py).
  - [`cognee/modules/users/api_key/get_api_keys.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/api_key/get_api_keys.py).
  - [`cognee/modules/users/api_key/hash_api_key.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/api_key/hash_api_key.py) — SHA-256 + `HASH_API_KEY` env toggle.
  - [`cognee/modules/users/api_key/exceptions.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/api_key/exceptions.py) — error types and message strings.
- Storage table: `user_api_key`. Schema in [../auth.md §11](../auth.md#11-database-schema-seaorm-migration).

## 2. Endpoints

### 2.1 `GET /api/v1/auth/api-keys` — list the caller's API keys

- **Auth**: `required` (`AuthenticatedUser`). Source: [`get_api_key_management_router.py:25-26`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/api_keys/routers/get_api_key_management_router.py#L25-L26).
- **Path params**: none.
- **Query params**: none.
- **Request body**: none.
- **Response body** (`200 OK`, `application/json`): JSON array of `ApiKeyListItemDTO`:

  ```json
  [
    {
      "key":   "<masked-or-raw>",
      "label": "a1b2c3d4****",
      "name":  "<str>" | null,
      "id":    "<uuid>"
    },
    ...
  ]
  ```

  - When `HASH_API_KEY=false` (default — matches Python): `key` is the raw stored value (the full 64-hex string). Source: [`get_api_key_management_router.py:48-56`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/api_keys/routers/get_api_key_management_router.py#L48-L56).
  - When `HASH_API_KEY=true` (opt-in — same env var as Python): `key` is the literal string `"************"` (12 asterisks). Source: [`get_api_key_management_router.py:39-47`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/api_keys/routers/get_api_key_management_router.py#L39-L47).
  - `label` is always populated as `<first 8 hex chars of original raw key> + "****"` — generated at creation time and stored alongside the key.
  - `name` is the user-supplied display name (nullable).
  - `id` is the `user_api_key.id` UUID.

  Empty list when the user has no keys. Order is the database default (insert order, by `id`); no explicit `ORDER BY` in Python — replicate.

- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | No / invalid / expired credential. |
  | `500` | `{"error": {"message": "Failed to query API keys for user <id>."}}` | Underlying DB error. Note the unusual `error.message` envelope used by API-key endpoints — see §3. The Python `ApiKeyQueryError` in [`exceptions.py:15-19`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/api_key/exceptions.py#L15-L19) carries the message string but Python's get-handler does not catch it; it propagates as a 500. We match by mapping `ApiError::Internal` to a generic `{"detail": "Internal server error"}`. (The Python wire shape of unhandled exceptions is FastAPI's default `{"detail": "Internal Server Error"}`.) |

- **Side effects**: pure read.
- **Delegation target**: `auth::api_keys::list_api_keys(state, user_id) -> Result<Vec<UserApiKey>, DbError>`. The handler maps the result to `ApiKeyListItemDTO`, applying the `HASH_API_KEY` mask logic at the wire layer.
- **Validation rules**: none.
- **Rate / size limits**: defaults from [../architecture.md §8](../architecture.md#8-middleware-stack).
- **OpenAPI**: `tags = ["auth"]`. Operation id: `auth:list_api_keys`. Response schema: `array<ApiKeyListItemDTO>`.
- **Telemetry**: span `cognee.api.auth.api_keys.list`. Attributes: `user.id`, `count = <len>`.
- **Python parity notes**:
  - The endpoint sends a telemetry event `"Api Key Management API Endpoint Invoked"` via `send_telemetry(...)`. Source: [`get_api_key_management_router.py:27-33`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/api_keys/routers/get_api_key_management_router.py#L27-L33). Rust has no PostHog telemetry layer in this phase; we emit the equivalent as a `tracing::info!` and document the missing send. Tracked in open questions.
  - The masked-key sentinel is exactly 12 asterisks. Do not change to 16 or `"<redacted>"` — clients may pattern-match.

### 2.2 `POST /api/v1/auth/api-keys` — create a new API key

- **Auth**: `required`.
- **Path params**: none.
- **Query params**: none.
- **Request body**: `application/json`, `ApiKeyCreationPayloadDTO`. Pydantic source: [`get_api_key_management_router.py:18-19`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/api_keys/routers/get_api_key_management_router.py#L18-L19):

  | JSON field | Type | Required | Default | Notes |
  |---|---|---|---|---|
  | `name` | `String` | no | `null` | Display label for the key (e.g. `"my laptop"`). Server stores as-is. |

- **Response body** (`200 OK`, `application/json`): a single `ApiKeyCreatedDTO`:

  ```json
  {
    "key":   "<raw 64-char hex>",
    "label": "<8 hex chars>****",
    "name":  "<str>" | null,
    "id":    "<uuid>"
  }
  ```

  Source: [`get_api_key_management_router.py:73-80`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/api_keys/routers/get_api_key_management_router.py#L73-L80).

  - `key` is the **raw, unmasked, freshly generated** 256-bit hex string. **Returned exactly once** — the client must persist it; subsequent `GET /api-keys` calls will mask it (when `HASH_API_KEY=true`). Even when `HASH_API_KEY=false`, the row stored in the DB is the same value, but treat the `POST` response as the single source of truth for the client.
  - `label` is `key[..8] + "****"`. Source: [`create_api_key.py:35`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/api_key/create_api_key.py#L35).
  - `name` is the input `name` (nullable).
  - `id` is the `user_api_key.id` UUID generated at insert time.

  Status: `200`, **not** `201` — Python uses default 200 (no explicit `status_code=` on the route). Replicate.

- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | No / invalid / expired credential. |
  | `400` | `{"error": {"message": "You have reached the maximum number of API keys."}}` | User already has 10 keys. The `error.message` envelope is unique to the API-keys router. Source: [`get_api_key_management_router.py:82-83`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/api_keys/routers/get_api_key_management_router.py#L82-L83) catches `ApiKeyCreationError` and returns `JSONResponse(status_code=400, content={"error": {"message": error.message}})`. |
  | `400` | `{"error": {"message": "Failed to create API key, please try again."}}` | DB insert failure. Same `error.message` envelope. Source: [`create_api_key.py:50-53`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/api_key/create_api_key.py#L50-L53). |
  | `400` | `{"detail": [...], "body": {...}}` | Pydantic-style validation: malformed JSON. (No required fields beyond an optional `name`, so a missing-field 400 is unlikely.) |

  **Important**: the API-keys router uses an unusual `{"error": {"message": "..."}}` shape for application-level errors, instead of the cognee-wide `{"detail": "..."}` shape. This is **deliberate** — Python wraps it differently because `JSONResponse(status_code=400, content={"error": ...})` is what the route returns directly. We **must** match exactly (clients parse `error.message`). See [../auth.md §8.10](../auth.md#810-apiv1authapi-keys--post).

- **Side effects**:
  - Inserts a row into `user_api_key` with `(user_id, api_key=prepared_value, label, name)`. `prepared_value` is `sha256_hex(raw)` when `HASH_API_KEY=true`, raw otherwise. Source: [`create_api_key.py:33-49`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/api_key/create_api_key.py#L33-L49).
  - Generates `raw` via `secrets.token_hex(32)` in Python = 64 hex chars. Rust equivalent: `rand::random::<[u8; 32]>()` or `OsRng::fill_bytes(&mut [u8; 32])`, then hex-encode. See [../auth.md §5](../auth.md#5-api-keys).
  - **Returns the raw key to the client** even when stored hashed. The function explicitly does `user_api_key.api_key = api_key` (raw) before returning the SQLAlchemy object so the response carries the raw value. Source: [`create_api_key.py:48`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/api_key/create_api_key.py#L48). Replicate exactly.

- **Delegation target**:
  - `auth::api_keys::create(state, user_id, name) -> Result<NewApiKey, ApiKeyError>`:
    1. Count existing keys. If >= `cfg.max_api_keys_per_user` (default 10), return `Err(ApiKeyError::LimitReached)`.
    2. Generate raw key (32 random bytes → 64 hex chars).
    3. Compute `prepared = sha256_hex(raw)` if `cfg.hash_api_key` else `raw`.
    4. Compute `label = &raw[..8].to_owned() + "****"`.
    5. Insert row.
    6. Return `NewApiKey { id, raw_key, label, name }`.
  - Handler maps `ApiKeyError::LimitReached` → 400 with `error.message` envelope; `ApiKeyError::DbInsert` → 400 with `error.message` envelope ("Failed to create API key, please try again.").

- **Validation rules**:
  - `name` length cap: enforce `<= 255` chars to prevent abuse. Python has no explicit cap; we add one as a defensive measure. (Open question: do we keep this?)
  - `name`: trim trailing whitespace; empty string after trim becomes `None`. Python does not trim — replicate exactly (no trim).
- **Rate / size limits**: defaults.
- **OpenAPI**: `tags = ["auth"]`. Operation id: `auth:create_api_key`. Request body: `ApiKeyCreationPayloadDTO`. Responses: `200 ApiKeyCreatedDTO`, `400 ApiKeyErrorEnvelopeDTO` (the `{error: {message}}` shape).
- **Telemetry**: span `cognee.api.auth.api_keys.create`. Attributes: `user.id`, `name_set = bool` (do not log the name itself; usernames-as-labels can leak PII), `result = "success" | "limit_reached" | "db_error"`. **Never log the raw key, the label, or the SHA-256 hash.**
- **Python parity notes**:
  - The 200-on-success / 400-on-error split with the unique `error.message` envelope is a quirk of this router only. Do not propagate to other endpoints.
  - The 10-key limit is config-driven via Pydantic `BaseSettings` in [`create_api_key.py:16-22`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/api_key/create_api_key.py#L16-L22). Rust: read from `AuthContext.max_api_keys_per_user` per [../auth.md §7](../auth.md#7-authconfig).
  - The label format (`first8 + "****"`) is exact — do not change to `"key_a1b2c3d4..."` or similar.

### 2.3 `DELETE /api/v1/auth/api-keys/{api_key_id}` — delete a key

- **Auth**: `required`.
- **Path params**:
  - `api_key_id: Uuid` — the key's `id` (not the raw key value).
- **Query params**: none.
- **Request body**: none.
- **Response body** (`200 OK`, `application/json`): the value returned by `delete_api_key()`. **Python's [`delete_api_key.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/api_key/delete_api_key.py) does not return anything explicit** — it implicitly returns `None`, which FastAPI serializes as `null`. Source: [`get_api_key_management_router.py:97-99`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/api_keys/routers/get_api_key_management_router.py#L97-L99). We emit `null` body, `200 OK`, content-type `application/json`. **Note**: [../auth.md §8.11](../auth.md#811-apiv1authapi-keysapi_key_id--delete) says "deletion status payload returned by `delete_api_key()`" — that is misleading; the function returns nothing. This doc is canonical: response body is `null`.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | No / invalid / expired credential. |
  | `400` | `{"detail": [...], "body": {...}}` | `api_key_id` is not a valid UUID — Pydantic path-param validation. |
  | `500` | `{"detail": "Internal Server Error"}` (Python default) | Python raises `ApiKeyDeletionError` ("No API key found for user <id>." or "Failed to delete API key for user <id>."), which is **not** caught by the route handler. It propagates as a 500 via FastAPI's default exception handler. Source: [`delete_api_key.py:19-34`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/api_key/delete_api_key.py#L19-L34). We **match this quirk** for compat: missing key = 500, not 404. See open questions for whether to fix. |

- **Side effects**:
  - Deletes the row matching `(id = api_key_id, user_id = caller.id)`. The `user_id` filter ensures a user cannot delete another user's key by guessing its UUID. Source: [`delete_api_key.py:21`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/api_key/delete_api_key.py#L21).
  - On DB failure, rolls back and re-raises.

- **Delegation target**:
  - `auth::api_keys::delete(state, user_id, api_key_id) -> Result<(), ApiKeyError>`:
    1. `DELETE FROM user_api_key WHERE id = ? AND user_id = ?`.
    2. If 0 rows affected, return `Err(ApiKeyError::NotFound)`.
    3. Else `Ok(())`.
  - Handler maps `NotFound` → `ApiError::Internal(...)` (matching Python's accidental 500). Tracked as a parity quirk.

- **Validation rules**:
  - `api_key_id` must parse as a UUID. Invalid UUIDs → 400 from the path extractor (matches Python's Pydantic path-param validator).
- **Rate / size limits**: defaults.
- **OpenAPI**: `tags = ["auth"]`. Operation id: `auth:delete_api_key`. Path param: `api_key_id: uuid`. Response: `200` with `null` body schema.
- **Telemetry**: span `cognee.api.auth.api_keys.delete`. Attributes: `user.id`, `api_key_id`, `result = "success" | "not_found" | "db_error"`.
- **Python parity notes**:
  - The user-scoping (`WHERE user_id = ?`) is the only authorization control. There is no separate "owner" check beyond this.
  - The "delete missing key returns 500" behavior is a Python bug that nonetheless determines the wire contract. Replicate; document an open question to fix in a coordinated change across both stacks.

## 3. Cross-cutting behavior

- **Authentication**: every endpoint requires `AuthenticatedUser`. Authorization is implicit — operations always scope to the caller's own keys via `WHERE user_id = caller.id`. There is no admin-style "list all users' keys" endpoint.
- **Error envelope**: this router is the *only* place in cognee where errors use the `{"error": {"message": "..."}}` shape instead of the canonical `{"detail": "..."}`. The Rust implementation must emit this shape for `POST /api-keys` failures specifically. We expose a dedicated `ApiError::ApiKeyEnvelope(message: String)` variant whose `IntoResponse` writes the unusual shape. Do not let it leak into other routers. See [../auth.md §8.10](../auth.md#810-apiv1authapi-keysapi_key_id--post).
- **Storage policy**: governed by `HASH_API_KEY` env var. Rust matches Python: default `false` (raw key stored), opt into `true` for SHA-256-at-rest. The mode is deployment-wide; both stacks must use the same value to share a database. See [../auth.md §5](../auth.md#5-api-keys).
- **Lookup**: although this router does not expose a "use the key" endpoint, the same `user_api_key.api_key` column drives the `X-Api-Key` header lookup in the auth extractor. See [../auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution).
- **Per-user limit**: 10 keys, configurable via `AuthContext.max_api_keys_per_user`. Source: [`create_api_key.py:16-22`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/api_key/create_api_key.py#L16-L22).
- **Tenant scoping**: keys belong to users, not tenants. A user with multiple tenant memberships will have all their keys appear in every tenant context. There is no per-tenant key isolation in phase-1.

## 4. DTO definitions

```rust
// crates/http-server/src/dto/api_keys.rs

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Request body for `POST /api/v1/auth/api-keys`.
/// Pydantic source: `ApiKeyCreationPayload(InDTO)` in
/// `get_api_key_management_router.py:18-19`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ApiKeyCreationPayloadDTO {
    /// User-supplied label; nullable.
    #[serde(default)]
    pub name: Option<String>,
}

/// One row in the response array of `GET /api/v1/auth/api-keys`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiKeyListItemDTO {
    /// Raw 64-hex value when `HASH_API_KEY=false` (default, matches Python),
    /// or the literal `"************"` (12 asterisks) when `HASH_API_KEY=true`.
    pub key: String,
    /// First 8 chars of the original raw key + `"****"`.
    pub label: String,
    /// User-supplied display label; nullable.
    pub name: Option<String>,
    pub id: Uuid,
}

/// Response body for `POST /api/v1/auth/api-keys`. Returned exactly once.
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiKeyCreatedDTO {
    /// Raw 64-hex value. NEVER returned again — clients must persist it.
    pub key: String,
    pub label: String,
    pub name: Option<String>,
    pub id: Uuid,
}

/// 400 error envelope unique to the api-keys router.
/// Wire shape: `{"error": {"message": "..."}}`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ApiKeyErrorEnvelopeDTO {
    pub error: ApiKeyErrorDetail,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ApiKeyErrorDetail {
    pub message: String,
}
```

The `ApiKeyErrorEnvelopeDTO` is intentionally distinct from the workspace-wide `ApiError` JSON; it is emitted by `ApiError::ApiKeyEnvelope(String)` and only by handlers in this file.

## 5. Implementation tasks

1. Add `ApiKeyCreationPayloadDTO`, `ApiKeyListItemDTO`, `ApiKeyCreatedDTO`, `ApiKeyErrorEnvelopeDTO`, `ApiKeyErrorDetail` in `crates/http-server/src/dto/api_keys.rs`.
2. Add `ApiError::ApiKeyEnvelope(String)` variant in `crates/http-server/src/error.rs` with an `IntoResponse` impl that emits the `{"error": {"message": ...}}` shape.
3. Add `auth::api_keys` module in `crates/http-server/src/auth/api_keys.rs`:
   - `list(state, user_id) -> Result<Vec<UserApiKey>, DbError>`.
   - `create(state, user_id, name) -> Result<NewApiKey, ApiKeyError>` (generates the raw key, hashes if configured, inserts).
   - `delete(state, user_id, api_key_id) -> Result<(), ApiKeyError>`.
   - Helpers: `generate_raw_key() -> String` (64 hex chars), `prepare_for_storage(raw, cfg) -> String`, `compute_label(raw) -> String`.
4. Add `ApiKeyRepository` trait in `cognee-database` (or the existing `cognee-database::auth` module) with the three SQL operations. SeaORM impl in the same crate.
5. Add the three handlers in `crates/http-server/src/routers/api_keys.rs`:
   ```rust
   async fn get_list(State, AuthenticatedUser) -> Result<Json<Vec<ApiKeyListItemDTO>>, ApiError>;
   async fn post_create(State, AuthenticatedUser, Json<ApiKeyCreationPayloadDTO>)
       -> Result<Json<ApiKeyCreatedDTO>, ApiError>;
   async fn delete_one(State, AuthenticatedUser, Path<Uuid>)
       -> Result<Json<serde_json::Value>, ApiError>;  // body: null
   ```
6. Wire under `/api/v1/auth/api-keys` in `build_router` (the Python mount uses `/api/v1/auth` + the router's own `/api-keys` prefix; we replicate by `.nest("/auth/api-keys", api_keys::router())`).
7. Add OpenAPI annotations (`tags = ["auth"]`, the unusual error envelope on POST, masked response example on GET).
8. Add unit tests:
   - GET with no keys → empty array; with 3 keys → 3 items, all `key="************"` when hashed.
   - POST happy path → 200 + raw key returned + DB row stored hashed.
   - POST when at 10-key limit → 400 with `{"error":{"message":"You have reached the maximum number of API keys."}}`.
   - DELETE happy path → 200 + `null` + row gone.
   - DELETE with non-existent UUID → 500 (matching Python's quirk; mark with `#[should_panic(expected = "NotFound")]`-style test, or assert exact 500 wire response).
   - DELETE with another user's key UUID → same as non-existent (because of the `user_id` filter); 500.
9. Add integration tests in `crates/http-server/tests/test_api_keys.rs` covering the full create-list-delete cycle and the round-trip "create key, then auth a request with it".
10. Add cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_api_keys.py`: Python creates a key, Rust reads it via `X-Api-Key`; vice versa.

## 6. Open questions

1. **DELETE-missing-returns-500 quirk**. Python's `delete_api_key` raises `ApiKeyDeletionError` on missing rows, which reaches FastAPI as an unhandled exception → 500. Strictly speaking we should match for wire-compat. Better long-term: return 404 in both stacks (coordinated fix). Phase-1: match (500). Track a follow-up to fix both stacks together.
2. **Telemetry parity (PostHog)**. Python sends a `send_telemetry(...)` event on every endpoint hit. Rust does not currently implement PostHog telemetry. Decide: (a) skip (cleanest), (b) port via a `Telemetry` trait with a no-op default. Recommendation: (a); document the gap in a CHANGELOG.
3. **Name-length cap**. Python has no cap. To match Python exactly we apply no cap either; reject the earlier proposal of a 255-char limit. JSON-body-size limits at the middleware layer ([../architecture.md §8](../architecture.md#8-middleware-stack)) provide the only effective bound.
4. **Empty `name` handling**. JSON `"name": ""` produces `Some("")` after deserialization. Python stores it as `""`, displays it as `""` in the list. We replicate; we do not coerce `""` to `None`.

## 7. References

- Python router: [`cognee/api/v1/api_keys/routers/get_api_key_management_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/api_keys/routers/get_api_key_management_router.py)
- Python helpers: [`cognee/modules/users/api_key/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/users/api_key)
- Python user_api_key model: [`cognee/modules/users/models/UserApiKey.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/UserApiKey.py)
- Python user manager (api-key lookup hook `get_by_token`): [`cognee/modules/users/get_user_manager.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/get_user_manager.py)
- Mount: [`cognee/api/client.py:218`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L218)
- Companion auth doc: [../auth.md §5 — API keys](../auth.md#5-api-keys), [../auth.md §11 — database schema](../auth.md#11-database-schema-seaorm-migration)
- Sibling routers: [auth.md](auth.md), [users.md](users.md)
- Error model: [../architecture.md §9](../architecture.md#9-error-handling)
