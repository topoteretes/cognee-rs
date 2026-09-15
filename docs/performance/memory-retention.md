# Memory retention: measuring cognify's bytes-per-document slope

SDK-507 ("Bound in-memory retention in cognify") proposes three tiers of fix —
clone elimination, per-wave flush, full streaming — and gates the second and
third on **measuring the bytes-per-document slope first**. Until this document,
nothing in the repo could produce that number: no allocator hook, no `dhat`, and
the only benchmark measured wall-clock.

This page describes the instrument that now exists, how to run it, and what the
first measurement says.

## What is instrumented

`cognee-cli bench` samples the process peak RSS at each phase boundary and
reports it in a `memory` block on the result JSON, alongside the corpus size and
cognify's own counts:

```jsonc
"memory": {
  "peak_rss_supported": true,
  "corpus_bytes": 18601,            // exact bytes of document text handed to add
  "corpus_documents": 50,
  "peak_rss_bytes_baseline": 25952256,        // after component init, before add
  "peak_rss_bytes_after_add": 147423232,
  "peak_rss_bytes_after_cognify": 382238720,  // the y-axis
  "peak_rss_bytes_after_search": 394772480,
  "add_high_water_delta_bytes": 121470976,
  "cognify_high_water_delta_bytes": 234815488,
  "cognify_chunks": 50,
  "cognify_embeddings": 331
}
```

The block is purely additive — the shared Python percentile orchestrator reads
the keys it knows by name and ignores it.

### What `peak_rss` is, precisely

The reading is `getrusage(2)`'s `ru_maxrss`, which is a **high-water mark over
the whole process lifetime**. It never decreases and cannot be reset. So:

* A per-phase sample is "the highest RSS the process had reached by the end of
  that phase", not that phase's own peak.
* A delta between two samples is how much the later phase *raised* the mark. A
  phase that allocates heavily but stays under an earlier peak reports zero.
* Resident set is not live allocated bytes: freed pages the allocator has not
  returned to the OS still count. RSS is an **upper bound** on live data, which
  is the conservative direction for an OOM question.
* It is a **whole-process** figure. It includes the vector store's buffers and
  the allocator's retained pages, so on its own it does **not** attribute growth
  to cognify's payload clones. That attribution needs an allocator-level
  profiler, which this repo still does not have.

`ru_maxrss`'s unit is famously not portable — bytes on Darwin, kilobytes on
Linux and the BSDs. The conversion is pinned by a unit test in
`crates/cli/src/commands/bench_rss.rs`; a 1024× error here would be a silent
1024× error in the only number this ticket turns on.

### Why no per-document figure appears in the result

Peak RSS at one corpus size is `baseline + slope × corpus`, and the baseline —
tokenizer, embedding engine, graph and vector backends, tokio and rayon stacks —
is around 200 MiB and fixed. One run's peak divided by its document count is
therefore mostly baseline and badly overstates the slope. **The slope only
exists across two or more corpus sizes.** That is why the bench emits raw
samples and the fit lives in a sweep script.

## Running the sweep

`scripts/perf/measure_retention.py` runs the bench at several `--num-memories`
sizes over one corpus — each in its own process, because `ru_maxrss` is a
process-lifetime mark — and fits peak RSS against corpus bytes, documents and
chunks.

```sh
# Offline, no API key: replays a committed cassette with mock embeddings.
python3 scripts/perf/measure_retention.py \
    --memories scripts/perf/fixtures/memories.json \
    --cassette scripts/perf/fixtures/cassette.json \
    --sizes 10,20,30,40,50

# Re-fit results already on disk without re-running anything.
python3 scripts/perf/measure_retention.py --reuse --sizes 10,20,30,40,50
```

The same runs print the cross-chunk edge-resolution counters (`cross_chunk`,
`max_cross_chunk_distance`), so one sweep settles both of SDK-507's tier-2
prerequisites.

The fit itself is doctested:

```sh
python3 -m doctest scripts/perf/measure_retention.py
```

### The stale-cassette trap

The replay mock's default miss policy is `EmptyGraph`, and it is **silent**: a
cassette that no longer matches the prompts returns empty graphs, extraction
produces no entities and no edges, and the run still reports `success: true`.
`bench`'s own stale-cassette guard defaults to a floor of one node, which chunk
and document nodes satisfy on their own, so it does not catch this.

A run in that state measures chunking, embedding and storage with no entities at
all. Its slope is a real lower bound but not the number this ticket is about.
The sweep script detects it (zero `attempted` edges) and prints a loud
structure-only banner. Pass `--min-graph-nodes <recorded baseline>` through to
`bench` to make the run fail instead.

## First measurement

