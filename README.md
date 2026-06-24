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

- **Rust** — edition 2024 workspace (resolver 3).
- **Vector store** — embedded [Qdrant](https://qdrant.tech/) (`segment`/`shard` engine) with metadata filtering.
- **Graph store** — embedded [Ladybug](https://crates.io/crates/lbug) graph database for knowledge-graph storage and traversal.
- **LLM** — OpenAI-compatible HTTP adapter (`OpenAIAdapter`, works with OpenAI/Ollama/vLLM/llama.cpp) plus an on-device `LiteRtAdapter` (Android, feature-gated).
- **Embeddings** — multi-provider engine: local ONNX Runtime (BGE-Small-v1.5), OpenAI-compatible HTTP, Ollama, and a mock provider for tests.
- **Relational metadata** — SQLite/Postgres via SeaORM.

See [docs/architecture.md](docs/architecture.md) for the full crate-by-crate breakdown of the workspace.

## Documentation

Full documentation lives in **[docs/](docs/README.md)**. Quick links:

| Topic | Doc |
|---|---|
| Getting started (first run) | [docs/getting-started.md](docs/getting-started.md) |
| Core concepts & terminology | [docs/concepts.md](docs/concepts.md) |
| How-to guides | [docs/guides/](docs/guides/README.md) |
| Project parts & crate map | [docs/architecture.md](docs/architecture.md) |
| What each operation does | [docs/operations.md](docs/operations.md) |
| Configuration (env vars, settings) | [docs/configuration.md](docs/configuration.md) |
| Tools — CLI / bindings / HTTP / backends | [docs/tools/](docs/tools/README.md) |
| HTTP server reference | [docs/http-server/](docs/http-server/README.md) |
| Roadmap — gaps, open questions, plans | [docs/roadmap/](docs/roadmap/README.md) |
| API reference (rustdoc) | `cargo doc --no-deps --open` |

## Language Bindings

Cognee-RS ships three language bindings on top of the Rust core, all sharing the
same SDK-tier implementation via `crates/bindings-common/`:

| Binding | README | Primary API |
|---|---|---|
| **Python** (PyO3) | [python/README.md](python/README.md) | `from cognee_py import Cognee` |
| **C API** (FFI) | [capi/README.md](capi/README.md) | `#include "cognee_sdk.h"` + `cg_sdk_*` |
| **JavaScript/TypeScript** (Neon) | [ts/README.md](ts/README.md) | `import { Cognee } from 'cognee-ts'` |

Each binding exposes the same core flow: `warm()` → `add()` → `cognify()` → `search()`.
All three share `crates/bindings-common` (portable op bodies + stable error codes), so
their SDK surfaces line up 1:1.

## Graph Backend Concurrency

For file-backed graph storage, Python's reference implementation documents a
default single-owning-process model for SQLite/Ladybug/LanceDB access, while
also supporting an opt-in Redis-backed shared Ladybug lock for multi-process
coordination. Rust currently matches that default model: Ladybug writes are
idempotent and serialized in-process, but cross-process locking is intentionally
out of scope.

## Quick Start

### Local LLM with Ollama

Cognee talks to any OpenAI-compatible chat endpoint. The simplest local option
is [Ollama](https://ollama.com/), which exposes an OpenAI-compatible API at
`http://localhost:11434/v1`:

```bash
ollama serve &
ollama pull llama3.2:3b   # small, fast (~2GB)
```

Then point cognee at it:

```bash
export OPENAI_URL=http://localhost:11434/v1
export OPENAI_TOKEN=not-needed
export OPENAI_MODEL=llama3.2:3b
```

For a fully scripted end-to-end demo (spins up Ollama in Docker, runs
add → cognify → search), see [demo/run_cognee_rust_demo.sh](demo/run_cognee_rust_demo.sh)
and the shared helpers in [demo/lib/demo_common.sh](demo/lib/demo_common.sh).

### Building the Project

```bash
cargo build --release
```

The CLI binary is `cognee-cli` (built from the `cognee-cli` crate).

### CLI Usage

The primary surface is the **memory API**: `remember`, `recall`, `improve`,
`forget`. These four high-level operations compose the lower-level
`add → cognify → memify → search` pipeline (`remember ≈ add + cognify + improve`;
`recall ≈ auto-routed search`).

```bash
# Store memory (add + cognify + improve). Inline text and/or file paths.
cognee-cli remember "Cognee turns data into a knowledge graph" ./notes.txt -d my_dataset

# Query memory — omit -t to let recall auto-route the retrieval strategy
cognee-cli recall "what did we learn about X?" -d my_dataset -k 10

# Enrich memory / bridge sessions
cognee-cli improve -d my_dataset

# Remove memory (a dataset, a single data item, or everything)
cognee-cli forget --all
```

`remember` with a `--session-id` records session memory; without one it persists
permanent, graph-backed memory. `recall` is session-aware and graph-backed.

#### Lower-level pipeline

The same work can be driven through the building blocks directly when you need
fine-grained control over each stage:

```bash
# 1. Ingest data into a dataset (defaults to "main_dataset")
cognee-cli add ./notes.txt "some inline text" -d my_dataset

# 2. Build the knowledge graph from one or more datasets
cognee-cli cognify -d my_dataset

# 1+2 in one step
cognee-cli add-and-cognify ./notes.txt -d my_dataset

# 3. Query it (default query type is GRAPH_COMPLETION)
cognee-cli search "what did we learn about X?" -t GRAPH_COMPLETION -d my_dataset -k 10
```

`add` also accepts HTTP(S) URLs. The ingestion pipeline fetches the page or file,
routes it by MIME type, stores URL metadata, and, after `cognify`, can create
`WebPage` / `WebSite` provenance nodes for URL-sourced chunks.

Other subcommands: `memify` (enrich an existing graph with triplet embeddings),
`delete`, `config` (`get`/`set`/`unset`), `run-sequence` (run a scripted
add/cognify/search sequence), and — when built with their feature flags —
`visualize` (render the graph to HTML), `serve`, and `disconnect` (cloud).

Run `cognee-cli <command> --help` for the full flag list. See
[docs/tools/cli.md](docs/tools/cli.md) for the subcommand reference, logging, and
LLM-retry configuration.

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
# Run the whole workspace (downloads embedding models if missing,
# single-threaded for LLM isolation):
bash scripts/run_tests_with_openai.sh

# Or a single test by name:
bash scripts/run_tests_with_openai.sh test_fact_extraction
```

This script sources `scripts/lib/common.sh`, which downloads the BGE-Small-v1.5
ONNX artifacts from HuggingFace if not already cached, then runs
`cargo test --workspace -- --nocapture --test-threads=1`. The relevant
environment variables are:

| Variable | Required | Default | Purpose |
|---|---|---|---|
| `OPENAI_URL` | Yes | — | OpenAI-compatible API base URL |
| `OPENAI_TOKEN` | Yes | — | API key (`not-needed` for local Ollama) |
| `OPENAI_MODEL` | No | `gpt-4o-mini` | LLM model name |
| `COGNEE_TEST_MODEL_DIR` | No | `target/models` | Cache dir for embedding models |
| `COGNEE_E2E_EMBED_MODEL_PATH` | No | auto from model dir | BGE-Small-v1.5 ONNX model |
| `COGNEE_E2E_TOKENIZER_PATH` | No | auto from model dir | BGE-Small tokenizer.json |

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

Cognee writes structured logs to **stdout** and (when a writable directory is
available) to a rotating file, owned by the [`cognee-logging`](crates/logging/)
crate (`cognee_logging::init_logging`, called by the CLI and HTTP server). The
full env-var table (`COGNEE_LOG_*`, `RUST_LOG`/`LOG_LEVEL`, `LOG_FILE_NAME`) and
the multi-process rotation warning are documented canonically in
[`docs/configuration.md` → Logging](docs/configuration.md#logging).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
