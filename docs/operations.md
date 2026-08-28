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
When session IDs are supplied it also distills each session's Q&A into curated,
entity-anchored "session-learnings" lesson documents (tagged with the
`session_learnings` node-set) and cognifies them into the permanent graph.
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

#### When a cognify run fails

A chunk or a file the pipeline cannot process is not a bare error: the chunking,
graph-extraction and summarization stages *collect* their failures, and the run
result carries a report naming which file, which chunk, which stage and what
went wrong. `CognifyResult.failures` carries it on success;
`CognifyError::RunFailed` carries the same report when the run failed.

What happens to what the run already wrote is decided at the end, from two
independent settings — *when to stop* (`FailFast`, the default, or `RunToEnd`)
and *what to sweep* (`WholeRun`, the default, `FailedItems`, or `Nothing`). The
full matrix is in
[Cognify failure handling](configuration.md#cognify-failure-handling); the end
states are:

- **Default (`FailFast` + `WholeRun`).** The run aborts at the first failed
  file, everything it created is removed, the run is recorded `ERRORED` and the
  call returns `Err`. The store converges to its pre-run state — what earlier
  runs completed is untouched, because the sweep selects only rows naming *this*
  run. Python's default, in execution and in end state.
- **`FailedItems` below the failure ratio.** The run *completes*. Only the
  failed files' contributions are removed; the files that finished are kept,
  indexed and marked complete; and the call returns `Ok` with the failed-file
  list in the report, so the next run knows exactly what to redo.
- **`FailedItems` over the ratio** escalates to a whole-run sweep and an errored
  run — a large enough share of failed chunks means something systemic, not one
  bad file.
- **`Nothing`** removes nothing. The escape hatch.

Under `FailFast` a run partitions its files three ways at the abort point:
*complete* (every chunk extracted — persisted and marked), *failed* (at least
one chunk failed) and *unreached* (never attempted). Only complete files are
persisted, which is what keeps a data item all-or-nothing; failed and unreached
files are indistinguishable to the next run and are simply redone.

A sweep deliberately keeps anything still claimed from outside its scope: an
entity a surviving file also produced, an artifact another run or another
dataset also names, or a row written before ownership tracking existed. The
worst case is a surplus artifact that keeps an owner, never an artifact with no
owner at all.

One caveat. **Cancellation is swept**, unlike Python — though there is no
caller-facing cancel handle on `cognify()` today, and simply dropping the future
runs no sweep at all.

All of the above applies to the **temporal** pipeline too: it records ownership
of its events, timestamps, intervals and entity nodes before it writes them, its
two LLM passes collect failures rather than swallowing them, and a failed
temporal run converges the same way.

#### Re-cognifying is now incremental — a behaviour change

`incremental_loading` is on by default and now does what it says. A successful
run marks each data item it finished, in Python's own `pipeline_status` format,
and the next run skips the marked items before it builds a pipeline — no
classification, no chunking, no LLM calls.

**Re-cognifying an already-complete dataset is therefore a no-op**, returning
`Ok` with `already_completed = true`. Deployments that relied on cognify
re-processing everything on every call will see it "stop doing anything"; set
`with_incremental_loading(false)` to restore the previous behaviour. A failed
run marks nothing, and a sweep clears the markers of whatever it rolled back, so
the next run redoes exactly the work that was lost.

Both branches share one marker key, matching Python, so **a `cognify()` and a
temporal `cognify()` over the same dataset are no-ops for each other**. Turn
incremental loading off to build both graphs over one dataset.

### memify (graph enrichment)

Standalone, idempotent enrichment: reads the existing graph, builds `Triplet`
objects from every edge (`"source → relationship → target"`), embeds them, and
indexes them into the `Triplet`/`text` vector collection for
`SearchType::TripletCompletion`. Pipeline:
[`cognee-cognify`](../crates/cognify/) (`memify()`).

### search (retrieval)

Unified orchestration across 16 retrieval strategies selected by `SearchType`
([`crates/search/src/types/search_type.rs`](../crates/search/src/types/search_type.rs)):
`GraphCompletion` (default), `GraphCompletionCot`, `GraphCompletionContextExtension`,
`GraphSummaryCompletion`, `TripletCompletion`, `RagCompletion`, `Chunks`,
`Summaries`, `Temporal`, `Cypher`, `NaturalLanguage`, `FeelingLucky`, `Feedback`,
`CodingRules`, `ChunksLexical`, `HybridCompletion` (combines a per-query BM25
lexical pass over chunks, vector search over chunks/entities/edge-facts, and
1-hop graph-neighborhood expansion around matched entities, then answers via
LLM completion). Entry: [`cognee-search`](../crates/search/)
(`SearchBuilder` / `SearchOrchestrator`).

## Additional operations

These live in the [`cognee`](../crates/lib/) `api` module (and `DatasetManager`):

| Operation | What it does | rustdoc |
|---|---|---|
| **delete** | Cascading removal of data/datasets across relational → graph → vector → file storage (with dry-run preview). | [`cognee-delete`](../crates/delete/) `DeleteService` |
| **update** | Re-ingest changed data and re-cognify the affected subset. | `api::update` |
| **prune** | Reset system or all state (`prune_system` / `prune_data`). | `api::prune` |
| **visualize** | Render the graph to a single-file d3.js HTML page (Graph / Schema / Memory / Semantic tabs + inspector). | [`cognee-visualization`](../crates/visualization/) |

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
