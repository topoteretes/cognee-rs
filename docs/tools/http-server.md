# HTTP server — `cognee-http-server`

> **Not in this repository.** `cognee-http-server` — the `axum` server that
> mirrors the Python FastAPI surface under `/api/v1/*` — is not part of this
> repository, and **neither is its documentation**: the architecture notes,
> auth and tenancy model, pipeline and websocket semantics, observability, and
> the per-router endpoint reference left with it. There is no
> `cognee-http-server` binary or library to build here, and no HTTP endpoint
> reference in this repo.

The server consumes the OSS crates (`cognee`, `cognee-components`,
`cognee-vector`, `cognee-graph`, `cognee-embedding`, …), so the library seams
documented in this repo are still the seams it builds against. What moved is
the HTTP layer itself: routers, DTOs, middleware, `AppState`, the binary entry
point, and their docs.

- Backend wiring the server relies on: [tools/backends.md](backends.md).
- Configuration reference for the SDK-side env vars the server shares:
  [configuration.md](../configuration.md).
- What each operation does, independent of the interface:
  [operations.md](../operations.md).
