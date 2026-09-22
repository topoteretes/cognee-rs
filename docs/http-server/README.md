# HTTP server

> **Moved — this folder is a reference, not a build target.**
> `cognee-http-server` no longer lives in this repository. It moved to the closed
> [`cognee-cloud-rs`](https://github.com/topoteretes/cognee-cloud-rs) repo, where
> it is `crates/cognee-http-server`; there is no `crates/http-server/` here any
> more. The 39 pages under this folder are kept because they are the **wire
> contract** — endpoint paths, verbs, request/response shapes, status codes and
> error envelopes — which is still accurate and is deep-linked from ~45 places
> across `docs/`. Every `crates/http-server/...` source path they cite is
> historical and will not resolve here; read those as
> `cognee-cloud-rs:crates/cognee-http-server/...`.

Design and reference for `cognee-http-server`, the `axum` server that mirrors the
Python FastAPI surface under `/api/v1/*`. This folder is the detailed reference;
[../tools/http-server.md](../tools/http-server.md) is now a stub pointing at the
closed repo.

## Cross-cutting design

- **[architecture.md](architecture.md)** — crate topology, dual-surface design (library + binary), middleware stack, config lifecycle.
- **[auth.md](auth.md)** — stub; the auth subsystem (JWT, fastapi-users parity, password-hash migration, bearer/cookie/api-key) lives in the closed `cognee-http-cloud` crate.
- **[pipelines.md](pipelines.md)** — background job lifecycle, `PipelineRunRegistry`, status mapping, durable vs live events.
- **[websocket.md](websocket.md)** — subscription model, status semantics, terminal close behavior.
- **[tenants.md](tenants.md)** — stub; multi-tenant schema, permission model, and ACL resolution lives in the closed `cognee-http-cloud` crate.
- **[observability.md](observability.md)** — span instrumentation and telemetry attributes for the server.

## Endpoints

- **[routers/](routers/README.md)** — one reference doc per router. **20 routers came from the OSS server** (now `cognee-cloud-rs:crates/cognee-http-server/src/routers/`); **11 routers live in the closed `cognee-http-cloud` crate** (auth, auth-register, auth-reset-password, auth-verify, api-keys, users, users-by-email, permissions, configuration, sync, checks). The closed-router docs in `routers/` are stubs that point at the [`cognee-cloud-rs`](https://github.com/topoteretes/cognee-cloud-rs) repo.

Open design questions for these areas are tracked in
[../roadmap/open-questions.md](../roadmap/open-questions.md).
