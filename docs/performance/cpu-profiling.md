# CPU profiling of the offline pipeline — findings

Where the `add → cognify → search` pipeline actually spends CPU, measured with
zero external API calls via the mock-replay harness. This is the baseline the
optimisation follow-ups are measured against.

If you just want to reproduce these numbers, jump to
[Reproducing](#reproducing).

## TL;DR

- At the 50-memory fixture the pipeline is **await/IO-bound**, not CPU-bound —
  cognify is only ~11% on-CPU, so a sampling profiler sees almost nothing worth
  optimising. The CPU hot paths only appear at document scale.
- On the large corpus (Moby-Dick, 135 chapters, ~1.2 MB → 1232 nodes /
  2744 edges) the picture is clear:
  - **`add` is the biggest CPU cost** — 8.5 s wall, ~57% on-CPU, dominated by
    ingestion/chunking.
  - **`cognify`'s on-CPU time is dominated by graph writes** —
    `cognee.db.graph.query` runs **1061 times for 982 ms (~74% of cognify's
    on-CPU time)**, i.e. roughly one Cypher round-trip per graph element.
- **Top actionable win:** cut the graph-write Cypher path (prepared/cached
  statements and/or batched upserts). It is the cleanest hotspot — a single
  span, ~pure CPU, scaling linearly with graph size.

## Why a large corpus was necessary

The 50-memory fixture cannot surface CPU bottlenecks — most of the wall-clock is
spent awaiting, not computing. The graph-query hotspot is invisible at that size
(below 3 ms, not even in the top-3 spans) and only dominates once the graph is
large. That is the whole reason for the committed Moby-Dick fixture.

| | 50 memories | Moby-Dick (135 ch) |
|---|---|---|
| corpus text | 21 KB | 1.2 MB |
| graph size | 150 nodes / 100 edges | 1232 nodes / 2744 edges |
| add (wall) | 3.0 s | 8.5 s |
| cognify (wall) | 0.86 s | 5.76 s |
| search (wall) | 0.25 s | 0.35 s |
| cognify on-CPU fraction | **11%** | **26%** |
| top cognify hotspot | token-count update (53 ms) | **graph.query — 982 ms / 1061 calls** |

## Method

Two complementary views, both fully offline (no API key):

- **CPU sampling** — `pprof-rs` (SIGPROF, no `perf`/root) behind the `profiling`
  feature, emitting a per-phase flamegraph. Answers *where the CPU goes*.
- **Wall-clock telemetry** — a `tracing` layer that harvests the pipeline's
  existing `#[instrument]` spans and records per-span **busy** (on-CPU) vs
  **idle** (awaiting) time, dumped per phase as `<phase>.telemetry.json`.
  Answers *where the wall-clock goes*, including the off-CPU time the sampler
  is blind to.

Runs are `--release`, pinned to one core with `taskset`, replaying the committed
cassette with deterministic mock embeddings. Aggregation is by span name across
all instances, so for parallel stages the summed busy time can exceed real
wall-clock — read it as relative attribution, not an exclusive timeline.

## Per-stage breakdown (Moby-Dick)

Busy = on-CPU, idle = awaiting. Top spans per phase:

**add — 8.5 s wall, ~57% on-CPU (11.9 s busy across the rayon pool):**

| busy | calls | span |
|---|---|---|
| 5597 ms | 1 | `ingestion.add_with_params` |
| 562 ms | 135 | `cognee.db.relational.datasets.attach_data_to_dataset` |
| 70 ms | 135 | `ingestion.persist_data_with_acl` |

Ingestion itself is the cost — chunking, hashing and content processing over the
whole book. This is the largest single block of CPU in the pipeline.

**cognify — 5.76 s wall, 26% on-CPU (1325 ms busy):**

| busy | calls | span |
|---|---|---|
| **982 ms** | **1061** | **`cognee.db.graph.query`** |
| 291 ms | 135 | `cognee.db.relational.data.update_data_token_count` |
| 24 ms | 1 | `cognee.db.relational.graph_storage.upsert_nodes` |

`cognee.db.graph.query` is **74% of cognify's on-CPU time** and 1 ms idle —
essentially pure compute. 1061 queries for 1232 nodes + 2744 edges is ~one
Cypher execution per graph element.

**search — 0.35 s wall, ~63% on-CPU (249 ms busy):** small in absolute terms;
`cognee.search` (123 ms) and retrieval (`get_context`, `graph_search`) split it.
Retrieval is now visible because the bench exercises the no-LLM retrievers
(`Chunks`, `Summaries`) alongside `GraphCompletion`.

## What's inside `graph.query`

The flamegraph corroborates the telemetry: the top on-CPU cluster in cognify is
`execute_query → …$cxxbridge…connection_query → ` into Ladybug's embedded C++
engine, with ANTLR `prepare`/Cypher-parsing frames visible underneath. The C++
portion is **under-counted** in the flamegraph — pprof's frame-pointer unwinding
truncates across the cxxbridge/C++ boundary — which is why the span telemetry
(which times the whole synchronous call) attributes more to it than the raw
sample percentages show. Both agree on the direction: **the cost is executing
Cypher, per graph write.**

## Prioritised optimisations (follow-ups)

Each of these is a separate follow-up issue/PR; this doc is the measurement, not
the fix.

1. **Graph-write Cypher path** (highest impact). 1061 queries / 982 ms / ~74% of
   cognify on-CPU, scaling with graph size. Two angles: cache/prepare the Cypher
   templates so the ANTLR parser stops re-parsing them, and/or batch node/edge
   writes into fewer queries. Expected: a large cut to cognify CPU.
2. **Ingestion/chunking CPU** (largest absolute cost). `add` is 57% on-CPU at
   scale, ~5.6 s in `ingestion.add_with_params`. Worth profiling the chunking +
   hashing path for redundant passes and allocations.
3. **Batch `update_data_token_count`** — 135 per-document DB updates, 291 ms
   on-CPU in cognify. A batched update would remove most of it.

## Caveats

- Numbers are one machine, single-core-pinned, replaying one cassette — read
  ratios and rankings, not absolute milliseconds.
- Span busy/idle sums across concurrent instances can exceed wall-clock (see
  Method); `add`'s 11.9 s busy vs 8.5 s wall is the rayon pool, not an error.
- Flamegraph C++ frames are under-counted (frame-pointer unwinding), so
  `graph.query`'s share is a lower bound in the sampled view.

## Reproducing

Everything below is offline and needs no API key (the cassette is committed).
See [`scripts/perf/README.md`](../../scripts/perf/README.md) for the full recipe
including how the cassette was recorded.

```sh
# Large-doc corpus (already committed; regenerate only if needed):
python3 scripts/perf/build_large_corpus.py

# Replay + profile, offline:
MOCK_LLM=true MOCK_EMBEDDING=deterministic \
  taskset -c 0 cargo run --release -p cognee-cli --features bench,profiling -- bench \
    --mock-llm --mock-memories scripts/perf/fixtures/large/cassette.json \
    --memories scripts/perf/fixtures/large/memories.json \
    --profile-dir target/perf-profiles/large \
    --min-graph-nodes 1232 \
    --output /tmp/mock_large.json
```

Artifacts land in `target/perf-profiles/large/`: `<phase>.svg` (flamegraph) and
`<phase>.telemetry.json` (wall-clock breakdown). `--min-graph-nodes 1232` asserts
the recorded baseline so a stale cassette fails loudly instead of profiling an
empty graph.
