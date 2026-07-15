<div align="center">

<a href="https://github.com/topoteretes/cognee">
  <img src="https://raw.githubusercontent.com/topoteretes/cognee/refs/heads/dev/assets/cognee-logo-transparent.png" alt="Cognee Logo" height="60">
</a>

# Cognee-RS — Rust AI Memory

**On-device AI memory in Rust.** Turn raw text, files, and URLs into a
persistent, queryable memory. Cognee-RS built to boot up fast (350ms) and do fast searches (260ms) and
to be a drop-in companion to the Python [`cognee`](https://github.com/topoteretes/cognee) SDK.


<p align="center">
  <a href="https://docs.cognee.ai/rust/getting-started">Getting Started</a>
  ·
  <a href="https://docs.cognee.ai">Docs</a>
  ·
  <a href="https://docs.cognee.ai/rust/architecture">Architecture</a>
  ·
  <a href="https://github.com/topoteretes/cognee">Python cognee</a>
</p>

</div>

---

## How it works

The four-verb **memory API** (`remember` / `recall` / `improve` / `forget`)
`remember` ingests and builds the graph; `recall` auto-routes retrieval over it.



---

## Quick Start

The fastest way in is the `cognee-cli` binary and its memory API: **`remember`**
what you know, **`recall`** when you need it, **`improve`** the graph from
feedback, **`forget`** what's stale.

### Prerequisites

- **Rust toolchain** — install [rustup](https://rustup.rs); the repo's pinned
  toolchain (Rust 1.90, declared in `rust-toolchain.toml`) is selected
  automatically. The workspace is edition 2024 / resolver 3; MSRV is 1.89.
- An **LLM API key** (OpenAI-compatible). 

Build the CLI from source with Cargo. 

### Build

```bash
cargo build --release   # -> target/release/cognee-cli
```

The default feature set wires the fully embedded, no-external-service stack:
**SQLite** (relational), **Ladybug** (graph), and **Lancedb**.

```bash
# put it on your PATH for the snippets below
export PATH="$PWD/target/release:$PATH"
```

### Configure the LLM

A `.env` file in the working directory is auto-loaded. The **only** required
setting is the LLM API key:

```bash
export LLM_API_KEY="sk-..."        # canonical name (OPENAI_TOKEN is an accepted alias)
# optional overrides:
export LLM_MODEL="openai/gpt-5-mini"     # the compiled default is openai/gpt-5-mini
export LLM_ENDPOINT="https://..."  # alias: OPENAI_URL; empty -> OpenAI's API
```

To run embeddings fully localy, set `EMBEDDING_PROVIDER=onnx` (or `ollama`).

**Fully local with [Ollama](https://ollama.com)** (LLM via Ollama, embeddings local):

```bash
ollama serve &
ollama pull llama3.2:3b

export OPENAI_URL=http://localhost:11434/v1
export OPENAI_TOKEN=not-needed      # dummy value still required — the LLM client checks for a non-empty key
export OPENAI_MODEL=llama3.2:3b
export EMBEDDING_PROVIDER=ollama    # or onnx — otherwise embeddings still call OpenAI
```

### Your first memory

```bash
# store, then ask — this is the whole loop
cognee-cli remember "Cognee turns raw data into a queryable knowledge graph."
cognee-cli recall   "what does cognee do?"
```

`remember` ingests, builds the knowledge graph, and runs a self-improvement pass
(disable with `--no-improve`). `recall` auto-routes the search type for you when
`--query-type` is omitted.

**Session memory** — scope facts to a transient conversation cache instead of the
permanent graph:

```bash
cognee-cli remember "we ship Friday" --session-id chat-42
cognee-cli recall   "when do we ship?" --session-id chat-42
```

**Forget** — clean up (exactly one target is required):

```bash
cognee-cli forget --all              # everything you own
cognee-cli forget -d main_dataset    # one whole dataset
cognee-cli forget --data-id <uuid> --dataset-name main_dataset   # one item
```

**Improve** — nudge the graph from feedback:

```bash
cognee-cli improve -d main_dataset --node-name "Cognee" --feedback-alpha 0.1
```

### Memory API at a glance

| Command | Purpose | Shared flags |
|---|---|---|
| `remember <data…>` | add + cognify (+ improve) | `-d/--dataset-name`, `--session-id`, `--no-improve`, `--tenant-id` |
| `recall <query>` | smart search (auto-routes) | `-d/--datasets`, `-t/--query-type`, `-k/--top-k` (10), `--session-id`, `-f/--output-format` |
| `improve` | reinforce graph from feedback | `-d/--dataset-name`, `--session-id`, `--node-name`, `--feedback-alpha` (0.1), `--tenant-id` |
| `forget` | delete memory | one of `--all` / `-d` / `--data-id`+`--dataset-name`, `--tenant-id` |

> Flags are not uniform across subcommands: only `recall`/`search` accept
> `-t/--query-type`, `-k/--top-k`, and `-f/--output-format`; `forget` has no `-k`
> or `--session-id`. Mixing them across subcommands fails clap parsing.

> CLI config is also persisted at `~/.config/cognee-rust/config.json` (via
> `cognee-cli config set`). Precedence is **defaults < JSON config < env vars** —
> explicit env vars always win, but a stale `config.json` can override
> `.env`-implied defaults.



### Using it from Rust

The library crates are published on
[crates.io](https://crates.io/crates/cognee-lib). Depend on the top-level
`cognee-lib` crate:

```bash
cargo add cognee-lib          # or add `cognee-lib = "0.1"` under [dependencies]
```

For local development against the in-repo sources, point the dependency at a
path instead: `cognee-lib = { path = "crates/lib" }`.

There is a high-level one-call API — `cognee_lib::prelude::remember()` /
`recall()` / `forget()` / `improve()` — that mirrors the Python functions.
**Be aware:** these are not self-contained. Each takes a set of pre-built
components (pipelines, LLM, storage, graph DB, vector DB, embedding engine,
session manager, …) — as `Arc<dyn …>` handles (`remember`/`improve`) or borrowed
references to already-wired orchestrators (`recall`/`forget`), so you must wire
the component graph first. They are "one call" only after the wiring.

The lowest-friction wiring root is `ComponentManager`, which lazily builds the
engines from env/`Settings`:

```rust
use cognee_lib::ComponentManager;
use cognee_lib::config::ConfigManager;

let cm = ComponentManager::new(ConfigManager::from_env());
let storage   = cm.storage().await?;
let database  = cm.database().await?;
let graph_db  = cm.graph_db().await?;
let vector_db = cm.vector_db().await?;
let embedding = cm.embedding_engine().await?;
let llm       = cm.llm().await?;
```

From there you build an `AddPipeline` and a `SearchOrchestrator` (via
`SearchBuilder`) and call `add(...)` / `cognify(...)` /
`orch.search(&SearchRequest{..})`. See [examples/add_example.rs](examples/add_example.rs),
[examples/cognify_example.rs](examples/cognify_example.rs), and
`crates/bindings-common/src/services.rs` (`CogneeServices::build`) for the
canonical wiring.


## Language Bindings

The convenient `Cognee` class — `new(settings)` → `warm()` → `remember()` /
`recall()` (or the lower-level `add()` / `cognify()` / `search()`) — is exposed
by the bindings, not the raw Rust crate. `warm()` resolves `owner_id` and builds +
caches the component graph once, giving you the wiring-free experience the
pure-Rust path lacks. All four bindings share the same SDK-tier implementation
via `crates/bindings-common/`, so their surfaces line up 1:1.

| Binding | Install | README | Primary API |
|---|---|---|---|
| **JavaScript/TypeScript** (Neon) | `npm install @cognee/cognee-ts` ([npm](https://www.npmjs.com/package/@cognee/cognee-ts)) | [ts/README.md](ts/README.md) | `import { Cognee } from '@cognee/cognee-ts'` |
| **Python** (PyO3) | build from source (`maturin develop`) — not yet on PyPI | [python/README.md](python/README.md) | `from cognee_py import Cognee` |
| **C API** (FFI) | build from source — see README | [capi/README.md](capi/README.md) | `#include "cognee_sdk.h"` + `cg_sdk_*` |
| **Java** (JNI) | build from source (cdylib + `mvn -f java/pom.xml install`) — see README; not yet on Maven Central | [java/README.md](java/README.md) | `import ai.cognee.Cognee;` |


### Objectives

- **Small-model support**: run with on-device models (Phi4 class + embeddings).
- **90+ correctness**: keep the basic cognee ability to reach similar correctness
  to the Python Cognee SDK (90+%).
- **On-device vs Cloud ability**: transformation tasks + orchestration design
  support on-device and cloud mode. 
- **Multimodal support**: the implementation supports multimodal data ingestion.
- **Retrieval**: optimally 0.6 sec on a reasonably sized knowledge base.

### Orchestration requirements

- **Memory Control**: control over the memory used by the ingestion pipeline.
- **CPU control**: control over threads and parallelization in the ingestion pipeline.
- **Autonomous task scheduling**: dynamic scheduling of memory-tasks.

## Graph Backend Concurrency

For file-backed graph storage, Python's reference implementation documents a
default single-owning-process model for SQLite/Ladybug/LanceDB access, while
also supporting an opt-in Redis-backed shared Ladybug lock for multi-process
coordination. Rust currently matches that default model: Ladybug writes are
idempotent and serialized in-process, but cross-process locking is intentionally
out of scope.

## Running Tests

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

### Logging

Cognee writes structured logs to **stdout** and (when a writable directory is
available) to a rotating file, owned by the [`cognee-logging`](crates/logging/)
crate (`cognee_logging::init_logging`, called by the CLI and HTTP server). The
full env-var table (`COGNEE_LOG_*`, `RUST_LOG`/`LOG_LEVEL`, `LOG_FILE_NAME`) is
documented in [Configuration → Logging](https://docs.cognee.ai/rust/configuration#logging).