Taken on `c6bedef`, macOS 26.2, Apple M4 Pro, 48 GiB, `rustc 1.91.1`, release
build, `--mock-llm` replay with `MOCK_EMBEDDING=deterministic`, 1536-dimension
embeddings.

### Live extraction — `scripts/perf/fixtures/memories.json`, 10→50 documents

The 50-memory fixture's cassette is current, so these runs extract real entities
(204 edges attempted at n=50).

| docs | corpus | chunks | embeddings | edges | peak after add | peak after cognify |
|-----:|-------:|-------:|-----------:|------:|---------------:|-------------------:|
| 10 | 3.5 KiB | 10 | 57 | 33 | 140.0 MiB | 236.1 MiB |
| 20 | 7.0 KiB | 20 | 126 | 77 | 140.0 MiB | 271.3 MiB |
| 30 | 10.7 KiB | 30 | 204 | 129 | 140.6 MiB | 313.3 MiB |
| 40 | 14.4 KiB | 40 | 270 | 167 | 140.2 MiB | 348.5 MiB |
| 50 | 18.2 KiB | 50 | 331 | 204 | 140.6 MiB | 363.9 MiB |

**Slope: ~3.49 MB of peak RSS per document (= per chunk here), R² = 0.98,
intercept 207 MiB.**

### Structure only — `scripts/perf/fixtures/large/memories.json`, 17→135 chapters

The large cassette is **stale on `c6bedef`**: all four runs extracted zero edges.
These numbers are a lower bound covering chunking, embedding and storage with no
entities.

| docs | corpus | chunks | embeddings | edges | peak after cognify |
|-----:|-------:|-------:|-----------:|------:|-------------------:|
| 17 | 180.7 KiB | 20 | 40 | 0 | 228.3 MiB |
| 34 | 313.6 KiB | 38 | 76 | 0 | 245.2 MiB |
| 68 | 641.4 KiB | 78 | 156 | 0 | 277.5 MiB |
| 135 | 1.2 MiB | 150 | 300 | 0 | 349.2 MiB |

**Slope: ~123 bytes of peak RSS per corpus byte, ~971 KB per chunk, R² = 0.996.**

### Reading the two together

* Retention grows **linearly** with corpus size in both, at high R². There is no
  plateau in the measured range.
* Text volume is not the driver. The structure-only run retains ~971 KB per
  ~8.4 KB chunk — about 116× the chunk text. The ticket's model predicts far
  less: text at ~7 copies is ~59 KB, and the vector term
  (2 embeddings/chunk × 1536 dims × 4 B × 4 copies) is ~98 KB, for ~157 KB in
  total. The measurement is **~6× the model**, on the run that has no entities
  at all — so the model is missing most of the bytes even in its easiest case.
* Entities dominate. Adding live extraction takes the per-chunk figure from
  ~971 KB to ~3.49 MB, and the 50-document live run peaks *higher* (364 MiB) than
  the 135-document structure-only run (349 MiB) on 1/65th of the text.
* `peak_rss_bytes_after_add` is flat across corpus size in both sweeps: `add`
  streams to disk and retains nothing corpus-proportional. The growth is
  cognify's.
* Extrapolating the live slope to the 171,888-document corpus that prompted
  SDK-507 gives roughly **560 GiB**, against a 30 GiB box. The ticket's own
  estimate was 3.3 GB. The extrapolation is three orders of magnitude beyond the
  measured range and is not a prediction — but the direction is not in doubt.

### Cross-chunk edge resolution

`cross_chunk = 0` and `max_cross_chunk_distance = 0` on the live 50-document run
(204 edges attempted, 2 recovered by name). On that corpus, run-global endpoint
resolution buys nothing a per-chunk wave would not, so the hazard SDK-507 warns
about for tier 2 — a wave-global registry losing cross-wave recoveries — costs
zero there.

This is one data point on a corpus of 50 unrelated memories, which is the case
*least* likely to have entities recurring across documents. A coherent corpus is
exactly where the number would be non-zero. Re-measure on the large corpus once
its cassette is re-recorded.

## What this settles, and what it does not

**Settled:** peak RSS scales linearly with corpus size, steeply, and the growth
is in cognify rather than add. The slope is now measurable, repeatably and
offline, by anyone.

**Not settled:** *where* the bytes are. Peak RSS is whole-process, so it cannot
distinguish cognify's payload clones — the thing tiers 2 and 3 would fix — from
the vector store's buffers or from allocator pages that were freed but not
returned. The structure-only run retaining ~6× what the
clone-plus-vector model accounts for is a strong hint that payload clones are
*not* the dominant term, which would make a per-wave flush the wrong fix. Confirming that needs an
allocator-level profiler (`dhat`, heaptrack), which this repo still lacks.

So tiers 2 and 3 remain gated — no longer on "there is no measurement", but on
attributing the measurement.
