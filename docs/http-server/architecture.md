# HTTP Server — Architecture Decisions

This document locks in the structural decisions for the Rust HTTP server port of [`cognee/api/client.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py). Scope here is *how the server is assembled*: crates, libraries, runtime, middleware stack, configuration, lifecycle. Endpoint-by-endpoint specs, auth internals, and pipeline-job details live in their own sub-documents in this folder (`auth.md`, `pipelines.md`, `routers/*.md`, etc.).

## 1. Goals & non-goals

### Goals

- **Dual surface, two artifacts**: the server is a **library** (`cognee-http-server`) that any Rust program can embed, plus a **standalone binary** (`cognee-http-server`, a new executable distinct from `cognee-cli`) that ships as a ready-to-run server. The library is the primary artifact; the binary is a thin `main()` over it.
- **Independent of `cognee-cli`**: the new binary does not extend `cognee-cli`. They are separate executables with separate argument surfaces and separate release cadences. `cognee-cli` keeps its existing subcommand set unchanged.
- **Byte-compatible endpoint layer**: URL paths, HTTP verbs, request/response JSON shapes, status codes, and error envelopes mirror Python so existing clients (frontend, MCP, SDKs) work unmodified.
- **Reuse the existing Rust stack**: routers delegate into `cognee` facades; no business logic re-implementation in the server crate.
- **Feature-gated SDK exposure**: any HTTP surface re-exported from `cognee` is behind a non-default `server` feature. SDK-only consumers (Android runner, wasm targets, embedders that just want `add`/`cognify`/`search`) compile zero axum/tower/hyper code.
- **Testable**: the `Router` is constructible in tests without binding a socket.

### Non-goals

- **Not a FastAPI clone**: we do not wrap every FastAPI concept (dependency tree, Pydantic v2 coercion rules, `Depends` chaining). We pick Rust-idiomatic equivalents that produce the same wire behavior.
- **Not a fastapi-users clone**: the auth flows are compatible at the HTTP level, but the internal abstractions are ours. Covered in a separate auth sub-document.
- **No full-stack bundling in phase 1**: the server does not serve the Next.js frontend bundle. Deployments keep the existing Python pattern (cognee-frontend + backend on different ports).

## 2. Dual-surface design

```
┌──────────────────────────────┐     ┌──────────────────────────────┐
│  cognee-http-server          │     │  any Rust application        │
│  (new standalone binary)     │     │                              │
│                              │     │  // direct dependency:       │
│  fn main() {                 │     │  //   cognee-http-server     │
│    let cfg = Config::parse();│     │  // OR via cognee's      │
│    let state = AppState::    │     │  //   `server` feature.      │
│      build(cfg).await?;      │     │                              │
│    cognee_http_server::run(  │     │  let router = cognee_http_   │
│      cfg.addr(), state)      │     │     server::build_router(    │
│      .await                  │     │       state).await?;         │
│  }                           │     │  axum::serve(listener,       │
│                              │     │     router).await?;          │
└──────────┬───────────────────┘     └──────────┬───────────────────┘
           │                                    │
           └───────────────┬────────────────────┘
                           ↓
         ┌──────────────────────────────────────┐
         │  crates/http-server (library)        │
         │  package: cognee-http-server         │
         │                                      │
         │  - build_router(AppState) -> Router  │
         │  - run(addr, AppState) -> Result<()> │
         │  - AppState (constructor helpers)    │
         │  - types re-exported for embedders   │
         └──────────────────┬───────────────────┘
                            ↓
         ┌──────────────────────────────────────┐
         │  cognee (existing facade)        │
         │  + cognee-{ingestion, cognify, …}    │
         │                                      │
         │  cognee::http  ←  feature-gated  │
         │     (re-exports cognee-http-server   │
         │      only when `server` feature is   │
         │      enabled by the consumer)        │
         └──────────────────────────────────────┘

         cognee-cli (existing CLI binary)  —  NOT IN THIS DIAGRAM.
         No new subcommand. cognee-cli does not depend on the
         HTTP server crate.
```

**Why two binaries, not one**: keeping `cognee-http-server` separate from `cognee-cli` decouples the operational concerns. The CLI is a one-shot tool that runs to completion (`add`, `cognify`, `search`, `serve` for the cloud OAuth flow); the HTTP server is a long-running daemon with its own deployment story (systemd unit, Docker `CMD`, k8s `Deployment`, init scripts). Bundling them would force every CLI invocation to compile in the entire HTTP stack, and every server deployment to ship the CLI surface — neither is desirable. They share business logic via `cognee`, not via a shared binary.

**Why the library exists at all**: the binary is a thin `main()`; everything reusable (the `Router`, middleware, lifecycle helpers, `AppState` builder) lives in the library so tests, embedded runners (Android, custom embedders, integration suites), and downstream consumers who want the cognee HTTP surface inside their own binary all share one code path. `axum::serve` plus a `Router` is what `cognee_http_server::run` provides — embedders are free to take the `Router` and host it themselves (TLS, custom listener, behind a tower service stack of their own).

## 3. Crate topology

**New crate**: `crates/http-server/` (package name `cognee-http-server`). It hosts **both** the library and the standalone binary in a single Cargo package. The binary is a thin `fn main()` that depends only on the library exports, so the library remains the single source of truth.

```
crates/http-server/
├── Cargo.toml              # [lib] + [[bin]] cognee-http-server
├── src/
│   ├── lib.rs              # Library API: build_router, run, AppState builder
│   ├── main.rs             # Binary entry point (feature-gated by `bin`)
│   ├── state.rs            # AppState (Clone + Send + Sync, holds Arc<…>)
│   ├── error.rs            # ApiError + IntoResponse
│   ├── config.rs           # HttpServerConfig (host/port/CORS/…); reads env
│   ├── auth/               # JWT, cookies, API keys (sub-doc: auth.md)
│   ├── dto/                # Pydantic-parity request/response structs
│   ├── routers/            # One file per FastAPI router
│   ├── middleware/         # CORS, tracing, validation, auth extractor
│   ├── observability/      # Span buffer feeding /api/v1/activity/spans
│   ├── openapi.rs          # utoipa::OpenApi assembly
│   └── lifecycle.rs        # Startup migrations, default user, shutdown
└── tests/                  # End-to-end tests via axum Router
```

**Why a new crate, not a module in `cognee`**:

- Keeps the tower/axum dependency graph out of the core SDK.
- Lets CI build `cognee` without pulling HTTP framework deps when compiling SDK-only targets (Android runner, wasm, embedded consumers).
- Mirrors how `cognee-cloud` was introduced as a standalone crate behind a feature.

**Why one crate hosts both library and binary**:

- The library and binary are versioned and released together.
- The binary is genuinely a thin shell (parse args → build state → call `run()`); a separate binary crate would be ~30 lines of boilerplate plus its own `Cargo.toml`.
- Cargo's `[[bin]]` + `required-features` mechanism makes the extra deps (`clap`, `dotenv`, `tokio` macros) opt-in, so library consumers don't pay the cost.

### Cargo.toml shape (essentials)

```toml
# crates/http-server/Cargo.toml
[package]
name = "cognee-http-server"
version.workspace = true
edition.workspace = true

[lib]
name = "cognee_http_server"
path = "src/lib.rs"

[[bin]]
name = "cognee-http-server"
path = "src/main.rs"
required-features = ["bin"]

[features]
default = []
# Enables the standalone-binary entry point and its arg-parsing deps.
bin = ["dep:clap", "dep:dotenv", "tokio/macros", "tokio/rt-multi-thread", "tokio/signal"]

[dependencies]
# Library deps (always compiled when the crate is built as a library).
axum                 = "0.8"
tokio                = { workspace = true, features = ["full"] }
# … (full list in §20)

# Binary-only deps (optional; pulled in by the `bin` feature only).
clap                 = { workspace = true, features = ["derive", "env"], optional = true }
dotenv               = { workspace = true, optional = true }
```

Library consumers do `cognee-http-server = { path = "../http-server" }`. To install the binary: `cargo install --path crates/http-server --features bin`, or in workspace builds `cargo build -p cognee-http-server --features bin --bin cognee-http-server`.

### Re-export through `cognee` is feature-gated

`cognee` exposes the server under `cognee::http` **only** behind a non-default `server` feature. With the feature off (the default), `cognee` does not depend on `cognee-http-server` at all, no axum/tower code is compiled, and `cognee::http` does not exist as a module.

```toml
# crates/lib/Cargo.toml
[features]
default = [ /* existing list, NO `server` here */ ]
server  = ["dep:cognee-http-server"]

