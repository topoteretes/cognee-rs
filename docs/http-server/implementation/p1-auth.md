# Implementation: P1 — Authentication stack

> **Status: Done — commit 0459963**

## 1. Goal

Land the auth subsystem of the new `cognee-http-server` crate so every wire-visible behaviour matches the Python fastapi-users surface byte-for-byte: JWT (HS256, audience `fastapi-users:auth`), `auth_token` cookie, `X-Api-Key` header, the `AuthenticatedUser` extractor, the `/api/v1/auth/{login,logout,me,register,forgot-password,reset-password,request-verify-token,verify}` endpoints, the `/api/v1/auth/api-keys` management surface, the fastapi-users `/api/v1/users` CRUD plus the cognee-specific `/api/v1/users/get-user-id` lookup, and the SeaORM migration that aligns `users` + `user_api_key` columns with the Python-seeded schema. Email delivery is stubbed via a `Mailer` trait whose default `LoggingMailer` impl matches Python's `logger.info(...)` behaviour; the SMTP impl is deferred to P7.

## 2. References (read these before starting)

- [implementation/README.md](README.md) — phase doc template (§1–§7) and invariants (atomic steps, `Verify:` clause).
- [plan.md](../plan.md) — P1 scope; phase ordering.
- [architecture.md](../architecture.md) — `AppState`, `ApiError`, middleware stack, OpenAPI assembly.
- [auth.md](../auth.md) — every detail of the auth subsystem. **The canonical source for JWT format, cookie attributes, password hashing strategy, API-key storage, the `AuthenticatedUser` extractor, and per-endpoint contracts.**
- Per-router specs (must read all before starting):
  - [routers/auth.md](../routers/auth.md) — `/login`, `/logout`, `/me`.
  - [routers/auth-register.md](../routers/auth-register.md) — `/register`.
  - [routers/auth-reset-password.md](../routers/auth-reset-password.md) — `/forgot-password`, `/reset-password`.
  - [routers/auth-verify.md](../routers/auth-verify.md) — `/request-verify-token`, `/verify`.
  - [routers/api-keys.md](../routers/api-keys.md) — `/api/v1/auth/api-keys` CRUD.
  - [routers/users.md](../routers/users.md) — `/api/v1/users` CRUD.
  - [routers/users-by-email.md](../routers/users-by-email.md) — `/get-user-id`.
- [routers/README.md](../routers/README.md) — cross-router conventions (error envelopes, auth-mode declaration, DTO naming, telemetry).

## 3. Prerequisites — P0 done

P1 depends on **P0** ([p0-foundation.md](p0-foundation.md)) being merged. Specifically the steps below assume the following are already in place:

