# Tools

The ways to drive cognee-rust, the backends it runs on, and the supporting
dev/ops tooling.

## Interfaces

How you invoke the pipeline. All cover the same operations
([operations.md](../operations.md)).

- **[cli.md](cli.md)** — `cognee-cli`: subcommands, flags, `config`, retries, logging.
- **[bindings.md](bindings.md)** — Python / C / JavaScript / Java SDKs (shared `bindings-common`) + config-setter ergonomics.
- **[http-server.md](http-server.md)** — `cognee-http-server`: launch the binary or embed the library. Endpoint specs under [../http-server/](../http-server/README.md).

## Backends

- **[backends.md](backends.md)** — pluggable providers (LLM, embeddings, vector, graph, relational, storage, session, ontology, tokenizer) with their selecting config keys and rustdoc links.

## Dev & ops tooling

- **Observability** — [../observability/opentelemetry.md](../observability/opentelemetry.md) (OTLP tracing) and [../observability/send_telemetry.md](../observability/send_telemetry.md) (opt-out product analytics).
- **Logging** — [../configuration.md#logging](../configuration.md#logging).
- **Visualization** — `cognee-cli visualize` (see [cli.md](cli.md)); [`cognee-visualization`](../../crates/visualization/).
- **Benchmarking** — [../performance/mock-benchmark.md](../performance/mock-benchmark.md) (offline mock-LLM benchmark) and its [design rationale](../performance/python-approach.md).
- **Build troubleshooting** — [../build/lbug-rebuilds.md](../build/lbug-rebuilds.md).
- **Releasing** — [../RELEASE.md](../RELEASE.md).
