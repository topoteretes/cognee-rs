# Mock-LLM Percentile Benchmark — Python Approach & Rust Porting Strategy

This document explains, in detail, how the Python `cognee` repository benchmarks
the `add → cognify → search` pipeline with **mocked LLM and embedding backends**,
why that design is valuable, and how we intend to port it to `cognee-rust`.

It is the design-rationale document behind the offline mock benchmark. The
implementation has landed; for the user-facing workflow see
[mock-benchmark.md](mock-benchmark.md).

---

## 1. Where the Python code lives

The Python reference lives in the sibling checkout at `../cognee` (i.e.
`/home/dmytro/dev/cognee/cognee`). The relevant files are:

| File | Role |
|---|---|
| [`statistics_percentile_report.py`](../../../cognee/cognee/tests/performance/statistics_percentile_report.py) | **Orchestrator / reporter.** Runs the bench N times, computes percentiles, prints a table, emits a Chart.js HTML report. |
| [`statistics_percentile/bench_cognee.py`](../../../cognee/cognee/tests/performance/statistics_percentile/bench_cognee.py) | **Bench driver.** One full pipeline run (prune → setup → add → cognify → search), phase-timed, writes a JSON result. |
| [`statistics_percentile/memories.json`](../../../cognee/cognee/tests/performance/statistics_percentile/memories.json) | Input corpus: array of `{title, content, references}` memories. |
| [`statistics_percentile/mock_memories.json`](../../../cognee/cognee/tests/performance/statistics_percentile/mock_memories.json) | Hand-authored mock responses: per-title `knowledge_graph` + `summary`. |

> Note: `../cognee` is the canonical checkout for this work. Do **not** clone the
> Python repo into `/tmp`; reference `../cognee` directly so the orchestrator and
> fixtures stay in sync with whatever revision is checked out there.

---

## 2. The two-layer Python design

The Python tooling is deliberately split into an **orchestrator** and a **bench
driver**, communicating only through a subprocess boundary and a JSON file.

### 2.1 Orchestrator — `statistics_percentile_report.py`

A pure-stdlib script (`argparse`, `json`, `subprocess`, `time`, `datetime`,
`pathlib`). **It never imports `cognee`** — it treats the bench as a black box:

1. For each of `--runs N` iterations, it shells out to `bench_cognee.py
   --output <tmpfile> <forwarded flags>` (`run_single`, line ~59).
2. It reads the JSON the bench wrote, and adds its own `wall_time_s` (measured by
   timing the subprocess).
3. After all runs it computes per-metric statistics — `min`, `max`, `mean`, and
   `p50/p75/p90/p95/p99` — via a linear-interpolation `percentile()` helper
   (line ~48).
4. It prints a fixed-width table (`print_report`) and writes a self-contained
   Chart.js HTML report (`generate_html`).

Two details matter for the port:

- **Hardcoded invocation.** The command is built as
  `[sys.executable, str(BENCH_SCRIPT), ...]` (line ~63). There is no env var or
  flag to redirect it — so pointing it at a non-Python bench requires a tiny edit
  (see §5.3).
- **Mock skips the cooldown.** Between real-LLM runs the orchestrator sleeps 60s
  to avoid rate limits (`if i != 1 and not args.mock_llm: time.sleep(60)`,
  line ~363). In `--mock-llm` mode the sleep is skipped — runs are back-to-back.

The required JSON contract (what the bench must emit) — `build_report` reads each
key with `r[metric]` (not `.get`), so **all of these must be present**:

```
add_time_s, cognify_time_s, total_ingest_time_s,
search_time, prune_time_s, db_setup_time_s
```

plus `config` (`llm_model`, `embedding_model`, `embedding_dimensions`),
`status` (object: phase → `"success"` | `"failed: …"`), `success` (bool), and
`memories_count`. `wall_time_s` is added by the orchestrator, not the bench.

### 2.2 Bench driver — `bench_cognee.py`

One pipeline run, timing each phase with `time.time()`:

1. `prune` (`prune_data` + `prune_system`) — clean slate, timed as `prune_time_s`.
2. `setup` — DB setup, timed as `db_setup_time_s`.
3. `add` — `cognee.add(text_list)`, timed as `add_time_s`.
4. `cognify` — `cognee.cognify(...)`, timed as `cognify_time_s`.
5. `search` — one `cognee.search(...)`, timed as `search_time`.

Each phase is wrapped in try/except so a failure is recorded in `status` rather
than aborting the run. Results are serialized to `--output`.

---

## 3. How the mock LLM / embedding works (the core idea)

The mock path is `_install_mocks()` in `bench_cognee.py` (line ~86). It relies on
**Python monkey-patching** — two runtime substitutions installed before the
pipeline runs:

### 3.1 Mock LLM — content-aware substitution

It replaces `LLMGateway.acreate_structured_output` with a function that:

- Scans the incoming `text_input` for any **memory title** present in
  `mock_memories.json`.
- On a match, returns that memory's **pre-authored** `KnowledgeGraph` (for graph
  extraction) or `SummarizedContent` (for summarization) — parsed straight from
  the fixture.
- On no match, returns an empty `KnowledgeGraph(nodes=[], edges=[])` (or a stub
  summary).

So the mock is **content-aware**, not a fixed queue: the same chunk always yields
the same graph, regardless of call order or batching.

### 3.2 Mock embedding — deterministic hash vectors

It swaps the embedding engine for one whose `embed_text` derives each vector from
`sha256(text)` (4-byte little-endian float slices, clamped to `[-1, 1]`). Vectors
are deterministic and content-stable across runs — so search results don't drift.

### 3.3 Why this is the valuable part

Mocking both backends removes **all network latency and all nondeterminism**:

