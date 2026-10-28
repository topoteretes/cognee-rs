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
picks the retrieval strategy by the first matching rule, as Python does: a
quoted phrase → `CHUNKS_LEXICAL`, coding-rules wording → `CODING_RULES`, a time
preposition before an absolute date → `TEMPORAL`, anything else →
`HYBRID_COMPLETION`. A routed type that the backend rejects, or a lexical or
rules search that finds nothing, is retried as `HYBRID_COMPLETION`. It is session-aware (reads
session history when given a `--session-id`) and graph-backed. Results are
returned to the caller (printed to stdout by the CLI).

`recall ≈ auto-routed search`. rustdoc: `api::recall`.

### improve

Runs the self-improvement loop over a dataset as nine gated stages, in
Python's registry order: `feedback_weights`, `persist_session_qa`,
`persist_agent_traces`, `extract_agent_context`, `distill_sessions`,
`update_user_preferences`, `build_truth_subspace`, `review_conflicts`,
`global_context_index`. The first seven read the given sessions and are
skipped with `no_session_ids` without them; the last two work on the graph
alone and are opt-in. Each stage reports a `StageResult` (`status`, `reason`,
`error`, `counts`, `duration_ms`) on the returned `ImproveResult`, whose
`status` summarises them (`completed`, `errored`, `skipped`, `running`).

The run fails open — a failed stage is recorded and the next one runs —
except that a failure in `persist_session_qa` (losing session Q&A would be data
loss) stops the run with `ApiError::ImproveAborted` carrying the partial
result, and an exhausted LLM budget skips every later stage with
`budget_exhausted`. The stages that draft text with an LLM are skipped with
`no_llm_configured` when none is configured. `extract_agent_context`,
`update_user_preferences` and `review_conflicts` are not ported yet and always
report a skip reason. rustdoc: `api::improve`.

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

Turns stored data into a knowledge graph in five stages: **classify** documents →
**chunk** text → **extract** entities/relationships (LLM, batched) **and
summarize** (conditional) — one fused stage running both concurrently over the
same chunks, matching Python's `extract_graph_and_summarize` → **add data
points** (six vector collections + provenance to the relational DB) →
**extract DLT FK edges**. Configurable via `CognifyConfig`
(chunk strategy, custom prompts/schemas, temporal mode). Pipeline:
[`cognee-cognify`](../crates/cognify/) (`cognify()` / `cognify_datasets()`).

Dates come out of the default extraction too, with no separate temporal
pipeline: a node the LLM types `Timestamp` whose name is an absolute time
(`1867`, `1969-07`, `23 March 1947`, `from 1803 to 1815`) becomes a shared
`Timestamp` graph node — one per normalized time string, carrying
`time_at`/`time_until` (ms since the epoch, UTC, half-open) and its
`precision` — that `*_at` edges point at and every chunk naming it `contains`.
Timestamps are not embedded. A name that does not parse stays an ordinary
entity. Year-less dates in the text ("27 April", "four weeks later") are
resolved against the last stated year earlier in the document and handed to the
default prompt as normalization hints; a custom prompt receives none.