[dependencies]
cognee-http-server = { path = "../http-server", optional = true }
```

```rust
// crates/lib/src/lib.rs
#[cfg(feature = "server")]
pub mod http {
    //! HTTP server surface. Available only when the `server` feature is enabled.
    //! Consumers who only need the embedded server inside their own binary should
    //! prefer this re-export over taking a direct dependency on `cognee-http-server`,
    //! to keep their dependency closure aligned with the rest of the cognee crates.
    pub use cognee_http_server::*;
}
```

**Important**: `cognee-cli` does **not** enable the `server` feature on `cognee`, and does not gain a `serve-http` subcommand. The HTTP server ships as the separate `cognee-http-server` binary defined inside `crates/http-server/`. See §17.

## 4. HTTP framework — `axum` 0.8

### Comparison

| Framework | Pros | Cons | Decision |
|---|---|---|---|
| **axum** | Tokio-native; tower middleware; typed extractors; first-class WebSocket + SSE + multipart; macro-free routing; `Router::into_service()` is testable without a socket; aligns with the rest of the async Rust ecosystem (`tokio`, `hyper`, `reqwest`) already in the workspace. | Tower concepts have a learning curve; 0.x API still shifts between minor versions. | **Chosen** |
| actix-web | Mature; good perf. | Uses its own actor runtime (not pure tokio); dependency weight; ergonomics favor handler-style over tower middleware; less natural fit with existing `tokio` + `tower` usage elsewhere. | Rejected |
| warp | Elegant filter combinators. | Compile-time error messages are notorious; maintenance cadence has slowed. | Rejected |
| rocket | Great DX. | Historically tied to nightly; now sync-first; not a fit for the heavily async workload. | Rejected |
| hyper (raw) | Zero cost; total control. | Too low-level — we would end up reimplementing axum. | Rejected |

### Framework-level features we rely on

- **`axum::Router::with_state(AppState)`** for dependency injection.
- **`axum::extract::{Json, Query, Path, Multipart, State}`** for typed request parsing.
- **`axum::response::IntoResponse`** for error polymorphism.
- **`axum::extract::ws::WebSocketUpgrade`** for `/api/v1/cognify/subscribe/{id}`.
- **`tower_http::cors::CorsLayer`** — replaces FastAPI's `CORSMiddleware`.
- **`tower_http::trace::TraceLayer`** — replaces FastAPI's request logging.
- **`tower::ServiceExt::oneshot`** — lets tests drive the router without a socket.

## 5. Async runtime — `tokio` (multi-thread)

Already the workspace runtime; every cognee crate uses it. Server entry point:

```rust
pub async fn run(addr: SocketAddr, state: AppState) -> Result<(), ServerError> {
    let app = build_router(state).await?;
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}
```

The standalone `cognee-http-server` binary uses `#[tokio::main(flavor = "multi_thread")]` directly on its `fn main()` — it is a server, not a one-shot CLI, so the simplest tokio entry point is appropriate. Embedders driving the library from their own runtime call `cognee_http_server::run(addr, state).await` from inside their existing tokio runtime.

