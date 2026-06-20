# HTTP server

Design and reference for `cognee-http-server`, the `axum` server that mirrors the
Python FastAPI surface under `/api/v1/*`. To **run or embed** it, start at
[../tools/http-server.md](../tools/http-server.md). This folder is the detailed
reference.

## Cross-cutting design

- **[architecture.md](architecture.md)** — crate topology, dual-surface design (library + binary), middleware stack, config lifecycle.
- **[auth.md](auth.md)** — JWT format, fastapi-users parity, password-hash migration, the three auth mechanisms (bearer / cookie / api-key).
- **[pipelines.md](pipelines.md)** — background job lifecycle, `PipelineRunRegistry`, status mapping, durable vs live events.
- **[websocket.md](websocket.md)** — subscription model, status semantics, terminal close behavior.
- **[tenants.md](tenants.md)** — multi-tenant schema, permission model, ACL resolution.
- **[observability.md](observability.md)** — span instrumentation and telemetry attributes for the server.

## Endpoints

- **[routers/](routers/README.md)** — one reference doc per router (31 routers): mount, endpoints, DTOs, behavior, parity notes.

Open design questions for these areas are tracked in
[../roadmap/open-questions.md](../roadmap/open-questions.md).
