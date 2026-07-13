# Performance benchmark fixtures & scripts

This directory holds the offline (zero-API) benchmark harness for the
`cognee-cli bench` subcommand.

## Contents

- `fixtures/memories.json` — the input corpus: a 50-element JSON array of
  `{title, content, references}` objects, copied verbatim from the Python
  reference fixture
  `../cognee/cognee/tests/performance/statistics_percentile/memories.json`.
  The corpus contains **no random content**, so chunk text — and therefore the
  cassette lookup hashes — are stable across runs.
- `fixtures/cassette.json` — recorded LLM responses for that corpus
  (`LlmCassette`, version 1, recorded against `gpt-4o-mini`). Committed so that
  anyone — and CI — can replay the full `add → cognify → search` pipeline with
  **no API key**.
- `run_mock_bench.sh` — drives the shared Python percentile orchestrator against
  the Rust `cognee-cli bench` in offline `--mock-llm` mode (deterministic mock
  embeddings + the committed cassette). See the script header for env-var
  overrides (`RUNS`, `COGNEE_PY`, `BENCH_BIN`, `CASSETTE`, `MEMORIES`, …).

## Running the offline benchmark (no API key)

```sh
RUNS=3 scripts/perf/run_mock_bench.sh
```

Or a single mock run directly:

```sh
MOCK_LLM=true MOCK_EMBEDDING=deterministic \
  cargo run --release -p cognee-cli --features bench -- bench \
    --mock-llm --mock-memories scripts/perf/fixtures/cassette.json \
    --memories scripts/perf/fixtures/memories.json \
    --output /tmp/mock_run.json
```

## Regenerating the cassette (`perf-record`)

Re-record the cassette whenever the corpus, the cognify prompts, or the chunking
behaviour changes (a stale cassette shows up as silent empty-graph fallbacks —
the mock run's graph node/edge counts drop far below the recorded run's).

This is the **only** step that needs real LLM credentials. It makes real
outbound calls against a cheap model over the small (50-memory) corpus.

```sh
# Provide credentials (LLM_API_KEY / LLM_ENDPOINT, or the OPENAI_TOKEN /
# OPENAI_URL aliases — e.g. via a repo-root .env):
set -a; . ./.env; set +a

COGNEE_RECORD_LLM="$(pwd)/scripts/perf/fixtures/cassette.json" \
MOCK_EMBEDDING=deterministic \
  cargo run --release -p cognee-cli --features bench -- bench \
    --memories scripts/perf/fixtures/memories.json \
    --llm-model gpt-4o-mini \
    --output /tmp/record_run.json
```

`RecordingLlm` wraps the real adapter, passes every call through unchanged, and
writes the cassette on flush/`Drop`. `MOCK_EMBEDDING=deterministic` keeps
embeddings free and offline — only the LLM responses are recorded.

After recording, verify the cassette replays with a full hit rate by running the
offline mock command above with the API credentials cleared and confirming
`success: true` plus graph node/edge counts identical to the recording run.

## Large-document scenario (Moby-Dick)

The 50-memory fixture is too small to be CPU-bound. Most of cognify's wall time
is await/IO, not compute (see `--profile-dir` below). For a profile that
actually surfaces CPU hot paths, use a large book. `build_large_corpus.py`
turns Project Gutenberg's Moby-Dick (~1.2 MB) into a 135-chapter corpus in the
same `{title, content, references}` shape:

```sh
# writes scripts/perf/fixtures/large/memories.json (committed, deterministic)
python3 scripts/perf/build_large_corpus.py
```

Record its cassette once. This is the only step that needs credentials (about
$0.40 on `gpt-4o-mini`). Start small to prove the loop before spending on the
full book.

`LLM_MAX_PARALLEL_REQUESTS=1` keeps the recording under a rate-limited key's
tokens-per-minute cap; the default of 20 concurrent requests will trip a 429 on
a large book. `LLM_MAX_RETRIES=8` absorbs any remaining spikes.

```sh
set -a; . ./.env; set +a   # LLM_API_KEY / LLM_ENDPOINT (or OPENAI_* aliases)

# Cheap dry-run: first 3 chapters only (about $0.05).
LLM_MAX_PARALLEL_REQUESTS=1 LLM_MAX_RETRIES=8 \
COGNEE_RECORD_LLM="$(pwd)/scripts/perf/fixtures/large/cassette.json" \
MOCK_EMBEDDING=deterministic \
  cargo run --release -p cognee-cli --features bench -- bench \
    --memories scripts/perf/fixtures/large/memories.json \
    --num-memories 3 --llm-model gpt-4o-mini --output /tmp/record_large.json

# Full book: drop --num-memories. The committed cassette records 1189 nodes.
```

Then replay and profile fully offline (no key). `--profile-dir` emits a per-phase
flamegraph SVG plus a `<phase>.telemetry.json` wall-clock breakdown.
`--min-graph-nodes` asserts the recorded baseline so a stale cassette fails
loudly instead of silently degrading to an empty graph:

```sh
MOCK_LLM=true MOCK_EMBEDDING=deterministic \
  taskset -c 0 cargo run --release -p cognee-cli --features bench,profiling -- bench \
    --mock-llm --mock-memories scripts/perf/fixtures/large/cassette.json \
    --memories scripts/perf/fixtures/large/memories.json \
    --profile-dir target/perf-profiles/large \
    --min-graph-nodes 1189 \
    --output /tmp/mock_large.json
```

The profiler feature is signal-based (SIGPROF), so it needs no `perf` and no
root. Pin a core with `taskset` and use `--release` for stable samples.
