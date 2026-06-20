# Getting Started

A five-minute path from a fresh checkout to your first stored-and-recalled
memory. For a deeper tour of the pipeline see [operations.md](operations.md);
for the full config surface see [configuration.md](configuration.md).

> The repository [`README.md`](../README.md) has a fuller Quick Start including a
> local-Ollama walkthrough. This page is the condensed, copy-pasteable version.

## 1. Build the CLI

```bash
cargo build --release
```

This produces the binary at `target/release/cognee-cli`. Everything below uses
that binary (add it to your `PATH` or call it by path).

## 2. Configure an LLM

cognee-rust needs an OpenAI-compatible chat endpoint. Set three env vars (a
`.env` file in the working directory is picked up automatically):

```bash
export OPENAI_URL="https://api.openai.com/v1"
export OPENAI_TOKEN="sk-..."
export OPENAI_MODEL="gpt-4o-mini"
```

Or point at a local [Ollama](https://ollama.com) server (no API key needed):

```bash
export OPENAI_URL="http://localhost:11434/v1"
export OPENAI_MODEL="qwen3:4b"
```

`OPENAI_URL` / `OPENAI_TOKEN` / `OPENAI_MODEL` are aliases for
`LLM_ENDPOINT` / `LLM_API_KEY` / `LLM_MODEL`. Embeddings default to the OpenAI
provider too; to run fully local, set `EMBEDDING_PROVIDER=ollama` (or `onnx`).
See [configuration.md](configuration.md) for the complete env-var surface
(LLM, embeddings, vector/graph DB, ontology, logging, …).

## 3. First run — the memory API

The **memory API** (`remember` / `recall` / `forget` / `improve`) is the
primary surface. `remember` runs `add → cognify → improve` in one call;
`recall` auto-routes the retrieval strategy.

```bash
# Store memory (inline text and/or file paths)
cognee-cli remember "Cognee turns raw data into a queryable knowledge graph."

# Ask about it (omit -t to let recall pick the strategy)
cognee-cli recall "what does cognee do?"
```

### Session memory

Pass `--session-id` to store/query a transient session cache instead of the
permanent graph:

```bash
cognee-cli remember "we decided to ship on Friday" --session-id chat-42
cognee-cli recall  "when are we shipping?"          --session-id chat-42
```

### Forgetting

```bash
cognee-cli forget --all                 # delete everything you own
cognee-cli forget -d main_dataset       # delete one dataset
```

## 4. The lower-level pipeline

`remember`/`recall` are convenience wrappers. For fine-grained control use the
explicit stages — `add → cognify → search` (plus `memify` for enrichment):

```bash
cognee-cli add "some text" -d my_dataset
cognee-cli cognify -d my_dataset
cognee-cli search "my query" -d my_dataset
```

Each stage and its flags are documented in [operations.md](operations.md).

## Where to go next

- [concepts.md](concepts.md) — the model behind add/cognify/search and node sets.
- [configuration.md](configuration.md) — every env var and `config` key.
- [tools/cli.md](tools/cli.md) — the full CLI command reference.
- [guides/](guides/README.md) — focused how-tos (custom prompts, ontology,
  temporal cognify, memify scoping, custom schemas).
