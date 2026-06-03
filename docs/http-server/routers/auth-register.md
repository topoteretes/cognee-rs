# Router: auth — register

The fastapi-users-provided register router, mounted alongside the cognee auth router under `/api/v1/auth`. A single endpoint (`POST /register`) creates a new user with a hashed password, returns the new user's `UserRead` shape on success, and emits the standard fastapi-users error envelope on duplicate-email or weak-password failures.

This doc captures the **wire contract** Rust must replicate; we do not reproduce fastapi-users' Python internals (UserManager, Strategy chains, etc.). Authoritative external reference: [fastapi-users register router docs](https://fastapi-users.github.io/fastapi-users/latest/configuration/routers/register/).

Companion docs: [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../tenants.md](../tenants.md), [auth.md](auth.md), [users.md](users.md).

## 1. Mount & file

- Mount prefix: `/api/v1/auth` (path: `/register`).
- Router file: `crates/http-server/src/routers/auth_register.rs` (combined with siblings into the auth router family — see [../architecture.md §7](../architecture.md#7-router-composition)).
- Python source: [`cognee/api/v1/users/routers/get_register_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_register_router.py) (one-liner that returns `fastapi_users.get_register_router(UserRead, UserCreate)`).
- Mounted in: [`cognee/api/client.py:200-204`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L200-L204).
- User schemas: [`cognee/modules/users/models/User.py:46-55`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/User.py#L46-L55):

  ```python
  class UserRead(schemas.BaseUser[uuid_UUID]):
      tenant_id: Optional[uuid_UUID] = None

  class UserCreate(schemas.BaseUserCreate):
      is_verified: bool = True
  ```

  cognee adds `tenant_id` to `UserRead` and overrides the default `is_verified=True` in `UserCreate` so freshly registered users skip the verify flow.

## 2. Endpoints

### 2.1 `POST /register` — create a new user

- **Auth**: `none`. Anyone can register; matches Python (no `Depends(get_authenticated_user)` on the fastapi-users register route). Document explicitly that this is by design — to support self-signup. Operators that want to lock signup must front the server with their own gateway / disable `/register` via a future config flag.
- **Path params**: none.
- **Query params**: none.
- **Request body**: `application/json`, `RegisterPayloadDTO` (Pydantic source: `UserCreate` extending `fastapi_users.schemas.BaseUserCreate`):

  | JSON field | Type | Required | Default | Notes |
  |---|---|---|---|---|
  | `email` | `String` (RFC 5322) | yes | — | Must be a syntactically valid email; case-insensitive uniqueness on `users.email`. |
  | `password` | `String` | yes | — | Min length: see §2.1 validation rules. |
  | `is_active` | `bool` | no | `true` | fastapi-users default. Settable but the `safe=True` flag in [`fastapi_users/router/register.py:55-57`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/register.py#L55-L57) drops it; always treated as `true`. |
  | `is_superuser` | `bool` | no | `false` | Same `safe=True` semantics — silently coerced to `false` regardless of input. |
  | `is_verified` | `bool` | no | `true` (cognee override) | cognee sets the default to `true` in [`User.py:50-51`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/User.py#L50-L51) so new users do not need to go through the verify flow. Same `safe=True` coercion: client-supplied value is ignored at the wire layer, the schema default applies. |

  **`safe=True` semantics**: fastapi-users treats `is_active`, `is_superuser`, `is_verified` as server-controlled fields. The client *may* send them, but the server overrides. We replicate exactly:
  - Accept the fields without raising (so existing clients don't break).
  - Always set `is_active=true`, `is_superuser=false`, `is_verified=true` regardless of input.

- **Response body** (`201 Created`, `application/json`): `UserReadDTO` (see §4):

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

  fastapi-users `BaseUser` carries the first five fields; cognee adds `tenant_id`. The newly created user has `tenant_id=null` by default — tenant assignment happens via `/api/v1/permissions/tenants/select` after registration.

- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `400` | `{"detail": "REGISTER_USER_ALREADY_EXISTS"}` | Email already in use. Source: [`fastapi_users/router/register.py:58-62`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/register.py#L58-L62). |
  | `400` | `{"detail": {"code": "REGISTER_INVALID_PASSWORD", "reason": "<str>"}}` | Password fails the user-manager's `validate_password` rule. Source: [`fastapi_users/router/register.py:63-70`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/register.py#L63-L70). cognee's `UserManager` does **not** override `validate_password`, so the inherited fastapi-users default applies (no length rule, only "password must not contain the user's email" — see [`BaseUserManager.validate_password`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/manager.py)). The wire shape is `detail` as a structured object (one of two places fastapi-users uses an object instead of a string in `detail` — the other is the same `code`/`reason` pair on update). |
  | `400` | `{"detail": [{"loc": [...], "msg": "...", "type": "..."}], "body": {...}}` | Pydantic-style validation error: missing `email`/`password`, malformed JSON, invalid `EmailStr`. Wire shape comes from the global `RequestValidationError` handler in [`client.py:173-176`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L173-L176). |
  | `422` | (suppressed by global handler) | FastAPI's default would be 422; the override in [`client.py:165-176`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L165-L176) demotes it to 400. Replicate. |
  | `500` | `{"detail": "..."}` | DB error. |

- **Side effects**:
  - Inserts a new row into `principals` (type=`user`) and `users`.
  - The bound `hashed_password` uses argon2id in Rust ([../auth.md §6](../auth.md#6-password-hashing)). Python uses bcrypt; argon2id rows are valid Python reads (pwdlib auto-detects).
  - Calls `Mailer::on_after_register(user)` (default `LoggingMailer` just emits a `tracing::info!`). Matches Python's [`get_user_manager.py:31-33`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/get_user_manager.py#L31-L33) which only logs.
  - **Does not** auto-login. The client must call `/login` afterward.
  - **Does not** assign the new user to any tenant. `tenant_id` is null until `/api/v1/permissions/tenants/select` is called.

- **Delegation target**:
  - `auth::register::create_user(state, payload) -> Result<User, RegisterError>` — wraps `state.lib.user_repo().create(...)` plus password hashing.
  - The handler maps `RegisterError::AlreadyExists` → `ApiError::RegisterUserAlreadyExists`, `RegisterError::InvalidPassword(reason)` → `ApiError::RegisterInvalidPassword(reason)`. Both variants emit the exact JSON shapes above.

- **Validation rules**:
  - `email`: parses as `EmailStr` (RFC 5322 + Pydantic's loose interpretation; we use `email_address::EmailAddress`). On parse failure → 400 with the structured `detail` array.
  - `password`: server-side rules in `validate_password` (Rust mirror of fastapi-users default):
    - Must be a non-empty string.
    - Must not contain the user's email substring (case-insensitive). Reason string: `"Password should not contain e-mail"`.
    - **No minimum length rule** in stock fastapi-users; cognee does not override. Rust matches verbatim — no length rule applied. The serde layer accepts any non-empty string.
  - `is_active`, `is_superuser`, `is_verified`: see `safe=True` semantics above.

- **Rate / size limits**: defaults from [../architecture.md §8](../architecture.md#8-middleware-stack). Phase 1: no signup rate limiting (matches Python). Documented in [../auth.md §14](../auth.md#14-security-considerations).
- **OpenAPI**: `tags = ["auth"]`. Operation id: `register:register` (matches fastapi-users name). `security = []` (override the global auth requirement). Request body: `RegisterPayloadDTO`. Responses: `201 UserReadDTO`, `400 ErrorDetailDTO`. The 400 example shows both `REGISTER_USER_ALREADY_EXISTS` and the `REGISTER_INVALID_PASSWORD` structured form.
- **Telemetry**: span `cognee.api.auth.register` with attributes:
  - `result = "success" | "user_already_exists" | "invalid_password" | "validation_error"`
  - `user.id` (only on success)
  - **Never log the email or the password.** The span attribute set is whitelisted; any `email`/`password` fields are stripped by the redaction layer per [../observability.md §3](../observability.md).
- **Python parity notes**:
  - The `safe=True` coercion is the most surprising fastapi-users behavior — a client sending `is_superuser=true` gets a `200` with `is_superuser=false`. We replicate.
  - The `REGISTER_INVALID_PASSWORD` wire shape uses **a JSON object inside `detail`**, not a string. Most other fastapi-users errors use a string. Replicate exactly; do not flatten.
  - Successful register returns `201`, not `200` — fastapi-users default. Source: [`fastapi_users/router/register.py:18-19`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/register.py#L18-L19).
  - The newly created user's `tenant_id` is `null`. UI clients calling `/me` immediately after register will see `null`. They must follow up with `/api/v1/permissions/tenants/select`.

## 3. Cross-cutting behavior

- **Authentication**: register is the only path besides `/login` and `/forgot-password` that bypasses auth — see [../auth.md §12](../auth.md#12-openapi-security-schemes) for the OpenAPI security override list.
- **Email-flow stub**: the `Mailer::on_after_register` default implementation is `LoggingMailer` (no-op + log). Production deployments wire `SmtpMailer` via `AppState`. Per [../auth.md §9](../auth.md#9-mailer-trait).
- **Password hashing**: per [../auth.md §6](../auth.md#6-password-hashing) — argon2id for new passwords. The bcrypt legacy path applies only to existing rows, which `register` never touches.
- **Error envelope**: the structured-detail form (`{"code", "reason"}`) is one of the two fastapi-users-specific exceptions to our string-detail convention. Implementation: `ApiError::RegisterInvalidPassword(reason: String)` IntoResponse-emits the structured shape. Do not reuse `ApiError::BadRequest(...)` — it would emit a string detail and break clients.
- **Deterministic IDs**: `users.id` is `uuid4()` (random) in Python, not content-addressed. Match exactly. Do not introduce `uuid5(email)`.

## 4. DTO definitions

```rust
// crates/http-server/src/dto/auth_register.rs

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Request body for `POST /api/v1/auth/register`.
/// Pydantic source: `UserCreate(BaseUserCreate)` from fastapi-users +
/// cognee override at `cognee/modules/users/models/User.py:50-51`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct RegisterPayloadDTO {
    pub email: String,
    pub password: String,
    /// Accepted but ignored — `safe=True` semantics, server forces `true`.
    #[serde(default)]
    pub is_active: Option<bool>,
    /// Accepted but ignored — `safe=True` semantics, server forces `false`.
    #[serde(default)]
    pub is_superuser: Option<bool>,
    /// Accepted but ignored — `safe=True` semantics, server forces the cognee default `true`.
    #[serde(default)]
    pub is_verified: Option<bool>,
}

/// Response body shared with `/me`, `/users/{id}`, etc.
/// Pydantic source: cognee `UserRead(BaseUser[UUID])` extending fastapi-users
/// with `tenant_id`. See `cognee/modules/users/models/User.py:46-48`.
/// Centralized in `crates/http-server/src/dto/users.rs` and re-exported by
/// `auth-register.md`, `auth-verify.md`, and `users.md`. Derives must match
/// across all three sites.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UserReadDTO {
    pub id: Uuid,
    pub email: String,
    pub is_active: bool,
    pub is_superuser: bool,
    pub is_verified: bool,
    pub tenant_id: Option<Uuid>,
}

/// Wire shape for the `REGISTER_INVALID_PASSWORD` / `RESET_PASSWORD_INVALID_PASSWORD` /
/// `UPDATE_USER_INVALID_PASSWORD` errors. fastapi-users emits this nested under `detail`.
#[derive(Debug, Serialize, ToSchema)]
pub struct InvalidPasswordDetailDTO {
    pub code: &'static str,   // e.g. "REGISTER_INVALID_PASSWORD"
    pub reason: String,
}
```

`#[serde(rename_all = "snake_case")]` is **not** required since every field is already snake_case. `UserReadDTO` is also used by [auth-verify.md](auth-verify.md), [users.md](users.md), and the broader `/me` shape question discussed in [auth.md §15 open questions](../auth.md#15-open-questions); centralize in `dto/users.rs` and re-export.

## 5. Implementation tasks

1. Add `RegisterPayloadDTO`, `UserReadDTO`, `InvalidPasswordDetailDTO` in `crates/http-server/src/dto/users.rs` (note: `UserReadDTO` belongs to the shared users DTO module since it is reused).
2. Add `auth::register::create_user(state, payload)` in `crates/http-server/src/auth/register.rs` (handles email parse, password hash, user-row insert, `Mailer::on_after_register` invocation, error mapping).
3. Add `validate_password(password, email) -> Result<(), InvalidPasswordReason>` in `crates/http-server/src/auth/password.rs` — mirrors fastapi-users' default rule.
4. Add the handler in `crates/http-server/src/routers/auth_register.rs`:
   ```rust
   async fn post_register(
       State(state): State<AppState>,
       Json(payload): Json<RegisterPayloadDTO>,
   ) -> Result<(StatusCode, Json<UserReadDTO>), ApiError> { … }
   ```
5. Wire under `/api/v1/auth` in `build_router` alongside the other auth routers ([../architecture.md §7](../architecture.md#7-router-composition)).
6. Add OpenAPI annotations: `security = []`, response examples for both `400` shapes.
7. Add unit tests: empty body → 400 with detail array; duplicate email → `REGISTER_USER_ALREADY_EXISTS`; password contains email → `REGISTER_INVALID_PASSWORD` with structured detail; `safe=True` coercion (client sends `is_superuser=true`, response has `is_superuser=false`); happy path → 201 + new row in `users`.
8. Add integration tests in `crates/http-server/tests/test_auth_register.rs`.
9. Add cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_auth_register.py`: register via Python then `/login` via Rust succeeds; register via Rust then read via Python's `/users/me` succeeds.

## 6. Open questions

1. **Minimum password length**. Stock fastapi-users has no minimum and Rust matches: no rule applied at the application layer. Operators wanting a length policy should configure it at a reverse-proxy / WAF / SDK layer outside the application.
2. **Disabling self-signup**. fastapi-users has no flag and Rust matches: registration is always enabled. Operators wanting a closed signup model deny the route at a reverse-proxy layer.
3. **Rate limiting**. fastapi-users has none and Rust matches: no rate limit on `POST /register`. Operators wanting rate limits configure them at a reverse-proxy / WAF layer outside the application — the same workaround Python deployments use.
4. **`tenant_id` initialization**. Default is `null`, matching Python. UI clients that immediately call `/me` may misinterpret null as "no tenant exists". The Python frontend handles this by routing the user to `/api/v1/permissions/tenants/select`; Rust expects the same flow.
5. **`safe=True` coercion telemetry**. Python silently coerces `is_superuser`, `is_verified`, `is_active` to safe defaults without logging. Rust matches: silent coercion, no warning emitted on non-default inputs.
6. **Argon2 cost on registration vs login**. Registration hashes a fresh password; login verifies. Both use the same OWASP 2024 baseline. On constrained Android targets we may want lower `m` (memory) for registration so signup is not multi-second. Bench and decide; tracked in [../auth.md §15 question 1](../auth.md#15-open-questions).

## 7. References

- Python wrapper: [`cognee/api/v1/users/routers/get_register_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_register_router.py)
- Mount: [`cognee/api/client.py:200-204`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L200-L204)
- User schemas: [`cognee/modules/users/models/User.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/User.py)
- fastapi-users register router (vendored upstream): [`fastapi_users/router/register.py`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/register.py)
- fastapi-users error codes: [`fastapi_users/router/common.py`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/common.py)
- fastapi-users docs: [https://fastapi-users.github.io/fastapi-users/latest/configuration/routers/register/](https://fastapi-users.github.io/fastapi-users/latest/configuration/routers/register/)
- Companion auth doc: [../auth.md](../auth.md) (§3 JWT, §6 password hashing, §8.4 register, §9 Mailer)
- Tenants schema: [../tenants.md](../tenants.md) (§3.2 users)
- Sibling routers: [auth.md](auth.md), [auth-reset-password.md](auth-reset-password.md), [auth-verify.md](auth-verify.md), [users.md](users.md)
