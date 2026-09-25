# HTTP server

Design and reference for `cognee-http-server`, the `axum` server that mirrors the
Python FastAPI surface under `/api/v1/*`. To **run or embed** it, start at
[../tools/http-server.md](../tools/http-server.md). This folder is the detailed
reference.

## Cross-cutting design

- **[architecture.md](architecture.md)** — crate topology, dual-surface design (library + binary), middleware stack, config lifecycle.
- **[auth.md](auth.md)** — stub; the auth subsystem (JWT, fastapi-users parity, password-hash migration, bearer/cookie/api-key) lives in the closed `cognee-http-cloud` crate.
- **[pipelines.md](pipelines.md)** — background job lifecycle, `PipelineRunRegistry`, status mapping, durable vs live events.
- **[websocket.md](websocket.md)** — subscription model, status semantics, terminal close behavior.
- **[tenants.md](tenants.md)** — stub; multi-tenant schema, permission model, and ACL resolution lives in the closed `cognee-http-cloud` crate.
- **[observability.md](observability.md)** — span instrumentation and telemetry attributes for the server.

## Endpoints

- **[routers/](routers/README.md)** — one reference doc per router. **20 routers live in OSS** (`crates/http-server/src/routers/`); **11 routers live in the closed `cognee-http-cloud` crate** (auth, auth-register, auth-reset-password, auth-verify, api-keys, users, users-by-email, permissions, configuration, sync, checks). The closed-router docs in `routers/` are stubs that point at the [`cognee-cloud-rs`](https://github.com/topoteretes/cognee-cloud-rs) repo.

Open design questions for these areas are tracked in
[../roadmap/open-questions.md](../roadmap/open-questions.md).
