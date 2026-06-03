# Router: auth — verify

The fastapi-users-provided email-verification router. Two endpoints implement the standard "request token → submit token" flow:

1. `POST /request-verify-token` — given an email, mint a verify JWT and trigger the mailer hook. Always returns 202 to avoid enumeration.
2. `POST /verify` — given a verify JWT, set `users.is_verified = true` and return the updated user.

The verify JWT uses its own secret (`FASTAPI_USERS_VERIFICATION_TOKEN_SECRET`) and audience (`fastapi-users:verify`) so verify tokens cannot impersonate session or reset tokens.

**Cognee specifically defaults `is_verified=True` on registration** ([`User.py:50-51`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/User.py#L50-L51)), so this router is largely dormant in cognee deployments. We still implement it byte-for-byte for parity with fastapi-users and for any operator that flips the registration override.

This doc captures the **wire contract** Rust must replicate; we do not reproduce fastapi-users' Python internals. Authoritative external reference: [fastapi-users verify router docs](https://fastapi-users.github.io/fastapi-users/latest/configuration/routers/verify/).

Companion docs: [../architecture.md](../architecture.md), [../auth.md](../auth.md), [auth.md](auth.md), [auth-register.md](auth-register.md), [auth-reset-password.md](auth-reset-password.md).

## 1. Mount & file

- Mount prefix: `/api/v1/auth` (paths: `/request-verify-token`, `/verify`).
- Router file: `crates/http-server/src/routers/auth_verify.rs`.
- Python source: [`cognee/api/v1/users/routers/get_verify_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_verify_router.py) — one-liner returning `fastapi_users.get_verify_router(UserRead)`.
- Mounted in: [`cognee/api/client.py:212-216`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L212-L216).
- Verify JWT secret env var: `FASTAPI_USERS_VERIFICATION_TOKEN_SECRET`. Source: [`get_user_manager.py:26`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/get_user_manager.py#L26).
- Verify JWT audience: `fastapi-users:verify` (fastapi-users default; not overridden by cognee).
- Verify JWT lifetime: 3600 s (fastapi-users default; not overridden).
- Companion auth spec: [../auth.md §3](../auth.md#3-jwt-format) (JWT format), [../auth.md §8.7–§8.8](../auth.md#87-apiv1authrequest-verify-token--post) (brief sketch).

## 2. Endpoints

### 2.1 `POST /request-verify-token` — issue a verification token

- **Auth**: `none`. Public endpoint.
- **Path params**: none.
- **Query params**: none.
- **Request body**: `application/json`, `RequestVerifyTokenPayloadDTO`:

  | JSON field | Type | Required | Notes |
  |---|---|---|---|
  | `email` | `String` | yes | RFC 5322; Pydantic `EmailStr`. fastapi-users sets `embed=True` so wire is `{"email": "..."}`. Source: [`fastapi_users/router/verify.py:22`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/verify.py#L22). |

- **Response body** (`202 Accepted`, body `null`, content-type `application/json`): fastapi-users returns `None`; FastAPI serializes as `null`. We emit literal `null`. Source: [`fastapi_users/router/verify.py:17-35`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/verify.py#L17-L35).
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `400` | `{"detail": [{"loc":[...],"msg":"...","type":"..."}], "body": {...}}` | Missing email field, malformed JSON, or invalid email syntax (caught by the global `RequestValidationError` handler in [`client.py:165-176`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L165-L176)). |
  | `202` | `null` | User does not exist (silently swallowed to prevent enumeration). |
  | `202` | `null` | User exists but is inactive (silently swallowed). |
  | `202` | `null` | User exists and is **already verified** (silently swallowed; no token minted). Source: [`fastapi_users/router/verify.py:30-32`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/verify.py#L30-L32). |
  | `500` | `{"detail": "..."}` | DB error. |

- **Side effects**:
  - If user exists, is active, and is **not yet verified**: mint a verify JWT (HS256, `cfg.verify_secret`, audience `["fastapi-users:verify"]`, lifetime 3600 s, claims `sub`, `aud`, `exp`, `iat`, plus a `email` claim — see open questions for `email_fgpt`).
  - Invokes `Mailer::send_email_verify(user, token)`. Default `LoggingMailer` emits `tracing::info!` matching Python's [`get_user_manager.py:42-45`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/get_user_manager.py#L42-L45).
  - **Does not** modify any DB row. The token is the only state.

- **Delegation target**:
  - `auth::verify::request_verify_token(state, email) -> Result<(), Infallible>`:
    1. Look up user by email; on miss, return `Ok(())`.
    2. If `is_active=false` or `is_verified=true`, return `Ok(())`.
    3. Mint a verify JWT via `auth::encode_verify_jwt(user, cfg)`.
    4. Await `state.mailer.send_email_verify(user, &token)`.
  - Handler returns `(StatusCode::ACCEPTED, Json(serde_json::Value::Null))`.

- **Validation rules**:
  - `email` must parse as `EmailStr`.
  - No rate limiting in phase 1 (matches Python). Documented in open questions.
- **Rate / size limits**: defaults from [../architecture.md §8](../architecture.md#8-middleware-stack).
- **OpenAPI**: `tags = ["auth"]`. Operation id: `verify:request-token` (matches fastapi-users name; note the hyphen — yes, fastapi-users names it that way in [`verify.py:18`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/verify.py#L18)). `security = []`. Response schema empty.
- **Telemetry**: span `cognee.api.auth.request_verify_token`. Attributes: `result = "token_minted" | "user_not_found" | "user_inactive" | "already_verified"`. **Never log the email or token.**
- **Python parity notes**:
  - All four "no-op" branches (missing user / inactive / already verified / `UserNotExists`) collapse into a single 202+null response. Replicate exactly.
  - The body is `null` literal, not `{}`.
  - The token is *not* returned in the response — the only delivery channel is the mailer.

### 2.2 `POST /verify` — set is_verified=true

- **Auth**: `none`. The verify token *is* the credential.
- **Path params**: none.
- **Query params**: none.
- **Request body**: `application/json`, `VerifyPayloadDTO`. **Note**: fastapi-users uses `Body(..., embed=True)` so the wire shape is `{"token": "..."}` (one-key object), not the bare token. Source: [`fastapi_users/router/verify.py:65-66`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/verify.py#L65-L66).

  | JSON field | Type | Required | Notes |
  |---|---|---|---|
  | `token` | `String` | yes | Verify JWT. |

- **Response body** (`200 OK`, `application/json`): `UserReadDTO` (the same shape as `/register`):

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

  Source: [`fastapi_users/router/verify.py:68-71`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/verify.py#L68-L71). The returned user shows the **post-update** state, so `is_verified` is `true`.

- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `400` | `{"detail": "VERIFY_USER_BAD_TOKEN"}` | Token invalid, expired, signature mismatch, audience mismatch, or user does not exist. Source: [`fastapi_users/router/verify.py:72-76`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/verify.py#L72-L76). |
  | `400` | `{"detail": "VERIFY_USER_ALREADY_VERIFIED"}` | User already has `is_verified=true`. Source: [`fastapi_users/router/verify.py:77-81`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/verify.py#L77-L81). |
  | `400` | `{"detail": [...], "body": {...}}` | Pydantic-style validation: missing `token`, malformed JSON. |
  | `500` | `{"detail": "..."}` | DB error. |

- **Side effects**:
  - Updates `users.is_verified = true` on success.
  - **Does not** auto-login the user. **Does not** clear or set the auth cookie.

- **Delegation target**:
  - `auth::verify::verify_user(state, token) -> Result<User, VerifyError>`:
    1. Decode JWT (HS256, audience `fastapi-users:verify`, expiry checked). Decode failure → `VerifyError::BadToken`.
    2. Look up user by `jwt.sub`. Missing user / `is_active=false` → `VerifyError::BadToken`.
    3. (If we replicate fastapi-users' behavior:) verify the JWT's `email` claim still matches the user's current email — if a user changed their email, old verify tokens are invalidated. Mismatch → `VerifyError::BadToken`.
    4. If `user.is_verified == true` → `VerifyError::AlreadyVerified`.
    5. `UPDATE users SET is_verified = true WHERE id = ?`.
  - Handler maps: `BadToken` → `ApiError::VerifyUserBadToken`, `AlreadyVerified` → `ApiError::VerifyUserAlreadyVerified`. Returns `Json(UserReadDTO::from(user))`.

- **Validation rules**:
  - `token`: parses as JWT (HS256, verify secret, audience `fastapi-users:verify`).
  - All decode failures collapse into `VERIFY_USER_BAD_TOKEN`.
- **Rate / size limits**: defaults.
- **OpenAPI**: `tags = ["auth"]`. Operation id: `verify:verify`. `security = []`. Both 400 examples.
- **Telemetry**: span `cognee.api.auth.verify`. Attributes: `result = "success" | "bad_token" | "already_verified" | "user_not_found"`, `user.id` (only on success). **Never log the token.**
- **Python parity notes**:
  - The four internal exceptions (`InvalidVerifyToken`, `UserNotExists`) collapse into `VERIFY_USER_BAD_TOKEN`; only `UserAlreadyVerified` gets its own response. Replicate.
  - Successful response body is the *post-update* `UserRead` — `is_verified=true` in the returned shape. Confirm by inspection.
  - Cognee's `is_verified=True` registration default means in a default deployment, the only path to `/verify` with a non-already-verified user is when an admin manually inserts `is_verified=false` rows. Document.

## 3. Cross-cutting behavior

- **Verify JWT format**: HS256, secret `FASTAPI_USERS_VERIFICATION_TOKEN_SECRET`, audience `["fastapi-users:verify"]`, lifetime 3600 s. Defined globally in [../auth.md §3](../auth.md#3-jwt-format). The audience validator on `/verify` rejects any JWT with a different `aud` — session JWTs and reset JWTs cannot be replayed here.
- **Mailer abstraction**: `Mailer::send_email_verify(user, token)` is the only side effect of `/request-verify-token`. Default `LoggingMailer` is a no-op + log; `SmtpMailer` is feature-gated per [../auth.md §9](../auth.md#9-mailer-trait).
- **Authentication mode**: both endpoints are public (`security = []`). They join `/login`, `/register`, `/forgot-password`, `/reset-password` in the public-paths list per [../auth.md §12](../auth.md#12-openapi-security-schemes).
- **Error envelope**: both `VERIFY_USER_BAD_TOKEN` and `VERIFY_USER_ALREADY_VERIFIED` are string-detail errors (no structured `{code, reason}` object). This contrasts with the password errors that *do* use the structured form.
- **`UserReadDTO`**: shared with `auth-register.md` and `users.md`; centralize in `crates/http-server/src/dto/users.rs` and re-export.
- **`is_verified` impact on login**: cognee currently does not gate `/login` on `is_verified` (because the registration default is `true`). When the operator flips this — by enabling fastapi-users' `requires_verification=true` — `/login` will start returning `LOGIN_USER_NOT_VERIFIED`. See [auth.md §2.1](auth.md#21-post-login--exchange-credentials-for-a-jwt) error responses.

## 4. DTO definitions

```rust
// crates/http-server/src/dto/auth_verify.rs

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Request body for `POST /api/v1/auth/request-verify-token`.
/// Pydantic source: `email: EmailStr = Body(..., embed=True)`.
/// Wire shape: `{"email": "..."}` (one-key object).
#[derive(Debug, Deserialize, ToSchema)]
pub struct RequestVerifyTokenPayloadDTO {
    pub email: String,
}

/// Request body for `POST /api/v1/auth/verify`.
/// Pydantic source: `token: str = Body(..., embed=True)`.
/// Wire shape: `{"token": "..."}` (one-key object).
#[derive(Debug, Deserialize, ToSchema)]
pub struct VerifyPayloadDTO {
    pub token: String,
}
```

`UserReadDTO` is shared — see [auth-register.md §4](auth-register.md#4-dto-definitions).

`/request-verify-token` returns `null`, so no response DTO is needed (use `Json(serde_json::Value::Null)`).

## 5. Implementation tasks

1. Add `RequestVerifyTokenPayloadDTO` and `VerifyPayloadDTO` in `crates/http-server/src/dto/auth_verify.rs`.
2. Add `auth::verify` module:
   - `encode_verify_jwt(user, cfg) -> Result<String, AuthError>` — mirrors login JWT but uses `cfg.verify_secret` and `cfg.verify_audience`. Optionally include `email_fgpt` claim (see open questions).
   - `decode_verify_jwt(token, cfg) -> Result<Claims, AuthError>` — audience-validating decode.
   - `request_verify_token(state, email)`.
   - `verify_user(state, token)`.
3. Decide on `email_fgpt` claim parity (see open question 1) and either replicate fastapi-users' email-fingerprint check or skip it.
4. Add the two handlers in `crates/http-server/src/routers/auth_verify.rs`:
   ```rust
   async fn post_request_verify_token(
       State(state): State<AppState>,
       Json(p): Json<RequestVerifyTokenPayloadDTO>,
   ) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> { ... }

   async fn post_verify(
       State(state): State<AppState>,
       Json(p): Json<VerifyPayloadDTO>,
   ) -> Result<Json<UserReadDTO>, ApiError> { ... }
   ```
5. Wire under `/api/v1/auth` alongside the other auth routers.
6. Add OpenAPI annotations: `security = []`, error examples for both 400 shapes on `/verify`.
7. Add unit tests:
   - `/request-verify-token` with non-existent / inactive / already-verified email → 202 + null + no mailer call.
   - `/request-verify-token` with valid pending email → 202 + null + mailer captured token.
   - `/verify` with bad / expired / wrong-audience token → 400 `VERIFY_USER_BAD_TOKEN`.
   - `/verify` with valid token for already-verified user → 400 `VERIFY_USER_ALREADY_VERIFIED`.
   - `/verify` happy path → 200 + `UserReadDTO` with `is_verified=true` + DB row updated.
8. Add integration tests in `crates/http-server/tests/test_auth_verify.rs` using a `ConsoleMailer`.
9. Add cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_auth_verify.py`: Python mints a verify token, Rust accepts it; vice versa.

## 6. Open questions

1. **`email_fgpt` (or analogous) claim replication**. fastapi-users embeds the user's current email (or a fingerprint) in the verify JWT so a token issued for one email is invalidated if the user changes email afterward. Two options:
   - **Replicate**: include `email` (or `email_fgpt`) claim, reject on mismatch. Matches Python; resists email-change-bypass attacks.
   - **Skip**: rely on JWT expiry only.
   - Recommendation: replicate; pin the wire format with a fixture-based parity test.
2. **Cognee's `is_verified=True` default**. With the cognee override, `/request-verify-token` and `/verify` are dead paths in 99% of deployments. We still implement them for parity but they will be lightly tested in production. Worth a note in the changelog so operators flipping the registration default know what they are unlocking.
3. **`requires_verification` env var**. fastapi-users supports `requires_verification=true`, but cognee does not propagate this knob. Rust matches: no `AUTH_REQUIRES_VERIFICATION` env var. Operators wanting verification gates must apply them at a reverse-proxy / WAF layer.
4. **Telemetry of verify success rate**. No application-level counter — Python doesn't expose one. Rust matches. Operators can derive the metric from access logs at an external observability layer.
5. **Re-issue cooldown**. Python has no cooldown on `/request-verify-token`. Rust matches: no cooldown, no in-memory `LruCache`. Operators wanting one configure it at a reverse-proxy layer.
6. **Verify token sent via URL**. Python embeds the token in a URL; Rust matches. The known token-leak-via-logs smell is preserved for wire compatibility. Operators wanting a short-token exchange should layer it on top of the existing endpoints.

## 7. References

- Python wrapper: [`cognee/api/v1/users/routers/get_verify_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_verify_router.py)
- Mount: [`cognee/api/client.py:212-216`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L212-L216)
- Python user manager (verify secret): [`cognee/modules/users/get_user_manager.py:26`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/get_user_manager.py#L26)
- Python user model (cognee `is_verified=True` override): [`cognee/modules/users/models/User.py:50-51`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/User.py#L50-L51)
- fastapi-users verify router (vendored upstream): [`fastapi_users/router/verify.py`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/verify.py)
- fastapi-users verify router docs: [https://fastapi-users.github.io/fastapi-users/latest/configuration/routers/verify/](https://fastapi-users.github.io/fastapi-users/latest/configuration/routers/verify/)
- Companion auth doc: [../auth.md](../auth.md) (§3 JWT, §8.7–§8.8 verify endpoints, §9 Mailer)
- Sibling routers: [auth.md](auth.md), [auth-register.md](auth-register.md), [auth-reset-password.md](auth-reset-password.md), [users.md](users.md)
- Validation handler: [../architecture.md §10](../architecture.md#10-request-validation)