An extracted entity's id derives from its **name** (`Entity.id_for(name)`), as in
Python, so the same name from two chunks or documents is one node; several nodes
sharing a name inside one chunk's graph get deterministic ordinal ids instead.
An extracted node with an empty type gets no `EntityType` and no `is_a` edge.
`COGNEE_COGNIFY_EXTRACTION_STRATEGY=cascade` swaps the single extraction call
for Python's multi-round cascade (see
[configuration](configuration.md#chunking--tokenizer)).

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

- **Default (`FailFast` + `WholeRun`).** The run stops at the first failed
  *batch* — not the first failed file, see below — everything it created is
  removed, the run is recorded `ERRORED` and the call returns `Err`. The store
  converges to its pre-run state — what earlier runs completed is untouched,
  because the sweep selects only rows naming *this* run. Python's default, in
  execution and in end state.
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

The abort point is a *batch* boundary, not a file boundary, and how much work
`FailFast` actually saves depends on how big that batch is. Graph extraction
stops after the batch that contained the failure — `chunks_per_batch`, default
2000, so a dataset under 2000 chunks is one batch and every extraction call is
dispatched before the failure is noticed. Temporal extraction stops after
`data_per_batch`, default 20 files. Summarization has no batch loop: it stops
dispatching as soon as a fatal failure comes back, but the calls already in
flight (up to `max_parallel_extractions`) are neither cancelled nor discarded.
`FailFast` is an upper bound on wasted spend, not a promise of none — see
[Cognify failure handling](configuration.md#cognify-failure-handling).

A sweep deliberately keeps anything still claimed from outside its scope: an
entity a surviving file also produced, an artifact another run or another
dataset also names, or a row written before ownership tracking existed. The
worst case is a surplus artifact that keeps an owner, never an artifact with no
owner at all.

That claim is recorded even when the artifact is not written. Graph extraction
filters out edges the graph already holds, so a file cognified in a later run
adds no second copy of an edge an earlier file created — but it still records
ownership of it. Without that row the earlier file would be the edge's only
owner, and deleting it would take the edge's vectors away from a file that still
references it. Ownership is per (dataset, data item): an edge shared across two
*datasets* is still governed by each dataset's own exclusivity check, the same
limitation entity ownership has.

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

Unified orchestration across 18 retrieval strategies selected by `SearchType`
([`crates/search/src/types/search_type.rs`](../crates/search/src/types/search_type.rs)):
`GraphCompletion`, `GraphCompletionCot`, `GraphCompletionContextExtension`,
`GraphCompletionDecomposition`, `GraphSummaryCompletion`, `GraphReport`, `TripletCompletion`, `RagCompletion`, `Chunks`,
`Summaries`, `Temporal`, `Cypher`, `NaturalLanguage`, `FeelingLucky`, `Feedback`,
`CodingRules`, `ChunksLexical`, `HybridCompletion` (default; combines vector search over
chunks/summaries/entities/edge-facts and
1-hop graph-neighborhood expansion around matched entities, then answers via
LLM completion). A hybrid search falls back to `GraphCompletion` when it sets
`node_type`, `neighborhood_depth` or `feedback_influence`, or when the
`DocumentChunk_text` collection is missing; setting `wide_search_top_k` or
`triplet_distance_penalty` with hybrid is an error. Entry: [`cognee-search`](../crates/search/)
(`SearchBuilder` / `SearchOrchestrator`).

The graph retrievers (`GraphCompletion` and its CoT, context-extension and
summary variants) rank triplets around the vector-search hits. A triplet's
score, as in Python, sums its source node's, target node's and edge's cosine
distances, each scaled by `2 - importance_weight` (0.5 when absent) and, with
`feedback_influence > 0`, blended with its `feedback_weight` (higher feedback
ranks better). An element with no vector match takes `triplet_distance_penalty`
(6.5) before that scaling — 9.75 at the default weight — and is not blended.
`importance_weight` is not clamped, as in Python: above about 1.69 the scaled
penalty is blended like a real match, and above 2 it turns negative and ranks
first. By default they
rank only the edges touching a hit. `neighborhood_depth` (a positive integer;
the CLI's `--neighborhood-depth`, the bindings' `neighborhoodDepth`) switches to
Python's neighborhood projection: load the `depth`-hop neighborhood of the
nearest `neighborhood_seed_top_k` hits (all of them when unset), score the
nodes the expansion found against the query (`VectorDB::score_by_ids`; an
adapter without it leaves them at the distance penalty), and rank every loaded
edge. Combined with `node_name`, which disables the seed search, it returns no
triplets, as in Python.

`brute_force_triplet_search_batch` is Python's batch mode
(`query_batch=`): several queries share one graph load and come back as one
ranked list each. As in Python it searches every vector collection without a
limit and loads the whole graph (or the NodeSet subgraph), so it costs far more
than a single search on a large store — `search.triplet_neighbor_cap` does not
bound it, and `wide_search_top_k`, `node_name` (in the vector search) and
`neighborhood_depth` do not apply. Unlike Python it skips `Triplet_text`, whose
ids are never graph nodes. `GraphRetrieverCore::search_batch` sends a lone
query to the single-query search instead, as `get_triplets_batch` does.

`GraphCompletionDecomposition` (`GraphCompletionDecompositionRetriever`, Python's
retriever of the same name) is its main caller. One structured LLM call splits the
question into at most five sub-questions (falling back to the question itself when
the call fails), one batch search fetches their triplets, and the batches are merged
without duplicates. With `retriever_specific_config.decomposition_mode`
`"answer_per_subquery"` (the default) each sub-question is answered over its own
triplets, concurrently, and the question is answered over those answers;
`"combined_triplets_context"` answers once over the merged triplets. Each context
item carries a `decomposition` entry (the sub-questions and the triplet's rank in
each) so the completion can rebuild the per-sub-question contexts. An empty graph is
`NotFound`, and a batch of queries is rejected.

`GraphReport` (`GraphReportRetriever`, Python's retriever of the same name) ignores
the query and reports on the whole graph from `get_graph_data`: the top-`top_k`
hubs (default 10) by normalised degree plus normalised PageRank (networkx's
defaults, computed here), entity-to-entity edges across different node sets, and
EXTRACTED vs DERIVED edge counts, followed by LLM-suggested questions. `NodeSet`
containers and `belongs_to_set` edges are left out and parallel edges count once,
as in Python's `nx.DiGraph`. Its context is one item holding the Markdown report and
the structured hubs, connections and counts; an `only_context` search answers the bare
report text, as Python's does. An empty graph gets a fixed report, not
`NotFound`. Python computes PageRank only when `scipy` is installed (it is not a
cognee dependency) and otherwise ranks by degree alone, so its PageRank column reads
`0.0000` on a default install; Rust always computes it.

`Temporal` is `HybridCompletion` reranked by the time window the question asks
about (`TemporalHybridRetriever`, Python's retriever of the same name). The hybrid
candidate fetch — chunks and entities at `candidate_top_k`, default `4 × top_k`,
settable as `retriever_specific_config.candidate_top_k` — runs concurrently with an
LLM call that extracts the window (UTC, start inclusive, end exclusive; relative
dates are not resolved, so no clock is read). The chunks attached to the
`Timestamp` nodes inside the window join the candidates, the candidates attached to
one — directly, or for a chunk through an entity it `contains` — move to the front,
tightest date first (for "18 March 1965" a chunk dated that day beats one that only
says "1965"; dates at least as precise as the question keep their similarity order),
and the same cut plain hybrid uses keeps `top_k`. With nothing to rerank by it
answers with the plain hybrid result and records why: `empty_graph`,
`no_time_constraint` / `invalid_interval` (no window), `no_temporal_match`,
`no_candidate_overlap` (`TemporalHybridRetriever::retrieve` returns these
diagnostics with the context). The `Timestamp` nodes are default cognify's; the
`Event` nodes of the separate temporal-cognify pipeline are not read, so such a
graph gets plain hybrid answers, as in Python.

## Additional operations

These live in the [`cognee`](../crates/lib/) `api` module (and `DatasetManager`):

| Operation | What it does | rustdoc |
|---|---|---|
| **delete** | Cascading removal of data/datasets across relational → graph → vector → file storage (with dry-run preview). | [`cognee-delete`](../crates/delete/) `DeleteService` |
| **update** | Re-ingest changed data and re-cognify the affected subset. | `api::update` |
| **prune** | Reset system or all state (`prune_system` / `prune_data`). | `api::prune` |
| **visualize** | Render the graph to a single-file d3.js HTML page (Graph / Schema / Memory / Semantic tabs + inspector). | [`cognee-visualization`](../crates/visualization/) |
| **export** | Write the graph as a COGX archive — the portable format Python cognee re-imports, locally or by uploading the packed `.cogx.tar.gz` to its remember endpoint. | [`cognee-migration`](../crates/migration/) |

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
| export | `cognee-cli export` | _(none yet)_ | — |

CLI flags and feature gates: [tools/cli.md](tools/cli.md). HTTP request/response
shapes: documented with the server, which is not part of this repository (see
[tools/http-server.md](tools/http-server.md)). Binding method
names per language: [tools/bindings.md](tools/bindings.md).