## 6. Application state & dependency injection

FastAPI uses `Depends(...)` chains. Axum's idiomatic equivalent is a single `AppState` passed via `Router::with_state`, plus extractor-level dependencies for per-request concerns (auth user, DB transaction).

```rust
#[derive(Clone)]
pub struct AppState {
    pub lib: Option<Arc<ComponentHandles>>,     // composition of ComponentManager
    pub auth: Arc<AuthContext>,                 // JWT secret, cookie config, user repo
    pub pipelines: Arc<dyn cognee_core::PipelineRunRegistry>, // background-job lifecycle (cognee-core component)
    pub spans: Arc<SpanBuffer>,                 // in-memory OTEL-style buffer
    pub sync: Arc<SyncRegistry>,                // /api/v1/sync state
    pub config: Arc<HttpServerConfig>,          // host/port/CORS/env flags
}
```

- All fields are `Arc<…>` so `AppState: Clone` is cheap; axum clones per request.
- `ComponentHandles` is a thin wrapper around the existing `ComponentManager` + `ConfigManager` so routers can call `state.lib.add().execute(...)` etc. without reconstructing components per request. It is `Option<…>` because tests and library embedders may construct an `AppState` before the backends are wired.
- New HTTP-specific types live beside the server (`SpanBuffer`, `SyncRegistry`) and are *not* pushed into `cognee`. The pipeline-run lifecycle is handled by `cognee_core::PipelineRunRegistry` (a reusable component shared with the CLI / MCP / embedders, see [pipelines.md](pipelines.md)) — `AppState` holds an `Arc<dyn PipelineRunRegistry>` rather than an HTTP-local registry struct.

**Python parity note**: in Python, `get_authenticated_user` is a `Depends(...)`. In Rust, we implement it as an axum extractor (`FromRequestParts`) that reads the JWT/cookie/API key, looks up the user in the relational DB via `AppState::auth`, and returns an `AuthenticatedUser` value. Handlers that need the user simply add it as a parameter:

```rust
async fn post_add(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    mut multipart: Multipart,
) -> Result<Json<AddResponse>, ApiError> { … }
```

## 7. Router composition

