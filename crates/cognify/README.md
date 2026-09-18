# cognee-cognify

Knowledge-graph extraction pipeline (classify → chunk → extract → summarize → index) and the memify enrichment pipeline.

## Pluggable graph extraction

By default the extraction stage calls an LLM once per chunk through `FactExtractor`.
`graph_backend::ChunkGraphExtractor` is the extension point that replaces that call
with something else — a local NER/relation model, an on-device runtime, a rule engine —
so the pipeline can run offline, on-premise or cheaply at volume:

```rust,ignore
let config = CognifyConfig::default().with_graph_backend(Arc::new(my_backend));
```

A backend returns exactly one result per `ChunkRef`, in input order, and may also take
over chunk summarization by reporting `summarizes_chunks() == true`:

```rust,ignore
async fn extract_graphs(
    &self,
    chunks: &[ChunkRef<'_>],
    ctx: &ExtractionContext<'_>,
) -> Result<Vec<ChunkGraphResult>, GraphBackendError>;
// ChunkGraphResult = Result<KnowledgeGraph, ChunkExtractionError>
```

Failure has two granularities, and the inner one is the one to reach for. A
`ChunkExtractionError` in a chunk's own slot fails **that chunk only** — its siblings in
the same batch still land in the graph, exactly as the LLM path loses one chunk to one
failed call. The outer `GraphBackendError` fails the **whole batch** and is for
model-won't-load / runtime-missing situations; since `chunks_per_batch` defaults to 2000,
a realistic run is one batch, so using it for a single bad chunk fails the entire run.
A failed chunk still occupies its slot: dropping it is an `ArityMismatch`, not a failure
report.

Everything after extraction — the abort-time partition, per-chunk failure accounting,
DB-aware edge dedup, node/edge expansion, ownership rows and the graph writes — is
backend-neutral and shared with the LLM path.

The only implementor in this repository is `MockChunkGraphExtractor`, which
`tests/graph_backend_seam.rs` drives the whole stage through. That is deliberate: real
implementations are expected to live in other crates. See the `graph_backend` module
docs before concluding the seam is dead code.

Part of [cognee-rs](https://github.com/topoteretes/cognee-rs) — see the [project README](../../README.md) for an architecture overview and how the pieces fit together.

## License

Dual-licensed under [MIT](../../LICENSE-MIT) or [Apache-2.0](../../LICENSE-APACHE), at your option.
