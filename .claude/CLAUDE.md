# Cognee-Rust Project Guide

## Project Overview

Cognee-Rust is a Rust port of the Python [cognee](https://github.com/topoteretes/cognee) library — an AI memory pipeline that transforms raw data into persistent, queryable knowledge graphs. The goal is both to run on edge devices (Android, embedded) with local models and to serve as a drop-in replacement of the Python `cognee` SDK, while maintaining 90%+ correctness parity.

**Core pipeline:** `add (ingest)` → `cognify (knowledge graph extraction)` → `search (context retrieval)`

## Python Reference Codebase

The Python implementation in the [cognee repository](https://github.com/topoteretes/cognee) (under the `cognee/` directory) serves as the reference for all Rust ports. If you need the Python sources for reference, clone with `git clone --depth 1 https://github.com/topoteretes/cognee.git /tmp/cognee-python`. If the task requires understanding of the Python codebase, read the documentation in that repository (e.g. `/tmp/cognee-python/README.md`, docs, and inline docstrings) before proceeding.

## Rust Workspace Structure

```
cognee-rust/
├── Cargo.toml                  # Workspace root (edition 2024, resolver 3)
├── crates/
│   ├── models/                 # Core data types: Data, Dataset, DataInput, Document, DocumentChunk
│   ├── storage/                # File storage abstraction (StorageTrait, LocalStorage)
│   ├── database/               # Metadata DB abstraction (DatabaseTrait, SqliteDatabase)
│   ├── ingestion/              # Ingest pipeline + content hashing + URL crawler
│   ├── chunking/               # Text chunking (word→sentence→paragraph→TextChunker)
│   ├── cognify/                # Full cognify pipeline (classify → chunk → extract graph → summarize → store)
│   ├── search/                 # Search pipeline with multiple retrieval strategies
│   ├── session/                # Session management and session store
│   ├── embedding/              # Multi-provider embedding engine (ONNX, OpenAI, Ollama, Mock)
│   ├── llm/                    # LLM provider abstraction (OpenAI-compatible API adapter)
│   ├── graph/                  # Graph DB abstraction (Ladybug embedded graph)
│   ├── vector/                 # Vector DB abstraction (Qdrant embedded)
│   ├── ontology/               # Ontology resolution (RDF/JSON-LD loader, NoOp resolver)
│   ├── delete/                 # Dataset/data deletion across all backends
│   ├── core/                   # Task pipeline orchestration framework
│   ├── lib/                    # Top-level library aggregating all crates
│   ├── cli/                    # CLI binary (add, cognify, search, delete, config, run-sequence)
│   ├── utils/                  # Shared utilities
│   └── test-utils/             # Mock implementations (MockStorage, MockGraphDB, MockVectorDB)
├── capi/                       # C API bindings (FFI)
├── js/                         # JavaScript/Node bindings (Neon)
├── python/                     # Python bindings (PyO3)
├── android/                    # Android runner
├── examples/                   # Usage examples (add, cognify, embeddings, fact extraction, qdrant, ladybug, etc.)
├── demo/                       # Demo scripts (host and Android)
├── scripts/                    # Build, check, and deployment scripts
├── docs/                       # Documentation
├── e2e-cross-sdk/              # Cross-SDK E2E tests (Rust ↔ Python interop)
└── .github/workflows/          # CI: lib-tests.yml, lint.yml, capi-check.yml, js-check.yml, python-check.yml
```

### Crate Details

**cognee-models** — Core data types shared across crates: `Data`, `Dataset`, `DataInput`, `Document`, `DocumentChunk`, `Entity`, `KnowledgeGraph`, etc. Pure data structures, no traits.

**cognee-storage** — Abstract file storage layer. Trait: `StorageTrait` (+ `StorageExt`, `StorageWriter`). Impls: `LocalStorage`, `MockStorage`.

**cognee-database** — Database abstraction for metadata persistence. Traits: `IngestDb`, `SearchHistoryDb`, `DeleteDb`. Impl: `DatabaseConnection` (SQLite via SeaORM, implements all three traits).

**cognee-ingestion** — Pipeline for ingesting data: streams content, computes hashes, deduplicates, and stores. Main type: `AddPipeline`. No trait abstraction — uses `StorageTrait` and `IngestDb` from sibling crates.

**cognee-chunking** — Text chunking strategies ported from the Python chunking hierarchy (word → sentence → paragraph). Main entry point: `ExtractTextChunksPipeline`. Trait: `TokenCounter`. Impls: `WordCounter` (whitespace fallback), `HuggingFaceTokenCounter` (BPE/WordPiece via `tokenizers` crate, behind `hf-tokenizer` feature), `TikTokenCounter` (cl100k_base BPE for OpenAI models, behind `tiktoken` feature). `TokenCounterKind` enum with `from_env()` auto-selects the best counter based on `EMBEDDING_PROVIDER` and `COGNEE_TOKEN_COUNTER` env vars.

**cognee-cognify** — Knowledge graph extraction pipeline: classify documents, chunk text, extract entities/relationships via LLM, summarize, store to graph and vector DBs. Main types: `CognifyPipeline`, `CognifyConfig`, `FactExtractor`, `SummaryExtractor`.

**cognee-search** — Unified search orchestration across multiple retrieval strategies. Main types: `SearchBuilder`, `SearchOrchestrator`. `SearchType` enum defines 15 search modes (GraphCompletion, RagCompletion, Chunks, Summaries, Temporal, Cypher, etc.) with corresponding retriever implementations.

**cognee-session** — Session management and QA history storage. Trait: `SessionStore`. Impls: `FsSessionStore`, `RedisSessionStore`, `SeaOrmSessionStore`.

**cognee-embedding** — Text vectorization engine. Trait: `EmbeddingEngine`. Impls: `OnnxEmbeddingEngine` (local ONNX Runtime, BGE-Small-v1.5), `OpenAICompatibleEmbeddingEngine` (OpenAI/Azure/vLLM/llama.cpp/TEI via HTTP), `OllamaEmbeddingEngine` (Ollama `/api/embed`), `MockEmbeddingEngine` (zero vectors for testing). `EmbeddingConfig::from_env()` reads `EMBEDDING_PROVIDER`, `EMBEDDING_MODEL`, `EMBEDDING_ENDPOINT`, `EMBEDDING_API_KEY`, `MOCK_EMBEDDING` etc. and `create_engine()` factory returns the appropriate provider. Input sanitization via `sanitize_embedding_inputs()` / `handle_embedding_response()`.

**cognee-llm** — Async LLM abstraction with structured JSON output. Trait: `Llm` (+ auto-implemented `LlmExt`). Impls: `OpenAIAdapter` (OpenAI-compatible APIs, works with Ollama/vLLM), `LiteRtAdapter`.

**cognee-graph** — Graph database abstraction for knowledge graph storage and traversal. Trait: `GraphDBTrait` (+ `GraphDBTraitExt`). Impls: `LadybugAdapter` (embedded Ladybug), `MockGraphDB`.

**cognee-vector** — Vector database abstraction for similarity search. Trait: `VectorDB`. Impls: `QdrantAdapter` (embedded Qdrant), `MockVectorDB`.

**cognee-ontology** — RDF/OWL ontology integration for entity validation. Trait: `OntologyResolver`. Impls: `RdfLibOntologyResolver`, `NoOpOntologyResolver` (pass-through).

**cognee-delete** — Cascading deletion of data/datasets across all backends (relational DB → graph → vector → file storage). Main type: `DeleteService`.

**cognee-core** — Async runtime, task scheduling, and pipeline execution primitives. Traits: `PipelineWatcher`, `ExecStatusManager`. Impls: `NoopWatcher`, `RayonThreadPool`, `NoopExecStatusManager`.

**cognee-lib** — Unified public API facade that re-exports all crates.

**cognee-cli** — Command-line binary: `add`, `cognify`, `add-and-cognify`, `search`, `delete`, `config`, `run-sequence`.

**cognee-utils** — Shared utilities (retry logic, ID generation).

**cognee-test-utils** — Test helpers and mock implementations for integration tests.

## Architecture Patterns

- **Trait-based abstractions** — `StorageTrait`, `IngestDb`, `GraphDBTrait`, `VectorDB`, `EmbeddingEngine`, `Llm`, `SessionStore`, etc. enable backend swapping and mock testing
- **Prefer `dyn Trait`** — Use object-safe traits with `&dyn Trait` or `Arc<dyn Trait>` at call sites. Only use monomorphized generics when performance-critical or unavoidable.
- **Zero-copy where possible** — e.g. `WordChunk<'a>`, `SentenceChunk<'a>`, `ParagraphChunk<'a>` borrow `&str` slices via byte offset tracking, avoiding intermediate `String` allocations
- **`Arc` for shared ownership** — `Arc<dyn Trait>` in pipelines; `Arc<Mutex<T>>` in mocks
- **Async-first** — All I/O via tokio. Prefer `async` in trait methods since most components may have both local and remote implementations.
- **Streaming-first** — `DataInput::process_by_chunks()`, `StorageTrait::store_stream()`, `ContentHasher::hash_content_stream()` to avoid loading full files into memory
- **Deterministic IDs** — Same content + same owner = same UUID via `uuid5(NAMESPACE_OID, ...)` (content-addressed deduplication). Chunk IDs use `uuid5(NAMESPACE_OID, "{document_id}-{chunk_index}")`.
- **Error types per crate** — Each crate defines its own error enum via `thiserror` (e.g. `StorageError`, `ChunkingError`, `IngestionError`)

## Current State & Roadmap

### Implemented — full add → cognify → search pipeline
- **Data models** — `Data` (22-column Python-compat schema), `Dataset`, `DataInput` (Text, FilePath, Url, S3Path stub, Binary, DataItem), `Document`, `DocumentChunk`, `Entity`, `EntityType`, `EdgeType`, `Triplet`, `DataPoint`
- **File storage** — `LocalStorage` with `file://` absolute URIs matching Python format
- **SQLite metadata database** — SeaORM with migrations including Python-compat columns and tenant_id indexes
- **Ingestion pipeline** — Python-compatible `add()`: MD5 content hashing (configurable SHA256), deterministic UUID5 IDs, multi-tenant support, `text_<md5>.txt` naming, loader engine registry, URL crawling (`UrlFetcher` + `HtmlParser`), deduplication by content hash
- **Text chunking** — 3-level hierarchy (word → sentence → paragraph → `chunk_text`), ported from Python. `TokenCounter` trait with `WordCounter`, `HuggingFaceTokenCounter` (feature-gated), and `TikTokenCounter` (feature-gated) impls. `TokenCounterKind::from_env()` auto-selects counter based on embedding provider. `CognifyConfig.token_counter_kind` drives the selection in the pipeline.
- **Document classification** — mime_type/extension-based (text, pdf, csv, image, audio, dlt_row types recognized; only text extraction implemented)
- **Cognify pipeline** — 6 stages: classify → chunk → extract graph (LLM, batched, custom prompts) → summarize (conditional) → add data points (6 vector collections: DocumentChunk:text, Entity:name, EntityType:name, TextSummary:text, EdgeType:relationship_name, Triplet:text; provenance to relational DB) → extract DLT FK edges. Configurable via `CognifyConfig` builder with `ChunkStrategy::Paragraph` (default) and `ChunkStrategy::Recursive`.
- **Triplet embedding** — optional `"source → relationship → target"` indexing in vector DB
- **LLM integration** — `OpenAiAdapter` (OpenAI-compatible, works with Ollama/vLLM), `LiteRtAdapter` (Android local inference via LiteRT, feature-gated)
- **Embedding engine** — Multi-provider via `EmbeddingConfig::from_env()` + `create_engine()` factory. Providers: `OnnxEmbeddingEngine` (local ONNX Runtime, BGE-Small-v1.5 default), `OpenAICompatibleEmbeddingEngine` (OpenAI/Azure/vLLM/llama.cpp/TEI with retry and input sanitization), `OllamaEmbeddingEngine` (concurrent per-text requests, char-based truncation), `MockEmbeddingEngine` (zero vectors via `MOCK_EMBEDDING=true`). Env vars match Python SDK: `EMBEDDING_PROVIDER`, `EMBEDDING_MODEL`, `EMBEDDING_DIMENSIONS`, `EMBEDDING_ENDPOINT`, `EMBEDDING_API_KEY` (with `LLM_API_KEY` fallback)
- **Graph storage** — Ladybug embedded graph DB
- **Vector storage** — Embedded Qdrant with metadata filtering
- **Search pipeline** — 15 search types: GraphCompletion (default), GraphCompletionCot, GraphCompletionContextExtension, GraphSummaryCompletion, TripletCompletion, RagCompletion, Chunks, Summaries, Temporal, Cypher, NaturalLanguage, FeelingLucky, Feedback, CodingRules, ChunksLexical
- **Session management** — `SessionStore` trait with `FsSessionStore`, `RedisSessionStore`, `SeaOrmSessionStore` backends; integrated in search pipeline for QA history
- **Ontology resolution** — RDF/JSON-LD/Turtle ontology loading with entity type matching
- **Deletion** — scoped cascading across relational DB, graph DB, vector DB, and file storage (with dry-run via `preview()`)
- **CLI** — `add`, `cognify`, `add-and-cognify`, `search`, `delete`, `config`, `run-sequence`
- **Language bindings** — C API (`capi/`), Python via PyO3 (`python/`), JavaScript via Neon (`js/`), Android runner (`android/`)
- **Cross-SDK E2E tests** — `e2e-cross-sdk/` with Docker harness: add parity, cross-read, cognify structural comparison (Python ↔ Rust)
- **Test suite** — Python-compat ID tests, schema compatibility tests, E2E search matrix (9 search types), CLI E2E tests, deletion tests, embedding tests, fact extraction tests

### Not Yet Implemented (next steps)
- **Non-text document extraction** — Classification and loader registry exist for PDF, CSV, image, audio, but actual text extraction is not implemented (only text/* files are processed end-to-end)
- **S3 support** — `DataInput::S3Path` returns an error stub
- **URL processing in DataInput** — `DataInput::Url` in `process_by_chunks()` returns unsupported error (URL crawling works in ingestion pipeline but not in the streaming `DataInput` path)
- **Default tokenizer features in CI** — `HuggingFaceTokenCounter` and `TikTokenCounter` are behind optional feature flags (`hf-tokenizer`, `tiktoken`); CI builds may need to enable them explicitly

## Key Dependencies

| Crate | Purpose |
|---|---|
| `tokio` | Async runtime |
| `sea-orm` (SQLite, Postgres) | ORM for relational database (metadata, sessions, provenance) |
| `ort` (ONNX Runtime) | Local model inference (embeddings) |
| `qdrant` (segment, shard, common, edge — git deps) | Embedded vector storage |
| `lbug` | Embedded graph database (Ladybug) |
| `reqwest` (rustls-tls) | HTTP client (URL crawling, LLM/embedding APIs) |
| `scraper` | HTML parsing for URL ingestion |
| `sophia` / `sophia_turtle` / `sophia_jsonld` | RDF/OWL ontology parsing |
| `sha2` | Content hashing |
| `uuid` (v4, v5) | ID generation |
| `serde` / `serde_json` / `schemars` | Serialization and JSON schema generation |
| `chrono` | Timestamps |
| `tokenizers` | HuggingFace tokenization (embedding engine + chunking token counter) |
| `tiktoken-rs` | OpenAI cl100k_base BPE tokenization (chunking token counter, optional) |
| `tracing` | Structured logging and instrumentation |
| `async-trait` | Async trait support |
| `thiserror` | Error type derivation |
| `clap` | CLI argument parsing |
| `pyo3` | Python bindings |
| `neon` | Node.js/JavaScript bindings |

## Build & Development

```bash
# Format the code
cargo fmt

# Check compilation (all targets including tests and examples)
cargo check --all-targets

# Run clippy
cargo clippy --all-targets

# Run tests (debug mode by default, no --release unless explicitly asked)
source .env && cargo test

# After making changes, run the full check suite:
source .env && scripts/check_all.sh
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

The script sources `scripts/lib/common.sh` which downloads BGE-Small-v1.5 ONNX artifacts from HuggingFace if not already cached, then runs `cargo test --workspace -- --nocapture --test-threads=1`.

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

Runs in order: `cargo fmt --check` → `cargo check --all-targets` → `cargo clippy -- -D warnings` → C API check (`capi/scripts/check.sh`) → Python binding check (`python/scripts/check.sh`) → JS binding check (`js/scripts/check.sh`).

### CI (GitHub Actions)

`lib-tests.yml` runs on push/PR to main: builds, caches embedding models, runs `scripts/run_tests_with_openai.sh` with `OPENAI_KEY` secret. Also runs `cargo doc --no-deps`.

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