Each Python router (`cognee/api/v1/<module>/routers/get_*_router.py`) maps to one Rust file under `src/routers/`:

```rust
// crates/http-server/src/routers/add.rs
pub fn router() -> Router<AppState> {
    Router::new().route("/", post(post_add))
}

async fn post_add(/* extractors */) -> Result<Json<AddResponse>, ApiError> { … }
```

Assembly happens in `build_router`:

```rust
pub async fn build_router(state: AppState) -> Result<Router, ServerError> {
    let api_v1 = Router::new()
        .nest("/auth",          auth::router())
        .nest("/auth/api-keys", api_keys::router())
        .nest("/add",           add::router())
        .nest("/cognify",       cognify::router())
        .nest("/memify",        memify::router())
        .nest("/search",        search::router())
        .nest("/datasets",      datasets::router())
        .nest("/ontologies",    ontologies::router())
        .nest("/permissions",   permissions::router())
        .nest("/settings",      settings::router())
        .nest("/configuration", configuration::router())
        .nest("/visualize",     visualize::router())
        .nest("/delete",        delete::router())
        .nest("/update",        update::router())
        .nest("/responses",     responses::router())
        .nest("/llm",           llm::router())
        .nest("/sync",          sync::router())
        .nest("/users",         users::router())
        .nest("/notebooks",     notebooks::router())
        .nest("/checks",        checks::router())
        .nest("/activity",      activity::router())
        .nest("/remember",      remember::router())
        .nest("/recall",        recall::router())
        .nest("/sessions",      sessions::router())
        .nest("/improve",       improve::router())
        .nest("/forget",        forget::router());

    let app = Router::new()
        .route("/",               get(root))
        .nest("/api/v1",          api_v1)
        .nest("/health",          health::router())
        .route("/openapi.json",   get(openapi::openapi_json))
        .layer(middleware_stack(&state))
        .with_state(state);

    Ok(app)
}
```

