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
