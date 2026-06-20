# cognee-rust documentation

Documentation hub. Start here, or jump from the [project README](../README.md).
cognee-rust is a Rust AI-memory pipeline: **`add` → `cognify` → `search`**.

API/type detail is rendered from the source by rustdoc — build it with
`cargo doc --no-deps --open`. These pages link to it rather than restating
signatures.

## Start here

| If you want to… | Read |
|---|---|
| Understand the project and its parts | [architecture.md](architecture.md) |
| Know what each operation does | [operations.md](operations.md) |
| Configure it (env vars, settings) | [configuration.md](configuration.md) |
| Pick how to drive it (CLI / bindings / HTTP) | [tools/](tools/README.md) |
| Swap a backend (LLM, vector, graph, …) | [tools/backends.md](tools/backends.md) |
| See what's planned / not yet done | [roadmap/](roadmap/README.md) |

## Main parts

### Overview & operations
- **[architecture.md](architecture.md)** — workspace layout, crate-by-crate breakdown, design patterns, key dependencies, and the rustdoc guide. (Single source shared with `.claude/CLAUDE.md`.)
- **[operations.md](operations.md)** — `add`, `cognify`, `memify`, `search`, plus `delete`/`update`/`forget`/`prune`/`recall`/`remember`/`improve`/`visualize`, and how each maps onto the interfaces.

### Configuration
- **[configuration.md](configuration.md)** — canonical config reference: resolution order, every env var grouped by subsystem, the `ConfigManager` runtime API, and the CLI `config` subcommand. Logging lives here too.

### Tools
- **[tools/cli.md](tools/cli.md)** — the `cognee-cli` binary.
- **[tools/bindings.md](tools/bindings.md)** — Python / C / JavaScript SDKs.
- **[tools/http-server.md](tools/http-server.md)** — run or embed `cognee-http-server`.
- **[tools/backends.md](tools/backends.md)** — pluggable providers.
- **[tools/README.md](tools/README.md)** — index, incl. dev/ops tooling (observability, benchmarking, visualization, release).

### HTTP server (detail)
- **[http-server/](http-server/README.md)** — architecture, auth, pipelines, websocket, tenancy, observability, and a [per-router reference](http-server/routers/README.md).

### Observability & performance
- **[observability/opentelemetry.md](observability/opentelemetry.md)** — OTLP tracing.
- **[observability/send_telemetry.md](observability/send_telemetry.md)** — opt-out product analytics.
- **[performance/mock-benchmark.md](performance/mock-benchmark.md)** — offline mock-LLM benchmark (+ [rationale](performance/python-approach.md)).

### Build & release
- **[build/lbug-rebuilds.md](build/lbug-rebuilds.md)** — Ladybug rebuild troubleshooting.
- **[RELEASE.md](RELEASE.md)** — release runbook.

### Roadmap
- **[roadmap/](roadmap/README.md)** — gaps ([not-implemented](roadmap/not-implemented.md)), open design decisions ([open-questions](roadmap/open-questions.md)), and active implementation plans.
