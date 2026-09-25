# HTTP server — `cognee-http-server`

An `axum`-based server that exposes the Python FastAPI surface under
`/api/v1/*`. Built from [`cognee-http-server`](../../crates/http-server/), which
is both a **library** (embed it in any Rust program) and a **standalone binary**
(distinct from `cognee-cli`).

This page is the launch/embed surface. The endpoint specs, auth, pipelines,
websockets, tenancy, and observability live in
[docs/http-server/](../http-server/README.md).

## Run the binary

```bash
cargo build --release --features server   # or the default cognee-http-server build
cognee-http-server --host 0.0.0.0 --port 8000
```

Launch flags (each has an env fallback): `--host` (`HTTP_API_HOST`, `0.0.0.0`),
`--port` (`HTTP_API_PORT`, `8000`), `--env` (`ENV`), `--cors-allowed-origins`
(`CORS_ALLOWED_ORIGINS`). The full server env surface (auth, body limits,
pipeline registry, notebooks, health probes) is in
[`crates/http-server/src/config.rs`](../../crates/http-server/src/config.rs) and
summarized in [configuration.md §HTTP server](../configuration.md#http-server).

## Embed the library

```rust
let state = AppState::build(config).await?;
let router = cognee_http_server::build_router(state).await?;
// either drive it yourself…
axum::serve(listener, router).await?;
// …or use the helper that binds + serves:
cognee_http_server::run(addr, state).await?;
```

Public entry points: `build_router(AppState)` (assembles the router + middleware +
all sub-routers), `run(addr, AppState)` (binds a listener and serves), and
`AppState`. Routers delegate into `cognee` facades — no business logic is
re-implemented in the server crate. See the
[architecture decisions](../http-server/architecture.md) for the dual-surface
design and middleware stack, and [tools/backends.md](backends.md) for how the
server's databases/providers are wired.
