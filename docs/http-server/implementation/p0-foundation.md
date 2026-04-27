# Implementation: P0 — Foundation

> **Status: Done — commit 323e3e1**
>
> Minor deviation from spec: Health DTOs are defined inline in `src/routers/health.rs` rather than in a separate `src/dto/health.rs`. This is acceptable for P0 scope and can be refactored into a dedicated DTO module in a later phase if needed.

## 1. Goal

Land the `crates/http-server/` Cargo package — the new `cognee-http-server` crate that hosts both the embeddable `axum` library and the standalone `cognee-http-server` binary. Wire up `AppState`, `ApiError`, the configuration loader, the CORS / tracing / validation middleware, the `utoipa` OpenAPI document, the startup-lifecycle entry point, the root `/`, and the public health router (`/health` + `/health/detailed`). The phase ends with a buildable binary that boots, three integration tests covering health/root/OpenAPI/CORS, and the `cognee-lib` `server` feature plumbing in place. No auth, no DTOs beyond health, no business logic delegation — all of that lands in P1 and onwards.

## 2. References (read these before starting)

- Design rationale: [architecture.md](../architecture.md) — §3 crate topology, §6 AppState, §9 error model, §17 binary wiring, §13 OpenAPI, §14 startup lifecycle, §20 library list.
- Endpoint contract: [routers/health.md](../routers/health.md).
- Phase summary: [plan.md §4 P0](../plan.md#4-implementation-phases).
- Cross-router conventions (envelope exceptions, auth declaration, telemetry): [routers/README.md §3](../routers/README.md#3-cross-router-conventions).
- Implementation invariants (atomic steps, no rationale duplication, strict-Python-parity): [implementation/README.md](README.md).

## 3. Prerequisites

None — this is the first phase. P0 must land before any other HTTP-server implementation phase begins. Note: `cognee_core::PipelineRunRegistry` already exists (landed in commit 2425f19 as part of P3-prereq); this phase lands the `AppState::pipelines` slot as `Option<Arc<dyn PipelineRunRegistry>>` defaulting to `None` and lets P3 fill it in with a concrete `DefaultPipelineRunRegistry`.

**Workspace blocker**: the workspace-root `Cargo.toml` patches `hyper` to the Qdrant v0.14 fork (`[patch.crates-io] hyper = { git = "…", branch = "v0.14.26-qdrant" }`). `axum 0.8` and `tower-http 0.6` require `hyper 1.x`. The implementation agent must declare `axum`, `tower`, `tower-http`, and `hyper` as direct (non-workspace) deps in `crates/http-server/Cargo.toml` so that Cargo resolves two separate versions (v0.14 for the qdrant path, v1.x for the http-server path). No global `[workspace.dependencies]` entries for axum/tower/tower-http/hyper should be added. Confirm with `cargo metadata` that the two hyper versions coexist cleanly before proceeding to wiring steps.

## 4. Step-by-step

### Step 1: Create `crates/http-server/Cargo.toml`

- **File(s)**: `crates/http-server/Cargo.toml`.
- **Action**: Create a new Cargo manifest declaring `[package] name = "cognee-http-server"`, both `[lib] name = "cognee_http_server"` and `[[bin]] name = "cognee-http-server"` with `required-features = ["bin"]`, and a `[features]` section containing `default = []` plus `bin = ["dep:clap", "dep:dotenv", "tokio/macros", "tokio/rt-multi-thread", "tokio/signal"]`. Library `[dependencies]` are the full list from [architecture.md §16 / §20](../architecture.md#16-feature-gates) (axum 0.8, tower 0.5, tower-http 0.6 with `cors`/`trace`/`limit`, hyper 1.x, cognee-lib, cognee-models, serde, serde_json, utoipa 5 with `axum_extras`/`uuid`/`chrono`, jsonwebtoken 9, argon2 0.5, cookie 0.18, uuid, chrono, thiserror, anyhow, tracing, tracing-subscriber, secrecy 0.10, dashmap 6, bytes, futures, async-trait). Binary-only deps (`clap`, `dotenv`) are `optional = true`. Pull workspace versions where available; pin the axum/tower/tower-http/utoipa/jsonwebtoken/argon2/cookie/secrecy/dashmap versions inline. **IMPORTANT**: do NOT use `hyper = { workspace = true }` — the workspace `[patch.crates-io]` pins hyper to a v0.14 Qdrant fork; axum 0.8 requires hyper 1.x. Declare `hyper = { version = "1", features = ["server", "http1", "http2"] }` directly so Cargo resolves both versions in parallel.
- **Spec reference**: [architecture.md §3 (Cargo.toml shape)](../architecture.md#3-crate-topology) and [§20 (library list)](../architecture.md#20-selected-libraries-summary).
- **Verify**: `cargo metadata --format-version 1 --no-deps -p cognee-http-server > /dev/null`.

### Step 2: Add the crate to the workspace

- **File(s)**: `Cargo.toml` (workspace root).
- **Action**: Append `"crates/http-server"` to the `[workspace] members` array. Preserve alphabetical ordering if the existing list is sorted; otherwise append at the end and match the surrounding style. Do not change resolver, edition, or workspace dependency entries in this step.
- **Spec reference**: [architecture.md §3](../architecture.md#3-crate-topology).
- **Verify**: `cargo check -p cognee-http-server` (will fail until later steps land src files; the manifest itself must be acceptable to Cargo — `cargo metadata --no-deps` should succeed).

### Step 3: Create `src/lib.rs` skeleton

- **File(s)**: `crates/http-server/src/lib.rs`.
- **Action**: Add `pub mod` declarations for `state`, `error`, `config`, `middleware`, `openapi`, `lifecycle`, `routers`. Define two top-level async functions: `pub async fn build_router(state: state::AppState) -> Result<axum::Router, error::ServerError>` and `pub async fn run(addr: std::net::SocketAddr, state: state::AppState) -> Result<(), error::ServerError>`. For this step, both functions can be stubs that compile (e.g. `build_router` returns an empty `axum::Router::new()`, `run` calls `build_router` then `axum::serve`). Re-export `state::AppState`, `config::HttpServerConfig`, `error::{ApiError, ServerError}`. Final wiring (mounting routers, applying layers) lands in Step 13.
- **Spec reference**: [architecture.md §3 (crate topology), §5 (run signature), §7 (build_router)](../architecture.md#3-crate-topology).
- **Verify**: `cargo check -p cognee-http-server` once the modules referenced exist as empty files (after step 9 the check passes).

### Step 4: Create `src/state.rs` with `AppState`

- **File(s)**: `crates/http-server/src/state.rs`.
- **Action**: Define `#[derive(Clone)] pub struct AppState` with the fields listed in [architecture.md §6](../architecture.md#6-application-state--dependency-injection): `lib`, `auth`, `pipelines`, `spans`, `sync`, `config`. For P0, downscope every field that depends on un-landed phases:
  - `pub config: Arc<HttpServerConfig>` — populated.
  - `pub pipelines: Option<Arc<dyn cognee_core::PipelineRunRegistry>>` — `None` for now. `cognee_core::PipelineRunRegistry` already exists (landed in commit 2425f19 as part of P3-prereq). Annotate the field with `// TODO(P3): wire concrete DefaultPipelineRunRegistry here` so the slot is unmissable.
  - `pub lib`, `pub auth`, `pub spans`, `pub sync` — declare the field with the eventual `Arc<...>` type wrapped in `Option<...>` (or a placeholder `()` newtype) so the struct compiles before those crates exist. Each field carries a `// TODO(P<N>): wire <thing> in <phase>` comment matching the phase that owns it.
  - Provide `pub async fn AppState::build(config: HttpServerConfig) -> Result<Self, error::ServerError>` that constructs the struct with defaults / `None`s. The signature is what the binary's `main()` calls in step 14, so it must be stable.
  - `Arc` everything on the inside; the outer struct is `Clone` and shared per request.
- **Spec reference**: [architecture.md §6](../architecture.md#6-application-state--dependency-injection).
- **Verify**: `cargo check -p cognee-http-server`.

### Step 5: Create `src/error.rs`

- **File(s)**: `crates/http-server/src/error.rs`.
- **Action**: Define `pub enum ApiError` with the variants from [architecture.md §9](../architecture.md#9-error-handling) (`BadRequest(String)`, `Unauthorized`, `Forbidden(String)`, `NotFound(String)`, `Conflict(String)`, `Validation(ValidationDetails)`, `LoginBadCredentials`, `PipelineErrored(String)`, `Teapot`, `Internal(anyhow::Error)`) plus a `ValidationDetails` struct holding `detail: serde_json::Value` and `body: Option<serde_json::Value>`. Implement `axum::response::IntoResponse for ApiError` so each variant emits the Python-shaped JSON envelope. Also define `pub enum ServerError` (a `thiserror`-derived enum used at startup/run-time wrapping `std::io::Error`, `axum::Error`, `LifecycleError`, `anyhow::Error`). Add unit tests for the `IntoResponse` mapping covering at least `BadRequest`, `Unauthorized`, `Validation`, and `LoginBadCredentials`.
- **Spec reference**: [architecture.md §9](../architecture.md#9-error-handling). Envelope-deviation table: [routers/README.md §3.1](../routers/README.md#31-error-envelope) — health is on the deviation list and bypasses `ApiError` entirely.
- **Verify**: `cargo test -p cognee-http-server --lib error::tests`.

### Step 6: Create `src/config.rs` with `HttpServerConfig`

- **File(s)**: `crates/http-server/src/config.rs`.
- **Action**: Define `pub struct HttpServerConfig` with the fields enumerated in [architecture.md §11](../architecture.md#11-configuration) (`host`, `port`, `cors_allowed_origins: Vec<String>`, `ui_app_url`, `env: Environment`, `require_authentication`, `jwt_secret: SecretString`, `jwt_lifetime: Duration`, `body_limit: usize`). Provide `impl Default` matching the documented defaults (host `0.0.0.0`, port `8000`, `ui_app_url = "http://localhost:3000"`, `body_limit = 100 * 1024 * 1024`, `jwt_lifetime = 3600 s`). Add `pub fn from_env() -> Result<Self, ServerError>` that reads the matching env vars and overlays them on the defaults. `Environment` is a small enum (`Dev | Prod | Test`) with `FromStr`. Do not invoke this from inside routers — only `main()` and tests.
- **Spec reference**: [architecture.md §11](../architecture.md#11-configuration).
- **Verify**: `cargo test -p cognee-http-server --lib config::tests` (cover at least the defaults and one env-override case).

### Step 7: Create `src/middleware/cors.rs`

- **File(s)**: `crates/http-server/src/middleware/cors.rs`, `crates/http-server/src/middleware/mod.rs` (with `pub mod cors;`).
- **Action**: Implement `pub fn cors_layer(config: &HttpServerConfig) -> tower_http::cors::CorsLayer`. Resolve the origin list using `config.cors_allowed_origins` first, falling back to `[config.ui_app_url.clone()]` when the list is empty. Methods: `OPTIONS, GET, PUT, POST, DELETE`. `allow_credentials(true)`. `allow_headers(Any)`. Match the FastAPI defaults exactly so existing browser clients send credentials with no extra opt-in.
- **Spec reference**: [architecture.md §8](../architecture.md#8-middleware-stack).
- **Verify**: `cargo check -p cognee-http-server`. The behavioral check is the integration test in step 16.

### Step 8: Create `src/middleware/tracing.rs`

- **File(s)**: `crates/http-server/src/middleware/tracing.rs`.
- **Action**: Export `pub fn trace_layer() -> TraceLayer<...>` returning a configured `tower_http::trace::TraceLayer` with span fields `method`, `uri`, `status`, `latency_ms`. The library does NOT install a global subscriber — that is the binary's responsibility (step 14). Keep the layer generic so the binary, library embedders, and tests share one access-log shape. Configure on-failure to log at `error`, on-response at `debug` (per [observability.md §7](../observability.md#7-access-logging) the default access-log filter drops sub-`warn` lines for `/health` so probe traffic doesn't drown the log; that filter is applied via `tracing_subscriber::EnvFilter` in the binary, not here).
- **Spec reference**: [architecture.md §12](../architecture.md#12-logging--observability).
- **Verify**: `cargo check -p cognee-http-server`.

### Step 9: Create `src/middleware/validation.rs`

- **File(s)**: `crates/http-server/src/middleware/validation.rs`.
- **Action**: Define a custom `pub struct Json<T>(pub T)` that implements `axum::extract::FromRequest`. On `serde_json` failure it returns `ApiError::Validation` with the Python-shaped `{detail: [...], body: ...}` envelope rather than axum's default 400. This is the extractor every later router uses in place of `axum::Json`. Cross-field validation lives on the DTO (per `routers/README.md §3.5`); this extractor only handles the deserialization edge.
- **Spec reference**: [architecture.md §10](../architecture.md#10-request-validation).
- **Verify**: `cargo test -p cognee-http-server --lib middleware::validation::tests` — assert that a missing-required-field payload yields `ApiError::Validation` with the right detail shape.

### Step 10: Create `src/openapi.rs`

- **File(s)**: `crates/http-server/src/openapi.rs`.
- **Action**: Define a single root struct `pub struct ApiDoc` annotated with `#[derive(utoipa::OpenApi)]`. Declare `info(title = "Cognee API", version = "1.0.0")`. Register the security schemes `BearerAuth` (HTTP bearer, JWT) and `ApiKeyAuth` (`X-Api-Key` header) per [architecture.md §13](../architecture.md#13-openapi-generation--utoipa). Keep the `paths(...)` list empty for P0; routers register themselves into this list via `OpenApi::paths`-style additions in their own phases. Add `pub async fn openapi_json() -> impl IntoResponse` that returns `Json(ApiDoc::openapi())`. The route is registered in `build_router` (step 13).
- **Spec reference**: [architecture.md §13](../architecture.md#13-openapi-generation--utoipa).
- **Verify**: `cargo test -p cognee-http-server --test test_openapi` (added in step 16).

### Step 11: Create `src/lifecycle.rs`

- **File(s)**: `crates/http-server/src/lifecycle.rs`.
- **Action**: Implement `pub async fn on_startup(state: &AppState) -> Result<(), LifecycleError>` per [architecture.md §14](../architecture.md#14-startup-lifecycle). For P0, the body is just `run_startup_migrations(&state.lib.db()).await?;` (a stub helper if `state.lib` is still a placeholder — return `Ok(())` and log `tracing::debug!("startup migrations skipped: lib slot not yet wired")`), then `tracing::info!("Backend server has started");`. **Do not** add `bootstrap_default_principals` — that lands in P5. Define `pub enum LifecycleError` (thiserror) with one `MigrationFailed` variant for now. `build_router` will call `on_startup` before returning the assembled router (step 13).
- **Spec reference**: [architecture.md §14](../architecture.md#14-startup-lifecycle).
- **Verify**: `cargo check -p cognee-http-server`.

### Step 12: Create `src/routers/health.rs`

- **File(s)**: `crates/http-server/src/routers/health.rs`, `crates/http-server/src/routers/mod.rs` (`pub mod health;`).
- **Action**: Implement `pub fn router() -> axum::Router<AppState>` registering `GET /` → `get_shallow` and `GET /detailed` → `get_detailed`. The inner paths are `/` and `/detailed` because the `/health` prefix is supplied by `.nest()` in `build_router` (step 13). For P0, the `cognee_lib::health::HealthChecker` trait is not yet landed — implement a local `MockHealthChecker` (or read from the placeholder field on `AppState`) that returns `HEALTHY` for both shallow and detailed and an empty `components` map for detailed. Implement the success/failure JSON shapes verbatim per [routers/health.md §2](../routers/health.md#2-endpoints) and [§4 DTO definitions](../routers/health.md#4-dto-definitions): both endpoints bypass the `ApiError` envelope and return `axum::response::Response` directly. Status `200` when HEALTHY/DEGRADED on shallow, `503` when UNHEALTHY; `200` when HEALTHY on detailed, `503` on DEGRADED *or* UNHEALTHY. Keys differ between shapes — `health` (shallow) vs `status` (detailed) for the enum, and `reason` (shallow failure) vs `error` (detailed failure) for the failure-path message — replicate verbatim, do not unify. Add `#[utoipa::path(... security(()) ...)]` annotations so OpenAPI marks both endpoints as public. Inline unit tests cover HEALTHY/UNHEALTHY/DEGRADED matrices for both paths and the panic-in-checker failure shape, exactly as the per-router doc lists.
- **Spec reference**: [routers/health.md §2 / §4 / §5](../routers/health.md#2-endpoints).
- **Verify**: `cargo test -p cognee-http-server --lib routers::health::tests`.

### Step 13: Wire up `build_router`

- **File(s)**: `crates/http-server/src/lib.rs`.
- **Action**: Replace the stubbed `build_router` from step 3 with the real assembly. Mount `health::router()` at `/health` (NOT `/api/v1/health`). Add the root `/` route returning `axum::Json(serde_json::json!({"message": "Hello, World, I am alive!"}))` for Python parity. Register `GET /openapi.json` to `openapi::openapi_json`. Apply the layer stack from [architecture.md §8](../architecture.md#8-middleware-stack), outer-to-inner: `TraceLayer`, `CorsLayer` (from step 7), `DefaultBodyLimit::max(state.config.body_limit)` (default 100 MiB). `.with_state(state.clone())` last. Call `lifecycle::on_startup(&state).await?` before returning. Do NOT mount any `/api/v1/*` routers in P0 — that scaffold lands as part of P1 wiring.
- **Spec reference**: [architecture.md §7 / §8 / §14](../architecture.md#7-router-composition).
- **Verify**: `cargo check -p cognee-http-server` and the integration tests added in step 16.

### Step 14: Create `src/main.rs` (binary entry point)

- **File(s)**: `crates/http-server/src/main.rs`.
- **Action**: Add the standalone binary entry point per [architecture.md §17](../architecture.md#17-binary-cognee-http-server). `#[tokio::main(flavor = "multi_thread")] async fn main() -> anyhow::Result<()>`. Steps inside: (a) `let _ = dotenv::dotenv();` — loading `.env` is a binary-only concern; (b) `init_tracing()` — installs `tracing_subscriber::fmt` with `EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,ort=warn"))`. JSON output when `ENV=prod`, pretty when `ENV=dev`; (c) `let args = Args::parse();` — flat clap struct (no subcommands) with `--host`/`--port`/`--config`/`--cors-allowed-origins`/`--env`, each falling back to its env var via `#[arg(env = "...")]`; (d) `let cfg = HttpServerConfig::load(&args)?;`; (e) `let state = AppState::build(cfg.clone()).await?;`; (f) `let addr = format!("{}:{}", cfg.host, cfg.port).parse::<SocketAddr>()?;`; (g) `cognee_http_server::run(addr, state).await?;`. The binary is gated by the `bin` feature so library consumers do not pay the cost of `clap`/`dotenv`/`tokio` macros.
- **Spec reference**: [architecture.md §17](../architecture.md#17-binary-cognee-http-server).
- **Verify**: `cargo build -p cognee-http-server --features bin --bin cognee-http-server`. Run the binary with `--help` to confirm clap parsing.

### Step 15: Wire `cognee-lib::http` re-export

- **File(s)**: `crates/lib/Cargo.toml`, `crates/lib/src/lib.rs`.
- **Action**: In `crates/lib/Cargo.toml` add `cognee-http-server = { path = "../http-server", optional = true }` to `[dependencies]` and `server = ["dep:cognee-http-server"]` to `[features]`. **Do not** add `server` to `cognee-lib`'s `default` features (per [architecture.md §3.3](../architecture.md#3-crate-topology) and §16). In `crates/lib/src/lib.rs` add at module scope:
  ```rust
  #[cfg(feature = "server")]
  pub mod http {
      //! HTTP server surface; available only when the `server` feature is enabled.
      pub use cognee_http_server::*;
  }
  ```
  Confirm `cognee-cli`'s `Cargo.toml` does NOT enable `cognee-lib/server` and does NOT depend on `cognee-http-server`.
- **Spec reference**: [architecture.md §3.3 / §16](../architecture.md#3-crate-topology).
- **Verify**: `cargo check -p cognee-lib` (default features — must not pull axum); `cargo check -p cognee-lib --features server` (must pull and compile `cognee-http-server`).

### Step 16: Integration tests

- **File(s)**: `crates/http-server/tests/test_health.rs`, `crates/http-server/tests/test_root.rs`, `crates/http-server/tests/test_openapi.rs`, `crates/http-server/tests/test_cors.rs`, `crates/http-server/tests/support/mod.rs` (shared helpers — `build_test_state()`, `oneshot_get(...)`, etc.).
- **Action**: All tests use `tower::ServiceExt::oneshot` against a `cognee_http_server::build_router(state).await?` — no socket binding needed for these. `test_health.rs`: HEALTHY shallow → 200 + `{status: "ready", health: "healthy", version}`; UNHEALTHY shallow → 503 + `{status: "not ready", health: "unhealthy", ...}`; DEGRADED shallow → 200 (parity guard); HEALTHY detailed → 200 + body has all four critical-component keys present; DEGRADED detailed → 503 + full body (parity guard); panicking checker → shallow returns `{status, reason}`, detailed returns `{status, error}` (assert exact key names). `test_root.rs`: `GET /` → 200 + `{"message": "Hello, World, I am alive!"}`. `test_openapi.rs`: `GET /openapi.json` → 200 + valid JSON parseable as `serde_json::Value`; `components.securitySchemes` contains both `BearerAuth` and `ApiKeyAuth`. `test_cors.rs`: build a config with `cors_allowed_origins = ["http://example.test"]`, send `OPTIONS /health` with `Origin: http://example.test`, assert response carries `Access-Control-Allow-Origin: http://example.test`, `Access-Control-Allow-Credentials: true`, and that `Access-Control-Allow-Methods` contains the documented set; send a second preflight from a non-allowlisted origin and assert it does NOT echo the origin back. Skip the cross-SDK harness wiring — that lands in P8.
- **Spec reference**: [architecture.md §18](../architecture.md#18-testing-architecture), [routers/health.md §5 (tasks 7–8)](../routers/health.md#5-implementation-tasks).
- **Verify**: `cargo test -p cognee-http-server --tests`.

## 5. Tests

- `crates/http-server/tests/test_health.rs` — shallow + detailed, both 200 and 503 paths, plus the DEGRADED parity asymmetry (shallow stays 200, detailed flips 503) and the panic-in-checker failure shapes (`reason` vs `error` key names).
- `crates/http-server/tests/test_root.rs` — `GET /` returns the literal `{"message": "Hello, World, I am alive!"}` body and a 200 status.
- `crates/http-server/tests/test_openapi.rs` — `GET /openapi.json` returns valid JSON; the schema declares both `BearerAuth` and `ApiKeyAuth` security schemes; the document parses without errors.
- `crates/http-server/tests/test_cors.rs` — preflight `OPTIONS` from an allowlisted `Origin` returns the right `Access-Control-Allow-*` headers; non-allowlisted origins are not echoed back.
- Inline unit tests in `error.rs`, `config.rs`, `middleware/validation.rs`, `routers/health.rs` — covered as part of their respective steps.

## 6. Acceptance criteria

- [x] `cargo check --all-targets -p cognee-http-server` succeeds.
- [x] `cargo build -p cognee-http-server --features bin --bin cognee-http-server` succeeds and produces a runnable binary.
- [x] `cargo test -p cognee-http-server` passes (inline unit tests + the four integration tests above).
- [x] `cargo check -p cognee-lib` (default features, no `server`) succeeds AND does not pull `axum`/`tower-http`/`hyper` into its dep graph (verify with `cargo tree -p cognee-lib | grep -E 'axum|tower-http' | wc -l` returning 0).
- [x] `cargo check -p cognee-lib --features server` succeeds.
- [x] `cargo check -p cognee-cli` succeeds and `cognee-cli` does not gain a transitive dep on `cognee-http-server`.
- [x] `scripts/check_all.sh` passes (fmt, clippy with `-D warnings`, capi/python/js binding checks unchanged).
- [x] `cognee-http-server` binary boots locally, listens on the configured host:port, and serves `/`, `/health`, `/health/detailed`, and `/openapi.json` with the documented shapes (manual smoke check via `curl`).
- [x] Status row for **P0** in [implementation/README.md](README.md) flips **Draft → In Progress → Done** in the PR that lands this work.
- [x] Status row for **health** in [routers/README.md](../routers/README.md) flips **Draft → In Progress → Done** in the same PR.

## 7. Files touched

New (under `crates/http-server/`):

- `Cargo.toml`
- `src/lib.rs`
- `src/main.rs`
- `src/state.rs`
- `src/error.rs`
- `src/config.rs`
- `src/openapi.rs`
- `src/lifecycle.rs`
- `src/middleware/mod.rs`
- `src/middleware/cors.rs`
- `src/middleware/tracing.rs`
- `src/middleware/validation.rs`
- `src/routers/mod.rs`
- `src/routers/health.rs`
- `tests/support/mod.rs`
- `tests/test_health.rs`
- `tests/test_root.rs`
- `tests/test_openapi.rs`
- `tests/test_cors.rs`

Modified:

- `Cargo.toml` (workspace root) — `members` adds `crates/http-server`.
- `crates/lib/Cargo.toml` — adds optional `cognee-http-server` dep and `server` feature.
- `crates/lib/src/lib.rs` — adds the `#[cfg(feature = "server")] pub mod http` re-export block.
- `docs/http-server/implementation/README.md` — flip P0 status row.
- `docs/http-server/routers/README.md` — flip the `health` row's status.

Out of scope (do NOT touch in this phase):

- `crates/cli/` — no `serve-http` subcommand, no new dependency.
- Any `/api/v1/*` router file — those land in P1+.
- SeaORM migrations — first migration lands in P1 (`users`, `user_api_key`).
- `cognee_core::PipelineRunRegistry` — lands in P3-pre.
