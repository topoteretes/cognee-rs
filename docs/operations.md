# Operations

What cognee-rust *does*. The primary surface is the **memory API** —
**`remember`**, **`recall`**, **`improve`**, **`forget`** — four high-level
operations that compose the lower-level pipeline (`add → cognify → memify →
search`). Every operation is reachable from each interface (CLI, language
bindings, HTTP server) — see [tools/](tools/README.md). API/type detail lives in
rustdoc (`cargo doc --no-deps --open`); this page is the conceptual map.

## The memory API

Cognee's primary surface is four operations that turn raw input into queryable,
self-improving memory. They live in the [`cognee`](../crates/lib/) `api`
module (`cognee::api::{remember, recall, improve, forget}`) and surface as
the always-built `cognee-cli` verbs `remember` / `recall` / `improve` / `forget`.

```
input ──remember──▶ memory (graph + vectors, optionally session)
query ──recall────▶ auto-routed answers
        improve───▶ enriched / bridged memory
        forget────▶ removed memory
```

### remember

Stores input as memory: it runs **add + cognify** and then, by default, the
**improve** enrichment pass. Accepts inline text and/or file paths.

- **Session memory** — pass a `--session-id` to scope the turn to a session
  (session-backed QA history).
- **Permanent graph memory** — omit `--session-id` and the input is persisted as
  permanent, graph-backed memory.

`remember ≈ add + cognify + improve`. rustdoc: `api::remember`.

### recall

Queries memory with **auto-routing**: when no query type is given, `recall`
picks an appropriate retrieval strategy automatically. It is session-aware (reads
session history when given a `--session-id`) and graph-backed. Results are
returned to the caller (printed to stdout by the CLI).

`recall ≈ auto-routed search`. rustdoc: `api::recall`.

### improve

Enriches memory and bridges sessions: runs the feedback/enrichment improvement
stages over the graph (memify-style triplet enrichment plus feedback weighting).
Can target specific sessions or graph nodes and tune the feedback weight.
rustdoc: `api::improve`.

### forget

Removes memory: a whole dataset, a specific data item, or everything. Cascades
across the relational, graph, and vector backends and file storage. rustdoc:
`api::forget`.

## Lower-level pipeline

The memory API composes these building blocks. They remain available directly
when you need fine-grained control over each stage. The classic flow is:

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

## Additional operations

These live in the [`cognee`](../crates/lib/) `api` module (and `DatasetManager`):

| Operation | What it does | rustdoc |
|---|---|---|
| **delete** | Cascading removal of data/datasets across relational → graph → vector → file storage (with dry-run preview). | [`cognee-delete`](../crates/delete/) `DeleteService` |
| **update** | Re-ingest changed data and re-cognify the affected subset. | `api::update` |
| **prune** | Reset system or all state (`prune_system` / `prune_data`). | `api::prune` |
| **visualize** | Render the graph to a self-contained d3.js HTML file. | [`cognee-visualization`](../crates/visualization/) |

## Operation → interface map

| Operation | CLI | HTTP route | Binding method |
|---|---|---|---|
| remember | `cognee-cli remember` | `POST /api/v1/remember` | `remember()` |
| recall | `cognee-cli recall` | `POST /api/v1/recall` | `recall()` |
| improve | `cognee-cli improve` | `POST /api/v1/improve` | `improve()` |
| forget | `cognee-cli forget` | `POST /api/v1/forget` | `forget()` |
| add | `cognee-cli add` | `POST /api/v1/add` | `add()` |
| cognify | `cognee-cli cognify` | `POST /api/v1/cognify` | `cognify()` |
| add + cognify | `cognee-cli add-and-cognify` | _(two calls)_ | — |
| memify | `cognee-cli memify` | `POST /api/v1/memify` | `memify()` |
| search | `cognee-cli search` | `POST /api/v1/search` | `search()` |
| delete | `cognee-cli delete` | `POST /api/v1/delete` | `delete*()` |
| update | _(via run-sequence)_ | `POST /api/v1/update` | `update()` |
| visualize | `cognee-cli visualize` | `POST /api/v1/visualize` | `visualize()` |

CLI flags and feature gates: [tools/cli.md](tools/cli.md). HTTP request/response
shapes: [http-server/routers/](http-server/routers/README.md). Binding method
names per language: [tools/bindings.md](tools/bindings.md).
