# cognee_pipeline

Python bindings for the [cognee-rust](https://github.com/topoteretes/cognee-rust)
pipeline engine, built with [PyO3](https://pyo3.rs/).

Cognee transforms raw text, files, and URLs into a persistent, queryable knowledge graph
via a three-stage pipeline: **add** (ingest) → **cognify** (extract) → **search** (retrieve).

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
import asyncio
import json
from cognee_pipeline import Cognee, SearchType

async def main():
    cognee = Cognee()           # optionally pass json.dumps(settings) to override defaults
    await cognee.warm()         # build engines and resolve the default user
    await cognee.add(
        {"type": "text", "text": "Cognee turns data into a knowledge graph."},
        "main_dataset",         # dataset_name is required
    )
    await cognee.cognify("main_dataset")   # dataset_name is required
    result = await cognee.search(
        "What does cognee do?",
        {"search_type": SearchType.GRAPH_COMPLETION},
    )
    print(result)

asyncio.run(main())
```

Set environment variables before running:

```bash
export OPENAI_URL=https://api.openai.com/v1
export OPENAI_TOKEN=sk-...
export MOCK_EMBEDDING=true   # skip ONNX model download for quick tests
```

### Upstream-compatible module-level API

An upstream-compatible API is available via `cognee_pipeline.compat` for
drop-in replacement of the Python `cognee` SDK:

```python
import asyncio
from cognee_pipeline import compat as cognee, SearchType

async def main():
    await cognee.add("Cognee turns data into a knowledge graph.", "main_dataset")
    await cognee.cognify("main_dataset")
    result = await cognee.search("What does cognee do?", SearchType.GRAPH_COMPLETION)
    print(result)

asyncio.run(main())
```

Install the optional `cognee` alias to use `import cognee` directly:

```bash
pip install "cognee_pipeline[drop-in]"
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

## Configuration

### Programmatic config

Pass a JSON settings string to the `Cognee` constructor to override env-derived
defaults. The overlay order is: compiled-in defaults < env vars < constructor argument.

```python
import json
from cognee_pipeline import Cognee

cognee = Cognee(json.dumps({
    "llm_endpoint": "https://api.openai.com/v1",
    "llm_api_key": "sk-...",
    "llm_model": "gpt-4o-mini",
    "embedding_provider": "openai",
    "embedding_model": "text-embedding-3-small",
    "embedding_dimensions": 1536,
}))
```

Generic key/value setters and config read-back via `cognee.config`:

```python
# Set individual keys (typed value, or explicit string form):
cognee.config.set("llm_model", "gpt-4o")
cognee.config.set_str("llm_api_key", "sk-...")

# Read back the current config (secrets are redacted):
cfg = cognee.config.get()
print(cfg)
```

### Environment variables

| Variable | Purpose |
|---|---|
| `OPENAI_URL` | LLM API base URL (OpenAI-compatible endpoint). |
| `OPENAI_TOKEN` | LLM API key. |
| `OPENAI_MODEL` | LLM model name (default: `gpt-4o-mini`). |
| `EMBEDDING_PROVIDER` | Embedding provider: `openai`, `ollama`, `onnx`, `mock`. |
| `EMBEDDING_MODEL` | Embedding model name. |
| `EMBEDDING_DIMENSIONS` | Embedding vector dimensions. |
| `EMBEDDING_ENDPOINT` | Embedding API base URL (falls back to `OPENAI_URL`). |
| `EMBEDDING_API_KEY` | Embedding API key (falls back to `OPENAI_TOKEN`). |
| `MOCK_EMBEDDING` | Set `true` to use zero-vector mock embeddings (no model download). |
| `COGNEE_BINDING_SUPPRESS_LOGS` | Suppress the auto-installed `pyo3-log` bridge. |
| `COGNEE_RUST_TELEMETRY` | Opt in to Python-side `send_telemetry` analytics (off by default). |
| `COGNEE_HOST_SDK` | Set by the upstream Python `cognee` SDK to suppress binding-armed analytics emission. |
| `RUST_LOG`, `LOG_LEVEL` | Standard `tracing-subscriber` env-filter level overrides. |
| `COGNEE_LOG_*`, `LOG_FILE_NAME` | Consumed by `setup_logging()` — see the workspace README's "Logging" section. |
| `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_HEADERS`, `OTEL_SERVICE_NAME` and other `OTEL_*` vars | Consumed by `setup_telemetry()`. |
| `TELEMETRY_DISABLED`, `ENV` | Honoured by `setup_telemetry_analytics()` via `cognee_telemetry::env::is_disabled`. |

## Operations reference

### Pipeline operations

#### add

Ingest one or more data items into a named dataset.

```python
# Text
await cognee.add({"type": "text", "text": "…"}, "my-dataset")

# File
await cognee.add({"type": "file", "path": "/abs/path/to/doc.txt"}, "my-dataset")

# URL
await cognee.add({"type": "url", "url": "https://example.com/article"}, "my-dataset")

# Multiple items at once
await cognee.add([
    {"type": "text", "text": "First document"},
    {"type": "file", "path": "/abs/path/two.txt"},
], "my-dataset")
```

#### cognify

Extract entities and relationships into the knowledge graph.

```python
await cognee.cognify("my-dataset")
```

#### add_and_cognify

Ingest and extract in a single call.

```python
result = await cognee.add_and_cognify(
    {"type": "text", "text": "…"},
    "my-dataset",
)
```

### Search and recall

#### search

Query the knowledge graph. Defaults to `GRAPH_COMPLETION`.

```python
result = await cognee.search("What is the capital of France?")

# With options
result = await cognee.search(
    "summarise recent events",
    {"search_type": SearchType.SUMMARIES, "top_k": 5, "datasets": ["news"]},
)
```

All 15 search types are supported (via `SearchType` enum):
`GRAPH_COMPLETION`, `SUMMARIES`, `CHUNKS`, `RAG_COMPLETION`, `TRIPLET_COMPLETION`,
`GRAPH_SUMMARY_COMPLETION`, `CYPHER`, `NATURAL_LANGUAGE`, `GRAPH_COMPLETION_COT`,
`GRAPH_COMPLETION_CONTEXT_EXTENSION`, `FEELING_LUCKY`, `FEEDBACK`, `TEMPORAL`,
`CODING_RULES`, `CHUNKS_LEXICAL`.

#### recall

Session-first routing: checks session QA history before falling back to graph search.

```python
result = await cognee.recall(
    "What did we discuss?",
    {"session_id": "session-uuid", "scope": "auto"},
)
```

### Memory operations

#### remember

Composite add + cognify with an optional improvement pass.

```python
await cognee.remember(
    {"type": "text", "text": "…"},
    "my-dataset",
    {"self_improvement": True},
)
```

#### memify

Index triplet embeddings from the existing knowledge graph.
Enables `TripletCompletion` search. Idempotent.

```python
await cognee.memify()
```

#### improve

Run the four-stage session-graph bridge pipeline.

```python
await cognee.improve({
    "dataset_name": "my-dataset",
    "session_ids": ["session-uuid"],
})
```

### Datasets

```python
datasets      = await cognee.datasets.list()
items         = await cognee.datasets.list_data(dataset_id)
has_content   = await cognee.datasets.has(dataset_id)
statuses      = await cognee.datasets.status([id1, id2])

await cognee.datasets.empty(dataset_id)
await cognee.datasets.delete_data(dataset_id, data_id)
await cognee.datasets.delete_all()
```

### Sessions

```python
entries = await cognee.sessions.get("session-uuid", {"last_n": 10})

await cognee.sessions.add_feedback("session-uuid", "qa-uuid", "Great answer!", 5)
await cognee.sessions.delete_feedback("session-uuid", "qa-uuid")

ctx = await cognee.sessions.get_graph_context("session-uuid")
await cognee.sessions.set_graph_context("session-uuid", "new context")
```

### Data lifecycle

```python
# Forget a single item
await cognee.forget({"kind": "item", "data_id": "uuid", "dataset": {"name": "my-dataset"}})

# Forget an entire dataset
await cognee.forget({"kind": "dataset", "dataset": {"name": "my-dataset"}})

# Forget everything
await cognee.forget({"kind": "all"})

# Replace a data item (delete → re-add → re-cognify)
await cognee.update("old-data-uuid", {"type": "text", "text": "updated content"}, "my-dataset")

# Remove all files from storage (metadata DB untouched)
await cognee.prune_data()

# Wipe graph, vector, metadata, and/or cache backends
await cognee.prune_system({"prune_graph": True, "prune_vector": True})
```

### Visualisation

```python
# Get the HTML string
html = await cognee.visualize()

# Write to a file (returns the absolute path)
path = await cognee.visualize_to_file({"destination_path": "/tmp/graph.html"})
```

Requires the `visualization` feature compiled into the native extension.

### Cloud: serve / disconnect

`serve` and `disconnect` are module-level functions because they operate on global
cloud state.

```python
from cognee_pipeline import serve, disconnect

# Direct mode (no Auth0 flow; headless-friendly)
result = await serve({"url": "http://localhost:8000", "api_key": "key"})
print(f"Connected to {result['service_url']}")

# Cloud mode (Auth0 device-code flow — requires a TTY)
await serve()

# Tear down
await disconnect()
await disconnect({"wipe_credentials": True})   # also removes the local credential cache
```

## Initialisation and observability

```python
import cognee_pipeline

# Optional: file logging (reads COGNEE_LOG_*, LOG_FILE_NAME, LOG_LEVEL).
cognee_pipeline.setup_logging()

# Optional: OTLP trace export (reads OTEL_* env vars).
cognee_pipeline.setup_telemetry()

# Optional: product-analytics emission (returns True if armed).
armed = cognee_pipeline.setup_telemetry_analytics()
print(f"analytics armed: {armed}")
```

When `cognee_pipeline` is imported, a minimal default subscriber is installed:
a `pyo3-log` bridge that forwards every Rust `tracing::*` event into Python's
standard `logging` module under the `cognee.*` logger tree. The host's
`logging.basicConfig` / `logging.dictConfig` controls level, format, and handlers.

```python
import logging
logging.basicConfig(level=logging.INFO)
logging.getLogger("cognee").setLevel(logging.DEBUG)

import cognee_pipeline  # Rust spans now flow into Python logging
```

Set `COGNEE_BINDING_SUPPRESS_LOGS=1` **before** importing `cognee_pipeline` to
skip the default subscriber. The host then owns all subscriber setup.

### Analytics defaults

For the Python binding, analytics emission is **OFF by default** — the upstream
Python `cognee` SDK is the canonical sender of `send_telemetry` events; the Rust
binding defers to it.

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

## Error handling

All async ops raise subclasses of `CogneeError`:

| Exception | Meaning |
|---|---|
| `CogneeValidationError` | Invalid input (bad data descriptor, unknown config key, malformed settings JSON). |
| `CogneeComponentError` | Component initialisation failed (DB connection, embedding model load). |
| `CogneeServiceBuildError` | Service warm-up failed (engine could not be constructed). |
| `CogneeUserBootstrapError` | Default user resolution failed during `warm()`. |
| `CogneeRuntimeError` | Pipeline or search runtime failure. |
| `CogneeUnsupportedError` | Operation not available for the current backend configuration. |
| `CogneeFeatureNotBuiltError` | Feature was not compiled into this build (e.g. `visualization`). |
| `CogneeUnknownConfigKeyError` | Unknown config key passed to `config.set_*` or constructor. |
| `CogneeConfigTypeMismatchError` | Wrong type for a config value. |
| `CogneeError` | Base class — catch this to handle any cognee error. |

```python
from cognee_pipeline import Cognee, CogneeError, CogneeValidationError

try:
    result = await cognee.search("query", {"search_type": "INVALID_TYPE"})
except CogneeValidationError as e:
    print(f"Bad input: {e}")
except CogneeError as e:
    print(f"Cognee error: {e}")
```

## Advanced: low-level pipeline engine

The original pipeline engine API is available directly from `cognee_pipeline`
for advanced orchestration use-cases that do not need the high-level SDK:

```python
import cognee_pipeline

task = cognee_pipeline.create_task(lambda val, ctx: val)

p = cognee_pipeline.Pipeline("my pipeline")
p.add_task(cognee_pipeline.TaskInfo(task))

ctx = cognee_pipeline.TaskContext.mock()
[result] = await p.execute([cognee_pipeline.CogneeValue.from_string("hello")], ctx)
```

All pipeline-engine symbols (`Pipeline`, `TaskInfo`, `createTask`, `CogneeValue`,
`TaskContext`, `RunHandle`, `CancellationHandle`, `CancellationToken`,
`cancellation_pair`, `ProgressToken`, `Watcher`) are available at the top level
of `cognee_pipeline`.

## Troubleshooting

- **`ImportError` on `cognee_pipeline`** — run `maturin develop` (or install from PyPI) first.
- **Embedding model download on first run** — set `MOCK_EMBEDDING=true` to skip it in tests.
- **`OPENAI_URL` / `OPENAI_TOKEN` not set** — all examples exit 0 with a `SKIP` message when these are absent; export them before running.
- **Analytics doubly-sent** — if using `pip install cognee` alongside this binding, do not set `COGNEE_RUST_TELEMETRY=1`.

## References

- Reference docs: [docs/python-bindings/](../docs/python-bindings/)
- Examples: [examples/](examples/)
- Observability: [docs/observability/opentelemetry.md](../docs/observability/opentelemetry.md), [docs/observability/send_telemetry.md](../docs/observability/send_telemetry.md)
- C API bindings: [capi/README.md](../capi/README.md)
- JS/TS bindings: [js/README.md](../js/README.md)
- cognee-rust workspace: [README.md](../README.md)
