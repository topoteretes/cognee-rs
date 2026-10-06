# HTTP server — `cognee-http-server`

> **Not in this repository.** `cognee-http-server` — the `axum` server that
> mirrors the Python FastAPI surface under `/api/v1/*` — lives in the closed
> [`cognee-cloud-rs`](https://github.com/topoteretes/cognee-cloud-rs)
> repository, at `crates/cognee-http-server`. **Its documentation moved with
> it**: the architecture notes, auth and tenancy model, pipeline and websocket
> semantics, observability, and the per-router endpoint reference are
> maintained in `cognee-cloud-rs` under `docs/http-server/`, next to the code
> they describe. There is no `cognee-http-server` binary or library to build
> here, and no HTTP endpoint reference in this repo.

The server consumes the OSS crates (`cognee`, `cognee-components`,
`cognee-vector`, `cognee-graph`, `cognee-embedding`, …) through a git submodule,
so the library seams documented in this repo are still the seams it builds
against. What moved is the HTTP layer itself: routers, DTOs, middleware,
`AppState`, the binary entry point, and their docs.

- Backend wiring the server relies on: [tools/backends.md](backends.md).
- Configuration reference for the SDK-side env vars the server shares:
  [configuration.md](../configuration.md).
- What each operation does, independent of the interface:
  [operations.md](../operations.md).

To run, embed, or read the endpoint reference for the server, work in
`cognee-cloud-rs`.
