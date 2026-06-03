# Router: users

The fastapi-users-provided users CRUD router. Five endpoints implement the standard pattern:

- `GET /me`, `PATCH /me` — operations on the caller's own record (any active user).
- `GET /{id}`, `PATCH /{id}`, `DELETE /{id}` — operations on arbitrary users (superuser only).

Mounted at `/api/v1/users`, distinct from `/api/v1/auth` (which carries `/login`, `/me` short-shape, and friends). The `/api/v1/users/me` endpoint here returns the **full `UserRead` shape**, in contrast to the cognee-custom `/api/v1/auth/me` which returns only `{"email"}` ([auth.md](auth.md)).

This doc captures the **wire contract** Rust must replicate; we do not reproduce fastapi-users' Python internals. Authoritative external reference: [fastapi-users users router docs](https://fastapi-users.github.io/fastapi-users/latest/configuration/routers/users/). The single endpoint at `/api/v1/users/get-user-id` lives in [users-by-email.md](users-by-email.md).

Companion docs: [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../tenants.md](../tenants.md), [auth.md](auth.md), [auth-register.md](auth-register.md).

## 1. Mount & file

- Mount prefix: `/api/v1/users`.
- Router file: `crates/http-server/src/routers/users.rs`.
- Python source: [`cognee/api/v1/users/routers/get_users_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_users_router.py) — one-liner returning `fastapi_users.get_users_router(UserRead, UserUpdate)`.
- Mounted in: [`cognee/api/client.py:258-262`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L258-L262).
- User schemas: [`cognee/modules/users/models/User.py:46-55`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/User.py#L46-L55):

  ```python
  class UserRead(schemas.BaseUser[uuid_UUID]):
      tenant_id: Optional[uuid_UUID] = None
  class UserUpdate(schemas.BaseUserUpdate):
      pass
  ```

  cognee adds `tenant_id` to `UserRead` only; `UserUpdate` is the stock fastapi-users shape.
- fastapi-users source (vendored upstream): [`fastapi_users/router/users.py`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/users.py).

## 2. Endpoints

### 2.1 `GET /me` — read the caller's user record

- **Auth**: `required` (`AuthenticatedUser`). The fastapi-users dependency is `current_user(active=True, verified=requires_verification)`. cognee passes no `requires_verification` flag, so the default `False` is used — verification is not gated.
- **Path params**: none.
- **Query params**: none.
- **Request body**: none.
- **Response body** (`200 OK`, `application/json`): `UserReadDTO` (see §4):

  ```json
  {
    "id": "<uuid>",
    "email": "<str>",
    "is_active": true,
    "is_superuser": false,
    "is_verified": true,
    "tenant_id": "<uuid>" | null
  }
  ```

  Source: [`fastapi_users/router/users.py:36-49`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/users.py#L36-L49).

- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | No / invalid / expired credential, or user is `is_active=false`. |

- **Side effects**: pure read. The `AuthenticatedUser` extractor already loaded the row.
- **Delegation target**: handler simply maps `AuthenticatedUser` → `UserReadDTO` (the same in-memory shape we already have).
- **Validation rules**: none.
- **Rate / size limits**: defaults.
- **OpenAPI**: `tags = ["users"]`. Operation id: `users:current_user` (matches fastapi-users name). Response schema: `UserReadDTO`. `security = [{BearerAuth: []}, {ApiKeyAuth: []}, {CookieAuth: []}]`.
- **Telemetry**: span `cognee.api.users.me`. Attributes: `user.id`.
- **Python parity notes**:
  - This is the *full* `UserRead` shape — distinct from `/api/v1/auth/me` which returns `{email}` only.
  - `tenant_id` is the user's *current* tenant per [../tenants.md §3.2](../tenants.md#32-users); may be `null` for a user who has not selected one.

### 2.2 `PATCH /me` — update the caller's user record

- **Auth**: `required`.
- **Path params**: none.
- **Query params**: none.
- **Request body**: `application/json`, `UserUpdatePayloadDTO`. Pydantic source: `fastapi_users.schemas.BaseUserUpdate`. All fields optional:

  | JSON field | Type | Required | Default | `safe=True` coerced? | Notes |
  |---|---|---|---|---|---|
  | `password` | `String` | no | — | no | New cleartext password. Validated. |
  | `email` | `String` | no | — | no | New email. Must be unique. |
  | `is_active` | `bool` | no | — | yes | fastapi-users `safe=True` drops this on the `/me` route — clients cannot self-deactivate. |
  | `is_superuser` | `bool` | no | — | yes | Same — clients cannot self-promote. |
  | `is_verified` | `bool` | no | — | yes | Same. |

  fastapi-users invokes `user_manager.update(... safe=True ...)` for `/me` so `is_active`/`is_superuser`/`is_verified` are stripped server-side before persisting. Source: [`fastapi_users/router/users.py:93-97`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/users.py#L93-L97).

- **Response body** (`200 OK`, `application/json`): `UserReadDTO` reflecting the post-update state.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | No / invalid / expired credential. |
  | `400` | `{"detail": "UPDATE_USER_EMAIL_ALREADY_EXISTS"}` | New email collides with another user. Source: [`fastapi_users/router/users.py:106-110`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/users.py#L106-L110). |
  | `400` | `{"detail": {"code": "UPDATE_USER_INVALID_PASSWORD", "reason": "<str>"}}` | New password fails `validate_password`. Same nested-detail shape as register. Source: [`fastapi_users/router/users.py:98-105`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/users.py#L98-L105). |
  | `400` | `{"detail": [...], "body": {...}}` | Pydantic validation error (e.g. malformed JSON, invalid email syntax). |
  | `500` | `{"detail": "..."}` | DB error. |

- **Side effects**:
  - Updates `users.email` (if provided), recomputes `users.hashed_password` (if `password` provided; argon2id), persists.
  - Changing email triggers no automatic re-verification flow in cognee (because `is_verified=True` is default). Document.
  - **Does not** invalidate existing JWT cookies / API keys even on email change.

- **Delegation target**:
  - `auth::users::update_self(state, user, payload) -> Result<User, UpdateError>`:
    1. Apply `safe=True` filtering — drop `is_active`/`is_superuser`/`is_verified` from payload.
    2. If `password` set: `validate_password(password, email)`. On failure → `UpdateError::InvalidPassword(reason)`. Else hash with argon2id.
    3. If `email` set: check uniqueness via `SELECT id FROM users WHERE email = ?`. If hit (and not equal to caller) → `UpdateError::EmailAlreadyExists`.
    4. `UPDATE users SET ...`.
  - Handler maps errors: `EmailAlreadyExists` → `ApiError::UpdateUserEmailAlreadyExists`, `InvalidPassword(reason)` → `ApiError::UpdateUserInvalidPassword(reason)`.

- **Validation rules**:
  - `email` (when present): parses as `EmailStr`.
  - `password` (when present): `validate_password` rule (non-empty, must not contain email substring).
- **Rate / size limits**: defaults.
- **OpenAPI**: `tags = ["users"]`. Operation id: `users:patch_current_user`. Both 400 examples documented.
- **Telemetry**: span `cognee.api.users.patch_me`. Attributes: `user.id`, `email_changed = bool`, `password_changed = bool`.
- **Python parity notes**:
  - The `safe=True` field-stripping is silent — a client sending `is_superuser=true` gets a 200 with their old `is_superuser` value unchanged. Replicate.
  - `UPDATE_USER_INVALID_PASSWORD` uses the structured `{code, reason}` detail shape; the email-conflict error uses the string-detail shape. Both shapes coexist.

### 2.3 `GET /{id}` — read a specific user (superuser only)

- **Auth**: `required` (superuser). The fastapi-users dependency is `current_user(active=True, verified=requires_verification, superuser=True)`. We need a stricter extractor variant: `RequireSuperuser` (or `AuthenticatedUser` plus `if !user.is_superuser { return 403 }`). The 403 path matches fastapi-users' [`current_user` failure response](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/authentication/authenticator.py).
- **Path params**:
  - `id: Uuid` — the target user's UUID.
- **Query params**: none.
- **Request body**: none.
- **Response body** (`200 OK`, `application/json`): `UserReadDTO`.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | No / invalid / expired credential. |
  | `403` | `{"detail": "Forbidden"}` | Caller is not a superuser. fastapi-users emits status 403 with no specific body; we standardize on `{"detail": "Forbidden"}` to match our `ApiError::Forbidden(...)`. |
  | `404` | `{"detail": "Not Found"}` (FastAPI default for `HTTPException(status_code=404)`) | User with that UUID does not exist. Source: [`fastapi_users/router/users.py:33-34`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/users.py#L33-L34). FastAPI's default 404 body is `{"detail": "Not Found"}`. |
  | `400` | `{"detail": [...]}` | `id` is not a valid UUID — Pydantic path-param validation. fastapi-users converts `InvalidID` to 404 internally; we replicate. |

- **Side effects**: pure read.
- **Delegation target**: `auth::users::get_by_id(state, id) -> Result<User, NotFound>`. Handler returns 404 on `NotFound`.
- **Validation rules**: `id` parses as UUID.
- **Rate / size limits**: defaults.
- **OpenAPI**: `tags = ["users"]`. Operation id: `users:user`. `security` includes the auth schemes.
- **Telemetry**: span `cognee.api.users.get_by_id`. Attributes: `user.id` (caller), `target_user_id`.
- **Python parity notes**:
  - Both invalid-UUID and non-existent-UUID collapse to 404 (not 400). Source: [`fastapi_users/router/users.py:32-34`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/users.py#L32-L34) catches `(UserNotExists, InvalidID)` → 404. Replicate.

### 2.4 `PATCH /{id}` — update a specific user (superuser only)

- **Auth**: `required` (superuser).
- **Path params**:
  - `id: Uuid` — the target user's UUID.
- **Query params**: none.
- **Request body**: `application/json`, `UserUpdatePayloadDTO` (same shape as `PATCH /me`).
- **Response body** (`200 OK`, `application/json`): `UserReadDTO` reflecting the post-update state.

  **Difference from `/me`**: the call uses `safe=False` ([`fastapi_users/router/users.py:181-183`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/users.py#L181-L183)), so `is_active`/`is_superuser`/`is_verified` ARE accepted and persisted. A superuser can demote/promote any other user.

- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | No / invalid / expired credential. |
  | `403` | `{"detail": "Forbidden"}` | Caller is not a superuser. |
  | `404` | `{"detail": "Not Found"}` | Target user does not exist or `id` is malformed. |
  | `400` | `{"detail": "UPDATE_USER_EMAIL_ALREADY_EXISTS"}` | Email conflict. |
  | `400` | `{"detail": {"code": "UPDATE_USER_INVALID_PASSWORD", "reason": "<str>"}}` | Weak password. |

- **Side effects**:
  - Mutates the target user row, including `is_active`/`is_superuser`/`is_verified` if the superuser sets them.
  - Re-hashes password to argon2id if `password` field present.
  - **Does not** invalidate the target user's JWT or API keys on demotion / deactivation. Documented as a security gap; resolution requires a denylist.

- **Delegation target**:
  - `auth::users::update_by_id(state, id, payload, safe = false) -> Result<User, UpdateError>`. Same internal flow as `update_self` but without the `safe=True` field-strip.
- **Validation rules**: same as `PATCH /me`.
- **Rate / size limits**: defaults.
- **OpenAPI**: `tags = ["users"]`. Operation id: `users:patch_user`.
- **Telemetry**: span `cognee.api.users.patch_user`. Attributes: caller `user.id`, `target_user_id`, fields changed.
- **Python parity notes**:
  - The `safe=False` semantic is the only behavioral difference from `PATCH /me`; replicate.
  - Even a superuser updating their *own* record via `PATCH /{id}` (where id == self.id) still uses `safe=False`. There is no protection against self-demotion (`is_superuser=false`). Replicate.

### 2.5 `DELETE /{id}` — delete a user (superuser only)

- **Auth**: `required` (superuser).
- **Path params**:
  - `id: Uuid`.
- **Query params**: none.
- **Request body**: none.
- **Response body** (`204 No Content`, empty body). Source: [`fastapi_users/router/users.py:199-202`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/users.py#L199-L202). Status is 204; FastAPI uses `Response` with no body. Replicate exactly — `axum::http::StatusCode::NO_CONTENT` plus an empty body.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | No / invalid / expired credential. |
  | `403` | `{"detail": "Forbidden"}` | Caller is not a superuser. |
  | `404` | `{"detail": "Not Found"}` | Target user does not exist or `id` malformed. |
  | `500` | `{"detail": "..."}` | DB / cascade error. |

- **Side effects**:
  - `DELETE FROM users WHERE id = ?` cascades to `principals` (FK ON DELETE CASCADE).
  - Cascades to all child rows via `principals.id` chains: `acls`, `user_roles`, `user_tenants`, `user_default_permissions`, `user_api_key`. Confirmed in [../tenants.md §3](../tenants.md#3-tables).
  - **Default user / default tenant guard**: per [../tenants.md §10](../tenants.md#10-multi-tenant-isolation-guarantees), the repository must reject deletion of the well-known default user. Add a guard in the handler that returns `ApiError::Forbidden("Cannot delete the default user")` when `id == well_known_default_user_id`.
  - **Self-delete**: a superuser deleting themselves is allowed in fastapi-users; cognee inherits this. Implementation must guard against the *default* user being the self-delete target (because they cannot be re-created without bootstrap).

- **Delegation target**:
  - `auth::users::delete_by_id(state, id) -> Result<(), DeleteError>`. Handler returns 204 on success.
- **Validation rules**: `id` parses as UUID.
- **Rate / size limits**: defaults.
- **OpenAPI**: `tags = ["users"]`. Operation id: `users:delete_user`. 204 response with no body schema.
- **Telemetry**: span `cognee.api.users.delete`. Attributes: caller `user.id`, `target_user_id`, `result = "success" | "default_user" | "not_found"`.
- **Python parity notes**:
  - Status is **204 No Content** (with no body), not `200 {}`. Replicate exactly — many clients explicitly check `response.status == 204`.
  - The user manager's `delete` hook (`Mailer::on_after_delete`?) does not exist in fastapi-users by default. cognee does not register one. We do nothing extra.

## 3. Cross-cutting behavior

- **Authentication mode**: every endpoint requires `AuthenticatedUser`. `/me`, `PATCH /me` need any active user. `GET /{id}`, `PATCH /{id}`, `DELETE /{id}` need a superuser. We add a `RequireSuperuser` extractor on top of `AuthenticatedUser` to enforce the latter at the type level.
- **`safe=True` vs `safe=False`**: this distinction is the core of fastapi-users' privilege model for self-edits vs admin-edits. We replicate by passing a `safe: bool` flag through `update_*` helpers; never push the flag into the public API surface.
- **`UserReadDTO` / `UserUpdatePayloadDTO`**: shared with [auth-register.md](auth-register.md) and [auth-verify.md](auth-verify.md). Centralize in `crates/http-server/src/dto/users.rs`.
- **Error envelope dual shape**: `UPDATE_USER_INVALID_PASSWORD` uses the `{code, reason}` form; `UPDATE_USER_EMAIL_ALREADY_EXISTS` and the rest use the string `detail` form. Same dual-shape rule as register / reset-password. Implementation: `ApiError::UpdateUserInvalidPassword(reason)` and `ApiError::UpdateUserEmailAlreadyExists`.
- **Verification gating (`requires_verification`)**: cognee does not pass this flag, so unverified active users can still call all endpoints here. Document in case operators flip the cognee-level toggle later.
- **Tenant scoping**: this router is **not** tenant-scoped. A superuser sees all users across all tenants — Python's behavior. There is no per-tenant filter on `GET /{id}` etc.
- **Fast-path 404 for invalid UUID**: matches fastapi-users' collapse of `InvalidID + UserNotExists → 404`. We do the same — both shapes converge at the handler.

## 4. DTO definitions

```rust
// crates/http-server/src/dto/users.rs

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Shared response DTO. Used by:
/// - GET/PATCH /api/v1/users/me
/// - GET/PATCH /api/v1/users/{id}
/// - POST /api/v1/auth/register
/// - POST /api/v1/auth/verify
/// Pydantic source: cognee `UserRead(BaseUser[UUID])` adding `tenant_id`
/// at `cognee/modules/users/models/User.py:46-48`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UserReadDTO {
    pub id: Uuid,
    pub email: String,
    pub is_active: bool,
    pub is_superuser: bool,
    pub is_verified: bool,
    pub tenant_id: Option<Uuid>,
}

