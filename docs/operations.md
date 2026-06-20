# Operations

What cognee-rust *does*. The core flow is **`add` → `cognify` → `search`**; the
rest are lifecycle and memory operations layered on the same backends. Every
operation is reachable from each interface (CLI, language bindings, HTTP server) —
see [tools/](tools/README.md). API/type detail lives in rustdoc
(`cargo doc --no-deps --open`); this page is the conceptual map.

## The core pipeline

```
raw data ──add──▶ stored + deduplicated ──cognify──▶ knowledge graph + vectors ──search──▶ answers
```

### add (ingest)

Streams input, computes a content hash, deduplicates, and persists the data plus
metadata. Accepts text, file paths, and HTTP(S) URLs (fetched and routed by MIME
type). Deterministic UUID5 IDs make the same content + owner reproducible across
SDKs. Pipeline: [`cognee-ingestion`](../crates/ingestion/) (`AddPipeline`).

### cognify (knowledge-graph extraction)

Turns stored data into a knowledge graph in six stages: **classify** documents →
**chunk** text → **extract** entities/relationships (LLM, batched) → **summarize**
(conditional) → **add data points** (six vector collections + provenance to the
relational DB) → **extract DLT FK edges**. Configurable via `CognifyConfig`
(chunk strategy, custom prompts/schemas, temporal mode). Pipeline:
[`cognee-cognify`](../crates/cognify/) (`cognify()` / `cognify_datasets()`).

### memify (graph enrichment)

Standalone, idempotent enrichment: reads the existing graph, builds `Triplet`
objects from every edge (`"source → relationship → target"`), embeds them, and
indexes them into the `Triplet`/`text` vector collection for
`SearchType::TripletCompletion`. Pipeline:
[`cognee-cognify`](../crates/cognify/) (`memify()`).

### search (retrieval)

Unified orchestration across 15 retrieval strategies selected by `SearchType`
([`crates/search/src/types/search_type.rs`](../crates/search/src/types/search_type.rs)):
`GraphCompletion` (default), `GraphCompletionCot`, `GraphCompletionContextExtension`,
`GraphSummaryCompletion`, `TripletCompletion`, `RagCompletion`, `Chunks`,
`Summaries`, `Temporal`, `Cypher`, `NaturalLanguage`, `FeelingLucky`, `Feedback`,
`CodingRules`, `ChunksLexical`. Entry: [`cognee-search`](../crates/search/)
(`SearchBuilder` / `SearchOrchestrator`).

## Lifecycle & memory operations

These live in the [`cognee-lib`](../crates/lib/) `api` module (and `DatasetManager`):

| Operation | What it does | rustdoc |
|---|---|---|
| **delete** | Cascading removal of data/datasets across relational → graph → vector → file storage (with dry-run preview). | [`cognee-delete`](../crates/delete/) `DeleteService` |
| **update** | Re-ingest changed data and re-cognify the affected subset. | `api::update` |
| **forget** | Remove specific remembered items / graph nodes from memory. | `api::forget` |
| **prune** | Reset system or all state (`prune_system` / `prune_data`). | `api::prune` |
| **recall** | Retrieve stored memories for a query (lower-level than search). | `api::recall` |
| **remember** | Persist a memory/QA turn into the graph + session history. | `api::remember` |
| **improve** | Run the feedback/enrichment improvement stages over the graph. | `api::improve` |
| **visualize** | Render the graph to a self-contained d3.js HTML file. | [`cognee-visualization`](../crates/visualization/) |

## Operation → interface map

| Operation | CLI | HTTP route | Binding method |
|---|---|---|---|
| add | `cognee-cli add` | `POST /api/v1/add` | `add()` |
| cognify | `cognee-cli cognify` | `POST /api/v1/cognify` | `cognify()` |
| add + cognify | `cognee-cli add-and-cognify` | _(two calls)_ | — |
| memify | `cognee-cli memify` | `POST /api/v1/memify` | `memify()` |
| search | `cognee-cli search` | `POST /api/v1/search` | `search()` |
| delete | `cognee-cli delete` | `POST /api/v1/delete` | `delete*()` |
| update | _(via run-sequence)_ | `POST /api/v1/update` | `update()` |
| forget | — | `POST /api/v1/forget` | `forget()` |
| recall | — | `POST /api/v1/recall` | `recall()` |
| remember | — | `POST /api/v1/remember` | `remember()` |
| improve | — | `POST /api/v1/improve` | `improve()` |
| visualize | `cognee-cli visualize` | `POST /api/v1/visualize` | `visualize()` |

CLI flags and feature gates: [tools/cli.md](tools/cli.md). HTTP request/response
shapes: [http-server/routers/](http-server/routers/README.md). Binding method
names per language: [tools/bindings.md](tools/bindings.md).