- **Speed** — runs are bound by local CPU/IO (chunking, UUID5, SQLite, graph,
  vector writes), not API round-trips. The 60s inter-run cooldown disappears.
- **Determinism** — identical inputs produce identical graphs/vectors, so timing
  variance reflects the *engine*, not the model provider.
- **Zero cost / offline / CI-friendly** — no API key, no quota, no flakiness.

In other words: the mock benchmark measures **pure pipeline overhead**, which is
exactly the number a Rust port wants to track and regression-test.

---

## 4. The fixture-authoring problem (and why we add a recorder)

In Python the mock responses in `mock_memories.json` are **hand-authored** — ~70KB
of manually written knowledge graphs and summaries, one per memory title. That is
tedious to produce and drifts from whatever the real model actually returns.

For the Rust port we improve on this with a **response recorder**: a decorator
that wraps the *real* LLM, forwards every call, and captures
`(input → parsed response)` pairs into a JSON **cassette**. The flow becomes:

```
record once (real LLM, costs API) ──▶ cassette.json ──▶ replay forever (mock, free)
```

This is the "VCR / cassette" pattern. It is strictly better than hand-authoring:
the fixture is generated from genuine model output and refreshed with a single
command. It is also reusable far beyond benchmarking — deterministic e2e tests,
offline demos, and debugging all benefit.

---

## 5. Porting strategy to Rust

The whole point of a Rust port of the mock benchmark is a deterministic,
zero-API, fast pipeline-overhead measurement that can run in CI and be compared
**directly** against the Python numbers.

### 5.1 The one architectural obstacle

Python injects mocks by monkey-patching a live process. **Rust cannot do that.**
The mock must instead be a **first-class, selectable provider**, exactly mirroring
how the embedding layer already exposes `MOCK_EMBEDDING`
([`crates/embedding/src/config.rs:194`](../../crates/embedding/src/config.rs#L194)).

The good news: every LLM call funnels through a single object-safe method —
`create_structured_output_with_messages_raw(messages, schema, opts) -> Value`
([`crates/llm/src/llm_trait.rs:54`](../../crates/llm/src/llm_trait.rs#L54)) — plus
`generate` and `transcribe_image`. That single chokepoint is where both the
recorder (decorator) and the replay mock plug in, and it returns the
already-parsed `Value` the pipeline consumes, so a recording is a perfectly
replayable cassette.

### 5.2 Replace title-substring matching with content-addressed hashing

Rather than port Python's fuzzy title-substring match, the Rust mock keys
responses on a stable `sha256(user-message content + canonical schema)`. This is
more robust (no false matches), and it fits the repo's existing deterministic-ID
philosophy (UUID5 content addressing).

### 5.3 Reuse the Python orchestrator instead of porting it

The orchestrator is pure stdlib and SDK-agnostic, so we **reuse it as-is** from
`../cognee` rather than re-implement percentile math + HTML in Rust. The only
change needed is to make its hardcoded invocation overridable — a ~3-line
`BENCH_CMD` env hook:

```python
BENCH_CMD = os.environ.get("BENCH_CMD")  # e.g. "/path/to/cognee-cli bench"
cmd = (shlex.split(BENCH_CMD) if BENCH_CMD
       else [sys.executable, str(BENCH_SCRIPT)]) + ["--output", tmp_path] + extra_args
```

Then `BENCH_CMD="cognee-cli bench" python ../cognee/.../statistics_percentile_report.py
--runs 10 --mock-llm` drives the Rust pipeline through the Python reporter, and
the Python and Rust numbers land in the same table/HTML — genuinely
apples-to-apples.

### 5.4 No new binary — a CLI subcommand

The orchestrator only needs *one process that runs all phases and writes the
JSON*. `cognee-cli` already is that single-process entry point (it wires up
`ComponentManager`, config, and the add/cognify/search pipelines). So the bench
driver becomes a **`cognee-cli bench` subcommand**, not a new binary — reusing all
existing bootstrapping. (The existing Criterion bench at
[`crates/bench/benches/batch_add_cognify.rs`](../../crates/bench/benches/batch_add_cognify.rs)
stays as-is for its HTTP / real-LLM scenario.)

### 5.5 What we deliberately do not port

- **Embedding *recording*.** A deterministic SHA-256 mock embedding (port of the
  Python scheme) is sufficient and far smaller than recording real vectors.
- **The HTML/percentile reporter.** Reused from `../cognee`, not reimplemented.

---

## 6. End-to-end picture (target state)

```
                ┌─────────────────────────────────────────────────────────┐
   record once  │  cognee-cli bench --memories memories.json              │
   (real LLM)   │     COGNEE_RECORD_LLM=cassette.json                      │
                │        OpenAIAdapter ──wrapped by──▶ RecordingLlm ──▶ cassette.json
                └─────────────────────────────────────────────────────────┘
                                          │
                                          ▼  (commit cassette.json)
                ┌─────────────────────────────────────────────────────────┐
   replay N×    │  BENCH_CMD="cognee-cli bench" \                          │
   (mock, free) │  python ../cognee/.../statistics_percentile_report.py \  │
                │      --runs 10 --mock-llm                                │
                │                                                          │
                │  per run:  cognee-cli bench --mock-llm --output r.json   │
                │     MOCK_LLM=true  ──▶ ReplayLlm(cassette.json)          │
                │     MOCK_EMBEDDING=deterministic ──▶ hash vectors        │
                │                                                          │
                │  orchestrator ──▶ percentile table + Chart.js HTML       │
                └─────────────────────────────────────────────────────────┘
```

See [mock-benchmark.md](mock-benchmark.md) for how to run the offline benchmark.
