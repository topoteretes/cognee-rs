# HTTP server — `cognee-http-server`

> **Stub. The crate is no longer in this repository.** `cognee-http-server` —
> the `axum` server that mirrors the Python FastAPI surface under `/api/v1/*` —
> moved to the closed [`cognee-cloud-rs`](https://github.com/topoteretes/cognee-cloud-rs)
> repository, where it lives at `crates/cognee-http-server`. There is no
> `cognee-http-server` binary or library to build here any more, so the
> `cargo install`, launch-flag and `build_router` recipes this page used to
> carry have been removed rather than left pointing at a directory that does
> not exist.

The server consumes the OSS crates (`cognee`, `cognee-components`,
`cognee-vector`, `cognee-graph`, `cognee-embedding`, …) through a git submodule,
so the library seams documented in this repo are still the seams it builds
against. What moved is the HTTP layer itself: routers, DTOs, middleware,
`AppState` and the binary entry point.

- Wire contract / endpoint reference (kept here, still accurate as a
  description of the API): [docs/http-server/](../http-server/README.md).
- Backend wiring the server relies on: [tools/backends.md](backends.md).
- Configuration reference, including the server-side env vars:
  [configuration.md](../configuration.md).

To run or embed the server, work in `cognee-cloud-rs`.