/// Request body for `PATCH /api/v1/users/me` and `PATCH /api/v1/users/{id}`.
/// All fields optional. fastapi-users uses `safe=True` for /me (drops the
/// privileged fields) and `safe=False` for /{id} (keeps them).
/// Pydantic source: `UserUpdate(BaseUserUpdate)` from fastapi-users +
/// cognee no-op subclass at `cognee/modules/users/models/User.py:54-55`.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct UserUpdatePayloadDTO {
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    /// Stripped on `/me`; persisted on `/{id}` (superuser only).
    #[serde(default)]
    pub is_active: Option<bool>,
    /// Stripped on `/me`; persisted on `/{id}` (superuser only).
    #[serde(default)]
    pub is_superuser: Option<bool>,
    /// Stripped on `/me`; persisted on `/{id}` (superuser only).
    #[serde(default)]
    pub is_verified: Option<bool>,
}
```

`InvalidPasswordDetailDTO` (with `code = "UPDATE_USER_INVALID_PASSWORD"`) is shared with [auth-register.md §4](auth-register.md#4-dto-definitions) and [auth-reset-password.md §4](auth-reset-password.md#4-dto-definitions). The `code` field varies per endpoint; same struct, different constants.

## 5. Implementation tasks

1. Add `UserReadDTO` and `UserUpdatePayloadDTO` in `crates/http-server/src/dto/users.rs` (the canonical home; the auth-* docs re-export from here).
2. Add `RequireSuperuser` extractor in `crates/http-server/src/auth/mod.rs`. Composes `AuthenticatedUser` with an `is_superuser` check; returns 403 on failure.
3. Add `auth::users` module in `crates/http-server/src/auth/users.rs`:
   - `update_self(state, user, payload)` — uses `safe=True` semantics.
   - `update_by_id(state, id, payload)` — uses `safe=False` semantics.
   - `get_by_id(state, id) -> Result<User, NotFound>`.
   - `delete_by_id(state, id) -> Result<(), DeleteError>` with default-user guard.
4. Add the five handlers in `crates/http-server/src/routers/users.rs`:
   ```rust
   async fn get_me(AuthenticatedUser) -> Json<UserReadDTO>;
   async fn patch_me(State, AuthenticatedUser, Json<UserUpdatePayloadDTO>) -> Result<Json<UserReadDTO>, ApiError>;
   async fn get_user_by_id(State, RequireSuperuser, Path<Uuid>) -> Result<Json<UserReadDTO>, ApiError>;
   async fn patch_user_by_id(State, RequireSuperuser, Path<Uuid>, Json<UserUpdatePayloadDTO>) -> Result<Json<UserReadDTO>, ApiError>;
   async fn delete_user_by_id(State, RequireSuperuser, Path<Uuid>) -> Result<StatusCode, ApiError>;  // returns NO_CONTENT
   ```
5. Wire under `/api/v1/users` in `build_router` alongside the by-email router (see [users-by-email.md](users-by-email.md)).
6. Add OpenAPI annotations: 401/403/404 examples for the by-id routes, 400 dual-shape for the patch routes, 204 for delete.
7. Add unit tests:
   - GET /me → 200 + caller's `UserReadDTO`.
   - PATCH /me with `is_superuser=true` → 200 with `is_superuser` unchanged (silent strip).
   - PATCH /me with weak password → 400 structured.
   - PATCH /me with conflicting email → 400 string detail.
   - GET /{id} as non-superuser → 403; as superuser with valid id → 200; with invalid UUID → 404; with non-existent UUID → 404.
   - PATCH /{id} as superuser with `is_superuser=false` on target → 200, target row updated.
   - DELETE /{id} on default user → 403; on regular user → 204 + cascade verified.
8. Add integration tests in `crates/http-server/tests/test_users.rs` covering cascade behavior end-to-end.
9. Add cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_users.py`.

