# Router: auth (login / logout / me)

The bread-and-butter session router. It accepts an OAuth2-style password form on `/login`, mints a fastapi-users compatible JWT, sets the auth cookie, returns the bearer token in JSON, and exposes the trivial `/me` lookup that every UI uses to render "logged in as ____". `/logout` clears the cookie.

This router covers the three custom routes [`cognee/api/v1/users/routers/get_auth_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_auth_router.py) defines explicitly. The fastapi-users-provided register / reset-password / verify routers, even though they share the same `/api/v1/auth` mount, live in their own per-router docs ([auth-register.md](auth-register.md), [auth-reset-password.md](auth-reset-password.md), [auth-verify.md](auth-verify.md)). The API-key management surface mounted under the same prefix is in [api-keys.md](api-keys.md).

Companion docs: [../plan.md](../plan.md), [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../tenants.md](../tenants.md), [../observability.md](../observability.md).

## 1. Mount & file

- Mount prefix: `/api/v1/auth`
- Router file: `crates/http-server/src/routers/auth.rs`
- Python source: [`cognee/api/v1/users/routers/get_auth_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_auth_router.py)
- Mounted in: [`cognee/api/client.py:198`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L198) — `app.include_router(get_auth_router(), prefix="/api/v1/auth", tags=["auth"])`.
- Cross-cutting auth spec: [../auth.md §3 (JWT)](../auth.md#3-jwt-format), [../auth.md §4 (Cookie)](../auth.md#4-cookie-format), [../auth.md §6 (Password hashing)](../auth.md#6-password-hashing), [../auth.md §8.1–§8.3](../auth.md#81-apiv1authlogin--post).

## 2. Endpoints

### 2.1 `POST /login` — exchange credentials for a JWT

- **Auth**: `none`. The request *is* the credential. We deliberately do not run the auth extractor here.
- **Path params**: none.
- **Query params**: none.
- **Request body**: `application/x-www-form-urlencoded` (OAuth2 password grant — `OAuth2PasswordRequestForm`). Fields:

  | Form field | Type | Required | Notes |
  |---|---|---|---|
  | `username` | `String` | yes | The user's email address. fastapi-users keeps the OAuth2 spelling `username` even though we treat it as an email. |
  | `password` | `String` | yes | Cleartext over TLS; never logged. |
  | `grant_type` | `String` | no | OAuth2 spec optional; ignored if present (fastapi-users does the same). |
  | `scope` | `String` | no | Ignored. |
  | `client_id` / `client_secret` | `String` | no | Ignored. |

  In Rust, parse via `axum::extract::Form<LoginPayloadDTO>` (axum will URL-decode and reject non-form content types).
- **Response body** (`200 OK`, `application/json`):

  ```json
  {"access_token": "<jwt>", "token_type": "bearer"}
  ```

  Plus `Set-Cookie: <cookie_name>=<jwt>; HttpOnly; Path=/; SameSite=Lax; Max-Age=<JWT_LIFETIME_SECONDS>` and (when `cookie_domain` is set) `Domain=…`. See [../auth.md §4](../auth.md#4-cookie-format) for the exact attribute set; do not duplicate here.

  The `<jwt>` is the same token returned in the JSON body — Python returns one token, splits it across cookie and JSON. We replicate.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `400` | `{"detail": "LOGIN_BAD_CREDENTIALS"}` | Wrong email, wrong password, or `is_active=false`. Source: [`get_auth_router.py:21-23`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_auth_router.py#L21-L23). |
  | `400` | `{"detail": "LOGIN_BAD_CREDENTIALS"}` | Missing `username` / `password` form fields. Python's [`request_validation_exception_handler`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L165-L176) overrides validation errors *only on `/api/v1/auth/login`* to this exact shape. Rust's custom `Form` extractor replicates the override (see [../architecture.md §10](../architecture.md#10-request-validation)). |
  | `400` | `{"detail": "LOGIN_USER_NOT_VERIFIED"}` | Only when `requires_verification=true`; we default to `false` to match Python's `is_verified=True` default in [`User.py:48-50`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/User.py#L48-L50). |
  | `500` | `{"detail": "..."}` | Underlying DB error during `authenticate_user`. |

  All `ApiError` variants per [../architecture.md §9](../architecture.md#9-error-handling).

- **Side effects**:
  - Successful login transparently re-hashes a bcrypt password to argon2id ([../auth.md §6](../auth.md#6-password-hashing)).
  - Emits a tracing span (`cognee.api.auth.login`) with `auth_method = "password"` (no `user.id` until the lookup succeeds; on success, set it).
  - **Does not** issue a refresh token. fastapi-users doesn't either; long-lived tokens are out of scope for phase 1.
  - **Does not** emit `Mailer::on_after_login` — Python only logs at INFO level in [`get_user_manager.py:on_after_login`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/get_user_manager.py); we do the same with `tracing::info!`.

- **Delegation target**: a free function in `cognee-http-server`'s `auth` module (not pushed into `cognee-lib` because `cognee-lib`'s SDK consumers don't need login):
  - `auth::login(state: &AppState, email: &str, password: &str) -> Result<AuthenticatedUser, AuthError>` — looks up user by email, verifies password, returns the user.
  - `auth::encode_login_jwt(user: &User, cfg: &AuthConfig) -> Result<String, AuthError>` — defined in [../auth.md §3](../auth.md#3-jwt-format).
  - `auth::login_cookie(jwt: &str, cfg: &AuthConfig) -> SetCookie` — builds the `Set-Cookie` header per [../auth.md §4](../auth.md#4-cookie-format).
  - The handler chains the three.
- **Validation rules**:
  - `username` must parse as an email (use `email_address::EmailAddress` or a thin regex matching Pydantic's `EmailStr`); on failure → `LOGIN_BAD_CREDENTIALS` (same shape as wrong creds; we deliberately do *not* leak "invalid email format" so as not to enable enumeration).
  - `password` length: enforce `1..=1024` to bound CPU spent on hashing; reject longer with `LOGIN_BAD_CREDENTIALS`.
- **Rate / size limits**: defaults from [../architecture.md §8](../architecture.md#8-middleware-stack) (100 MiB body — overkill for a form; that's fine). No per-IP rate limiting in phase 1; documented in [../auth.md §14](../auth.md#14-security-considerations).
- **OpenAPI**: `tags = ["auth"]`. Operation id: `auth:login` (matches fastapi-users naming convention `auth:<backend>.login`). `security = []` (override the global). Request body `application/x-www-form-urlencoded` with `LoginPayloadDTO` schema. Responses: `200 LoginResponseDTO`, `400 ErrorDetailDTO`.
- **Telemetry**: span `cognee.api.auth.login` with attributes:
  - `auth_method = "password"`
  - `result = "success" | "bad_credentials" | "not_verified"`
  - `user.id` (only on success — never log the email)
  - Standard fields per [../observability.md §3.3](../observability.md#33-span-instrumentation-conventions).
- **Python parity notes**:
  - The `OAuth2PasswordRequestForm` quirk where unknown form fields (`grant_type`, `scope`, `client_id`, `client_secret`) are silently accepted — replicate. Reject only when *required* fields are missing.
  - The login response always sets the cookie even if the client never reads it — UI clients depend on this. Replicate.
  - The custom `RequestValidationError` handler in [`client.py:165-176`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L165-L176) is path-scoped to `/api/v1/auth/login`. Our equivalent is the custom Form extractor that swaps the error envelope; do not apply it elsewhere or `/register` etc. will lose their structured errors.

### 2.2 `POST /logout` — clear the auth cookie

- **Auth**: `required` (`AuthenticatedUser`). Source: [`get_auth_router.py:42`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_auth_router.py#L42).
- **Path params**: none.
- **Query params**: none.
- **Request body**: empty.
- **Response body** (`200 OK`, `application/json`): `{}` literal empty object. Plus `Set-Cookie: <cookie_name>=; Max-Age=0; Path=/; …` (matches `response.delete_cookie(...)` semantics; same domain attribute as on login).
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | No / invalid / expired credential. |

- **Side effects**: emits a deletion cookie. **Does not** invalidate the JWT server-side — we have no denylist in phase 1; documented in [../auth.md §14](../auth.md#14-security-considerations). The bearer header (if used) remains technically usable until `exp`; this is identical to Python's behavior.
- **Delegation target**: `auth::logout_cookie(cfg) -> SetCookie`. The handler builds the cookie and returns the empty object.
- **Validation rules**: none.
- **Rate / size limits**: defaults.
- **OpenAPI**: `tags = ["auth"]`. Operation id: `auth:logout`. `security = [{BearerAuth: []}, {ApiKeyAuth: []}, {CookieAuth: []}]`. `200` response with empty object schema.
- **Telemetry**: span `cognee.api.auth.logout` with `user.id` and `auth_method`.
- **Python parity notes**:
  - Python returns `{}` (empty object), not `204`. Replicate.
  - The deletion cookie's `Domain` attribute is set only when `AUTH_TOKEN_COOKIE_DOMAIN` is non-empty; otherwise omitted. Source: [`default_transport.py:5-9`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/authentication/default/default_transport.py#L5-L9).

### 2.3 `GET /me` — current user shape

- **Auth**: `required`.
- **Path params**: none.
- **Query params**: none.
- **Request body**: none.
- **Response body** (`200 OK`, `application/json`):

  ```json
  {"email": "<str>"}
  ```

  **Important**: Python's custom `/me` returns *only* `email`. It does **not** return id / is_active / is_superuser / tenant_id. Source: [`get_auth_router.py:50-54`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_auth_router.py#L50-L54). The fastapi-users-provided `/me` (under `/api/v1/users/me`) returns the full `UserRead` shape; that is described in [users.md](users.md). [../auth.md §8.3](../auth.md#83-apiv1authme--get) lists `id`, `email`, `is_active`, etc.; that is wrong for *this* endpoint and will be reconciled — see open questions.

- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `{"detail": "Unauthorized"}` | No / invalid / expired credential. |
  | `500` | `{"detail": "Failed to create default user: …"}` | Only when `REQUIRE_AUTHENTICATION=false` and the lazy default-user creation fails. Source: [`get_authenticated_user.py:36-42`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/methods/get_authenticated_user.py#L36-L42). |

- **Side effects**: none. Pure read.
- **Delegation target**: handler reads `user.email` directly from the `AuthenticatedUser` extractor; no library call.
- **Validation rules**: none.
- **Rate / size limits**: defaults.
- **OpenAPI**: `tags = ["auth"]`. Operation id: `auth:get_me_short`. Response schema: `MeShortResponseDTO { email: String }`.
- **Telemetry**: span `cognee.api.auth.me` with `user.id`.
- **Python parity notes**:
  - The shape is *not* `UserRead`. It is a single-key object. This intentional divergence from the fastapi-users `/users/me` exists because the SPA's session-bootstrap call only needs the email.
  - Frontend code expecting `is_superuser` / `tenant_id` should hit `/api/v1/users/me` instead. We document this so the Rust port doesn't accidentally widen the response.

## 3. Cross-cutting behavior

- **Authentication mechanism precedence** (api-key > bearer > cookie > default-user): defined globally in [../auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution). Per-endpoint behavior here only deviates by *requiring* (login: none; logout/me: required).
- **Cookie attribute set**: defined globally in [../auth.md §4](../auth.md#4-cookie-format). Both `/login` and `/logout` use the same cookie name (`AUTH_TOKEN_COOKIE_NAME`, default `auth_token`), the same `SameSite=Lax`, the same `Path=/`. The login cookie carries the JWT and `Max-Age=<lifetime>`; the logout cookie has empty value and `Max-Age=0`.
- **Error envelope**: per [../auth.md §8.1](../auth.md#81-apiv1authlogin--post), the `LOGIN_BAD_CREDENTIALS` envelope is `{"detail": "LOGIN_BAD_CREDENTIALS"}` — a string, not a structured object. The Rust `ApiError::LoginBadCredentials` variant exists specifically to emit this exact shape; do not reuse `ApiError::BadRequest("LOGIN_BAD_CREDENTIALS")` because that produces `{"detail": "LOGIN_BAD_CREDENTIALS"}` only when the helper is wired correctly — prefer the variant for clarity.
- **Tenant scoping**: this router does not look at tenants directly. The `AuthenticatedUser.tenant_id` is populated by the extractor and consumed downstream. See [../tenants.md §3.2](../tenants.md#32-users) for the column.
- **`REQUIRE_AUTHENTICATION=false` mode**: `/login` still works (a real user can still be authenticated by password). `/logout` and `/me` fall back to the default user when no credential is present — confirmed in [../auth.md §10](../auth.md#10-require_authentication-semantics).

## 4. DTO definitions

```rust
// crates/http-server/src/dto/auth.rs

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Request body for `POST /api/v1/auth/login`.
/// Wire format: `application/x-www-form-urlencoded` (OAuth2 password grant).
/// Pydantic source: `fastapi.security.OAuth2PasswordRequestForm`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct LoginPayloadDTO {
    /// Email address of the user. fastapi-users keeps the OAuth2 spelling.
    pub username: String,
    pub password: String,
    /// Always ignored; accepted for OAuth2 compliance.
    #[serde(default)]
    pub grant_type: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
}

/// Successful login response body.
#[derive(Debug, Serialize, ToSchema)]
pub struct LoginResponseDTO {
    /// JWT (HS256, audience `fastapi-users:auth`). Same value as the cookie.
    pub access_token: String,
    /// Always the literal string `"bearer"`.
    pub token_type: &'static str,
}

/// Response body for `GET /api/v1/auth/me` — note: cognee's custom shape, NOT fastapi-users `UserRead`.
/// Pydantic source: ad-hoc dict in `get_auth_router.py:52-54`.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeShortResponseDTO {
    pub email: String,
}

/// Response body for `POST /api/v1/auth/logout`. Always `{}`.
#[derive(Debug, Default, Serialize, ToSchema)]
pub struct LogoutResponseDTO {}
```

The DTOs use plain `serde` field names that already match Python's snake_case wire format; no `#[serde(rename_all = "snake_case")]` needed because every field is already snake_case.

The `LOGIN_BAD_CREDENTIALS` / `LOGIN_USER_NOT_VERIFIED` payload shape (a string in `detail`) is emitted by `ApiError::LoginBadCredentials` / `ApiError::LoginUserNotVerified`; no DTO needed — see [../architecture.md §9](../architecture.md#9-error-handling).

## 5. Implementation tasks

1. Add DTO structs in `crates/http-server/src/dto/auth.rs` per §4.
2. Add `auth::login(state, email, password)`, `auth::encode_login_jwt`, `auth::login_cookie`, `auth::logout_cookie` in `crates/http-server/src/auth/`.
3. Add handler functions in `crates/http-server/src/routers/auth.rs`:
   - `post_login(State, Form<LoginPayloadDTO>) -> Result<(SetCookie, Json<LoginResponseDTO>), ApiError>`.
   - `post_logout(State, AuthenticatedUser) -> Result<(SetCookie, Json<LogoutResponseDTO>), ApiError>`.
   - `get_me(AuthenticatedUser) -> Json<MeShortResponseDTO>`.
4. Add the custom `Form` extractor that maps validation errors on `/api/v1/auth/login` to `ApiError::LoginBadCredentials` (path-scoped; do not apply globally).
5. Wire the router into `build_router` under `/api/v1/auth` (combined with the four sibling auth routers; see [../architecture.md §7](../architecture.md#7-router-composition)).
6. Add OpenAPI annotations via `#[utoipa::path(...)]`, set `security = []` on `/login`.
7. Add unit tests in the same file: empty form → 400 `LOGIN_BAD_CREDENTIALS`; happy path → 200 + cookie; logout → cookie deletion header.
8. Add integration tests in `crates/http-server/tests/test_auth.rs` exercising the bcrypt-rehash path against a Python-seeded fixture.
9. Add cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_auth.py` (Python issues a JWT, Rust accepts it on `/me`; Rust issues a JWT, Python accepts it).

## 6. Open questions

1. **`/me` shape divergence vs auth.md**. [../auth.md §8.3](../auth.md#83-apiv1authme--get) lists the full `UserRead` shape but Python's `get_auth_router.py` returns only `{"email"}`. Proposed answer: trust the source; this doc is correct, [../auth.md §8.3](../auth.md#83-apiv1authme--get) is wrong and should be updated. Either (a) keep Python's narrow shape (recommended, matches existing UI clients) or (b) widen to `UserRead` (breaks any client that does not expect extra keys). Recommendation: (a). File a follow-up to fix [../auth.md §8.3](../auth.md#83-apiv1authme--get).
2. **Cookie deletion `Path` attribute**. Python's `response.delete_cookie` defaults to `Path=/`. We must emit the same path so browsers actually delete the cookie. Confirm by inspection of FastAPI's `delete_cookie` source.
3. **`is_active=false` on login**. Python returns `LOGIN_BAD_CREDENTIALS` (not a dedicated `LOGIN_USER_INACTIVE`) for inactive users — confirmed in [`authenticate_user.py:17-18`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/authentication/methods/authenticate_user.py#L17-L18). We do the same. Worth a regression test.
4. **Bcrypt re-hash on login concurrency**. If two requests arrive for the same user with a bcrypt hash, both will recompute argon2id and race on the `UPDATE`. SQLite + `last write wins` is fine; for Postgres we should use `WHERE hashed_password = :old_hash` to make it idempotent. Decide once Postgres support lands.
5. **`LOGIN_USER_NOT_VERIFIED` reachability**. We default `requires_verification=false`, so this branch is dead unless an operator flips a future env var. Worth documenting the env var (Python uses fastapi-users `requires_verification`; we expose `AUTH_REQUIRE_VERIFICATION=true|false`, default `false`).
6. **Cookie name on logout vs login mismatch**. If an operator changes `AUTH_TOKEN_COOKIE_NAME` mid-deployment, in-flight users' old cookies will not be deleted by `/logout`. Phase-1: document; phase-2: emit deletion for both old and new names if both are configured.

## 7. References

- Python custom router: [`cognee/api/v1/users/routers/get_auth_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_auth_router.py)
- Python authentication helper: [`cognee/modules/users/authentication/methods/authenticate_user.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/authentication/methods/authenticate_user.py)
- Python user-manager hooks: [`cognee/modules/users/get_user_manager.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/get_user_manager.py)
- Python authenticated-user dependency: [`cognee/modules/users/methods/get_authenticated_user.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/methods/get_authenticated_user.py)
- Python JWT strategy: [`cognee/modules/users/authentication/get_client_auth_backend.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/authentication/get_client_auth_backend.py)
- Python cookie transport: [`cognee/modules/users/authentication/default/default_transport.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/authentication/default/default_transport.py)
- Python validation handler override for `/login`: [`cognee/api/client.py:165-176`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L165-L176)
- fastapi-users error codes: [`fastapi_users/router/common.py`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/common.py)
- Rust auth subsystem: [../auth.md](../auth.md)
- Rust error envelope: [../architecture.md §9](../architecture.md#9-error-handling)
- Rust telemetry conventions: [../observability.md §3](../observability.md#3-tracing-stack--tracing--custom-layer)
