# Router: auth — reset-password

The fastapi-users-provided password-reset router. Two endpoints implement the standard "send email with token, then accept token + new password" flow:

1. `POST /forgot-password` — given an email, mint a reset JWT and trigger the mailer hook. Always returns 202 to avoid email enumeration.
2. `POST /reset-password` — given a reset JWT and a new password, validate and persist.

Both rely on a separate JWT secret (`FASTAPI_USERS_RESET_PASSWORD_TOKEN_SECRET`) and audience (`fastapi-users:reset`) so reset tokens cannot impersonate session tokens. The router emits the standard fastapi-users error envelopes for bad-token and weak-password failures.

This doc captures the **wire contract** Rust must replicate; we do not reproduce fastapi-users' Python internals. Authoritative external reference: [fastapi-users reset-password router docs](https://fastapi-users.github.io/fastapi-users/latest/configuration/routers/reset/).

Companion docs: [../plan.md](../plan.md), [../architecture.md](../architecture.md), [../auth.md](../auth.md), [auth.md](auth.md), [auth-register.md](auth-register.md), [auth-verify.md](auth-verify.md).

## 1. Mount & file

- Mount prefix: `/api/v1/auth` (paths: `/forgot-password`, `/reset-password`).
- Router file: `crates/http-server/src/routers/auth_reset_password.rs`.
- Python source: [`cognee/api/v1/users/routers/get_reset_password_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_reset_password_router.py) — one-liner returning `fastapi_users.get_reset_password_router()`.
- Mounted in: [`cognee/api/client.py:206-210`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L206-L210).
- Reset JWT secret: `FASTAPI_USERS_RESET_PASSWORD_TOKEN_SECRET`. Source: [`get_user_manager.py:23-25`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/get_user_manager.py#L23-L25).
- Reset JWT audience: `fastapi-users:reset` (fastapi-users default; not overridden by cognee).
- Reset JWT lifetime: 3600 s (fastapi-users default; cognee does not override).
- Companion auth spec: [../auth.md §3 (JWT)](../auth.md#3-jwt-format) shows reset/verify secrets/audiences. [../auth.md §8.5–§8.6](../auth.md#85-apiv1authforgot-password--post) duplicates a brief sketch — this doc is the canonical source for reset endpoints.

## 2. Endpoints

### 2.1 `POST /forgot-password` — issue a reset token

- **Auth**: `none`. Anonymous email submission.
- **Path params**: none.
- **Query params**: none.
- **Request body**: `application/json`, `ForgotPasswordPayloadDTO`:

  | JSON field | Type | Required | Notes |
  |---|---|---|---|
  | `email` | `String` | yes | RFC 5322; uses Pydantic `EmailStr`. fastapi-users sets `embed=True` so the body must be `{"email": "..."}`, not the bare string. Source: [`fastapi_users/router/reset.py:48`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/reset.py#L48). |

- **Response body** (`202 Accepted`, body **empty**, content-type `application/json` with body `null`): fastapi-users returns `None`, FastAPI serializes to `null`. We emit a literal `null` body, status 202, content-type `application/json`. Source: [`fastapi_users/router/reset.py:42-44`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/reset.py#L42-L44).
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `400` | `{"detail": [{"loc":[...],"msg":"...","type":"..."}], "body": {...}}` | Missing email field, malformed JSON, or invalid email syntax (caught by the global `RequestValidationError` handler in [`client.py:165-176`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L165-L176)). |
  | `202` | `null` | Email does not exist (we **deliberately** return success to prevent enumeration). Source: [`fastapi_users/router/reset.py:53-54`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/reset.py#L53-L54). |
  | `202` | `null` | User exists but is inactive — same shape; we silently swallow the `UserInactive` exception. Source: [`fastapi_users/router/reset.py:58-59`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/reset.py#L58-L59). |
  | `500` | `{"detail": "..."}` | Underlying DB error (rare, indicates a bug). |

- **Side effects**:
  - If the user exists and is active: mint a reset JWT (HS256, secret = `FASTAPI_USERS_RESET_PASSWORD_TOKEN_SECRET`, audience = `["fastapi-users:reset"]`, lifetime = 3600 s, claims include `sub = user.id`, `aud`, `exp`, plus fastapi-users' `password_fgpt` fingerprint claim — see open questions).
  - Invokes `Mailer::send_password_reset(user, token)`. Default `LoggingMailer` emits `tracing::info!` with the token (matches Python's [`get_user_manager.py:36-39`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/get_user_manager.py#L36-L39): `logger.info("User %s has forgot their password. Reset token: %s", user.id, token)`).
  - **Does not** mark the user as locked / pending. The token is the only state.
  - Status code is always 202, even when the email does not exist.

- **Delegation target**:
  - `auth::reset::forgot_password(state, email) -> Result<(), Infallible>` — internal lookup; caller never sees an error. The function:
    1. Looks up user by email; on miss, returns `Ok(())`.
    2. If user is inactive, returns `Ok(())`.
    3. Mints a reset JWT via `auth::encode_reset_jwt(user, cfg)`.
    4. Awaits `state.mailer.send_password_reset(user, &token)`.
  - The handler then returns `(StatusCode::ACCEPTED, Json::<()>(()))`.

- **Validation rules**:
  - `email` must parse as `EmailStr`. Empty body / missing field → 400 with structured detail array (handled by our custom JSON extractor; see [../architecture.md §10](../architecture.md#10-request-validation)).
  - We do **not** rate-limit by email at this layer in phase 1 — Python doesn't either. Documented in open questions; abuse mitigation is a follow-up.

- **Rate / size limits**: defaults from [../architecture.md §8](../architecture.md#8-middleware-stack).
- **OpenAPI**: `tags = ["auth"]`. Operation id: `reset:forgot_password` (matches fastapi-users name). `security = []`. Request body schema `ForgotPasswordPayloadDTO`. Response: `202` with empty schema.
- **Telemetry**: span `cognee.api.auth.forgot_password`. Attributes:
  - `result = "token_minted" | "user_not_found" | "user_inactive"` — internal only; **never logged in prod-shaped JSON output** to prevent enumeration via log mining.
  - **Never log the email or the token**. Token redaction is mandatory per [../observability.md §1](../observability.md#1-goals--non-goals) (secret redaction is a goal). The default mailer's "log the token" behavior is an explicit dev-only convenience; in prod the mailer is replaced.

- **Python parity notes**:
  - Always-202 is intentional; do not "improve" by returning 404 on missing email.
  - The body is literally `null`, not `{}`. We emit `null` to match.
  - The reset-password JWT carries a `password_fgpt` fingerprint claim that fastapi-users uses to invalidate tokens when the password changes (so an old reset token cannot be used after a successful reset). We must replicate or document non-replication; tracked in open questions.

### 2.2 `POST /reset-password` — set a new password

- **Auth**: `none`. The reset token *is* the credential; we explicitly do not require a session.
- **Path params**: none.
- **Query params**: none.
- **Request body**: `application/json`, `ResetPasswordPayloadDTO`. **Note** the fastapi-users wire shape: both `token` and `password` are top-level fields with `Body(...)` (not `embed=True`). Source: [`fastapi_users/router/reset.py:70-72`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/reset.py#L70-L72):

  | JSON field | Type | Required | Notes |
  |---|---|---|---|
  | `token` | `String` | yes | Reset JWT minted by `/forgot-password`. |
  | `password` | `String` | yes | New cleartext password. Server validates (see §2.2 validation rules). |

- **Response body** (`200 OK`, body **empty**): fastapi-users' `reset_password` handler does not return an explicit response; default content-type is `application/json` with body `null`. Source: [`fastapi_users/router/reset.py:68-93`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/reset.py#L68-L93).
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `400` | `{"detail": "RESET_PASSWORD_BAD_TOKEN"}` | Token invalid, expired, signature mismatch, audience mismatch, user not found, or user inactive. Source: [`fastapi_users/router/reset.py:76-84`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/reset.py#L76-L84). |
  | `400` | `{"detail": {"code": "RESET_PASSWORD_INVALID_PASSWORD", "reason": "<str>"}}` | Password fails `validate_password`. Same nested-detail shape as `REGISTER_INVALID_PASSWORD`. Source: [`fastapi_users/router/reset.py:85-92`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/reset.py#L85-L92). |
  | `400` | `{"detail": [...], "body": {...}}` | Pydantic-style validation error: missing `token`/`password`, malformed JSON. |
  | `500` | `{"detail": "..."}` | DB error. |

- **Side effects**:
  - Replaces `users.hashed_password` with the argon2id hash of the new password.
  - Invalidates outstanding reset tokens for the same user (via the `password_fgpt` claim — when present and matching the old hash).
  - **Does not** auto-login the user — they must call `/login` afterward.
  - **Does not** invalidate existing session JWTs (no denylist; documented in [../auth.md §14](../auth.md#14-security-considerations)).
  - Optionally re-hashes from bcrypt to argon2id as a side effect — same logic as the login re-hash path ([../auth.md §6](../auth.md#6-password-hashing) bidirectional compatibility section).

- **Delegation target**:
  - `auth::reset::reset_password(state, token, new_password) -> Result<(), ResetError>`:
    1. Decode the reset JWT (HS256, audience check, expiry check). On any failure → `ResetError::BadToken`.
    2. Look up `user_id = jwt.sub`. If not found or inactive → `ResetError::BadToken`.
    3. Verify `password_fgpt` claim matches the user's current hashed password (only if we replicate fastapi-users' fingerprint behavior; see open questions). On mismatch → `ResetError::BadToken`.
    4. Run `validate_password(new_password, user.email)`. On failure → `ResetError::InvalidPassword(reason)`.
    5. argon2id-hash the new password and `UPDATE users SET hashed_password = ?`.
  - Handler maps errors: `BadToken` → `ApiError::ResetPasswordBadToken`, `InvalidPassword(reason)` → `ApiError::ResetPasswordInvalidPassword(reason)`.

- **Validation rules**:
  - `token`: parses as JWT (HS256, reset secret, audience `fastapi-users:reset`). All decode failures collapse into `RESET_PASSWORD_BAD_TOKEN`.
  - `password`: same `validate_password` rule as registration — must not contain the user's email substring; non-empty.
- **Rate / size limits**: defaults.
- **OpenAPI**: `tags = ["auth"]`. Operation id: `reset:reset_password`. `security = []`. Examples for both 400 shapes.
- **Telemetry**: span `cognee.api.auth.reset_password`. Attributes: `result = "success" | "bad_token" | "invalid_password" | "user_inactive"`, `user.id` (only on success). **Never log the token.**
- **Python parity notes**:
  - The four distinct internal exceptions (`InvalidResetPasswordToken`, `UserNotExists`, `UserInactive`, plus expiry) all collapse into `RESET_PASSWORD_BAD_TOKEN`. Replicate exactly — clients should not be able to distinguish "expired" from "user deleted" via the wire response.
  - `password` field is **not** embedded under a `body` key. The wire is `{"token": "...", "password": "..."}` at the top level (fastapi-users uses `Body(...)` without `embed`).

## 3. Cross-cutting behavior

- **Reset JWT format**: HS256, secret `FASTAPI_USERS_RESET_PASSWORD_TOKEN_SECRET`, audience `["fastapi-users:reset"]`, lifetime 3600 s. Defined globally in [../auth.md §3](../auth.md#3-jwt-format). The `aud` validator on `/reset-password` must reject any JWT issued for `fastapi-users:auth` or `fastapi-users:verify` — that is, do not accept a session token in place of a reset token.
- **Mailer abstraction**: `Mailer::send_password_reset(user, token)` is the only side effect of `/forgot-password`. Default `LoggingMailer` is a no-op + log; `SmtpMailer` is a feature-gated SMTP impl per [../auth.md §9](../auth.md#9-mailer-trait).
- **Authentication mode**: both endpoints are public (`security = []`). They join `/login`, `/register`, `/request-verify-token`, `/verify` in the public-paths list per [../auth.md §12](../auth.md#12-openapi-security-schemes).
- **Error envelope**: `RESET_PASSWORD_BAD_TOKEN` uses the string-detail form; `RESET_PASSWORD_INVALID_PASSWORD` uses the structured `{code, reason}` form. Same dual-shape rule as register; see [auth-register.md §3](auth-register.md#3-cross-cutting-behavior).
- **`REQUIRE_AUTHENTICATION=false` mode**: irrelevant — both endpoints declare `security = []`, so they bypass auth regardless of the global setting.
- **Cookie**: this router does **not** clear or set the auth cookie. A user who resets their password is not auto-logged in.

## 4. DTO definitions

```rust
// crates/http-server/src/dto/auth_reset_password.rs

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Request body for `POST /api/v1/auth/forgot-password`.
/// Pydantic source: `email: EmailStr = Body(..., embed=True)` in fastapi-users
/// reset router. Wire shape is `{"email": "..."}` — `embed=True` matters.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ForgotPasswordPayloadDTO {
    pub email: String,
}