This matches the Python order in [`cognee/api/client.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py) so OpenAPI tag ordering is stable.

## 8. Middleware stack

Composition order (outer → inner), assembled as a single tower layer in `middleware/mod.rs`:

1. **`TraceLayer::new_for_http()`** — request/response tracing; span includes method, path, status, latency.
2. **`CorsLayer`** — mirrors [`CORSMiddleware`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L115-L121). Reads `CORS_ALLOWED_ORIGINS` (comma-separated) or falls back to `UI_APP_URL` (default `http://localhost:3000`). Methods: `OPTIONS,GET,PUT,POST,DELETE`. `allow_credentials(true)`, `allow_headers(Any)`.
3. **Request-body size limit** — `DefaultBodyLimit::max(100 * 1024 * 1024)` by default (configurable); critical for multipart uploads.
4. **Exception mapping** — handled via `IntoResponse for ApiError`, not a middleware; see §9.

Auth is *not* a global middleware. It is per-route via the `AuthenticatedUser` extractor so public endpoints (`/health`, `/`, `/api/v1/auth/login`) don't require it. This mirrors FastAPI, where auth is a `Depends(...)` parameter on handlers that need it.

## 9. Error handling

Python's [`request_validation_exception_handler`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L165-L176) and [`exception_handler(CogneeApiError)`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L179-L195) produce specific JSON shapes:

- `RequestValidationError` → `400 {"detail": [...], "body": ...}` (or `{"detail": "LOGIN_BAD_CREDENTIALS"}` for login).
- `CogneeApiError` → `<status_code> {"detail": "<message> [<name>]"}`.
- Improperly defined `CogneeApiError` → `418 {"detail": "An unexpected error occurred."}`.

Rust counterpart:

```rust
pub enum ApiError {
    BadRequest(String),              // 400 {"detail": "..."}
    Unauthorized,                    // 401 {"detail": "Unauthorized"}
    Forbidden(String),               // 403 {"detail": "..."}
    NotFound(String),                // 404
    Conflict(String),                // 409
    Validation(ValidationDetails),   // 400 {"detail": [...], "body": ...}
    LoginBadCredentials,             // 400 {"detail": "LOGIN_BAD_CREDENTIALS"}
    PipelineErrored(String),         // 420 for /improve, 500 elsewhere
    Teapot,                          // 418 fallback for malformed errors
    Internal(anyhow::Error),         // 500
}

impl IntoResponse for ApiError { /* writes Python-shaped JSON */ }
```

All handlers return `Result<T, ApiError>`. Handlers never call `panic!` or `unwrap` — this is already a project-wide rule (see [CLAUDE.md](../../.claude/CLAUDE.md)).

## 10. Request validation

FastAPI uses Pydantic for automatic coercion and validation. Axum uses `serde` deserialization; additional validation rules run inside the handler.

- **Simple shapes** (typed fields, required/optional): handled by `#[derive(Deserialize)]` on DTO structs. A missing required field or wrong type produces a serde error that the custom `Json` extractor (`src/middleware/validation.rs`) converts into `ApiError::Validation`.
- **Cross-field rules** (e.g. `ForgetPayloadDTO` requires exactly one of `data_id`/`dataset`/`everything=true`): a handler-level `validate()` method on the DTO, returning `ApiError::BadRequest` with a Python-matching message.
- **Custom Json extractor**: we wrap `axum::Json` so deserialization errors produce the Python error shape instead of axum's default.

## 11. Configuration

The HTTP server has its own configuration surface, independent of `cognee-cli`'s. The binary loads its config in this precedence order (lowest → highest):

1. Struct defaults (`HttpServerConfig::default()`).
2. `.env` file in the current working directory (loaded via `dotenv` at the top of `main()`).
3. Shell environment variables.
4. Command-line flags on the `cognee-http-server` binary (e.g. `cognee-http-server --host 0.0.0.0 --port 8000`). All flags fall back to the matching env var via clap's `#[arg(env = "…")]` so containerized deployments work without flags.

Library consumers (embedders) do not use this loader — they construct `HttpServerConfig` directly and pass it into `AppState::build(...)`. Reading env vars is a binary-only concern.

```rust
#[derive(Clone, Debug, Deserialize)]
pub struct HttpServerConfig {
    pub host: String,                       // HTTP_API_HOST, default "0.0.0.0"
    pub port: u16,                          // HTTP_API_PORT, default 8000
    pub cors_allowed_origins: Vec<String>,  // CORS_ALLOWED_ORIGINS
    pub ui_app_url: String,                 // UI_APP_URL, default http://localhost:3000
    pub env: Environment,                   // ENV, default Prod
    pub require_authentication: bool,       // REQUIRE_AUTHENTICATION, default true
    pub jwt_secret: SecretString,           // AUTH_JWT_SECRET; generated on boot if unset
    pub jwt_lifetime: Duration,             // AUTH_JWT_LIFETIME_SECONDS, default 3600
    pub body_limit: usize,                  // HTTP_BODY_LIMIT_BYTES, default 100 MiB
}
```

`HttpServerConfig::from_env()` is a standalone function; we do **not** read env inside routers. That keeps tests hermetic (tests supply a config directly).

## 12. Logging & observability

- Reuse the workspace's `tracing` + `tracing_subscriber` setup. The standalone `cognee-http-server` binary installs its own subscriber (the library crate does NOT call `set_global_default` — embedders install theirs). See `init_tracing()` in §17 and the full subscriber composition in [observability.md §3.2](observability.md#32-subscriber-composition).
- **Access log**: emitted by `tower_http::trace::TraceLayer`. Fields: `method`, `uri`, `status`, `latency_ms`, `user_id` (when extractor succeeded), `pipeline_run_id` (when applicable).
- **Structured fields in prod**: the binary's `EnvFilter::try_from_default_env()` + `tracing_subscriber::fmt` setup mirrors the existing CLI's pattern at [`crates/cli/src/main.rs`](../../crates/cli/src/main.rs). JSON output when `ENV=prod`, pretty when `ENV=dev`.
- **Span buffer** for `/api/v1/activity/spans`: a custom `tracing::Layer` that keeps the last N spans in a ring buffer. Detailed in a separate observability sub-document.

## 13. OpenAPI generation — `utoipa`

Python hand-rolls `custom_openapi()` to inject `ApiKeyAuth` + `BearerAuth` security schemes (see [client.py:126–162](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L126-L162)). We replace that with `utoipa`:

- `#[utoipa::path(...)]` on handler functions.
- `#[derive(ToSchema)]` on DTO structs.
- A single `#[derive(OpenApi)]` root struct in `src/openapi.rs` declaring:
  - `info.title = "Cognee API"`, `info.version = "1.0.0"`.
  - `components.securitySchemes = { BearerAuth, ApiKeyAuth }`.
  - Global `security = [{BearerAuth: []}, {ApiKeyAuth: []}]` when `REQUIRE_AUTHENTICATION=true`.
- Served at `GET /openapi.json`.
- A parity test snapshots the generated schema and diffs against Python's to flag drift.

## 14. Startup lifecycle

Python's [`lifespan`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L74-L98) runs migrations, ensures a default user exists, and logs `"Backend server has started"`. Rust equivalent lives in `src/lifecycle.rs`:

```rust
pub async fn on_startup(state: &AppState) -> Result<(), LifecycleError> {
    run_startup_migrations(&state.lib.db()).await?;       // SeaORM migrations
    bootstrap_default_principals(&state.lib).await?;      // creates the default tenant, the four
                                                          // canonical permissions, the default user,
                                                          // and tenant membership — see
                                                          // tenants.md §6 for the procedure
    state.lib.warm_embedders().await?;                // optional pre-warm
    tracing::info!("Backend server has started");     // grepable in docker logs
    Ok(())
}
```

`build_router` calls `on_startup` before returning the assembled `Router`. This keeps the startup/shutdown flow compatible with `axum::serve` (which has no lifespan hook).

## 15. Graceful shutdown

- Listen for `SIGTERM` and `SIGINT` via `tokio::signal`.
- On signal: stop accepting new connections, let in-flight requests finish (axum's `with_graceful_shutdown`), then call `on_shutdown` to close DB pools, flush tracing, and call `state.pipelines.shutdown()` on the `cognee_core::PipelineRunRegistry` — that aborts every in-flight run and writes `DATASET_PROCESSING_ERRORED` rows so a restart doesn't show stale `STARTED` state ([pipelines.md §12](pipelines.md#12-crash--restart-recovery)).

## 16. Feature gates

Three layers of gating, matched to who pays the compile cost:

### Layer 1 — `cognee-http-server` crate

- The **library** is unconditional within the crate. Anyone who depends on `cognee-http-server` gets the library code; that's the whole point of pulling it in.
- The **binary** is opt-in via a `bin` feature on the crate (see §3 Cargo.toml). Library-only consumers don't compile `clap`/`dotenv` or the `main.rs` entry point.
- Optional sub-features for individual capabilities — e.g. `redis-sessions`, `websocket` (always on by default; flag exists only so embedders can drop it for size-sensitive builds), `notebooks` (binds to the Python sandbox once that lands).

### Layer 2 — `cognee` re-export

- `cognee` defines a non-default `server` feature: `server = ["dep:cognee-http-server"]`.
- `cognee-http-server` is declared as `optional = true` in `cognee`'s `[dependencies]`, so disabling the feature truly removes it from the build graph (no transitive axum/tower compilation).
- The `cognee::http` module is wrapped in `#[cfg(feature = "server")]`. Without the feature, the path simply does not exist; downstream code that touches it fails to compile, which is the correct signal.
- The feature is **not** added to `cognee`'s `default` feature list. SDK consumers who do not need the HTTP surface (Android runner, wasm targets, library embedders that just call `add`/`cognify`/`search`) get a smaller dep graph by default.

### Layer 3 — `cognee-cli` is unaffected

- `cognee-cli` neither depends on `cognee-http-server` nor enables `cognee/server`. It does not gain a `serve-http` subcommand.
- Anyone wanting an HTTP server installs the `cognee-http-server` binary separately (`cargo install cognee-http-server --features bin`, or pulls the released binary). Distros and Docker images can ship both binaries side by side; users only deploy what they need.

### Library deps (full list)

```toml
# crates/http-server/Cargo.toml — [dependencies] (library only; see §3 for [features] and the bin-only deps)
axum                 = "0.8"
tokio                = { workspace = true, features = ["full"] }
tower                = "0.5"
tower-http           = { version = "0.6", features = ["cors", "trace", "limit"] }
hyper                = { version = "1", features = ["server", "http1", "http2"] }  # axum 0.8 needs hyper 1.x. The qdrant v0.14 hyper fork lives in closed cognee-cloud-rs, so hyper 1.x is the only version in the OSS graph.
cognee           = { path = "../lib" }       # no `server` feature on cognee here — that would be a cycle
cognee-models        = { path = "../models" }
serde                = { workspace = true, features = ["derive"] }
serde_json           = { workspace = true }
utoipa               = { version = "5", features = ["axum_extras", "uuid", "chrono"] }
jsonwebtoken         = "9"
argon2               = "0.5"
cookie               = "0.18"
uuid                 = { workspace = true, features = ["v4", "v5", "serde"] }
chrono               = { workspace = true, features = ["serde"] }
thiserror            = { workspace = true }
anyhow               = { workspace = true }
tracing              = { workspace = true }
tracing-subscriber   = { workspace = true }
secrecy              = "0.10"
dashmap              = "6"
bytes                = "1"
futures              = "0.3"
async-trait          = { workspace = true }
```

## 17. Binary: `cognee-http-server`

A new standalone executable that lives in the same crate as the library (see §3). It is **not** a `cognee-cli` subcommand. The binary's only job is to parse args + env, build `AppState`, and call into the library.

### Args

A flat clap surface — no subcommands. Every flag falls back to an env var so containerized deployments need no flags at all.

```rust
// crates/http-server/src/main.rs (compiled only with the `bin` feature)

use std::net::SocketAddr;
use clap::Parser;
use cognee_http_server::{AppState, HttpServerConfig};

#[derive(Parser, Debug)]
#[command(
    name = "cognee-http-server",
    about = "Cognee HTTP server (FastAPI-compatible)",
    version,
)]
struct Args {
    #[arg(long, env = "HTTP_API_HOST", default_value = "0.0.0.0")]
    host: String,
    #[arg(long, env = "HTTP_API_PORT", default_value_t = 8000)]
    port: u16,
    /// Path to a JSON config file. Optional; env vars override.
    #[arg(long, env = "COGNEE_HTTP_CONFIG")]
    config: Option<std::path::PathBuf>,
    /// Comma-separated CORS origins. Falls back to UI_APP_URL.
    #[arg(long, env = "CORS_ALLOWED_ORIGINS")]
    cors_allowed_origins: Option<String>,
    #[arg(long, env = "ENV", default_value = "prod")]
    env: String,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    // 1. Load .env (binary-only concern)
    let _ = dotenv::dotenv();

    // 2. Init tracing subscriber (the library does NOT install a global subscriber)
    init_tracing();

    // 3. Parse + assemble config
    let args = Args::parse();
    let cfg  = HttpServerConfig::load(&args)?;          // applies precedence from §11

    // 4. Build state and run
    let state = AppState::build(cfg.clone()).await?;
    let addr: SocketAddr = format!("{}:{}", cfg.host, cfg.port).parse()?;
    cognee_http_server::run(addr, state).await?;
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,ort=warn"));
    let _ = fmt().with_env_filter(filter).with_target(false).try_init();
}
```

### Naming & coexistence with `cognee-cli`

- The new binary is named **`cognee-http-server`** (matches the package name; unambiguous on `$PATH`).
- The existing `cognee-cli serve` subcommand (commit `ab18925`) handles the cloud OAuth flow. It stays as-is; this work does not modify `cognee-cli`.
- A user who wants both installs both: the `cognee-cli` binary (built from source — `cargo build --release -p cognee-cli`; it is not published to crates.io) and `cargo install cognee-http-server --features bin`. Distribution-wise (Docker, deb/rpm, brew) we publish them as two artifacts.

### Deployment shape

- **Docker**: a new stage in `Dockerfile` builds `cognee-http-server` and sets it as the default `CMD`. The existing `cognee-cli` image stays separate.
- **systemd**: `ExecStart=/usr/local/bin/cognee-http-server`; environment supplied through `EnvironmentFile=`.
- **k8s**: a `Deployment` running the server image; readiness probe hits `GET /health`, liveness probe hits `GET /` (the lightweight root).

## 18. Testing architecture

Four layers, all in `crates/http-server/tests/`:

1. **Unit tests** (`#[cfg(test)]` inline): DTO serialization, error → response mapping, JWT encode/decode.
2. **Router tests** (no socket): build the router, drive it via `tower::ServiceExt::oneshot`:
   ```rust
   let response = app.oneshot(Request::builder().method("POST").uri("/api/v1/add")
       .header("authorization", bearer(&user))
       .body(multipart_body()).unwrap()).await.unwrap();
   assert_eq!(response.status(), StatusCode::OK);
   ```
   Fast, hermetic; the default mode for CI.
3. **Integration tests** (bound socket): launch `axum::serve` on `127.0.0.1:0`, hit it with `reqwest`. Reserved for WebSocket and streaming-download tests.
4. **Cross-SDK HTTP parity** (future): a new suite under `e2e-cross-sdk/` that runs Python uvicorn + Rust binary side-by-side and diffs responses. Reuses the Docker harness pattern from `test_add_parity.py`.

Test DTOs, fixtures, and helpers live in `crates/http-server/tests/support/` — not in `cognee-test-utils`, because they are HTTP-specific.

## 19. Background task registry

Specified in [pipelines.md §6](pipelines.md#6-cognee_corepipelinerunregistry--the-new-component): a new `cognee_core::PipelineRunRegistry` trait + impl, runtime-agnostic `Stream`-based subscribe API, implements the existing `cognee_core::PipelineWatcher` trait per-run, takes `Arc<dyn PipelineRunRepository>` for durable status writes. The HTTP server's `AppState::pipelines` field holds an `Arc<dyn PipelineRunRegistry>`; handlers with `run_in_background=true` call `register_background(spec, work)`. WebSocket protocol in [websocket.md](websocket.md). Library refactor prerequisite (drop `run_in_background` from `cognee::api::remember()` and `improve()`) in [pipelines.md §2](pipelines.md#2-library-refactor-prerequisite).

## 20. Selected-libraries summary

Library-side deps (always compiled when `cognee-http-server` is linked):

| Concern | Library | Version |
|---|---|---|
| Web framework | `axum` | 0.8 |
| HTTP middleware | `tower-http` | 0.6 |
| Async runtime | `tokio` | workspace |
| HTTP types | `hyper` | workspace |
| Serialization | `serde` + `serde_json` | workspace |
| OpenAPI | `utoipa` + `utoipa-axum` | 5 |
| JWT | `jsonwebtoken` | 9 |
| Password hashing | `argon2` | 0.5 |
| Cookies | `cookie` | 0.18 |
| Concurrent map | `dashmap` | 6 |
| Secret handling | `secrecy` | 0.10 |
| Error derive | `thiserror` | workspace |
| Error propagation | `anyhow` | workspace |
| Logging | `tracing` | workspace |

Binary-only deps (gated behind the crate's `bin` feature; not pulled by library consumers):

| Concern | Library | Version |
|---|---|---|
| Arg parsing | `clap` | workspace (with `derive`, `env`) |
| `.env` loading | `dotenv` | workspace |
| Subscriber init | `tracing-subscriber` | workspace |

Test-only deps:

| Concern | Library |
|---|---|
| HTTP client for integration tests | `reqwest` |
| Driving routers without a socket | `tower::ServiceExt` |
| Black-box binary tests | `assert_cmd` |

No runtime GIL, no dynamic dispatch on handlers, no global mutable state — all dependencies injected via `AppState`.

## 21. Decisions deferred to later sub-documents

| Topic | Doc | Reason for deferral |
|---|---|---|
| Auth internals (JWT format, cookie layout, API key storage, password hash migration from bcrypt) | `auth.md` (stub — moved to closed `cognee-http-cloud`; see [`cognee-cloud-rs`](https://github.com/topoteretes/cognee-cloud-rs)) | Non-trivial; fastapi-users compatibility requires a careful spec. |
| Per-router endpoint contracts (request/response DTO field names, status codes, validation rules) | `routers/*.md` | Each router gets its own doc so it can be implemented + reviewed in isolation. |
| Background pipeline registry (schema, eviction policy, restart recovery) | `pipelines.md` | Needs a decision on persistence (in-memory only vs DB-backed). |
| WebSocket protocol | `websocket.md` | Needs exact message shape parity with the Python WS handler. |
| OTEL span buffer | `observability.md` | Needs a decision on `tracing-opentelemetry` vs custom layer. |
| Multi-tenant + RBAC schema | `tenants.md` (stub — moved to closed `cognee-http-cloud`; see [`cognee-cloud-rs`](https://github.com/topoteretes/cognee-cloud-rs)) | Requires SeaORM migrations matching the Python Alembic schema. |

## 22. Open questions

1. **Shared `ComponentHandles` instance vs per-request**: current decision is one shared `Option<Arc<ComponentHandles>>` in `AppState`. Validate under load once routers exist.
2. **DB pool sizing**: the existing `ComponentManager` holds the DB pool; we need to check its default size is sensible for a multi-connection HTTP server.
3. **Multipart temp storage**: `axum::extract::Multipart` streams into memory by default. For large uploads (`/add`), we may need to spool to disk — decide once we benchmark.
4. **JWT secret generation**: we default to per-boot random if `AUTH_JWT_SECRET` is unset. That means restarts invalidate all tokens. Accept for self-hosted dev; document loudly for prod.
5. **WebSocket auth**: cookies work naturally; bearer tokens need the `Sec-WebSocket-Protocol` header trick (same as Python). Confirmed in the WS sub-document.

## 23. References

- Python server entry point: [`cognee/api/client.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py).
- Python router modules: [`cognee/api/v1/*/routers/`](https://github.com/topoteretes/cognee/tree/main/cognee/api/v1).
- Existing CLI binary (unchanged by this work): [`crates/cli/src/main.rs`](../../crates/cli/src/main.rs).
- Existing library facade: [`crates/lib/src/lib.rs`](../../crates/lib/src/lib.rs) — the `cognee::http` re-export module is added behind a non-default `server` feature (see §16).
- Cloud crate (reference for "new feature-gated crate" pattern): [`crates/cloud/`](../../crates/cloud/).
- Memory API (callable via this server): [`crates/lib/src/api/`](../../crates/lib/src/api/) (`recall`, `remember`, `improve`, `forget`).