- `crates/http-server/` crate (library + `bin`-gated standalone binary) compiles green; `AppState`, `HttpServerConfig`, `ServerError`, the `ApiError` enum, the `IntoResponse` impl, the CORS / `TraceLayer` middleware stack, the `build_router` skeleton, the OpenAPI root `#[derive(OpenApi)]` struct with `BearerAuth` + `ApiKeyAuth` security schemes pre-declared, and the `/health`-router integration-test scaffold that uses `tower::ServiceExt::oneshot`. Per [architecture.md §3](../architecture.md#3-crate-topology), [architecture.md §6](../architecture.md#6-application-state--dependency-injection), [architecture.md §13](../architecture.md#13-openapi-generation--utoipa).
- The `users` table already exists in `crates/database/src/migrator/m20250422_000001_user_tenant_role_tables.rs` but is missing the `hashed_password` and `is_verified` columns. The `user_api_key` table does **not** exist at all in that migration — P1 Step 1 must create it from scratch. Confirm by inspecting that migration: `users` has `id`, `email`, `is_active`, `is_superuser`, `tenant_id`, `created_at`, `updated_at`; no `hashed_password`, no `is_verified`, no `user_api_key` table.
- `cognee-lib` exposes a `get_or_create_default_user()` helper (`cognee_lib::api::user::get_or_create_default_user`, re-exported via `cognee_lib::api::get_or_create_default_user`) and a relational DB pool (`SqliteDatabase` / `DatabaseConnection`) that this phase's repositories will plug into. The `AuthenticatedUser` extractor calls this as the `REQUIRE_AUTHENTICATION=false` fallback (Step 7) — wire it as `state.lib.get_or_create_default_user()` or expose a thin wrapper called `default_user()` on the `CogneeLib` facade added in P1.

If any of those is missing, fix in P0 before continuing.

### Library deps to add to `crates/http-server/Cargo.toml`

P1 introduces these new runtime dependencies (versions match [architecture.md §16 — Library deps](../architecture.md#16-feature-gates)):

```
jsonwebtoken = "9"
argon2       = "0.5"
bcrypt       = "0.16"
cookie       = "0.18"
secrecy      = "0.10"
subtle       = "2"
rand         = { workspace = true }
sha2         = { workspace = true }
async-trait  = { workspace = true }
email_address = "0.2"
```

`secrecy::SecretString` for the three JWT secrets so they zeroize on drop and never leak in `Debug` output. `subtle::ConstantTimeEq` for the API-key equality check. The `bcrypt` crate is needed only for the legacy verifier — never used to hash new passwords (per [auth.md §6](../auth.md#6-password-hashing)).

## 4. Step-by-step

The 18 steps below are atomic — each lands as a single commit, each has a `Verify` line, and no step produces a diff over ~300 lines. Recommended commit grouping:

1. Foundation steps (1–6): migration + auth primitives. Each is independent and reviewable on its own.
2. Extractors + traits (7–10): glue that the routers depend on.
3. Error / validation extensions (11–12): surface required by the per-router handlers.
4. Router handlers (13–16): one per per-router doc. Implementor reads the doc, does the steps in §5 of that doc verbatim.
5. OpenAPI + fixtures (17–18): housekeeping after the wire surface lands.

Implementors must **not** combine steps; the diff-bound rule per [implementation/README.md §invariants](README.md#invariants-for-every-phase-doc) is non-negotiable. Atomic commits also keep `git bisect` tractable when a cross-SDK parity test starts failing.

### Step 1: Add the `users` + `user_api_key` SeaORM reconciliation migration

- **File(s)**: `crates/database/src/migrator/m20260427_000001_http_auth_columns.rs`, `crates/database/src/migrator/mod.rs` (register the migration).
- **Action**: Add a new SeaORM migration that (a) adds the two columns missing from `m20250422_000001` to `users`: `hashed_password TEXT NOT NULL DEFAULT ''` and `is_verified BOOLEAN NOT NULL DEFAULT TRUE` — use `Table::alter().add_column_if_not_exists(...)` for idempotency; (b) creates the `user_api_key` table from scratch with `Table::create().if_not_exists()` — the table does not exist in any prior migration and must be fully defined here with all seven columns (`id`, `user_id`, `api_key`, `label`, `name`, `created_at`, `expires_at`); `api_key` is **not** UNIQUE (see [auth.md §11 — Indexes](../auth.md#11-database-schema-seaorm-migration)). The migration must be **idempotent against a Python-seeded DB** — use `IF NOT EXISTS` for every DDL statement so re-running on a Python-bootstrapped SQLite/Postgres file is a no-op.
- **Spec reference**: [auth.md §11](../auth.md#11-database-schema-seaorm-migration).
- **Verify**: `cargo test -p cognee-database migrator::tests::idempotent_against_python_seed` (test added in step 5 below). Also `cargo check --all-targets`.

### Step 2: Add `AuthContext` + secret/audience plumbing

- **File(s)**: `crates/http-server/src/auth/mod.rs` (re-export module barrel), `crates/http-server/src/auth/context.rs`.
- **Action**: Define the `AuthContext` struct exactly as in [auth.md §7](../auth.md#7-authcontext) — three `(secret, audience, lifetime)` triples (login / reset / verify), the cookie config, `require_authentication`, `hash_api_key`, `max_api_keys_per_user`, plus `Arc<dyn UserRepository>` and `Arc<dyn ApiKeyRepository>` slots (the trait stubs land in step 9). Add `AuthContext::from_config(&HttpServerConfig)` that reads env vars (`FASTAPI_USERS_JWT_SECRET`, `FASTAPI_USERS_RESET_PASSWORD_TOKEN_SECRET`, `FASTAPI_USERS_VERIFICATION_TOKEN_SECRET`, `JWT_LIFETIME_SECONDS`, `AUTH_TOKEN_COOKIE_NAME`, `AUTH_COOKIE_SECURE`, `AUTH_TOKEN_COOKIE_DOMAIN`, `REQUIRE_AUTHENTICATION`, `HASH_API_KEY`). Wire `Arc<AuthContext>` into the `AppState::auth` field already declared in P0. **Note**: the comment in `crates/http-server/src/state.rs` on the `auth` field reads `// TODO(P2): wire Arc<AuthContext> here` — that comment was written before P1 planning was finalized. The field type (`Option<Arc<()>>`) and the comment label are both stale; replace the field type with `Option<Arc<AuthContext>>` and update the comment to `// wired in P1 step 2`.
- **Spec reference**: [auth.md §7](../auth.md#7-authcontext), [auth.md §14](../auth.md#14-security-considerations) (production secret rejection guard for `super_secret`).
- **Verify**: `cargo check -p cognee-http-server`. Add a unit test asserting that `AuthContext::from_config` errors when `cfg.env == Environment::Prod && login_secret == "super_secret"`.

### Step 3: JWT encoder + decoder (login / reset / verify)

- **File(s)**: `crates/http-server/src/auth/jwt.rs`.
- **Action**: Implement `encode_login_jwt`, `encode_reset_jwt`, `encode_verify_jwt`, `decode_login_jwt`, `decode_reset_jwt`, `decode_verify_jwt` per [auth.md §3](../auth.md#3-jwt-format). All encoders share a private `encode_with(secret, audience, lifetime, sub)` helper. Decoders **must** call `Validation::set_audience(&[expected])`, set `algorithms = vec![Algorithm::HS256]`, `validate_exp = true`, `leeway = 0`. The reset encoder includes the `password_fgpt = sha256(hashed_password)[..N]` claim per [routers/auth-reset-password.md §5 task 3](../routers/auth-reset-password.md#5-implementation-tasks); the verify encoder includes the `email` claim per [routers/auth-verify.md §5 task 3](../routers/auth-verify.md#5-implementation-tasks). The `Claims` struct must serialize `aud` as a `Vec<String>` (fastapi-users emits an array, not a string).
- **Spec reference**: [auth.md §3](../auth.md#3-jwt-format).
- **Verify**: Inline `#[cfg(test)]` round-trip — encode with secret `"super_secret"`, fixed `sub` UUID, fixed `iat`; decode and assert `sub`/`aud`/`exp` match. Cross-impl fixture test added separately in step 16.

### Step 4: Cookie helpers

- **File(s)**: `crates/http-server/src/auth/cookie.rs`.
- **Action**: `login_cookie(jwt: &str, cfg: &AuthContext) -> HeaderValue` and `logout_cookie(cfg: &AuthContext) -> HeaderValue`. Use `cookie::Cookie::build((name, value))` then `.http_only(true).same_site(SameSite::Lax).path("/").max_age(cfg.login_lifetime).secure(cfg.cookie_secure)`; conditionally add `.domain(d)` when `cfg.cookie_domain` is `Some`. Logout cookie sets `Max-Age = 0` and empty value; same `Domain` rules. Both functions return a `HeaderValue` ready for `Response::headers_mut().append(SET_COOKIE, ...)`.
- **Spec reference**: [auth.md §4](../auth.md#4-cookie-format).
- **Verify**: Inline test that the formatted cookie string contains exactly `HttpOnly`, `Path=/`, `SameSite=Lax`, and (when configured) `Secure`; logout cookie contains `Max-Age=0`.

### Step 5: API-key generation, hashing, and label helpers

- **File(s)**: `crates/http-server/src/auth/api_key.rs`.
- **Action**: `generate_raw_key() -> String` returns 64 lowercase hex chars from 32 random bytes via `rand::rng().fill(...)`. `prepare_for_storage(raw, cfg) -> String` returns `sha256_hex(raw)` when `cfg.hash_api_key == true` else `raw.to_owned()`. `compute_label(raw) -> String` returns `format!("{}****", &raw[..8])`. `lookup_api_key(state, header) -> Option<User>` follows [auth.md §5 — Lookup](../auth.md#5-api-keys) — it uses `subtle::ConstantTimeEq` for the equality check ([auth.md §14 — constant-time](../auth.md#14-security-considerations)). The `HASH_API_KEY` env var defaults to `false` to match Python.
- **Spec reference**: [auth.md §5](../auth.md#5-api-keys), [auth.md §14 — constant-time](../auth.md#14-security-considerations).
- **Verify**: Unit test asserting `generate_raw_key()` returns 64 chars matching `^[0-9a-f]{64}$`; `prepare_for_storage` round-trips both modes; `compute_label("a1b2c3d4...")` returns `"a1b2c3d4****"` exactly.

### Step 6: Password hashing (argon2id new + bcrypt legacy)

- **File(s)**: `crates/http-server/src/auth/password.rs`.
- **Action**: `hash_new_password(plain: &str) -> Result<String, AuthError>` produces an argon2id PHC string with OWASP 2024 params (`m=19456, t=2, p=1`). `verify_password(stored: &str, plain: &str) -> Result<VerifyOutcome, AuthError>` inspects the stored prefix and dispatches to either `bcrypt::verify` (for `$2a$/$2b$/$2y$`) or `argon2::PasswordHash::new(...)` (for `$argon2id$`); returns `VerifyOutcome::Ok { needs_rehash: bool }` where `needs_rehash` is `true` iff the stored hash is bcrypt. `validate_password(password: &str, email: &str) -> Result<(), InvalidPasswordReason>` mirrors fastapi-users' default rule (non-empty + must not contain email substring; **no length rule** — see [routers/auth-register.md §2.1](../routers/auth-register.md#21-post-register--create-a-new-user)).
- **Spec reference**: [auth.md §6](../auth.md#6-password-hashing).
- **Verify**: Round-trip test for argon2id; verify-canned-bcrypt test using the fixture from step 16.5 below.

### Step 7: `AuthenticatedUser` + `OptionalAuthenticatedUser` extractors

- **File(s)**: `crates/http-server/src/auth/extractor.rs`.
- **Action**: Implement `FromRequestParts<AppState>` for `AuthenticatedUser` with the resolution order from [auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution): (1) `X-Api-Key` header → `lookup_api_key`; (2) `Authorization: Bearer <jwt>` → `decode_login_jwt`; (3) `Cookie: <cookie_name>=<jwt>` → `decode_login_jwt`; (4) when `cfg.require_authentication == false`, fall back to `state.lib.default_user()`; (5) else return `ApiError::Unauthorized`. Set the `AuthMethod` enum field for telemetry. `OptionalAuthenticatedUser` runs the same lookup but on any failure returns `None` instead of erroring. The active-status check (`is_active=false → 401`) must run after the user is resolved, not earlier.
- **Spec reference**: [auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution), [auth.md §10](../auth.md#10-require_authentication-semantics).
- **Verify**: Build router test — call `/api/v1/auth/me` with each of (no creds + `REQUIRE_AUTHENTICATION=false`), (cookie), (bearer), (X-Api-Key), and assert success in all four; with `REQUIRE_AUTHENTICATION=true` and no creds, assert 401 + `{"detail": "Unauthorized"}`. Test added inline; full integration test in step 13.

### Step 8: `RequireSuperuser` extractor

- **File(s)**: `crates/http-server/src/auth/extractor.rs` (extend).
- **Action**: Add `RequireSuperuser` extractor that wraps `AuthenticatedUser` and returns `ApiError::Forbidden("Forbidden")` when `!user.is_superuser`. Used by `GET/PATCH/DELETE /api/v1/users/{id}` per [routers/users.md §3](../routers/users.md#3-cross-cutting-behavior).
- **Spec reference**: [routers/users.md §3](../routers/users.md#3-cross-cutting-behavior).
- **Verify**: Inline test: regular user → 403; superuser → passes through.

### Step 9: `Mailer` trait + `LoggingMailer` (P1) and `ConsoleMailer` (test)

- **File(s)**: `crates/http-server/src/auth/mailer.rs`.
- **Action**: Define the trait per [auth.md §9](../auth.md#9-mailer-trait): three async methods (`send_register_welcome`, `send_password_reset`, `send_email_verify`). `LoggingMailer` is the default — each method emits a `tracing::info!(...)` matching Python's `get_user_manager.py:on_after_*` hooks (so the password-reset hook logs the token; see [routers/auth-reset-password.md §6 q4](../routers/auth-reset-password.md#6-open-questions) for the prod warning). `ConsoleMailer` (gated `#[cfg(test)]` or behind a `testing` feature) keeps a `Arc<Mutex<Vec<MailEvent>>>` so tests can `assert_eq!(mailer.events().len(), 1)`. **`SmtpMailer` is explicitly deferred to P7** — see the SMTP-impl note in [auth.md §9](../auth.md#9-mailer-trait); for P1 only `LoggingMailer` and `ConsoleMailer` exist. Wire `Arc<dyn Mailer>` into `AppState`.
- **Spec reference**: [auth.md §9](../auth.md#9-mailer-trait). **Stub via `LoggingMailer` for P1; SMTP impl deferred to P7.**
- **Verify**: `cargo check -p cognee-http-server`; the `ConsoleMailer` is exercised in step 14's tests.

### Step 10: `UserRepository` + `ApiKeyRepository` traits and SeaORM impls

- **File(s)**: `crates/database/src/auth.rs` (new module), wired through `crates/database/src/lib.rs`. SeaORM impls live in the same file.
- **Action**: Define `UserRepository` (async trait): `find_by_email`, `find_by_id`, `find_id_by_email`, `find_user_by_api_key`, `create`, `update`, `delete_by_id`, `count_for_tenant`. Define `ApiKeyRepository`: `list_by_user`, `count_by_user`, `insert`, `delete_by_id_and_user`. Both traits are `Send + Sync + 'static` and consumed via `Arc<dyn …>` (see [architecture.md §6](../architecture.md#6-application-state--dependency-injection) — prefer `dyn Trait` per CLAUDE.md). The SeaORM entity files reuse the existing models from `m20250422_000001`; only the trait wrappers are new.
- **Spec reference**: [auth.md §7](../auth.md#7-authcontext), [routers/users-by-email.md §5 task 2](../routers/users-by-email.md#5-implementation-tasks).
- **Verify**: `cargo test -p cognee-database auth::tests::sqlite_inmem_round_trip`. Tests insert/find/delete through both traits against `sqlite::memory:`.

### Step 11: Custom path-scoped `Form` extractor (login error-envelope override)

- **File(s)**: `crates/http-server/src/middleware/validation.rs` (extend the P0 custom `Json` extractor stub).
- **Action**: Add a `LoginForm<T>(pub T)` newtype wrapping `axum::extract::Form<T>` whose `FromRequest` impl maps any deserialization error to `ApiError::LoginBadCredentials` (instead of the generic `ApiError::Validation`). This **only** applies to the `/api/v1/auth/login` handler — do not register it globally or `/register` will lose its structured detail array. See [routers/auth.md §2.1 — error responses row 2](../routers/auth.md#21-post-login--exchange-credentials-for-a-jwt) and [routers/auth.md §7 Python parity](../routers/auth.md#7-references) for the Python-side path-scoped override.
- **Spec reference**: [routers/auth.md §2.1](../routers/auth.md#21-post-login--exchange-credentials-for-a-jwt), [architecture.md §10](../architecture.md#10-request-validation).
- **Verify**: Router test — POST `/api/v1/auth/login` with empty body; assert `400` and exact body `{"detail": "LOGIN_BAD_CREDENTIALS"}`. POST `/api/v1/auth/register` with empty body; assert `400` and a `detail` array (the standard validation shape — proves the override is path-scoped).

### Step 12: `ApiError` enum extensions for new variants

- **File(s)**: `crates/http-server/src/error.rs` (extend the enum from P0).
- **Action**: Add the variants required by P1: `LoginBadCredentials`, `LoginUserNotVerified`, `RegisterUserAlreadyExists`, `RegisterInvalidPassword(String)`, `ResetPasswordBadToken`, `ResetPasswordInvalidPassword(String)`, `VerifyUserBadToken`, `VerifyUserAlreadyVerified`, `UpdateUserEmailAlreadyExists`, `UpdateUserInvalidPassword(String)`, and the **dedicated `ApiKeyEnvelope(String)` variant** for the unique `{"error": {"message": "..."}}` shape used only by the api-keys router ([api-keys.md §3](../routers/api-keys.md#3-cross-cutting-behavior), [routers/README.md §3.1](../routers/README.md#31-error-envelope)). Extend the `IntoResponse` impl so the structured-detail variants emit `{"detail": {"code": "...", "reason": "..."}}` (per [auth-register.md §3](../routers/auth-register.md#3-cross-cutting-behavior)) and the `ApiKeyEnvelope` variant emits `{"error": {"message": "..."}}` exactly. Do **not** let `ApiKeyEnvelope` leak into other routers.
- **Spec reference**: [architecture.md §9](../architecture.md#9-error-handling), [auth.md §8](../auth.md#8-endpoints), [api-keys.md §2.2](../routers/api-keys.md#22-post-apiv1authapi-keys--create-a-new-api-key).
- **Verify**: Inline tests render each new variant and assert the exact JSON body + status code. The `ApiKeyEnvelope` test asserts the body is exactly `{"error":{"message":"…"}}`, **not** `{"detail":"…"}`.

### Step 13: Implement `auth.rs` router (login / logout / me)

- **File(s)**: `crates/http-server/src/dto/auth.rs`, `crates/http-server/src/routers/auth.rs`, `crates/http-server/src/auth/login.rs` (helper module).
- **Action**: Add the three DTOs from [routers/auth.md §4](../routers/auth.md#4-dto-definitions) (`LoginPayloadDTO`, `LoginResponseDTO`, `MeShortResponseDTO`, `LogoutResponseDTO`). Add `auth::login::authenticate(state, email, password)` which (a) loads the user, (b) verifies the password, (c) re-hashes from bcrypt to argon2id when `VerifyOutcome::needs_rehash` is `true` (per [auth.md §6](../auth.md#6-password-hashing)). Add the three handlers per [routers/auth.md §5 — Implementation tasks](../routers/auth.md#5-implementation-tasks). The login handler returns a tuple `(Response<Body>)` with `Set-Cookie` header + JSON body. The `/me` handler returns **only `{"email"}`** — not the full UserRead — per [routers/auth.md §2.3](../routers/auth.md#23-get-me--current-user-shape). Wire `auth::router()` into `build_router` under `/api/v1/auth` per [architecture.md §7](../architecture.md#7-router-composition).
- **Spec reference**: [routers/auth.md](../routers/auth.md).
- **Verify**: `cargo test --test test_auth_login` (test file added in §5).

### Step 14: Implement `auth_register.rs`, `auth_reset_password.rs`, `auth_verify.rs`

- **File(s)**: `crates/http-server/src/dto/users.rs` (centralize `UserReadDTO` + `UserUpdatePayloadDTO` + `InvalidPasswordDetailDTO` per [users.md §4](../routers/users.md#4-dto-definitions)), `crates/http-server/src/dto/auth_register.rs`, `crates/http-server/src/dto/auth_reset_password.rs`, `crates/http-server/src/dto/auth_verify.rs`, `crates/http-server/src/auth/register.rs`, `crates/http-server/src/auth/reset.rs`, `crates/http-server/src/auth/verify.rs`, `crates/http-server/src/routers/auth_register.rs`, `crates/http-server/src/routers/auth_reset_password.rs`, `crates/http-server/src/routers/auth_verify.rs`.
- **Action**: One router file per per-router doc — see [routers/README.md §2](../routers/README.md#2-per-doc-template). Each file follows its spec **verbatim** for status codes, error envelopes, mailer hook invocation, `safe=True` coercion (register), `password_fgpt` claim (reset), and the post-update `UserRead` body (verify). Wire all three under `/api/v1/auth` in `build_router`. Email delivery is stubbed via `state.mailer` (`LoggingMailer` by default — see step 9; **SMTP deferred to P7**). The `/forgot-password` and `/request-verify-token` endpoints always return `(StatusCode::ACCEPTED, Json(serde_json::Value::Null))` — never reveal user existence.
- **Spec reference**: [routers/auth-register.md](../routers/auth-register.md), [routers/auth-reset-password.md](../routers/auth-reset-password.md), [routers/auth-verify.md](../routers/auth-verify.md), [auth.md §9](../auth.md#9-mailer-trait).
- **Verify**: `cargo test --test test_auth_register --test test_auth_reset --test test_auth_verify`.

### Step 15: Implement `api_keys.rs` router (list / create / delete)

- **File(s)**: `crates/http-server/src/dto/api_keys.rs`, `crates/http-server/src/auth/api_keys_service.rs`, `crates/http-server/src/routers/api_keys.rs`.
- **Action**: DTOs from [api-keys.md §4](../routers/api-keys.md#4-dto-definitions): `ApiKeyCreationPayloadDTO`, `ApiKeyListItemDTO`, `ApiKeyCreatedDTO`, `ApiKeyErrorEnvelopeDTO`, `ApiKeyErrorDetail`. Service layer: `list`, `create` (10-key cap → `ApiError::ApiKeyEnvelope("You have reached the maximum number of API keys.")`), `delete` (replicate the [api-keys.md §2.3](../routers/api-keys.md#23-delete-apiv1authapi-keysapi_key_id--delete) **500-on-missing-key Python quirk** — map `ApiKeyError::NotFound` to `ApiError::Internal(...)`, **not** to `NotFound(...)`). The list handler masks `key` to `"************"` (12 asterisks, exact) when `cfg.hash_api_key == true`. Mount under `/api/v1/auth/api-keys` in `build_router` (note: that is `nest("/auth/api-keys", api_keys::router())` — the Python mount of `prefix="/api/v1/auth"` plus the router's own `/api-keys` prefix, see [api-keys.md §1](../routers/api-keys.md#1-mount--file)).
- **Spec reference**: [routers/api-keys.md](../routers/api-keys.md), [auth.md §5](../auth.md#5-api-keys).
- **Verify**: `cargo test --test test_api_keys`.

### Step 16: Implement `users.rs` + `users_by_email.rs` routers

- **File(s)**: `crates/http-server/src/dto/users_by_email.rs`, `crates/http-server/src/auth/users_service.rs`, `crates/http-server/src/routers/users.rs`, `crates/http-server/src/routers/users_by_email.rs`.
- **Action**: Five fastapi-users CRUD handlers per [routers/users.md §5](../routers/users.md#5-implementation-tasks): `GET /me`, `PATCH /me` (`safe=True`), `GET /{id}`, `PATCH /{id}` (`safe=False`), `DELETE /{id}` (returns `204 No Content`, **not** `200 {}`; default-user guard rejects with `ApiError::Forbidden("Cannot delete the default user")`). The by-email handler returns `200 {"user_id": "..."}` on hit and `404 {"detail": "User not found"}` (the literal string, capital-U, two words — [users-by-email.md §2.1](../routers/users-by-email.md#21-post-get-user-id--resolve-email-to-uuid)). Mount both under `/api/v1/users` per [users-by-email.md §1](../routers/users-by-email.md#1-mount--file). Add the `RequireSuperuser` extractor to the three by-id routes.
- **Spec reference**: [routers/users.md](../routers/users.md), [routers/users-by-email.md](../routers/users-by-email.md).
- **Verify**: `cargo test --test test_users`.

### Step 17: OpenAPI annotations + per-handler `#[utoipa::path(...)]`

- **File(s)**: All `crates/http-server/src/routers/auth*.rs`, `api_keys.rs`, `users.rs`, `users_by_email.rs`. Plus `crates/http-server/src/openapi.rs` (component registration).
- **Action**: Add `#[utoipa::path(...)]` to every handler. `security = []` on the public endpoints listed in [auth.md §12](../auth.md#12-openapi-security-schemes): `/login`, `/register`, `/forgot-password`, `/reset-password`, `/request-verify-token`, `/verify`. All others inherit the global `[{BearerAuth: []}, {ApiKeyAuth: []}]` declared in P0. The two security schemes themselves are already registered in P0; this step only adds per-handler operation entries. Tags: `["auth"]` for everything in `/api/v1/auth/*` (including api-keys, per [api-keys.md §2.1](../routers/api-keys.md#21-get-apiv1authapi-keys--list-the-callers-api-keys)), `["users"]` for `/api/v1/users/*`. Operation ids match the fastapi-users names listed in each per-router doc (e.g. `auth:login`, `register:register`, `users:current_user`).
- **Spec reference**: [auth.md §12](../auth.md#12-openapi-security-schemes); each per-router doc's "OpenAPI" sub-bullet.
- **Verify**: `curl /openapi.json | jq '.paths."/api/v1/auth/login".post.security'` returns `[]`. Snapshot test diffs the generated JSON against a golden fixture.

### Step 18: Cross-SDK JWT compat fixture + bcrypt-migration fixture

- **File(s)**: `crates/http-server/tests/fixtures/auth/python_login_jwt.txt`, `python_bcrypt_hash.txt`, `python_argon2_hash.txt`.
- **Action**: Commit three text fixtures per [auth.md §13](../auth.md#13-testing-strategy): (1) a Python-issued login JWT minted with `secret="super_secret"`, `lifetime=3600`, `sub` = a fixed UUID, `iat` = a fixed unix timestamp; (2) a bcrypt hash of the string `"correct horse battery staple"`; (3) an argon2id hash of the same string. Generate fixtures one-time using a small Python script committed under `crates/http-server/tests/fixtures/auth/gen.py` so the values are reproducible (script not run by CI; outputs are checked in). The fixtures back the cross-impl tests in §5.
- **Spec reference**: [auth.md §3 — Compatibility test](../auth.md#3-jwt-format), [auth.md §13](../auth.md#13-testing-strategy).
- **Verify**: `cargo test --test test_jwt_cross_compat --test test_password_hash_migration`.

## 5. Tests

Eight integration test files under `crates/http-server/tests/`, each driven via `tower::ServiceExt::oneshot` against the assembled router (no socket bind — see [architecture.md §18 — Layer 2](../architecture.md#18-testing-architecture)). Use a fresh `sqlite::memory:` `AppState` per test via the `support::build_test_state()` helper (already exists in `crates/http-server/tests/support/mod.rs` from P0); P1 extends this module with additional helper functions listed below.

| Test file | Coverage |
|---|---|
| `tests/test_auth_login.rs` | `POST /login` form-body happy path → 200 with `{access_token, token_type:"bearer"}` + `Set-Cookie` header containing `HttpOnly; Path=/; SameSite=Lax`; bad creds → `400 {"detail":"LOGIN_BAD_CREDENTIALS"}`; missing `username` field → same `LOGIN_BAD_CREDENTIALS` envelope (proves the path-scoped override from step 11); inactive user → `LOGIN_BAD_CREDENTIALS` (per [auth.md §6 q3](../routers/auth.md#6-open-questions)); `POST /logout` → 200 + deletion cookie; `GET /me` with each of cookie / bearer / `X-Api-Key` → 200 with `{"email":"…"}` only. |
| `tests/test_auth_register.rs` | Happy path → 201 + `UserReadDTO`; duplicate email → `400 {"detail":"REGISTER_USER_ALREADY_EXISTS"}`; password contains email → `400 {"detail":{"code":"REGISTER_INVALID_PASSWORD","reason":"…"}}`; `safe=True` coercion (client sends `is_superuser=true` → response has `is_superuser=false`); empty body → `400` with `detail` **array** (proves register kept the structured-detail validation, while login rewrites it). |
| `tests/test_auth_reset.rs` | `POST /forgot-password` for unknown email → 202 + `null` body + zero mailer events; for known email → 202 + `null` + one `password_reset` event in `ConsoleMailer`; `POST /reset-password` with bad token / expired token / audience-mismatched token (login JWT) → 400 `RESET_PASSWORD_BAD_TOKEN`; weak password → 400 structured envelope; happy path → 200 + DB row updated to argon2id hash. |
| `tests/test_auth_verify.rs` | `POST /request-verify-token` for unknown / inactive / already-verified email → 202 + null + zero events; for pending user → 202 + null + one `email_verify` event; `POST /verify` with bad token → 400 `VERIFY_USER_BAD_TOKEN`; for already-verified user → 400 `VERIFY_USER_ALREADY_VERIFIED`; happy path → 200 + post-update `UserReadDTO` with `is_verified=true`. |
| `tests/test_api_keys.rs` | Full create-list-delete cycle; **max-keys cap** at 10 → `400 {"error":{"message":"You have reached the maximum number of API keys."}}` (proves the unique error envelope from step 12); listing with `HASH_API_KEY=true` masks `key` to `"************"` (exactly 12 asterisks); `DELETE` of non-existent UUID → 500 (the documented Python quirk from [api-keys.md §2.3](../routers/api-keys.md#23-delete-apiv1authapi-keysapi_key_id--delete)); cross-user `DELETE` (try to delete user B's key while authenticated as user A) → also 500 (filter is `WHERE user_id = caller.id`). |
| `tests/test_users.rs` | `GET /me` → full `UserReadDTO`; `PATCH /me` with `is_superuser=true` → 200 with `is_superuser` unchanged (silent-strip); `PATCH /me` with conflicting email → 400 string-detail; `PATCH /me` with weak password → 400 structured-detail; `GET /{id}` as non-superuser → 403; as superuser with valid id → 200; with malformed UUID → 404 (collapsed); `PATCH /{id}` as superuser with `is_superuser=false` on target → 200 + row updated; `DELETE /{id}` on default user → 403; on regular user → 204; **`POST /get-user-id` happy path → 200 + UUID; missing user → 404 + exact `"User not found"` body; case-mismatch → 404 (case-sensitive parity)**. |
| `tests/test_jwt_cross_compat.rs` | Decode the Python-issued JWT fixture from step 18 with `decode_login_jwt`; assert `sub`/`aud`/`exp` match the documented values from [auth.md §3](../auth.md#3-jwt-format). Also encode a Rust JWT with the same fixed `(secret, sub, iat)` and assert the resulting token string equals a Python-issued snapshot byte-for-byte (pins canonical JSON serialization — claim order, no whitespace). |
| `tests/test_password_hash_migration.rs` | Verify the bcrypt hash fixture authenticates `"correct horse battery staple"`; on a successful login flow, assert the stored hash in the DB is **rewritten to argon2id** afterwards (read the `users.hashed_password` column and assert it starts with `$argon2id$`); verify the argon2id fixture round-trips. |

All tests run under `cargo test -p cognee-http-server` in debug mode (no `--release` per the project CLAUDE.md).

### Test scaffolding

`crates/http-server/tests/support/mod.rs` (added in P0; extended here) exposes:

- `build_test_state() -> AppState` — already in P0; P1 upgrades its implementation to run P1 migrations against `sqlite::memory:`, inject a `ConsoleMailer`, and set `HASH_API_KEY=false`. Each test calls this once.
- `build_test_state_with(cfg_overrides) -> AppState` — new in P1; same as above with a closure that mutates the `AuthContext` (e.g. `cfg.hash_api_key = true`, `cfg.require_authentication = false`) before assembly.
- `bearer(user) -> HeaderValue`, `cookie(user) -> HeaderValue` — mint a login JWT for the given user and wrap it in the appropriate header.
- `seed_user(state, email, password) -> User` — calls `auth::register::create_user` and returns the row.
- `seed_superuser(state, ...)` — same but flips `is_superuser=true` after insert.

These helpers keep each test ≤30 lines and avoid duplicating the `AppState` build dance.

### Common parity pitfalls (read once before starting the router handlers)

These are the byte-level gotchas that the per-router docs flag but that are easy to miss when reading sequentially. Each has bitten Python ports of fastapi-users in the past.

1. **`/api/v1/auth/me` returns ONLY `{"email"}`** — not the full `UserRead`. [auth.md §8.3](../auth.md#83-apiv1authme--get) and [routers/auth.md §2.3](../routers/auth.md#23-get-me--current-user-shape) both confirm the narrow `{"email": "<str>"}` shape. The discrepancy noted in [routers/auth.md §6 q1](../routers/auth.md#6-open-questions) has been resolved — auth.md §8.3 was corrected. Emit only `email`.
2. **`/api/v1/users/me` returns the full `UserReadDTO`** — six fields including `tenant_id`. Do not collapse with `/auth/me`.
3. **`safe=True` is silent.** Register and `PATCH /me` accept `is_superuser` / `is_active` / `is_verified` in the request body and silently drop them. Returning a 400 "field not allowed" is wrong; mirror Python's silent strip.
4. **`DELETE /api/v1/users/{id}` is `204`, not `200 {}`.** Many clients pattern-match on the status code, not the body.
5. **The 12-asterisk masked-key sentinel is exact.** Not 16, not `"<redacted>"`. Matters when `HASH_API_KEY=true`. See [api-keys.md §2.1](../routers/api-keys.md#21-get-apiv1authapi-keys--list-the-callers-api-keys).
6. **The api-keys error envelope is unique.** `{"error": {"message": "..."}}`, never `{"detail": "..."}`. Only the `ApiError::ApiKeyEnvelope` variant emits this; do not let any other handler reuse it ([routers/README.md §3.1](../routers/README.md#31-error-envelope)).
7. **`DELETE /api/v1/auth/api-keys/{id}` of a missing key returns 500.** This is a Python bug we replicate for wire compat; document via `#[ignore]`-flagged future-fix marker but assert the 500 in the test today.
8. **The case-sensitivity of `/get-user-id`.** Python uses `WHERE email = ?` with no `LOWER()`; we replicate. The test must include a case-mismatch case asserting 404 ([users-by-email.md §6 q1](../routers/users-by-email.md#6-open-questions)).
9. **`/forgot-password` and `/request-verify-token` are 202 + literal `null` body** (not `{}`, not 200). Always-202 prevents user-existence enumeration.
10. **Reset / verify JWTs use different secrets and audiences.** A login JWT presented to `/reset-password` must be rejected with `RESET_PASSWORD_BAD_TOKEN`. The audience validator is the only line of defense; do not skip `Validation::set_audience(&[…])`.
11. **Re-hash on login is best-effort.** If the `UPDATE users SET hashed_password = ?` fails (e.g. transient DB error) the login still succeeds; log a `tracing::warn!` and move on. Never fail a successful auth because the rehash failed.
12. **`HASH_API_KEY` is deployment-wide, not per-row.** Switching it mid-deployment invalidates every key. Emit a startup `tracing::warn!` if any row's `api_key` length contradicts the configured mode (per [auth.md §11 — Mode-switch concern](../auth.md#11-database-schema-seaorm-migration)).

### Test invocation

Most tests run under plain `cargo test -p cognee-http-server`. The cross-impl tests (`test_jwt_cross_compat`, `test_password_hash_migration`) read fixtures from `tests/fixtures/auth/` and are CI-friendly (no network, no LLM, no embedding model — they are pure crypto round-trips). The `bash scripts/run_tests_with_openai.sh` driver from CLAUDE.md is **not** required for P1 — these tests do not call any LLM.

## 6. Acceptance criteria

- [x] `cargo check --all-targets -p cognee-http-server` is clean.
- [x] `cargo check --all-targets` (workspace) is clean — proves the new crate did not break a sibling.
- [x] All eight P1 test files in §5 pass under `cargo test -p cognee-http-server`.
- [x] `scripts/check_all.sh` is green (formatting, clippy with `-D warnings`, capi check, python check, js check).
- [x] **Cross-SDK JWT round-trip verified**: a Rust-issued login token authenticates against a Python uvicorn server, **and** a Python-issued login token authenticates against `cognee-http-server`. Run by hand against a local uvicorn during phase review; codify in P8 e2e harness.
- [x] **Cross-SDK API-key compat (default `HASH_API_KEY=false`)**: create a key in Rust via `POST /api/v1/auth/api-keys`, send the raw key as `X-Api-Key` to a Python uvicorn pointing at the same DB; Python authenticates the request. The reverse direction also works.
- [x] [implementation/README.md](README.md) status table — flip the P1 row from **Draft** to **Done**.
- [x] [routers/README.md](../routers/README.md) status table — flip rows 2–8 (auth, auth-register, auth-reset-password, auth-verify, api_keys, users, users-by-email) from **Draft** to **Done**.

## 7. Files touched

New files (created in this phase):

```
crates/database/src/migrator/m20260427_000001_http_auth_columns.rs
crates/database/src/auth.rs
crates/http-server/src/auth/mod.rs
crates/http-server/src/auth/context.rs
crates/http-server/src/auth/jwt.rs
crates/http-server/src/auth/cookie.rs
crates/http-server/src/auth/api_key.rs
crates/http-server/src/auth/api_keys_service.rs
crates/http-server/src/auth/password.rs
crates/http-server/src/auth/extractor.rs
crates/http-server/src/auth/mailer.rs
crates/http-server/src/auth/login.rs
crates/http-server/src/auth/register.rs
crates/http-server/src/auth/reset.rs
crates/http-server/src/auth/verify.rs
crates/http-server/src/auth/users_service.rs
crates/http-server/src/dto/auth.rs
crates/http-server/src/dto/auth_register.rs
crates/http-server/src/dto/auth_reset_password.rs
crates/http-server/src/dto/auth_verify.rs
crates/http-server/src/dto/api_keys.rs
crates/http-server/src/dto/users.rs
crates/http-server/src/dto/users_by_email.rs
crates/http-server/src/routers/auth.rs
crates/http-server/src/routers/auth_register.rs
crates/http-server/src/routers/auth_reset_password.rs
crates/http-server/src/routers/auth_verify.rs
crates/http-server/src/routers/api_keys.rs
crates/http-server/src/routers/users.rs
crates/http-server/src/routers/users_by_email.rs
crates/http-server/tests/test_auth_login.rs
crates/http-server/tests/test_auth_register.rs
crates/http-server/tests/test_auth_reset.rs
crates/http-server/tests/test_auth_verify.rs
crates/http-server/tests/test_api_keys.rs
crates/http-server/tests/test_users.rs
crates/http-server/tests/test_jwt_cross_compat.rs
crates/http-server/tests/test_password_hash_migration.rs
crates/http-server/tests/fixtures/auth/python_login_jwt.txt
crates/http-server/tests/fixtures/auth/python_bcrypt_hash.txt
crates/http-server/tests/fixtures/auth/python_argon2_hash.txt
crates/http-server/tests/fixtures/auth/gen.py
```

Existing files modified:

```
crates/database/src/migrator/mod.rs            # register the new migration
crates/database/src/lib.rs                     # re-export auth module
crates/database/Cargo.toml                     # add argon2 / bcrypt deps if needed
crates/http-server/Cargo.toml                  # add jsonwebtoken, argon2, bcrypt, cookie, secrecy, subtle, rand, sha2
crates/http-server/src/lib.rs                  # re-export auth module
crates/http-server/src/error.rs                # add ApiError variants from step 12
crates/http-server/src/state.rs                # add Mailer + AuthContext slots if not in P0
crates/http-server/src/middleware/validation.rs# add path-scoped LoginForm extractor
crates/http-server/src/openapi.rs              # register new operation entries
crates/http-server/src/lib.rs (build_router)   # nest auth/api-keys/users routers
docs/http-server/implementation/README.md      # flip P1 status to Done at end of phase
docs/http-server/routers/README.md             # flip rows 2-8 to Done at end of phase
```

Out-of-scope for P1 (do not touch — covered in later phases):

- `SmtpMailer` impl ([auth.md §9](../auth.md#9-mailer-trait)) — P7.
- Tenants / RBAC tables and the `/api/v1/permissions` router — P5 ([tenants.md](../tenants.md)).
- WebSocket auth handshake — P3 ([websocket.md](../websocket.md)).
- Pipeline-job dispatch — P3 ([pipelines.md](../pipelines.md)).
- Cross-SDK pytest harness — P8 ([e2e-parity.md](../e2e-parity.md)).
