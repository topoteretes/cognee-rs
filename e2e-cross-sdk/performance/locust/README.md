# Locust Benchmarks for Cognee-Rust HTTP Server

This directory contains a port of the upstream Python Cognee Locust benchmark,
adapted to this Rust repository.

## Mode A (Implemented): Authorization Disabled

The benchmark defaults to no-auth mode by running the Rust HTTP server with:

- `REQUIRE_AUTHENTICATION=false`

Requests are valid without `X-Api-Key`.

## Files

- `locust_performance_analysis.py`: benchmark scenarios and standalone runner
- `bootstrap_rust.py`: simple bootstrap metadata helper for wrappers
- `requirements.txt`: Python dependencies for load tests

## Scenarios

- `MultiDatasetCogneeTest` (enabled): each virtual user uses its own dataset
- `SingleDatasetCogneeTest` (disabled): shared-dataset flow, kept disabled for parity with upstream caveat

The task flow is sequential per user:

1. `POST /api/v1/add`
2. `POST /api/v1/cognify` with `runInBackground=false`
3. `POST /api/v1/search` with `only_context=true`

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