## 6. Open questions

1. **Self-demotion guard**. A superuser can `PATCH /{id}` themselves to `is_superuser=false`, which bricks the system if no other superuser exists. Phase-1: match Python (no guard). Phase-2: add a guard that rejects when the result would leave zero superusers. Decide before phase-2.
2. **Cookie / API-key invalidation on demotion or deactivation**. Setting `is_active=false` on a user does not currently revoke their existing JWT (no denylist). The `AuthenticatedUser` extractor *does* check `is_active` on every request via the user-manager's `current_user(active=True)` chain, so the next request will fail — but in-flight WebSockets will not. Document; phase-2: explicit close on WebSocket layer.
3. **Email change confirmation**. Changing `email` via `PATCH /me` immediately persists without re-verification. With cognee's `is_verified=True` default, the new email is still considered verified. This is a small phishing risk (compromised account → email change → password reset to new email → game over). Phase-2: optional re-verify-on-email-change behind an env var.
4. **`users:user` 403 body shape**. fastapi-users `current_user(superuser=True)` raises `HTTPException(status_code=403)` with no specific body — FastAPI emits `{"detail": "Forbidden"}` by default. Confirm by testing against Python; commit to a fixed string.
5. **Who can list all users**. There is no `GET /users` listing endpoint. Frontends needing to list users must use `/api/v1/permissions/tenants/{id}/users` (per-tenant) or query the DB directly. Document; do not add a global list in phase-1.
6. **`UserUpdate` extra fields**. fastapi-users accepts only `password`/`email`/`is_*` — `tenant_id` is **not** in `UserUpdate` even though it is in `UserRead`. A client sending `{"tenant_id": "..."}` to `PATCH /me` will see it silently ignored (Pydantic strips unknown fields by default in this schema). Replicate; confirm in tests.

## 7. References

- Python wrapper: [`cognee/api/v1/users/routers/get_users_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_users_router.py)
- Mount: [`cognee/api/client.py:258-262`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L258-L262)
- User schemas: [`cognee/modules/users/models/User.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/User.py)
- fastapi-users users router (vendored upstream): [`fastapi_users/router/users.py`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/users.py)
- fastapi-users users router docs: [https://fastapi-users.github.io/fastapi-users/latest/configuration/routers/users/](https://fastapi-users.github.io/fastapi-users/latest/configuration/routers/users/)
- Companion auth doc: [../auth.md](../auth.md) (§3 JWT, §6 password hashing, §11 schema, §13 superuser tests)
- Tenants schema: [../tenants.md §3.2](../tenants.md#32-users)
- Sibling routers: [auth.md](auth.md), [auth-register.md](auth-register.md), [auth-verify.md](auth-verify.md), [users-by-email.md](users-by-email.md)
