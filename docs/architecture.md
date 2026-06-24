# Architecture & project parts

cognee-rust is a Rust port of the Python [cognee](https://github.com/topoteretes/cognee)
library — an AI memory pipeline that turns raw data into persistent, queryable
knowledge graphs. It targets both edge devices (Android, embedded) with local
models and drop-in parity with the Python `cognee` SDK (90%+ correctness).

**Core pipeline:** `add (ingest)` → `cognify (knowledge-graph extraction)` →
`search (context retrieval)`. See [operations.md](operations.md) for what each
operation does and [configuration.md](configuration.md) for how to configure them.

This page is the single source of truth for the workspace layout, the crate
breakdown, and the cross-cutting design patterns. `.claude/CLAUDE.md` and the
root `README.md` link here rather than duplicating it.

## Workspace structure

```
cognee-rust/
├── Cargo.toml                  # Workspace root (edition 2024, resolver 3)
├── crates/
│   ├── models/                 # Core data types: Data, Dataset, DataInput, Document, DocumentChunk
│   ├── storage/                # File storage abstraction (StorageTrait, LocalStorage)
│   ├── database/               # Metadata DB abstraction (IngestDb/SearchHistoryDb/DeleteDb)
│   ├── ingestion/              # Ingest pipeline + content hashing + URL crawler
│   ├── chunking/               # Text chunking (word→sentence→paragraph→TextChunker)
│   ├── cognify/                # Full cognify pipeline + memify enrichment pipeline
│   ├── search/                 # Search pipeline with multiple retrieval strategies
│   ├── session/                # Session management and session store
│   ├── embedding/              # Multi-provider embedding engine (ONNX, OpenAI, Ollama, Mock)
│   ├── llm/                    # LLM provider abstraction (OpenAI-compatible API adapter)
│   ├── graph/                  # Graph DB abstraction (Ladybug embedded graph)
│   ├── vector/                 # Vector DB abstraction (Qdrant embedded)
│   ├── ontology/               # Ontology resolution (RDF/JSON-LD loader, NoOp resolver)
│   ├── delete/                 # Dataset/data deletion across all backends
│   ├── core/                   # Task pipeline orchestration framework
│   ├── http-server/            # axum HTTP server (library + cognee-http-server binary)
│   ├── visualization/          # Self-contained HTML knowledge-graph visualization (d3.js)
│   ├── observability/          # OpenTelemetry tracing pipeline (OTLP exporter, telemetry feature)
│   ├── telemetry/              # Product-analytics client (send_telemetry → prometh.ai, opt-out)
│   ├── logging/                # Shared file logging (rotation, Python-compatible plain formatter)
│   ├── lib/                    # Top-level library aggregating all crates (public api/ module)
│   ├── bindings-common/        # Shared SDK facade for the JS (Neon) + C-API bindings
│   ├── cli/                    # CLI binary (cognee-cli)
│   ├── bench/                  # Criterion benchmarks (add + cognify + search pipeline)
│   ├── utils/                  # Shared utilities
│   └── test-utils/             # Mock implementations (MockStorage, MockGraphDB, MockVectorDB)
├── capi/                       # C API bindings (FFI)
├── ts/                         # JavaScript/TypeScript/Node bindings (Neon)
├── python/                     # Python bindings (PyO3)
├── examples/                   # Usage examples using the cognee crates
├── demo/                       # Demo scripts (host and Android)
├── scripts/                    # Build, check, and deployment scripts
├── docs/                       # Documentation (this folder)
├── e2e-cross-sdk/              # Cross-SDK E2E tests (Rust ↔ Python interop)
└── .github/workflows/          # CI (ci.yml, http-parity.yml, ts-prebuild.yml)
```

## Crate breakdown

**cognee-models** — Core data types shared across crates: `Data`, `Dataset`, `DataInput`, `Document`, `DocumentChunk`, `Entity`, `KnowledgeGraph`, etc. Pure data structures, no traits.

**cognee-storage** — Abstract file storage layer. Trait: `StorageTrait` (+ `StorageExt`, `StorageWriter`). Impls: `LocalStorage`, `MockStorage`.

**cognee-database** — Database abstraction for metadata persistence. Traits: `IngestDb`, `SearchHistoryDb`, `DeleteDb`. Impl: `DatabaseConnection` (SQLite/Postgres via SeaORM, implements all three traits).

**cognee-ingestion** — Pipeline for ingesting data: streams content, computes hashes, deduplicates, and stores. Main type: `AddPipeline`. No trait abstraction — uses `StorageTrait` and `IngestDb` from sibling crates.

**cognee-chunking** — Text chunking strategies ported from the Python chunking hierarchy (word → sentence → paragraph). Main entry point: `ExtractTextChunksPipeline`. Trait: `TokenCounter`. Impls: `WordCounter` (whitespace fallback), `HuggingFaceTokenCounter` (BPE/WordPiece, behind `hf-tokenizer`), `TikTokenCounter` (cl100k_base BPE, behind `tiktoken`). `TokenCounterKind::from_env()` auto-selects the counter based on `EMBEDDING_PROVIDER` and `COGNEE_TOKEN_COUNTER`.

**cognee-cognify** — Knowledge-graph extraction pipeline: classify documents, chunk text, extract entities/relationships via LLM, summarize, store to graph and vector DBs. Entry points: `cognify()` / `cognify_datasets()`; main types: `CognifyConfig`, `CognifyInput`, `CognifyResult`, `FactExtractor`, `SummaryExtractor`. Also houses the **memify** sub-module (`MemifyConfig`, `MemifyResult`, `memify()`): reads the existing graph, creates triplet embeddings, indexes them for `SearchType::TripletCompletion`.

**cognee-search** — Unified search orchestration across multiple retrieval strategies. Main types: `SearchBuilder`, `SearchOrchestrator`. `SearchType` enum defines 15 search modes with corresponding retriever implementations.

**cognee-session** — Session management and QA-history storage. Trait: `SessionStore`. Impls: `FsSessionStore`, `RedisSessionStore`, `SeaOrmSessionStore`.

**cognee-embedding** — Text vectorization engine. Trait: `EmbeddingEngine`. Impls: `OnnxEmbeddingEngine` (local ONNX Runtime, BGE-Small-v1.5), `OpenAICompatibleEmbeddingEngine` (OpenAI/Azure/vLLM/llama.cpp/TEI via HTTP), `OllamaEmbeddingEngine`, `MockEmbeddingEngine`. `EmbeddingConfig::from_env()` + `create_engine()` factory select the provider. See [configuration.md](configuration.md#embedding).

**cognee-llm** — Async LLM abstraction with structured JSON output. Trait: `Llm` (+ auto-implemented `LlmExt`). Impls: `OpenAIAdapter` (OpenAI-compatible APIs, works with Ollama/vLLM), `LiteRtAdapter` (Android local inference, feature-gated).

**cognee-graph** — Graph database abstraction for knowledge-graph storage and traversal. Trait: `GraphDBTrait` (+ `GraphDBTraitExt`). Impls: `LadybugAdapter` (embedded Ladybug), `PgGraphAdapter` (feature `postgres`), `MockGraphDB`. Concurrency: Rust matches Python's default single-owning-process model for file-backed Ladybug; cross-process locking is intentionally out of scope (see [roadmap/](roadmap/README.md)).

**cognee-vector** — Vector database abstraction for similarity search. Trait: `VectorDB`. Impls: `QdrantAdapter` (embedded Qdrant), `MockVectorDB`.

**cognee-ontology** — RDF/OWL ontology integration for entity validation. Trait: `OntologyResolver`. Impls: `RdfLibOntologyResolver`, `NoOpOntologyResolver` (pass-through).

**cognee-delete** — Cascading deletion of data/datasets across all backends (relational → graph → vector → file storage). Main types: `DeleteService`, `AuthorizedDeleteService`.

**cognee-core** — Async runtime, task scheduling, and pipeline-execution primitives. Traits: `PipelineWatcher`, `ExecStatusManager`. Impls: `NoopWatcher`, `RayonThreadPool`, `NoopExecStatusManager`.

**cognee-http-server** — `axum`-based HTTP server. Library exposes `build_router`, `run`, and `AppState`; also builds the `cognee-http-server` binary. Routers mirror the Python FastAPI surface under `/api/v1/*`. See [http-server/](http-server/README.md).

**cognee-visualization** — Self-contained HTML knowledge-graph visualization (d3.js v7, force-directed, Canvas). Entry points: `visualize`/`render`/`render_multi_user`. Surfaces via the CLI `visualize` subcommand.

**cognee-observability** — OpenTelemetry tracing pipeline. Bridges `#[tracing::instrument]` sites into an OTLP exporter. Entry point: `init_telemetry` (tracing layer + RAII `TelemetryGuard`). Activated by `COGNEE_TRACING_ENABLED=true` or a non-empty `OTEL_EXPORTER_OTLP_ENDPOINT`; real exporter behind the `telemetry` feature. See [observability/opentelemetry.md](observability/opentelemetry.md).

**cognee-telemetry** — Product-analytics client (`send_telemetry`). Fire-and-forget POST to `https://test.prometh.ai` per public API call; opt out with `TELEMETRY_DISABLED`, `ENV=test|dev`, or `--no-default-features`. See [observability/send_telemetry.md](observability/send_telemetry.md).

**cognee-logging** — Shared file-based logging: rotation, the Python-compatible plain-text formatter, and a noise-suppressing `EnvFilter`. Entry point: `init_logging`, called by the CLI and HTTP server. Env-var surface documented in [configuration.md §logging](configuration.md#logging).

**cognee-bench** — Criterion benchmark crate (`batch_add_cognify`) exercising the add + cognify + search pipeline.

**cognee-bindings-common** — Shared SDK facade for the Neon JS and C-API bindings: `SdkError` (+ `code()`), `HandleState`, `CogneeServices`, and neon-free JSON wire helpers. Not a new user-facing Rust API — that remains `cognee_lib::api`.

**cognee-lib** — Unified public API facade. Re-exports all crates and adds an `api/` module mirroring the Python SDK: `forget`, `update`, `prune`, `recall`, `remember`, `improve`, plus `DatasetManager`. Houses the shared `Settings`/`ConfigManager` and runtime setters.

**cognee-cli** — Command-line binary (`cognee-cli`). See [tools/cli.md](tools/cli.md).

**cognee-utils** — Shared utilities: retry logic, deterministic ID generation (`generate_node_id`, `NAMESPACE_OID`, …), secret redaction (`redact`), and tracing attribute keys.

**cognee-test-utils** — Test helpers and mock implementations for integration tests.

## Architecture patterns

- **Feature strategy** — Individual crates define optional features with no defaults (`default = []`). The umbrella library (`cognee-lib`) and the CLI (`cognee-cli`) enable all non-platform-specific features by default, so a plain `cargo build` gives a fully-featured binary. Platform-specific features (e.g. `android-litert`) and `testing` stay opt-in. New feature-gated capabilities should be propagated up through `cognee-lib`/`cognee-cli` defaults unless platform- or test-only.
- **Trait-based abstractions** — `StorageTrait`, `IngestDb`, `GraphDBTrait`, `VectorDB`, `EmbeddingEngine`, `Llm`, `SessionStore`, etc. enable backend swapping and mock testing.
- **Prefer `dyn Trait`** — object-safe traits via `&dyn Trait` / `Arc<dyn Trait>` at call sites; monomorphized generics only when performance-critical.
- **Zero-copy where possible** — `WordChunk<'a>`, `SentenceChunk<'a>`, `ParagraphChunk<'a>` borrow `&str` slices via byte-offset tracking.
- **`Arc` for shared ownership** — `Arc<dyn Trait>` in pipelines; `Arc<Mutex<T>>` in mocks.
- **Async-first** — all I/O via tokio; trait methods are `async` since components may be local or remote.
- **Streaming-first** — `DataInput::process_by_chunks()`, `StorageTrait::store_stream()`, `ContentHasher::hash_content_stream()` avoid loading full files into memory.
- **Deterministic IDs** — same content + owner ⇒ same UUID via `uuid5(NAMESPACE_OID, …)` (content-addressed dedup). Chunk IDs: `uuid5(NAMESPACE_OID, "{document_id}-{chunk_index}")`.
- **Error types per crate** — each crate defines its own `thiserror` enum (`StorageError`, `ChunkingError`, `IngestionError`, …).

## Key dependencies

| Crate | Purpose |
|---|---|
| `tokio` | Async runtime |
| `sea-orm` (SQLite, Postgres) | Relational DB ORM (metadata, sessions, provenance) |
| `ort` (ONNX Runtime) | Local model inference (embeddings) |
| `qdrant` (segment/shard/common/edge — git deps) | Embedded vector storage |
| `lbug` | Embedded graph database (Ladybug) |
| `reqwest` (rustls-tls) | HTTP client (URL crawling, LLM/embedding APIs) |
| `scraper` | HTML parsing for URL ingestion |
| `sophia` / `sophia_turtle` / `sophia_jsonld` | RDF/OWL ontology parsing |
| `uuid` (v4, v5) / `sha2` | ID generation / content hashing |
| `serde` / `serde_json` / `schemars` | Serialization + JSON schema |
| `tokenizers` / `tiktoken-rs` | Tokenization (embedding + chunking token counters) |
| `tracing` / `tracing-subscriber` | Structured logging + instrumentation |
| `opentelemetry` / `opentelemetry-otlp` / `tracing-opentelemetry` | OTLP trace export (`telemetry` feature) |
| `axum` / `tower` / `tower-http` | HTTP server |
| `async-trait` / `thiserror` / `clap` / `criterion` | Trait async / errors / CLI / benchmarks |
| `pyo3` / `neon` | Python / JavaScript bindings |

## Browsing the API docs (rustdoc)

API and type detail are documented inline in the code and rendered by rustdoc —
the rest of these docs link to it rather than restating signatures.

```bash
# Build & open the whole workspace's API docs (no external deps):
cargo doc --no-deps --open

# Or a single crate:
cargo doc -p cognee-cognify --no-deps --open
```

CI already runs `cargo doc --no-deps` on every push (no hosted docs.rs site —
build locally). Each crate's `lib.rs` carries a top-level `//!` summary; start
from `cognee-lib` (the facade) and follow the re-exports.

| Area | Crate (package) | Start at |
|---|---|---|
| Public SDK facade | `cognee-lib` | `api` module, `ConfigManager` |
| Ingest | `cognee-ingestion` | `AddPipeline` |
| Chunking | `cognee-chunking` | `TokenCounter`, `text_chunker` |
| Cognify / memify | `cognee-cognify` | `cognify`, `memify`, `CognifyConfig` |
| Search | `cognee-search` | `SearchBuilder`, `SearchType` |
| Embedding | `cognee-embedding` | `EmbeddingEngine`, `EmbeddingConfig` |
| LLM | `cognee-llm` | `Llm`, `OpenAIAdapter` |
| Graph | `cognee-graph` | `GraphDBTrait`, `LadybugAdapter` |
| Vector | `cognee-vector` | `VectorDB`, `QdrantAdapter` |
| Delete | `cognee-delete` | `DeleteService` |
| HTTP server | `cognee-http-server` | `build_router`, `run`, `AppState` |