/// Request body for `POST /api/v1/auth/reset-password`.
/// Pydantic source: `token: str = Body(...), password: str = Body(...)` in
/// fastapi-users reset router. Wire shape is `{"token":"...","password":"..."}`
/// at the top level (no `embed`).
#[derive(Debug, Deserialize, ToSchema)]
pub struct ResetPasswordPayloadDTO {
    pub token: String,
    pub password: String,
}
```

The error-detail DTO `InvalidPasswordDetailDTO` is shared with [auth-register.md §4](auth-register.md#4-dto-definitions) — the `code` field is set to `"RESET_PASSWORD_INVALID_PASSWORD"` here.

Both endpoints return empty / `null` bodies on success, so no response DTO is needed beyond `()` / `Json::<serde_json::Value>(json!(null))`.

## 5. Implementation tasks

1. Add `ForgotPasswordPayloadDTO`, `ResetPasswordPayloadDTO` in `crates/http-server/src/dto/auth_reset_password.rs`.
2. Add `auth::reset` module:
   - `encode_reset_jwt(user, cfg) -> Result<String, AuthError>` (mirrors login JWT but with `cfg.reset_secret` and `cfg.reset_audience`).
   - `decode_reset_jwt(token, cfg) -> Result<Claims, AuthError>` (same audience-validating pattern as login decode).
   - `forgot_password(state, email)`.
   - `reset_password(state, token, new_password)`.
3. Replicate fastapi-users' `password_fgpt` claim: include `password_fgpt = sha256(hashed_password)[..N]` in reset JWTs and verify on `/reset-password`. Required for strict parity with Python — fastapi-users emits this claim and rejects reset attempts after a password change without it.
4. Add the two handlers in `crates/http-server/src/routers/auth_reset_password.rs`:
   ```rust
   async fn post_forgot_password(
       State(state): State<AppState>,
       Json(p): Json<ForgotPasswordPayloadDTO>,
   ) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> { ... }

   async fn post_reset_password(
       State(state): State<AppState>,
       Json(p): Json<ResetPasswordPayloadDTO>,
   ) -> Result<Json<serde_json::Value>, ApiError> { ... }
   ```
5. Wire under `/api/v1/auth` alongside the other auth routers.
6. Add OpenAPI annotations: `security = []`, both 400 example shapes for `/reset-password`.
7. Add unit tests:
   - `/forgot-password` with missing email → 400 detail-array; with non-existent email → 202 + null; with existing email → 202 + mailer invoked once.
   - `/reset-password` with bad token → 400 `RESET_PASSWORD_BAD_TOKEN`; with expired token → same; with audience-mismatched token (login JWT) → same; with valid token + invalid password → 400 `RESET_PASSWORD_INVALID_PASSWORD` structured; happy path → 200 + DB row updated.
8. Add integration tests in `crates/http-server/tests/test_auth_reset_password.rs` using a `ConsoleMailer` to assert token capture.
9. Add cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_auth_reset_password.py`: Python mints a reset token, Rust accepts it; vice versa. Requires both stacks to share the reset secret env var.

