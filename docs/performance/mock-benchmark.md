# Offline mock-LLM benchmark — how-to

This guide ties together the record/replay mock LLM, the deterministic mock
embeddings, and the `cognee-cli bench` driver into a single offline workflow: a
benchmark of the full `add → cognify → search` pipeline that runs with **no API
key**.

For the design rationale (why we mock both the LLM and the embeddings, and the
overall porting strategy) read [python-approach.md](python-approach.md) first —
in particular the end-to-end diagram in
[§6 End-to-end picture](python-approach.md#6-end-to-end-picture-target-state).

## What the mock benchmark is

The benchmark exercises the real pipeline but substitutes the two non-deterministic,
API-billed components:

- the **LLM** — replaced by a content-addressed *replay mock* that looks up
  recorded responses in a cassette by `sha256(input)`, so the same prompt always
  returns the same recorded answer;
- the **embedding engine** — replaced by a *deterministic mock* that derives each
  vector from `sha256` of the input text.

Because both are deterministic and offline, anyone (and CI) can reproduce the
pipeline timings from the committed fixtures. The flow is **record once → replay
forever**:

```
record once (real LLM, costs API) ──▶ cassette.json ──▶ replay forever (mock, free)
```

The cassette and corpus are committed under
[`scripts/perf/fixtures/`](../../scripts/perf/fixtures/) (`cassette.json`,
`memories.json`).

## Environment variables

| Variable | Value | Effect |
|----------|-------|--------|
| `MOCK_LLM` | `true` | Use the replay mock LLM instead of a real provider. |
| `MOCK_LLM_CASSETTE` | path | Cassette the replay mock loads when the mock is enabled via config rather than the CLI flag. (The `bench` subcommand instead requires the cassette via `--mock-memories`; see below.) |
| `COGNEE_RECORD_LLM` | path | Wrap the real LLM in a recorder and write the cassette to this path on flush/drop. This is the record path — needs real credentials. |
| `MOCK_EMBEDDING` | `deterministic` | Use the deterministic SHA-256 mock embedding engine (offline, free). |

`MOCK_LLM` / `MOCK_LLM_CASSETTE` / `COGNEE_RECORD_LLM` are read in
`cognee`'s config (`crates/lib/src/config.rs`); `MOCK_EMBEDDING` is read in
the embedding config (`crates/embedding/src/config.rs`).

## Feature flags

Two named features gate the moving parts; both are **default-on** and pull in no
heavy dependencies, but are kept named so they can be compiled out.

- **`mock`** — the record/replay cassette mock in `cognee-llm`. Surfaced as
  **`mock-llm`** in `cognee` and `cognee-cli` (`mock-llm = ["cognee/mock-llm"]`
  → `["cognee-llm/mock"]`).
- **`bench`** — the `cognee-cli bench` subcommand.

To build without them:

```sh
# CLI without the bench subcommand and without the mock LLM:
cargo build -p cognee-cli --no-default-features
# …or keep other defaults and drop just one (re-list the defaults you want).
```

## Running the benchmark

### N-run percentile report (recommended)

[`scripts/perf/run_mock_bench.sh`](../../scripts/perf/run_mock_bench.sh) drives
the shared Python percentile orchestrator against the Rust CLI in offline mode,
producing a percentile table plus an HTML report. It defaults to the committed
fixtures and needs no API key:

```sh
RUNS=3 scripts/perf/run_mock_bench.sh
```

Output lands in `target/perf/` by default: `report.json` (the full report —
percentile `stats`, `config`, and the `raw_runs` array) and `report.html` (the
rendered percentile report). Override `RUNS`, `COGNEE_PY`,
`BENCH_BIN`, `CASSETTE`, `MEMORIES`, and `OUT_DIR` via env vars — see the script
header and [`scripts/perf/README.md`](../../scripts/perf/README.md).

### A single mock run directly

```sh
MOCK_LLM=true MOCK_EMBEDDING=deterministic \
  cargo run --release -p cognee-cli --features bench -- bench \
    --mock-llm --mock-memories scripts/perf/fixtures/cassette.json \
    --memories scripts/perf/fixtures/memories.json \
    --output /tmp/mock_run.json
```

For the `bench` subcommand, `--mock-llm` requires the cassette to be passed via
`--mock-memories` (it overrides `MOCK_LLM_CASSETTE`). `--num-memories N`
truncates the corpus to the first `N` entries.

## The bench result JSON contract

`cognee-cli bench` writes one result document per run (to `--output`, and to
stdout when `--output` is omitted). The field order and key names match Python so
the shared orchestrator can drive either SDK unchanged:

```jsonc
{
  "memories_count": 50,
  "add_time_s": 0.0,
  "cognify_time_s": 0.0,
  "total_ingest_time_s": 0.0,
  "prune_time_s": 0.0,
  "db_setup_time_s": 0.0,
  "search_time": 0.0,
  "status": {                 // per-phase: "success" or "failed: <msg>"
    "prune": "success",
    "db_setup": "success",
    "add": "success",
    "cognify": "success",
    "search": "success"
  },
  "success": true,
  "config": {
    "llm_model": "...",
    "embedding_model": "...",
    "embedding_dimensions": 0,
    "dataset_name": "bench_memories",
    "mock_llm": true
  },
  "node_count": 0,          // graph size after cognify (stale-cassette guard)
  "edge_count": 0
}
```

The ingest/prune/db-setup phase times are rounded to 3 decimals (Python
`round(x, 3)` parity); `search_time` is reported unrounded, matching Python. Once a run
completes and the result file is written the process exits `0` even if individual
phases failed — failures are captured in `status` / `success`. The process exits
nonzero only for catastrophic errors (bad arguments, unreadable corpus,
unwritable `--output`).

## Regenerating the cassette

Re-record the cassette whenever the corpus, the cognify prompts, or the chunking
behaviour changes (a stale cassette shows up as silent empty-graph fallbacks).
This is the only step that needs real LLM credentials.

The record command (`COGNEE_RECORD_LLM=…`) and the post-record verification step
are documented in the
[`## Regenerating the cassette (perf-record)`](../../scripts/perf/README.md#regenerating-the-cassette-perf-record)
section of [`scripts/perf/README.md`](../../scripts/perf/README.md) — follow it
there rather than copying the command here.
