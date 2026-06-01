# Locust Benchmarks for Cognee-Rust HTTP Server

This directory contains a port of the upstream Python Cognee Locust benchmark,
adapted to this Rust repository.

## Mode A (Implemented): Authorization Disabled

The benchmark defaults to no-auth mode by running the Rust HTTP server with:

- `REQUIRE_AUTHENTICATION=false`

Requests are valid without `X-Api-Key`.

## Files

- `locust_performance_analysis.py`: benchmark scenarios and standalone runner
- `requirements.txt`: Python dependencies for load tests

## Scenarios

- `MultiDatasetCogneeTest` (enabled): each virtual user uses its own dataset
- `SingleDatasetCogneeTest` (disabled): shared-dataset flow, kept disabled for parity with upstream caveat

The task flow is sequential per user:

1. `POST /api/v1/add`
2. `POST /api/v1/cognify` with `runInBackground=false`
3. `POST /api/v1/search` with `only_context=true`

## Prerequisites

The Rust HTTP server requires LLM and embedding providers to process the
add→cognify→search pipeline. Set these before running:

| Variable | Fallback | Required | Purpose |
|---|---|---|---|
| `LLM_API_KEY` | `OPENAI_TOKEN` | Yes | API key for the LLM provider |
| `LLM_ENDPOINT` | `OPENAI_URL` | Yes (non-OpenAI) | LLM API base URL (e.g. `https://api.openai.com/v1`) |
| `LLM_MODEL` | `OPENAI_MODEL` | No | Model name (default `gpt-4o-mini`) |
| `EMBEDDING_PROVIDER` | — | No | `onnx` (default), `openai`, `ollama`, `mock` |
| `EMBEDDING_MODEL_PATH` | — | Yes (onnx) | Path to BGE-Small-v1.5 `.onnx` model file |
| `EMBEDDING_TOKENIZER_PATH` | — | Yes (onnx) | Path to matching `tokenizer.json` |

If you have model files cached from the Rust test suite they are at
`target/models/` in the repo root (downloaded by `scripts/download_models.sh`).

For a quick no-LLM smoke test set `EMBEDDING_PROVIDER=mock` and any non-empty
`LLM_API_KEY` — the server will start but cognify results will be empty.

## Quick Start

From repository root:

```bash
python3 -m pip install -r e2e-cross-sdk/performance/locust/requirements.txt
python3 e2e-cross-sdk/performance/locust/locust_performance_analysis.py
```

This starts the Rust server automatically (unless disabled), runs Locust headless,
and writes results into:

- `e2e-cross-sdk/performance/locust/results/*.csv`
- `e2e-cross-sdk/performance/locust/results/*.html`
- `e2e-cross-sdk/performance/locust/results/*.log`

## Important Environment Variables

- `COGNEE_LOCUST_MANAGE_SERVER` (default `true`): start/stop Rust server automatically
- `COGNEE_LOCUST_UNIQUE_STATE` (default `true` when managed server mode is on): run the managed server with an isolated state root per benchmark run
- `COGNEE_LOCUST_STATE_ROOT` (optional): explicit state root for managed mode (if set, it is cleaned before each run)
- `COGNEE_LOCUST_KEEP_STATE` (default `false`): keep auto-generated temporary state roots instead of deleting them after run completion
- `HTTP_API_HOST` (default `127.0.0.1`)
- `HTTP_API_PORT` (default `8000`)
- `COGNEE_LOCUST_USERS` (default `10`)
- `COGNEE_LOCUST_SPAWN_RATE` (default `1`)
- `COGNEE_LOCUST_RUN_TIME` (default `5m`)
- `COGNEE_SEARCH_TYPE` (default `GRAPH_COMPLETION`)
- `COGNEE_HTTP_SERVER_BIN` (optional): path to prebuilt `cognee-http-server` binary

## Manual Server Mode

If you already run the server separately, set:

```bash
export COGNEE_LOCUST_MANAGE_SERVER=false
```

Then launch the benchmark script as above.

## State Isolation Notes

Managed mode now uses a fresh state root by default for each run, which avoids
cross-run contamination from previous graph/vector/sqlite data.

Examples:

```bash
# Default managed run with unique temporary state
./scripts/run_locust_http_server_bench.sh

# Managed run with a deterministic cleaned state location
COGNEE_LOCUST_STATE_ROOT=/tmp/cognee-bench-state ./scripts/run_locust_http_server_bench.sh

# Keep generated temporary state for post-run debugging
COGNEE_LOCUST_KEEP_STATE=true ./scripts/run_locust_http_server_bench.sh
```