## 6. Open questions

1. **`password_fgpt` claim replication**. fastapi-users includes a fingerprint of the current `hashed_password` in the reset JWT so an old token cannot be used after the password is changed. This is part of fastapi-users' `JWTStrategy.write_token` / `read_token` for reset tokens — not visible at the wire level but documented as a side-channel invariant. Two options:
   - **Replicate**: compute `password_fgpt = sha256(hashed_password)[..N]`, include it as a JWT claim, verify on reset. Keeps Python tokens valid in Rust and vice versa for in-flight tokens.
   - **Skip**: rely on the JWT's own expiry (1h) for invalidation. Simpler but a leaked token remains usable for an hour even after a manual password change.
   - Recommendation: replicate. Track explicitly so the cross-SDK parity tests cover it.
2. **Rate limiting `/forgot-password`**. With no rate limit, an attacker can hit `/forgot-password` repeatedly to flood a target's inbox or to fingerprint the mail-flow timing. Phase-1: no limit (matches Python). Phase-2: add a 3-per-15-min limit per email + 60-per-hour per IP. Defer.
3. **Cookie / session invalidation on reset**. Should resetting the password also clear the user's existing JWT cookies (force re-login on all devices)? Python does **not** — JWTs cannot be revoked without a denylist. We match. Document loudly in the user-facing changelog.
4. **Mailer non-default in prod**. The default `LoggingMailer` exposes the reset token in process logs. That is a security regression in prod. We need a startup check: if `cfg.env == "prod"` and the mailer is `LoggingMailer`, log a `tracing::warn!` (or refuse to start; bias toward warn for self-host friendliness).
5. **`/forgot-password` always-202 vs validation-error 400**. Today, malformed JSON / missing `email` returns 400 (Pydantic). A valid-but-nonexistent email returns 202. This dual behavior is a small enumeration vector: sending malformed JSON gets a different response than a non-existent email. fastapi-users does the same. Acceptable; document.
6. **Re-hash on reset**. After a successful reset, the new password is argon2id-hashed regardless of the previous hash algorithm. Confirm we do not need a second re-hash pass on next login.

## 7. References

- Python wrapper: [`cognee/api/v1/users/routers/get_reset_password_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/users/routers/get_reset_password_router.py)
- Mount: [`cognee/api/client.py:206-210`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L206-L210)
- Python user manager (reset secrets): [`cognee/modules/users/get_user_manager.py:23-25`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/get_user_manager.py#L23-L25)
- fastapi-users reset router (vendored upstream): [`fastapi_users/router/reset.py`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/router/reset.py)
- fastapi-users user-manager forgot/reset hooks: [`fastapi_users/manager.py`](https://github.com/fastapi-users/fastapi-users/blob/master/fastapi_users/manager.py)
- fastapi-users reset router docs: [https://fastapi-users.github.io/fastapi-users/latest/configuration/routers/reset/](https://fastapi-users.github.io/fastapi-users/latest/configuration/routers/reset/)
- Companion auth doc: [../auth.md](../auth.md) (§3 JWT, §6 password hashing, §8.5–§8.6 reset endpoints, §9 Mailer)
- Sibling routers: [auth.md](auth.md), [auth-register.md](auth-register.md), [auth-verify.md](auth-verify.md)
- Validation handler: [../architecture.md §10](../architecture.md#10-request-validation)
