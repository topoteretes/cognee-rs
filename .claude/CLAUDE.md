# Cognee-Rust Project Guide

## Project Overview

Cognee-Rust is a Rust port of the Python [cognee](https://github.com/topoteretes/cognee) library — an AI memory pipeline that transforms raw data into persistent, queryable knowledge graphs. The goal is both to run on edge devices (Android, embedded) with local models and to serve as a drop-in replacement of the Python `cognee` SDK, while maintaining 90%+ correctness parity.

**Core pipeline:** `add (ingest)` → `cognify (knowledge graph extraction)` → `search (context retrieval)`

## Python Reference Codebase

The Python implementation in the [cognee repository](https://github.com/topoteretes/cognee) (under the `cognee/` directory) serves as the reference for all Rust ports. If you need the Python sources for reference, clone with `git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python`. If the task requires understanding of the Python codebase, read the documentation in that repository (e.g. `/tmp/cognee-python/README.md`, docs, and inline docstrings) before proceeding.

## Workspace structure, crates, patterns & dependencies

The workspace layout, the crate-by-crate breakdown, the cross-cutting
architecture patterns, the key-dependency table, and the rustdoc guide live in
the public docs — single source of truth:

- **[docs/architecture.md](../docs/architecture.md)** — workspace tree, crate
  breakdown, architecture patterns, key dependencies, `cargo doc` guide.
- **[docs/operations.md](../docs/operations.md)** — what `add`/`cognify`/`memify`/`search` and the lifecycle ops do.
- **[docs/configuration.md](../docs/configuration.md)** — full env-var / `Settings` / `ConfigManager` reference.
- **[docs/tools/](../docs/tools/README.md)** — CLI, bindings, HTTP server, pluggable backends.
- **[docs/README.md](../docs/README.md)** — documentation hub / index.

Keep `docs/architecture.md` updated when crates are added or change — do not
re-introduce a duplicate crate list here.

## Build & Development

```bash
# Format the code
cargo fmt

# Check compilation (all targets including tests and examples)
cargo check --all-targets

# Run clippy
cargo clippy --all-targets

# Run tests (debug mode by default, no --release unless explicitly asked)
cargo test

# After making changes, run the full check suite:
scripts/check_all.sh
```

## Test Patterns

- **Async tests:** `#[tokio::test]` for all async test functions (only async runtime used)
- **Mock objects** (behind `testing` feature flag): `MockStorage` (HashMap-based), `MockGraphDB`, `MockVectorDB`. No MockDatabase — tests use real in-memory SQLite (`sqlite::memory:`). All mocks re-exported via `cognee-test-utils`.
- **Temp directories:** `tempfile::tempdir()` for isolated test environments
- **Inline tests:** `#[cfg(test)] mod tests` in source files for focused unit tests
- **Integration tests:** 27 files under `crates/*/tests/` across 12 crates (ingestion, cognify, search, database, embedding, session, CLI, etc.)
- **E2E tests:** CLI E2E via `assert_cmd`, integration tests requiring `COGNEE_E2E_EMBED_MODEL_PATH` / `COGNEE_E2E_TOKENIZER_PATH` env vars, cross-SDK tests in `e2e-cross-sdk/`
- **Conditional skipping:** Tests gracefully skip when required env vars or models are unavailable
- **Feature-gated tests:** e.g. `#![cfg(feature = "fs")]` for filesystem-specific session tests
- **Serial tests:** `#[serial_test::serial]` for PostgreSQL tests that cannot run in parallel
- **Test fixtures:** Ontology test files in `crates/ontology/tests/fixtures/`, shared test data modules in cognify and search

## Running Integration & E2E Tests

### Environment variables

Most integration tests require an OpenAI-compatible LLM and locally-downloaded embedding models. Configure via `.env` at the project root or export directly:

| Variable | Required | Default | Purpose |
|---|---|---|---|
| `OPENAI_URL` | Yes | — | OpenAI-compatible API base URL |
| `OPENAI_TOKEN` | Yes | — | API key |
| `OPENAI_MODEL` | No | `gpt-4o-mini` | LLM model name |
| `COGNEE_TEST_MODEL_DIR` | No | `target/models` | Directory for cached embedding models |
| `COGNEE_E2E_EMBED_MODEL_PATH` | No | auto from model dir | Path to BGE-Small-v1.5 ONNX model |
| `COGNEE_E2E_TOKENIZER_PATH` | No | auto from model dir | Path to BGE-Small tokenizer.json |

### Running Rust workspace tests

```bash
# Run all tests (downloads embedding models if missing, single-threaded for LLM isolation)
bash scripts/run_tests_with_openai.sh

# Run a specific test by name
bash scripts/run_tests_with_openai.sh test_fact_extraction
```

The script sources `scripts/lib/common.sh` and runs the workspace tests via `cargo nextest run --workspace --no-capture` when `cargo-nextest` is available (plus a separate `cargo test --workspace --doc` for doctests), falling back to `cargo test --workspace -- --nocapture --test-threads=1` otherwise. (Tests now use OpenAI embeddings by default; no local ONNX model is downloaded — tests that still require a local embedding model skip gracefully when it is absent.)

### Cross-SDK E2E tests (Python ↔ Rust)

Located in `e2e-cross-sdk/`. Docker-based harness that verifies parity between Python and Rust CLIs.

```bash
cd e2e-cross-sdk
docker compose up --build
```

**Architecture:** 3-stage Dockerfile builds both CLIs (Rust release binary + Python venv) into a single image. Tests run in pytest on a tmpfs workspace.

**Test suites:**
- `test_add_parity.py` — deterministic checks (no LLM needed): content hash, data/dataset IDs, file content, deduplication, metadata match between SDKs
- `test_cross_read.py` — schema compatibility: Rust reads Python DB and vice versa; Python adds then Rust cognifies (requires OpenAI)
- `test_cognify_structural.py` — LLM-dependent structural comparison with tolerances: node/edge counts within 50%, node type Jaccard similarity >= 0.3

**Fixture flow:** Python `add` runs first to bootstrap the DB and extract `owner_id`/`tenant_id`, then Rust is configured with the same IDs so UUID5 outputs are comparable.

### Full check suite

```bash
scripts/check_all.sh
```

Runs in order: `cargo fmt --check` → `cargo check --all-targets` → `cargo clippy -- -D warnings` → C API check (`capi/scripts/check.sh`) → Python binding check (`python/scripts/check.sh`) → TS binding check (`ts/scripts/check.sh`) → Java binding check (`java/scripts/check.sh`).

### CI (GitHub Actions)

`ci.yml` runs on push/PR to main: lint (fmt + check + clippy), tests (with `OPENAI_KEY` secret via `scripts/run_tests_with_openai.sh`), `cargo doc --no-deps`, and C/Python/TS/Java binding checks. `http-parity.yml` runs the cross-SDK Rust↔Python parity suite (`workflow_dispatch` only; see task 12). `ts-prebuild.yml` builds Neon prebuilt binaries for multiple platforms (publishes the `cognee-ts` npm package).

## Coding Conventions

- **`unwrap()` is forbidden in non-test code.** Use one of two alternatives:
  - `expect("reason why this can never panic at runtime")` — when an invariant guarantees the value is always `Some`/`Ok`. The message must explain *why* it cannot fail (e.g. `expect("chunk_start is set whenever we enter the emit branch")`). Do NOT just restate what failed.
  - Proper error/option propagation (`?`, `map_err`, `ok_or`, `match`, etc.) — when the operation can legitimately fail and the error should surface to the caller.
  - Allowed patterns that do not need changing: `Mutex::lock().unwrap()` and `RwLock::read/write().unwrap()` are acceptable because lock poisoning only occurs if a thread already panicked, and there is no meaningful recovery in that case. Add a `// lock poison is unrecoverable` comment when doing this.
- Use `thiserror` for custom error enums in library crates, `anyhow` in binaries/examples
- Prefer streaming (`AsyncRead + Unpin + Send`) over loading full content into memory
- Prefer `&str` borrows over `String` in intermediate data structures; use byte offset tracking for zero-copy slicing
- All public traits must be `Send + Sync` for multi-threaded async usage
- Use `Arc<T>` for shared ownership in pipeline structs
- UUID v5 for deterministic IDs (content-addressed), UUID v4 for random IDs
- Content hash always includes `owner_id` for per-tenant isolation
- Follow existing patterns: new crates go in `crates/`, expose public API through `lib.rs`
