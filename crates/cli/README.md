# cognee-cli

Single-user, local command-line interface over the cognee pipeline (`add` → `cognify` → `search` and the higher-level memory ops). It drives the embedded engine directly against your local databases — there is **no HTTP server** here (no `serve`, no `disconnect`); for the networked API use the `cognee-http-server` crate instead.

## Commands

- `add` — ingest text and/or files into a dataset.
- `cognify` — extract a knowledge graph from ingested data.
- `add-and-cognify` — ingest then cognify in one call.
- `memify` — run the self-improvement (memory enrichment) pass over the graph.
- `search` — query the knowledge graph (graph/RAG completion, chunks, summaries, code, cypher, temporal).
- `remember` — one-call store (add + cognify + improve).
- `recall` — smart memory query with auto-routed search type.
- `forget` — remove a data item, a dataset, or everything you own.
- `improve` — enrich existing memory and bridge sessions into the permanent graph.
- `delete` — lower-level deletion with soft/hard modes, dry-run, and ACL enforcement.
- `config` — get / set / list / unset / reset persisted CLI configuration.
- `run-sequence` — replay a JSON-described sequence of commands.
- `visualize` — render a graph visualization to HTML (requires the `visualization` feature).
- `bench` — run the performance orchestrator driver (requires the `bench` feature).

## Library target

In addition to the `cognee-cli` binary, the crate exposes a `cognee_cli` library target re-exporting the `cli`, `commands`, `config_store`, and `error` modules, so downstream consumers (such as the closed cloud superset binary) can reuse the command handlers and argument structs unchanged.

Part of [cognee-rs](https://github.com/topoteretes/cognee-rs) — see the [project README](../../README.md) for an architecture overview and how the pieces fit together.

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
