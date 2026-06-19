# HTTP Server — Authentication

This document specifies the authentication subsystem for the Rust HTTP server. It covers wire-level compatibility with Python's [fastapi-users](https://fastapi-users.github.io/fastapi-users/)-based stack, the password hashing migration plan, the API-key model, and the database schema. Endpoint contracts that *use* auth are described in their own router docs; this doc describes auth itself.

Companion docs: [architecture.md](architecture.md).

## 1. Goals & non-goals

### Goals

- **Wire compatibility** with the Python server so existing tokens, cookies, and API keys keep working without forced re-login or re-issuance.
- **Three coexisting mechanisms**: bearer JWT, signed-cookie JWT, and `X-Api-Key` header. The HTTP layer accepts any of them and resolves to a single `AuthenticatedUser`.
- **Drop-in fastapi-users JWT**: HS256, the same env-var-driven secret, the same `aud` claim, the same lifetime semantics. Python-issued tokens are accepted by Rust and vice versa.
- **Safe password hash migration**: argon2id for new passwords; existing bcrypt hashes (`$2b$…`) keep verifying; on successful login with a bcrypt hash, transparently re-hash to argon2id.
- **Bounded API-key model**: 10 keys per user. `HASH_API_KEY` env var matches Python: default `false` (plaintext at rest), set to `true` to opt into SHA-256-hashed-at-rest. No defaults are flipped relative to Python.
- **Email-flow stubs are explicit**: register, password reset, and verify routes exist and return the right shape; mail delivery is pluggable via a `Mailer` trait whose default implementation is a no-op that logs the token (matches Python's logging-only default — see [`get_user_manager.py` `on_after_forgot_password`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/get_user_manager.py)).

### Non-goals

- **Not a fastapi-users runtime clone.** Internals are ours; only the HTTP-visible surface is compatible. We don't reproduce fastapi-users abstractions (`UserManager`, `BaseUserManager`, `Strategy`, `Transport`).
- **Not OAuth2/OIDC in this phase.** Python ships with cookie+bearer+api-key only. Third-party OAuth providers (Google, GitHub) are deferred.
- **No SSO / SAML.**
- **No 2FA / MFA / WebAuthn** in phase 1.

## 2. Three auth mechanisms — precedence and resolution

Python registers three [`AuthenticationBackend`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/get_fastapi_users.py)s in this order: `[api_key_backend, api_auth_backend, client_auth_backend]`. The first one that yields a user wins. We keep the **same precedence** so behavior is unchanged.

```
incoming request
       │
       ├── X-Api-Key header present?  ──► look up after applying configured HASH_API_KEY mode → user
       │   (default `HASH_API_KEY=false` looks up the raw header value;
       │    `HASH_API_KEY=true` looks up the SHA-256 hash. Highest priority — short-circuits the others)
       │
       ├── Authorization: Bearer <jwt>?  ──► verify JWT → user
       │   (fastapi-users "bearer" backend, used by SDKs and the MCP)
       │
       └── Cookie: <auth_token>=<jwt>?  ──► verify JWT → user
           (fastapi-users "cookie" backend, used by the frontend)
```

### Rust extractor

A single `AuthenticatedUser` extractor implements `FromRequestParts<AppState>`:

```rust
pub struct AuthenticatedUser {
    pub id: Uuid,
    pub email: String,
    pub is_superuser: bool,
    pub is_verified: bool,
    pub is_active: bool,
    pub tenant_id: Option<Uuid>,
    pub auth_method: AuthMethod,    // for tracing / audit
}

pub enum AuthMethod { ApiKey, BearerJwt, CookieJwt, DefaultUser }
```

Resolution order in `from_request_parts`:

1. Try `X-Api-Key` (calls `state.auth.lookup_api_key(...)`).
2. Try `Authorization: Bearer …` (calls `state.auth.verify_jwt(...)`).
3. Try cookie `auth_token` (same JWT verifier).
4. If `REQUIRE_AUTHENTICATION=false`, fall back to `default_user_from_state(state)` (matches [`get_authenticated_user.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/methods/get_authenticated_user.py)).
5. Otherwise return `ApiError::Unauthorized` (401, `{"detail": "Unauthorized"}`).

A second extractor `OptionalAuthenticatedUser` performs the same lookup but never errors — used by handlers that want auth-aware behavior without forcing a login (e.g. `/health/detailed` could include the requester's email when present).

## 3. JWT format

### Algorithm and claims

| Claim | Type | Source | Notes |
|---|---|---|---|
| `sub` | string | `user.id.to_string()` | UUID v4. Required. |
| `aud` | array of strings | `["fastapi-users:auth"]` | fastapi-users default. **Must** match exactly when verifying. |
| `exp` | integer (Unix seconds) | `now + lifetime_seconds` | Rejected if past. |
| `iat` | integer (Unix seconds) | now | Optional in fastapi-users; we always set it. |

Algorithm: **HS256**. Header: `{"alg":"HS256","typ":"JWT"}`. No `kid` rotation in phase 1.

```json
// Decoded sample
{
  "sub": "0193b0f1-ea2c-7000-8000-000000000001",
  "aud": ["fastapi-users:auth"],
  "exp": 1745683200,
  "iat": 1745679600
}
```

Reset-password and email-verify tokens use the **same algorithm but different secrets and audiences**:

| Token kind | Secret env var | Audience | Default lifetime |
|---|---|---|---|
| Login / session | `FASTAPI_USERS_JWT_SECRET` | `["fastapi-users:auth"]` | `JWT_LIFETIME_SECONDS` (3600s) |
| Reset password | `FASTAPI_USERS_RESET_PASSWORD_TOKEN_SECRET` | `["fastapi-users:reset"]` | 3600s (fastapi-users default) |
| Email verify | `FASTAPI_USERS_VERIFICATION_TOKEN_SECRET` | `["fastapi-users:verify"]` | 3600s |

Source: [`get_user_manager.py:23-26`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/get_user_manager.py#L23-L26), [`get_client_auth_backend.py:18-21`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/authentication/get_client_auth_backend.py#L18-L21).

### Library

`jsonwebtoken` 9.x. Encode helper:

```rust
pub fn encode_login_jwt(user: &User, cfg: &AuthConfig) -> Result<String, AuthError> {
    let now = Utc::now().timestamp() as usize;
    let claims = Claims {
        sub: user.id.to_string(),
        aud: vec!["fastapi-users:auth".into()],
        exp: now + cfg.login_lifetime.as_secs() as usize,
        iat: now,
    };
    let header = Header::new(Algorithm::HS256);
    encode(&header, &claims, &EncodingKey::from_secret(cfg.login_secret.expose_secret().as_bytes()))
        .map_err(AuthError::JwtEncode)
}
```

Decoding **must** validate the audience:

```rust
let mut validation = Validation::new(Algorithm::HS256);
validation.set_audience(&["fastapi-users:auth"]);
validation.validate_exp = true;
validation.leeway = 0; // fastapi-users uses no leeway
```

### Compatibility test

Snapshot-test: encode a JWT with the Rust path using a fixed secret, fixed `user.id`, fixed `iat`, and assert the resulting token string equals a Python-issued token with the same inputs. This pins canonical JSON serialization (claim order, no whitespace) so subtle differences fail loudly.

## 4. Cookie format

| Attribute | Value | Source |
|---|---|---|
| Name | `AUTH_TOKEN_COOKIE_NAME` env var (default `auth_token`) | [`default_transport.py:15`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/authentication/default/default_transport.py#L15) |
| Value | `<jwt>` (same shape as bearer; HS256, `aud=["fastapi-users:auth"]`) | — |
| `HttpOnly` | `true` | [`default_transport.py:17`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/authentication/default/default_transport.py#L17) |
| `Secure` | `false` (configurable; **must** be `true` over HTTPS) | [`default_transport.py:16`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/authentication/default/default_transport.py#L16) |
| `SameSite` | `Lax` | [`default_transport.py:18`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/authentication/default/default_transport.py#L18) |
| `Domain` | `AUTH_TOKEN_COOKIE_DOMAIN` env var (default unset → no Domain attr) | [`default_transport.py:5-9`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/authentication/default/default_transport.py#L5-L9) |
| `Max-Age` | `JWT_LIFETIME_SECONDS` (default 3600) | fastapi-users derives this from JWT lifetime |
| `Path` | `/` | fastapi-users default |

**Decision**: keep `Secure=false` as the default for compatibility; document loudly that production must set it to `true` via `AUTH_COOKIE_SECURE=true`. Add `AUTH_COOKIE_SECURE` env var (Rust-only addition; Python hard-codes `false`).

Library: `cookie` 0.18. Build via `cookie::Cookie::build((name, value)).http_only(true).same_site(SameSite::Lax)…` and emit `Set-Cookie` on `POST /api/v1/auth/login`. On `POST /api/v1/auth/logout`, emit a deletion cookie (`Max-Age=0`, empty value).

## 5. API keys

### Generation

- **Format**: `secrets.token_hex(32)` in Python = 64 hex characters = 256 bits of entropy. Source: [`create_api_key.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/api_key/create_api_key.py).
- **Rust equivalent**: `rand::random::<[u8; 32]>()` via `rand::rng()` (or `OsRng::fill_bytes`), then hex-encode lowercase. Output is a 64-char string from `[0-9a-f]`.
- **Display**: returned **once** in the `POST /api/v1/auth/api-keys` response body; never stored in plaintext logs.
- **Label**: first 8 chars + `"****"` (e.g. `a1b2c3d4****`). Used in subsequent `GET /api/v1/auth/api-keys` listings so users can identify keys.

### Storage at rest

Python has a per-deploy toggle: [`HASH_API_KEY`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/api_key/hash_api_key.py) env var (default `false`). When `false`, keys are stored *in plaintext* in the `user_api_key` table and returned in API responses. The Rust port matches Python exactly — same env var, same default, same wire and storage behavior.

| Mode (matches Python) | Stored value | `GET /api-keys` returns |
|---|---|---|
| `HASH_API_KEY=false` (default) | `raw` | `"key": "<raw key>"` |
| `HASH_API_KEY=true` (opt in) | `sha256_hex(raw)` | `"key": "************"` |

The mode is shared with Python row-for-row: a Python-seeded `user_api_key` row authenticates correctly against the Rust server and vice versa, provided both deployments configure the same `HASH_API_KEY` value.

### Lookup

```rust
async fn lookup_api_key(state: &AppState, header: &str) -> Option<User> {
    let prepared = if state.auth.hash_api_key {
        sha256_hex(header.as_bytes())
    } else {
        header.to_owned()
    };
    state.lib.db().find_user_by_api_key(&prepared).await.ok()
}
```

### Limits

- **Per-user max**: 10 (`max_user_api_keys`, [`create_api_key.py:21-25`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/api_key/create_api_key.py#L21-L25)). On exceeding, `POST /api-keys` returns `400 {"error": {"message": "You have reached the maximum number of API keys."}}`.
- **No expiration in phase 1.** Future addition; column exists in schema as `expires_at TIMESTAMPTZ NULL`.

## 6. Password hashing

### Inventory of what Python uses

fastapi-users delegates to [`pwdlib`](https://github.com/frankie567/pwdlib) (post-passlib). Defaults to **bcrypt** with cost factor 12. Hash format: `$2b$12$<22-char-salt><31-char-hash>`.

### Rust strategy

Two-hash transition:

1. **New passwords** are hashed with **argon2id** (m=19456 KiB, t=2, p=1 — OWASP 2024 baseline). Hash format: `$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`.
2. **Existing bcrypt hashes** still verify. The verifier inspects the prefix:
   - `$2a$…` / `$2b$…` / `$2y$…` → use `bcrypt` crate
   - `$argon2id$…` → use `argon2` crate
   - anything else → reject as malformed
3. **Re-hash on successful login**: if a user logs in successfully and their stored hash is bcrypt, transparently re-compute the hash with argon2id and update the row. After enough logins, bcrypt naturally drains out of the table.

Library choice:

- `argon2` 0.5 (RustCrypto) — pure Rust, no FFI.
- `bcrypt` 0.16 — used only for the legacy verify path; not used to hash new passwords.

Source: [`get_user_manager.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/get_user_manager.py) (no explicit bcrypt mention; inherits from `BaseUserManager` which uses pwdlib).

### Bidirectional compatibility

A user registered in Python and logging in via Rust: bcrypt verify succeeds → re-hash to argon2id → store. The Python server still verifies argon2id hashes because pwdlib auto-detects the algorithm. So the migration is one-way (bcrypt → argon2id) but reads work both ways.

## 7. AuthConfig

```rust
#[derive(Clone)]
pub struct AuthContext {
    pub login_secret: SecretString,                 // FASTAPI_USERS_JWT_SECRET
    pub login_lifetime: Duration,                   // JWT_LIFETIME_SECONDS, default 3600
    pub login_audience: Vec<String>,                // ["fastapi-users:auth"]
    pub reset_secret: SecretString,                 // FASTAPI_USERS_RESET_PASSWORD_TOKEN_SECRET
    pub reset_audience: Vec<String>,                // ["fastapi-users:reset"]
    pub reset_lifetime: Duration,                   // 3600
    pub verify_secret: SecretString,                // FASTAPI_USERS_VERIFICATION_TOKEN_SECRET
    pub verify_audience: Vec<String>,               // ["fastapi-users:verify"]
    pub verify_lifetime: Duration,                  // 3600
    pub cookie_name: String,                        // AUTH_TOKEN_COOKIE_NAME, default "auth_token"
    pub cookie_secure: bool,                        // AUTH_COOKIE_SECURE, default false
    pub cookie_domain: Option<String>,              // AUTH_TOKEN_COOKIE_DOMAIN
    pub require_authentication: bool,               // REQUIRE_AUTHENTICATION, default true
    pub hash_api_key: bool,                         // HASH_API_KEY, default false (matches Python)
    pub max_api_keys_per_user: u8,                  // 10
    pub user_repo: Arc<dyn UserRepository>,         // talks to the relational DB
    pub api_key_repo: Arc<dyn ApiKeyRepository>,
}
```

`SecretString` (from the `secrecy` crate) zeroizes on drop and refuses to be `Debug`-printed.

## 8. Endpoints

This section is normative — the implementation must match these contracts byte-for-byte where Python does.

### 8.1 `/api/v1/auth/login` — `POST`

- **Body**: `application/x-www-form-urlencoded` with `username` and `password` (fastapi-users uses OAuth2-style password form, not JSON).
- **Success**: `200 OK`, body `{"access_token": "<jwt>", "token_type": "bearer"}`. Also emits `Set-Cookie: auth_token=<jwt>; …` so the SPA gets logged in.
- **Failure**: `400 {"detail": "LOGIN_BAD_CREDENTIALS"}` for wrong creds. The custom `RequestValidationError` handler (see [client.py:165-176](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L165-L176)) overrides validation errors on `/login` to this exact shape — the Rust validation layer must do the same.
- **Failure**: `400 {"detail": "LOGIN_USER_NOT_VERIFIED"}` if `require_verification=true` (we default to **false** to match Python's `is_verified=True` default in [`User.py:48-50`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/models/User.py#L48-L50)).

### 8.2 `/api/v1/auth/logout` — `POST`

- **Auth**: required.
- **Behavior**: invalidate the cookie by emitting `Set-Cookie: auth_token=; Max-Age=0`. JWTs cannot be invalidated server-side without a denylist; we accept this and document it.
- **Body**: empty.

### 8.3 `/api/v1/auth/me` — `GET`

- **Auth**: required.
- **Response**: `{"email": "<str>"}`. Cognee's custom auth router emits **only** the email field, not the full `UserRead` (verified against `cognee/api/v1/users/routers/get_auth_router.py`). The full user shape is exposed separately on `GET /api/v1/users/me` (fastapi-users standard, mounted at `/api/v1/users` — see [routers/users.md](routers/users.md)).

### 8.4 `/api/v1/auth/register` — `POST`

- **Body** (JSON): `{"email": "<str>", "password": "<str>", "is_active": bool?, "is_superuser": bool?, "is_verified": bool?}`. fastapi-users `BaseUserCreate`.
- **Response** (`201`): `UserRead` shape — full user object: `{"id": "<uuid>", "email": "<str>", "is_active": bool, "is_superuser": bool, "is_verified": bool, "tenant_id": "<uuid>"|null}`. Note: this is wider than `/me`'s response.
- **Errors**: `400 {"detail": "REGISTER_USER_ALREADY_EXISTS"}` on duplicate email; `400 {"detail": {"code": "REGISTER_INVALID_PASSWORD", "reason": "<str>"}}` on weak password. **No length rule** — stock fastapi-users `BaseUserManager.validate_password` only rejects a password that contains the user's email substring; cognee does not override this (see [routers/auth-register.md §2.1](routers/auth-register.md#21-post-register--create-a-new-user)).
- **Side effect**: invokes `Mailer::on_after_register(user)` (default no-op); for parity with [`get_user_manager.py:31-33`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/get_user_manager.py#L31-L33).

### 8.5 `/api/v1/auth/forgot-password` — `POST`

- **Body** (JSON): `{"email": "<str>"}`.
- **Response** (`202`): empty body.
- **Side effect**: if user exists, mint a reset JWT (audience `"fastapi-users:reset"`, secret `FASTAPI_USERS_RESET_PASSWORD_TOKEN_SECRET`) and invoke `Mailer::on_after_forgot_password(user, token)`. Default mailer logs the token (matches Python).
- **Same response whether or not the email exists** — prevents enumeration. Python does the same.

### 8.6 `/api/v1/auth/reset-password` — `POST`

- **Body** (JSON): `{"token": "<reset jwt>", "password": "<new password>"}`.
- **Response** (`200`): empty.
- **Errors**: `400 {"detail": "RESET_PASSWORD_BAD_TOKEN"}` on invalid token; `400 {"detail": {"code": "RESET_PASSWORD_INVALID_PASSWORD", "reason": "<str>"}}` on weak password.

### 8.7 `/api/v1/auth/request-verify-token` — `POST`

- **Body** (JSON): `{"email": "<str>"}`.
- **Response** (`202`): empty.
- **Side effect**: if user exists and is unverified, mint a verify JWT and call `Mailer::on_after_request_verify`.

### 8.8 `/api/v1/auth/verify` — `POST`

- **Body** (JSON): `{"token": "<verify jwt>"}`.
- **Response** (`200`): `UserRead`.
- **Errors**: `400 {"detail": "VERIFY_USER_BAD_TOKEN"}` / `400 {"detail": "VERIFY_USER_ALREADY_VERIFIED"}`.

### 8.9 `/api/v1/auth/api-keys` — `GET`

- **Auth**: required.
- **Response** (`200`): `[{ "key": "<str>", "label": "<str>", "name": "<str>"|null, "id": "<uuid>" }, …]`. The `key` field is `"************"` when `HASH_API_KEY=true`, raw otherwise. Source: [`get_api_key_management_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/api_keys/routers/get_api_key_management_router.py).

### 8.10 `/api/v1/auth/api-keys` — `POST`

- **Auth**: required.
- **Body** (JSON): `{"name": "<str>"|null}`.
- **Response** (`200`): `{"key": "<raw 64-hex>", "label": "<8 chars>****", "name": <str|null>, "id": "<uuid>"}`. The raw key is returned **once**; the client must persist it.
- **Errors**: `400 {"error": {"message": "You have reached the maximum number of API keys."}}` when ≥10. Note the unusual `error.message` envelope — Python wraps API-key errors differently than other endpoints; we match it.

### 8.11 `/api/v1/auth/api-keys/{api_key_id}` — `DELETE`

- **Auth**: required.
- **Path**: `api_key_id: Uuid`.
- **Response** (`200`): the deletion status payload returned by `delete_api_key()`.
- **Errors**: `400 {"error": {"message": "..."}}` on missing key or DB failure.

### 8.12 `/api/v1/users/get-user-id` — `POST`

- **Auth**: required.
- **Body** (JSON): `{"email": "<str>"}`.
- **Response** (`200`): `{"user_id": "<uuid>"}`.
- **Errors**: `404` if not found.

### 8.13 fastapi-users `/api/v1/users` CRUD

`GET /me`, `PATCH /me`, `GET /{id}`, `PATCH /{id}`, `DELETE /{id}` — fastapi-users standard. We re-implement the same routes with the same shapes. Detailed router contracts in `routers/users.md` (separate doc).

## 9. `Mailer` trait

```rust
#[async_trait]
pub trait Mailer: Send + Sync {
    async fn send_register_welcome(&self, user: &User) -> Result<(), MailerError>;
    async fn send_password_reset(&self, user: &User, token: &str) -> Result<(), MailerError>;
    async fn send_email_verify(&self, user: &User, token: &str) -> Result<(), MailerError>;
}

pub struct LoggingMailer;          // default; logs the token; matches Python on_after_*
pub struct SmtpMailer { … }        // optional; reads SMTP_HOST / SMTP_USER / SMTP_PASS / SMTP_FROM
pub struct ConsoleMailer;          // tests; writes to a Mutex<Vec<…>> buffer
```

`AppState` holds `Arc<dyn Mailer>`. Production deployments swap in `SmtpMailer`. Tests use `ConsoleMailer` and assert on the buffered messages.

## 10. `REQUIRE_AUTHENTICATION` semantics

Python: when `REQUIRE_AUTHENTICATION=false`, [`get_authenticated_user`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/methods/get_authenticated_user.py) falls back to a default user.

Rust: same. The `AuthenticatedUser` extractor calls the free function `default_user_from_state(state)` (in `src/auth/extractor.rs`) when no credential is present and `auth.require_authentication == false`.

OpenAPI emission: when `require_authentication == true`, every operation gets `security = [{BearerAuth: []}, {ApiKeyAuth: []}]`. Otherwise `security` is empty (matches Python's [`custom_openapi`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L126-L162) behavior).

## 11. Database schema (SeaORM migration)

The HTTP server's auth tables map 1:1 to Python's. Implemented in a new SeaORM migration in `crates/database/src/migrator/`.

### `users` (existing — extend)

Python's `users` table inherits `SQLAlchemyBaseUserTableUUID`:

| Column | Type | Notes |
|---|---|---|
| `id` | UUID PK, FK → `principals.id` ON DELETE CASCADE | — |
| `email` | VARCHAR(320) UNIQUE NOT NULL | — |
| `hashed_password` | VARCHAR(1024) NOT NULL | argon2id or bcrypt |
| `is_active` | BOOLEAN NOT NULL DEFAULT TRUE | — |
| `is_superuser` | BOOLEAN NOT NULL DEFAULT FALSE | — |
| `is_verified` | BOOLEAN NOT NULL DEFAULT TRUE | Python sets default `True` in `UserCreate` |
| `tenant_id` | UUID FK → `tenants.id` NULL | — |

If our existing Rust schema doesn't have these columns, add a migration; otherwise reconcile.

### `user_api_key` (existing in Python schema)

| Column | Type | Notes |
|---|---|---|
| `id` | UUID PK | `uuid4()` |
| `user_id` | UUID FK → `principals.id` ON DELETE CASCADE, INDEX | — |
| `api_key` | TEXT NOT NULL | hashed (`HASH_API_KEY=true`) or raw |
| `label` | TEXT NULL | display preview, e.g. `a1b2c3d4****` |
| `name` | TEXT NULL | user-provided label |
| `created_at` | TIMESTAMPTZ NOT NULL DEFAULT NOW() | new column (Python lacks it; harmless addition) |
| `expires_at` | TIMESTAMPTZ NULL | reserved for future expiration |

**Mode-switch concern**: `HASH_API_KEY` is a deployment-wide setting, not a per-row flag. Switching a running deployment from `false` to `true` (or vice versa) invalidates all existing API keys because the stored column is interpreted differently. Operators who toggle the flag must rotate every key. The Rust server emits a startup warning if any row's `api_key` length is inconsistent with the configured mode (e.g. mode=`true` but a row whose value isn't a 64-char hex SHA-256 string). Behavior matches Python exactly; we add the warning as a defensive log only — no automated remediation.

### `principals` (existing) and downstream tables (`tenants`, `roles`, `user_roles`, `user_tenants`, `permissions`, `acls`)

Covered in [tenants.md](tenants.md). The auth subsystem reads `tenant_id` from `users` but does not own those tables.

### Indexes

- `users(email)` UNIQUE — already in Python.
- `user_api_key(api_key)` is **not** unique — Python's schema has no such constraint, and adding one would cause writes to fail in cases Python accepts. The 256-bit entropy of `secrets.token_hex(32)` makes collisions astronomically unlikely; we accept the same probabilistic guarantee Python does.

## 12. OpenAPI security schemes

Emitted by `utoipa::OpenApi`:

```rust
SecurityScheme::ApiKey(ApiKeyValue::Header("X-Api-Key"))
    => "ApiKeyAuth"
SecurityScheme::Http(HttpAuth::Bearer)
    => "BearerAuth"
```

Global `security` list when `require_authentication=true`: `[{"BearerAuth": []}, {"ApiKeyAuth": []}]`. Per-operation `security: []` to opt out (used by `/health`, `/`, `/api/v1/auth/login`, `/api/v1/auth/register`, `/api/v1/auth/forgot-password`, `/api/v1/auth/reset-password`, `/api/v1/auth/request-verify-token`, `/api/v1/auth/verify`).

## 13. Testing strategy

| Layer | Tests |
|---|---|
| Unit | argon2id round-trip; bcrypt verify of canned hashes; JWT encode → decode → assert claims; cookie attribute formatter; SHA-256 API key prep. |
| Cross-impl | Decode a hand-crafted Python-issued JWT (built once with `PyJWT` and committed as a fixture) and assert the resulting `Claims`. |
| Router | `POST /login` with form body → 200 + `Set-Cookie` + access_token; bad creds → 400 with exact `LOGIN_BAD_CREDENTIALS` shape; `/me` with cookie + with bearer + with api-key all succeed; `/me` with no auth → 401 (or default user when `REQUIRE_AUTHENTICATION=false`). |
| Migration | Test verifying a bcrypt-hashed password from a Python-seeded DB succeeds and re-hashes to argon2id. |
| Negative | Replay-attack the `aud` field (token meant for `reset` rejected on `/me`); expired token rejected; tampered signature rejected. |

Test fixtures in `crates/http-server/tests/fixtures/auth/`:
- `python_login_jwt.txt` — a fastapi-users JWT minted with secret=`super_secret`, lifetime=3600.
- `python_bcrypt_hash.txt` — a bcrypt-hashed `"correct horse battery staple"`.
- `python_argon2_hash.txt` — same, argon2id.

## 14. Security considerations

- **Default secret in Python is `"super_secret"`** ([`get_client_auth_backend.py:20`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/authentication/get_client_auth_backend.py#L20)). We reject this default in production: if `cfg.env == "prod"` and `cfg.login_secret == "super_secret"`, refuse to start with a clear error. Same check for `RESET_PASSWORD` and `VERIFICATION` secrets.
- **JWT denylist**: not implemented in phase 1. Logout invalidates the cookie, not the underlying JWT. If a token leaks, rotating `FASTAPI_USERS_JWT_SECRET` is the only revocation. Document this.
- **Constant-time comparisons**: API key lookup must compare the SHA-256 hash with constant-time equality (`subtle::ConstantTimeEq`) to avoid timing leaks. The DB query itself is binary-safe; the code path in `lookup_api_key` should not branch on key contents before the equality check.
- **Brute-force on `/login`**: out of scope for phase 1. fastapi-users has no rate limit either. Add `tower::limit::ConcurrencyLimit` per-client in a follow-up.
- **Email enumeration on `/forgot-password`**: returns the same status whether or not the email exists. We do the same.
- **Cookie `Secure=false`** is unsafe over HTTP. Add the `AUTH_COOKIE_SECURE=true` env var so prod deployments can flip it without a code change.

## 15. Open questions

1. **Argon2 parameters**: the OWASP 2024 baseline (m=19456 KiB, t=2, p=1) is conservative on CPU. On constrained hardware (the Android runner, embedded targets) we may need a lower m. Decide once benchmarks land.
2. **Bcrypt re-hash trigger**: we re-hash on successful login. Should we also re-hash on `/api/v1/auth/reset-password` (after the user picks a new password)? Probably yes — it's free.
3. **JWT secret rotation**: phase 1 supports a single secret. Multi-key rotation (`kid` header + secret map) is a follow-up. (Also tracked at [architecture.md §22 Q4](architecture.md#22-open-questions) under "JWT secret generation"; resolve in one place.)
4. **Default user creation timing**: Python creates the default user lazily on first request when `REQUIRE_AUTHENTICATION=false`. The Rust startup hook also calls `ensure_default_user()`; the lazy path is a backup. Confirm we never create two default users from races.

## 16. References

- Python user manager: [`cognee/modules/users/get_user_manager.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/get_user_manager.py)
- Auth backends: [`cognee/modules/users/authentication/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/users/authentication)
  - Cookie + JWT: [`default/default_transport.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/authentication/default/default_transport.py), [`get_client_auth_backend.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/authentication/get_client_auth_backend.py)
  - Bearer JWT: [`api_bearer/api_bearer_transport.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/authentication/api_bearer/api_bearer_transport.py), [`get_api_auth_backend.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/authentication/get_api_auth_backend.py)
  - API-key header: [`api_key/get_api_key_transport.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/authentication/api_key/get_api_key_transport.py), [`get_api_key_backend.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/authentication/get_api_key_backend.py)
- API key creation: [`cognee/modules/users/api_key/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/users/api_key)
- Authenticated-user dependency: [`cognee/modules/users/methods/get_authenticated_user.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/methods/get_authenticated_user.py)
- API-key router: [`cognee/api/v1/api_keys/routers/get_api_key_management_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/api_keys/routers/get_api_key_management_router.py)
- fastapi-users JWT internals: [https://fastapi-users.github.io/fastapi-users/latest/configuration/authentication/strategies/jwt/](https://fastapi-users.github.io/fastapi-users/latest/configuration/authentication/strategies/jwt/)
- OpenAPI security shape: [`cognee/api/client.py:126-162`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L126-L162)
