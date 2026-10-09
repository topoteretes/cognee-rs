# Memory retention: measuring cognify's bytes-per-document slope

SDK-507 ("Bound in-memory retention in cognify") proposes three tiers of fix —
clone elimination, per-wave flush, full streaming — and gates the second and
third on **measuring the bytes-per-document slope first**. Until this document,
nothing in the repo could produce that number: no allocator hook, no `dhat`, and
the only benchmark measured wall-clock.

This page describes the instrument that now exists, how to run it, what the
first measurement says — and, since, where the bytes actually are and what was
done about them.

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
  to cognify's payload clones. That needs an allocator-level profiler — see
  [Attribution](#attribution-where-the-bytes-are).

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
* ~~Extrapolating the live slope to the 171,888-document corpus that prompted
  SDK-507 gives roughly **560 GiB**.~~ **Withdrawn** — see
  [Attribution](#attribution-where-the-bytes-are). Every run above stays below
  one 500-item graph write batch, so the fit measured a ramp that stops there,
  not a slope that continues.

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

## Attribution: where the bytes are

Peak RSS cannot say *what* holds the memory, so the measurement above was
followed by an allocator-level one: valgrind's `massif`, which needs no code
change or allocator swap and records the allocation stacks live at the peak.
The release profile already carries line tables, so the stacks resolve.

```sh
cargo build --release -p cognee-cli --features bench
# From outside the repo: its .env selects a vector provider bench does not register.
VECTOR_DB_PROVIDER=lancedb MOCK_EMBEDDING=deterministic RUST_LOG=warn \
  valgrind --tool=massif --depth=60 --massif-out-file=n2000.massif \
  /path/to/cognee-rust/target/release/cognee-cli bench --mock-llm \
    --mock-memories scripts/perf/fixtures/cassette.json \
    --memories <corpus.json> --num-memories 2000 --output n2000.json
ms_print n2000.massif | less    # the peak snapshot's tree is the answer
```

Use massif's default heap mode, not `--pages-as-heap`: lance and ladybug reserve
tens of GiB of address space they never touch, which that mode counts.

To get past the 50 documents the replay cassette covers, the sweep was rerun on
a synthetic corpus of 8,000 distinct ~470-byte documents. The cassette misses
every prompt, so these runs are **structure-only** (no entities) — but they are
the only runs that cross the batch sizes below.

### The measured slope was a ramp

At 50 documents the heap peak is almost all ladybug: 100 MiB is the buffer
manager's eviction queue, allocated once at startup, and ~94 MiB is one
`UNWIND [...] MERGE` statement in `LadybugAdapter::add_edges` — lbug binds the
inlined literal list into value vectors for the whole batch. Both are fixed or
capped: the batch is 500 items, and a standalone probe of the adapter shows node
writes flat past 500 and edge writes flat past 2,000–4,000. The synthetic sweep
on `main` bends accordingly:

| docs | peak RSS after cognify | marginal slope |
|-----:|-----------------------:|---------------:|
| 50 → 250 | 313 → 444 MiB | ~600–740 KB/doc |
| 250 → 500 | → 490 MiB | ~190 KB/doc |
| 500 → 2,000 | → 666 MiB | ~104–133 KB/doc |
| 2,000 → 8,000 | 651 → 1,523 MiB | ~149 KB/doc |

The last row is a separate sweep over the 8,000-document corpus, hence 651
rather than 666 MiB at 2,000; run-to-run spread is ~±15 MiB.

### What kept growing: vector copies

Past the ramp the peak moves into `index_data_points` → `index_points`. At 2,000
documents it held the ~4,000 `EdgeType_relationship_name` vectors (23.4 MiB a
copy) 7–8 times over: the caller's points, the adapter's id fold, its
membership-merge clone, the Arrow batch, two buffers in lance's encoder, and the
chunk/entity/summary vectors `add_data_points` generated up front and returned
in `CognifyResult::embeddings`. All but the encoder's grew with the corpus,
because cognify wrote each collection in one call.

Payload clones of chunk text — what tiers 2 and 3 of SDK-507 target — did not
appear at the peak.

### Fixes, and what they bought

* **The adapter's copies** (`perf(vector)`): the id fold borrows when no id
  repeats, the membership merge carries merged metadata instead of cloning
  points, and LanceDB writes go out in ~16 MiB batches.
* **The caller's copy** (`perf(cognify)`): each collection is embedded, built
  and written in ~16 MiB windows. `VectorDB::announce_bulk_write` tells the
  store the total first, so pgvector's HNSW deferral decides on the first window
  as it did for one call.
* **The retained vectors** (SDK-711): `CognifyResult::embeddings` is filled only
  when `CognifyConfig::retain_embeddings` is set; `embedding_count` carries the
  count either way.

Structure-only synthetic corpus, LanceDB, mock embeddings, 1536 dimensions:

| docs | `main` | adapter + windows | + no retained vectors |
|-----:|-------:|------------------:|----------------------:|
| 2,000 | 651 MiB | 606 MiB | 600 MiB |
| 4,000 | 975 MiB | 722 MiB | 734 MiB |
| 8,000 | 1,523 MiB | 838 MiB | 840 MiB |

The adapter and window fixes take the 4,000 → 8,000 marginal slope from
~144 KB to ~27–30 KB per document and the 8,000-document peak down 45%.

Dropping the retained vectors does not move the peak RSS, and massif says why:
over the whole 8,000-document run cognify's **heap is flat** — it sits at
~244–250 MiB from the first batch to the last, about 1 KB per document. With
the vector phase windowed, nothing corpus-sized is left on the heap for the
retained vectors to sit on top of; they still go, because at a larger scale or
with entity collections they would be the next corpus-sized allocation.

Two things follow:

* **The remaining ~27 KB/doc of RSS growth is not heap.** It is outside
  `malloc` — ladybug's buffer-pool pages and lance's mapped files are the
  candidates — or allocator pages freed but not returned. massif's heap mode
  cannot see it and `--pages-as-heap` drowns in reserved address space;
  attributing it needs `/proc/<pid>/smaps` sampled per phase.
* **The process-wide heap peak at 8,000 documents is in `bench`'s search
  phase, not cognify:** 277.7 MiB of lance decoding vector pages for a flat
  KNN over an unindexed collection, which is also why `peak_rss_bytes_after_search`
  (1,351 MiB) sits well above `peak_rss_bytes_after_cognify` (840 MiB). That
  is a search-side cost and outside SDK-507.

## What this settles, and what it does not

**Settled:** the slope the first measurement fitted was mostly ladybug's
batch-capped write buffers ramping up, and the part that kept growing was copies
of vectors on the write path — now bounded per window. Payload clones were not
the dominant term, so a per-wave flush (tier 2) is not the fix this data calls
for.

**Not settled:**

* **Live extraction past 50 documents.** Everything beyond that is
  structure-only until the large cassette is re-recorded. Entities add their
  own collections (`Entity_name`, `EntityType_name`, `Triplet_text`) to the
  same windowed path, but the graph side of a coherent corpus is unmeasured.
* **pgvector.** The attribution is on LanceDB; the Postgres write path has its
  own batching and was not profiled.
* **What is left.** Cognify's heap is flat; the remaining RSS growth is
  outside the heap and needs a page-level attribution (`smaps`) before tiers 2
  and 3 are reconsidered — neither would touch buffer-pool or mapped pages.
