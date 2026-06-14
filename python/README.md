# cognee_pipeline

Python bindings for the [cognee-rust](https://github.com/topoteretes/cognee-rust)
pipeline engine, built with [PyO3](https://pyo3.rs/).

## Installation

```bash
pip install cognee_pipeline
```

For local development from this repository:

```bash
cd python
maturin develop
```

## Quick start

```python
import cognee_pipeline

pipeline = cognee_pipeline.Pipeline()
# ... configure tasks, run, etc.
```

## Examples

Runnable example scripts are in the [`examples/`](examples/) directory. Each
script validates required env vars up front and exits 0 with a clear `SKIP`
message when they are absent, so all examples are safe to run in CI without
credentials.

| Script | Run command | What it covers |
|---|---|---|
| [`add_cognify_search.py`](examples/add_cognify_search.py) | `python examples/add_cognify_search.py` | Core add → cognify → search pipeline |
| [`memify_recall.py`](examples/memify_recall.py) | `python examples/memify_recall.py` | Triplet embeddings (memify) + session recall |
| [`datasets.py`](examples/datasets.py) | `python examples/datasets.py` | Dataset listing, status, deletion |
| [`sessions.py`](examples/sessions.py) | `python examples/sessions.py` | QA history, feedback, graph-context snapshots |
| [`config.py`](examples/config.py) | `python examples/config.py` | Programmatic config (LLM / embedding / DBs) |
| [`visualize.py`](examples/visualize.py) | `python examples/visualize.py` | Render knowledge graph to HTML |

All examples read LLM credentials from the environment. Set `MOCK_EMBEDDING=true`
to skip the ONNX model download and use mock embeddings (fast, no GPU required):

```bash
export OPENAI_URL=https://api.openai.com/v1
export OPENAI_TOKEN=sk-...
export MOCK_EMBEDDING=true
cd python && python examples/add_cognify_search.py
```

## Initialisation

cognee's Rust core uses `tracing` for structured diagnostics and
optionally exports spans via OpenTelemetry (OTLP). When
`cognee_pipeline` is imported, a minimal default subscriber is
installed so events are never silently dropped: a `pyo3-log` bridge
that forwards every Rust `tracing::*` event into Python's standard
`logging` module under the `cognee.*` logger tree.

The host application's standard `logging.basicConfig` /
`logging.dictConfig` controls level, format, and handlers from there
on. For example:

```python
import logging
logging.basicConfig(level=logging.INFO)
logging.getLogger("cognee").setLevel(logging.DEBUG)

import cognee_pipeline  # Rust spans now flow into Python logging
```

### Opt-out

Set `COGNEE_BINDING_SUPPRESS_LOGS=1` **before** importing
`cognee_pipeline` to skip the default subscriber. The host then owns
all subscriber setup.

```bash
COGNEE_BINDING_SUPPRESS_LOGS=1 python my_app.py
```

### Optional upgrades

Three idempotent setup functions are exported from
`cognee_pipeline`. Each one composes additional layers on top of the
default subscriber. Calling order does not matter; calling any of
them more than once is a no-op.

| Call | Effect | Idempotent |
|---|---|---|
| `setup_logging()` | Adds the rotating file appender (default `~/.cognee/logs/<ts>.log`, daily rotation, configurable via `COGNEE_LOG_*`, `LOG_FILE_NAME`, `LOG_LEVEL`, `RUST_LOG`). | Yes |
| `setup_telemetry()` | Composes an OTLP exporter when `OTEL_EXPORTER_OTLP_ENDPOINT` is set; reads all standard `OTEL_*` env vars. Defaults `service.name` to `cognee.python-binding` when unset (the user's explicit value always wins). Returns `None`. | Yes |
| `setup_telemetry_analytics()` | Arms product-analytics emission (`https://test.prometh.ai`) per the Python policy below. Returns `True` if armed by this call (or a prior call), `False` if the policy suppressed emission. | Yes |

Example with everything on:

```python
import cognee_pipeline

cognee_pipeline.setup_logging()              # file logging
cognee_pipeline.setup_telemetry()            # OTLP export
armed = cognee_pipeline.setup_telemetry_analytics()  # analytics
print(f"analytics armed: {armed}")
```

### Analytics defaults

For the Python binding, analytics emission is **OFF by default** —
the upstream Python `cognee` SDK is the canonical sender of
`send_telemetry` events; the Rust binding defers to it.

| Condition | Behaviour |
|---|---|
| `COGNEE_RUST_TELEMETRY` unset | `setup_telemetry_analytics()` returns `False` (not armed). |
| `COGNEE_RUST_TELEMETRY=1` and `COGNEE_HOST_SDK` unset | Armed. Returns `True`. |
| `COGNEE_HOST_SDK=<any non-empty>` | Not armed regardless of `COGNEE_RUST_TELEMETRY`. |

**Important — if you are using the upstream Python `cognee` SDK
(`pip install cognee`):** do **not** set `COGNEE_RUST_TELEMETRY=1`.
The upstream SDK is the canonical sender of `send_telemetry` events;
the Rust binding defers to it via the `COGNEE_HOST_SDK=python`
sentinel that the upstream package sets automatically.

## Environment variables

| Variable | Purpose |
|---|---|
| `COGNEE_BINDING_SUPPRESS_LOGS` | Suppress the auto-installed `pyo3-log` bridge. |
| `COGNEE_RUST_TELEMETRY` | Opt in to Python-side `send_telemetry` analytics (off by default). |
| `COGNEE_HOST_SDK` | Set by the upstream Python `cognee` SDK to suppress binding-armed analytics emission (decision 10). |
| `RUST_LOG`, `LOG_LEVEL` | Standard `tracing-subscriber` env-filter level overrides. |
| `COGNEE_LOG_*`, `LOG_FILE_NAME` | Consumed by `setup_logging()` — see the workspace README's "Logging" section. |
| `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME` and other `OTEL_*` vars | Consumed by `setup_telemetry()`. |
| `TELEMETRY_DISABLED`, `ENV` | Honoured by `setup_telemetry_analytics()` via `cognee_telemetry::env::is_disabled`. |

## References

- Observability docs: [docs/observability/opentelemetry.md](../docs/observability/opentelemetry.md), [docs/observability/send_telemetry.md](../docs/observability/send_telemetry.md)
