# Cognee-RS (Rust Edition)

**Cognee-RS** is a Rust-based experimental SDK for building **on-device AI memory** pipeline in rust.  
It’s designed to run efficiently on constrained devices (smartwatch, phone)

---

## Objectives

- **Small-model support**: The solution has to be able to run with on device models (Phi4 class + embeddings).
- **90+ correctness**: We aim to keep the basic cognee ability to reach similar correctness to Cognee SDK (90+%).
- **On-device vs Cloud ability**:  
  - Transformation tasks + orchestration design should support on-device and cloud mode.  
    - Cloud prep is not the immediate goal, but we’ll keep infra flexibility in mind.
- **Multimodal support**: POC has to support multimodal data ingestion.
- **Retrieval**: Has to be optimally 3-4 sec on a reasonably sized knowledge base.
---

## Orchestration requirements:
- **Memory Control**: Control over the memory used by the ingestion pipeline.
- **CPU control**: Control over threads and parallelization in the ingestion pipeline.
- **Autonomous task scheduling**: Dynamic scheduling of memory-tasks.


## Technology Stack

- **Rust** — We use rust  for the POC.
- **Qdrant** — Qdrant as vector storage.
- **BAML** — llm model management.  
- **Local models** — Phi4
- **Graph store** — We do not use graph database, as we store structure embeddings in the vector collections + optionally retrieve and build relevant subgraphs.

## Graph Backend Concurrency

For file-backed graph storage, Python's reference implementation documents a
default single-owning-process model for SQLite/Ladybug/LanceDB access, while
also supporting an opt-in Redis-backed shared Ladybug lock for multi-process
coordination. Rust currently matches that default model: Ladybug writes are
idempotent and serialized in-process, but cross-process locking is intentionally
out of scope.

## Quick Start

### Local LLM with Ollama

We provide a Docker setup for running Ollama with OpenAI-compatible API:

```bash
cd docker/ollama
./start.sh
```

This will start:
- **Ollama** with OpenAI-compatible API at `http://localhost:11434/v1`
- **Web UI** at `http://localhost:3000`
- Automatically pulls `llama3.2:3b` model (small, fast, ~2GB)

See [docker/ollama/README.md](docker/ollama/README.md) for detailed documentation.

### Building the Project

```bash
cargo build --release
```

### Android Local LLM (LiteRT-LM)

An Android-only local LLM backend is available through the `litert` provider.

Requirements:
- LiteRT wrapper crate fetched from `https://github.com/topoteretes/cognee-litert-lm.git`
- Android NDK toolchain configured (for example `aarch64-linux-android21-clang` in `PATH`)

Enable feature:

```bash
cargo check -p cognee-lib --features android-litert
```

Android compile check:

```bash
cargo check -p cognee-lib --features android-litert --target aarch64-linux-android
```

Configuration values:
- `llm_provider = "litert"`
- `llm_model = "/absolute/path/to/model.litertlm"` (local model path)
- `llm_endpoint = "cpu"` or `"gpu"` (optional backend hint)

Structured output behavior for LiteRT:
- The JSON schema is injected into the user prompt in compact JSON form.
- The model is instructed to return only one valid JSON object matching that schema.

### Running Tests

```bash
cargo test --workspace
```

For local full-suite execution (including LLM and ONNX/tokenizer dependent tests), use:

```bash
./scripts/run_tests_with_local_env.sh
```

This script initializes and exports the required test environment:

- `OPENAI_URL` (auto-detected from `http://localhost:11435/v1` or `http://localhost:11434/v1`, or pre-set value)
- `OPENAI_TOKEN` (defaults to `not-needed` for local Ollama)
- `OPENAI_MODEL` (uses pre-set value, otherwise auto-detected from `${OPENAI_URL}/models`, fallback `gpt-4o-mini`)
- `COGNEE_E2E_EMBED_MODEL_PATH` (defaults to `target/models/BGE-Small-v1.5-model_quantized.onnx`)
- `COGNEE_E2E_TOKENIZER_PATH` (defaults to `target/models/bge-small-tokenizer.json`)

If model/tokenizer files are missing, the script downloads them automatically.

## Observability

Cognee emits OpenTelemetry traces from every pipeline stage. To export them
to an OTLP collector:

```bash
cargo build --release --features telemetry
OTEL_EXPORTER_OTLP_ENDPOINT=https://otlp.your-collector:4317 \
  cognee-cli search --query "what did we ingest yesterday?"
```

See [`docs/observability/opentelemetry.md`](docs/observability/opentelemetry.md)
for the full guide (env vars, recipes for Grafana Tempo, Honeycomb, Dash0,
and in-cluster Collectors).

- **Product analytics** — opt-out HTTP events to
  `https://test.prometh.ai`. Mirrors Python's `send_telemetry`. See
  [`docs/observability/send_telemetry.md`](docs/observability/send_telemetry.md)
  for the full reference (env vars, payload schema, salt rotation,
  privacy notes).

### Logging

Cognee writes structured logs to **stdout** and (when a writable
directory is available) to a rotating file under
`~/.cognee/logs/<timestamp>.log`. File logging is owned by the
[`cognee-logging`](crates/logging/) workspace crate, which both the
CLI and HTTP server initialise via `cognee_logging::init_logging`.

| Variable | Default | Purpose |
|---|---|---|
| `COGNEE_LOG_FILE` | `true` | Master toggle (`false`/`0`/`no` disables file logging). |
| `COGNEE_LOGS_DIR` | `~/.cognee/logs` | Log directory. Falls back to `/tmp/cognee_logs` if the primary is unwritable. |
| `COGNEE_LOG_FORMAT` | `plain` | `plain` (Python-compatible text) or `json` (JSON lines). Applies to both stdout and file. |
| `COGNEE_LOG_ROTATION` | `daily` | One of `daily` / `hourly` / `minutely` / `never`. Time-based only; size-based rotation is a future enhancement. |
| `COGNEE_LOG_BACKUP_COUNT` | `5` | Files kept by the active rotation policy. |
| `COGNEE_LOG_MAX_FILES` | `10` | Startup-time cap; older files past this count are removed. |
| `LOG_LEVEL` | `info` | Fallback level when `RUST_LOG` is unset. `RUST_LOG` wins when both are set. |
| `LOG_FILE_NAME` | _(generated)_ | Set automatically by the parent process and inherited by children, so all processes append to one file. |

> **Multi-process warning** — when several cognee processes share a
> log file via `LOG_FILE_NAME`, rotation is not coordinated.
> Concurrent rotation events from multiple processes can corrupt
> the log. If you run sharded workers, give each shard a different
> `COGNEE_LOGS_DIR` (or unset `LOG_FILE_NAME` per shard).
