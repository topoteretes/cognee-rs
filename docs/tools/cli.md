# CLI — `cognee-cli`

The command-line binary, built from the [`cognee-cli`](../../crates/cli/) crate.
It drives the full pipeline and is also the on-device (Android) runner. Run
`cognee-cli <command> --help` for the authoritative flag list; the clap
definitions are in [`crates/cli/src/cli.rs`](../../crates/cli/src/cli.rs).

```bash
cargo build --release          # produces target/release/cognee-cli
```

## Subcommands

The **memory API** verbs (`remember` / `recall` / `improve` / `forget`) are the
primary surface; the `add` / `cognify` / `memify` / `search` commands below them
are the lower-level pipeline they compose. All of these are always built (not
feature-gated).

| Command | Purpose | Notable flags | Feature gate |
|---|---|---|---|
| `remember <data…>` | Store memory: `add` + `cognify` + (by default) `improve`. `<data…>` is inline text and/or file paths | `-d/--dataset-name` (`main_dataset`), `--session-id`, `--no-improve` (default OFF — improve runs by default), `--tenant-id` | — |
| `recall <query>` | Query memory with auto-routing (session-aware + graph-backed) | `-t/--query-type` (optional — omit to auto-route), `-d/--datasets` (repeatable), `-k/--top-k` (10), `--session-id`, `-f/--output-format` (`pretty`/`json`/`simple`, default `pretty`) | — |
| `improve` | Enrich memory / bridge sessions (feedback + enrichment stages) | `-d/--dataset-name` (`main_dataset`), `--session-id` (repeatable), `--node-name` (repeatable), `--feedback-alpha` (`0.1`), `--tenant-id` | — |
| `forget` | Remove memory (a dataset, a single data item, or everything) | `-d/--dataset-name` \| `--data-id` (UUID; requires `--dataset-name`; conflicts with `--all`) \| `--all`, `--tenant-id` | — |
| `add <inputs…>` | Ingest text / file paths / HTTP(S) URLs into a dataset | `-d/--dataset-name` (`main_dataset`), `--tenant-id` | — |
| `cognify` | Build the knowledge graph from one or more datasets | `-d/--datasets`, `--chunk-size`, `--chunker` (`TextChunker`/`LangchainChunker`/`CsvChunker`), `--ontology-file`, `-b/--background`, `--llm-max-retries`, `--llm-max-parallel-requests`, `--temporal-cognify` | — |
| `add-and-cognify <inputs…>` | `add` then `cognify` in one step | union of the above | — |
| `memify` | Enrich an existing graph with triplet embeddings | `-d/--datasets`, `--node-type`, `--node-name`, `--batch-size` (100) | — |
| `search <query>` | Query the graph/vectors | `-t/--query-type` (`GRAPH_COMPLETION`), `-d/--datasets`, `-k/--top-k` (10), `--system-prompt` / `--system-prompt-path`, `--session-id`, `-f/--output-format` (`pretty`/`json`), `--llm-max-retries` | — |
| `delete` | Remove data/datasets across all backends | `-d/--dataset-name` \| `--dataset-id`, `--data-id`, `--all`, `--mode` (`soft`/`hard`), `--dry-run`, `-f/--force` | — |
| `config get\|set\|unset <key>` | Read/write the persisted JSON config | — | — |
| `run-sequence` | Run a scripted add/cognify/search sequence | — | — |
| `visualize` | Render the graph to a self-contained HTML file | `-o/--output` (`~/graph_visualization.html`) | `visualization` |
| `bench` | Phase-timed benchmark driver | `--memories`, `--mock-llm`, `--output` | `bench` |

The feature-gated commands are enabled in the default build of `cognee-cli`
(except platform-specific ones). See [architecture.md §feature strategy](../architecture.md#architecture-patterns).

Cloud `serve` / `disconnect` no longer live in `cognee-cli`. They have moved
to the closed-source `cognee-cli-cloud` binary in the `cognee-cloud-rust`
sibling workspace (T15f). Build it with
`cargo build -p cognee-cli-cloud` from `cognee-cloud-rust/` and invoke as
`cognee-cli-cloud serve --url …` / `cognee-cli-cloud disconnect`.

## Memory API

The four primary verbs cover the common workflow end-to-end:

```bash
# Store memory (add + cognify + improve). Inline text and/or file paths.
cognee-cli remember "Cognee turns data into a knowledge graph" ./notes.txt -d my_dataset

# Scope a turn to a session (session-backed memory) instead of permanent graph memory
cognee-cli remember "follow-up note" --session-id chat-42

# Query memory — omit -t to let recall auto-route the retrieval strategy
cognee-cli recall "what did we learn about X?" -d my_dataset -k 10

# Enrich memory / bridge sessions
cognee-cli improve -d my_dataset --session-id chat-42

# Remove memory
cognee-cli forget --all
cognee-cli forget -d my_dataset
cognee-cli forget --data-id 00000000-0000-0000-0000-000000000000 -d my_dataset
```

`remember` with a `--session-id` records session memory; without one it persists
permanent, graph-backed memory. `recall` is session-aware and graph-backed; with
no `-t/--query-type` it auto-routes to a suitable retrieval strategy.

## Lower-level pipeline

The memory verbs compose `add → cognify → search`, which you can also drive
directly for fine-grained control (`remember ≈ add + cognify + improve`;
`recall ≈ auto-routed search`):

```bash
cognee-cli add ./notes.txt "some inline text" -d my_dataset
cognee-cli cognify -d my_dataset
cognee-cli search "what did we learn about X?" -t GRAPH_COMPLETION -d my_dataset -k 10
```

## `config` subcommand

Reads/writes `~/.config/cognee-rust/config.json`. Keys are the snake_case
`Settings` field names. See [configuration.md §CLI config](../configuration.md#cli-config-subcommand).

```bash
cognee-cli config set llm_max_retries 4
cognee-cli config get llm_model
cognee-cli config unset embedding_endpoint
```

## LLM retries

`--llm-max-retries N` (accepted by `cognify`, `add-and-cognify`, `search`)
overrides the retry count for structured-output LLM calls for that run; the
persistent default is the `llm_max_retries` config key (default `2`, minimum `1`).
The CLI flag wins over the config value. It is passed to `OpenAIAdapter` and
governs the strict-schema, function-call, and JSON-fallback parsing paths.

```bash
cognee-cli cognify --llm-max-retries 4
cognee-cli search "What is TechCorp?" --llm-max-retries 4
```

## Logging

The CLI calls `cognee_logging::init_logging` at startup. The env-var surface
(`COGNEE_LOG_*`, `RUST_LOG`/`LOG_LEVEL`, `LOG_FILE_NAME`) is shared with the HTTP
server and bindings and documented canonically in
[configuration.md §logging](../configuration.md#logging). Example — JSON logs to
a custom directory:

```bash
COGNEE_LOG_FORMAT=json COGNEE_LOGS_DIR=/var/log/cognee cognee-cli cognify -d main_dataset
```
