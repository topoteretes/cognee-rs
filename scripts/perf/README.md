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
