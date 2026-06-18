# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-06-16

Initial public release. A Rust port of the Python
[cognee](https://github.com/topoteretes/cognee) AI-memory pipeline, aiming for
drop-in cross-SDK compatibility.

### Added

- Full `add → cognify → search` pipeline with Python-compatible content hashing
  (MD5, configurable SHA-256), deterministic UUID5 IDs, `text_<md5>.txt` naming,
  and `file://` absolute URIs matching the Python format.
- SQLite metadata database (SeaORM) with a single baseline migration per chain
  and Python-compatible 22-column `Data` schema including tenant isolation.
- Text chunking (word → sentence → paragraph hierarchy) ported from Python, with
  pluggable token counters: `WordCounter` (whitespace fallback),
  `HuggingFaceTokenCounter` (feature-gated), and `TikTokenCounter` (feature-gated).
- Cognify knowledge-graph extraction pipeline (classify → chunk → extract
  entities/relationships via LLM → summarize → index → DLT FK edges).
- Memify standalone graph-enrichment pipeline: reads existing graph, creates and
  indexes triplet embeddings for `SearchType::TripletCompletion`.
- 15 search types: `GraphCompletion` (default), `GraphCompletionCot`,
  `GraphCompletionContextExtension`, `GraphSummaryCompletion`, `TripletCompletion`,
  `RagCompletion`, `Chunks`, `Summaries`, `Temporal`, `Cypher`, `NaturalLanguage`,
  `FeelingLucky`, `Feedback`, `CodingRules`, `ChunksLexical`.
- Multi-provider embedding engine (`EmbeddingConfig::from_env()` + factory):
  ONNX/BGE-Small-v1.5 (local), OpenAI-compatible (OpenAI/Azure/vLLM/llama.cpp/TEI),
  Ollama, and Mock (zero vectors for testing).
- LLM abstraction: `OpenAIAdapter` (works with Ollama/vLLM), `LiteRtAdapter`
  (Android local inference, feature-gated).
- Embedded graph storage (Ladybug) and vector storage (Qdrant) backends, plus
  PostgreSQL graph and pgvector adapters.
- Session management (`SessionStore` trait with `FsSessionStore`,
  `RedisSessionStore`, `SeaOrmSessionStore` backends).
- Ontology resolution (RDF/OWL/JSON-LD loader, `NoOpOntologyResolver`).
- Cascading deletion across relational DB, graph DB, vector DB, and file storage
  with dry-run preview.
- HTTP server (`axum`) mirroring the Python FastAPI surface under `/api/v1/*`
  (add, cognify, memify, search, datasets, delete, users, permissions, auth,
  sessions, notebooks, remember, etc.).
- Cloud serve/disconnect ports Python's `cognee/api/v1/serve/` (on-disk-format
  compatible).
- Top-level API parity: `forget`, `update`, `prune`, `recall`, `remember`,
  `improve`, `DatasetManager`, and runtime config setters.
- Knowledge-graph visualization: self-contained d3.js HTML (force-directed,
  Canvas rendering); CLI `visualize` subcommand.
- Observability: OpenTelemetry/OTLP tracing pipeline (`cognee-observability`,
  `telemetry` feature) and opt-out product analytics (`cognee-telemetry`).
- Structured file logging (`cognee-logging`) with rotation and a Python-compatible
  plain-text formatter.
- CLI binary (`cognee-cli`): `add`, `cognify`, `add-and-cognify`, `memify`,
  `search`, `delete`, `config`, `run-sequence`, `visualize`, `serve`, `disconnect`.
- Language bindings: C API (cbindgen FFI), Python (PyO3 / maturin), JavaScript
  (Neon / Node.js), and an Android runner with LiteRT local inference.
- Cross-SDK E2E test harness (`e2e-cross-sdk/`) verifying Python ↔ Rust
  interoperability for add parity, schema compatibility, and cognify structural
  comparison.
- MSRV declared as Rust 1.89 (edition 2024 + `resolver = "3"` require ≥ 1.85;
  `home@0.5.12` / `icu_collections@2.2.0` raise it to 1.88, and on x86_64 the
  embedded qdrant `quantization` crate uses AVX-512 target features/intrinsics
  — `avx512vl`, `avx512vpopcntdq`, `stdarch_x86_avx512` — stabilized in Rust
  1.89, which is the true floor). A dedicated CI lane (`msrv` job) verifies the
  floor on every push.

### Notes

- Known gaps tracked for follow-up releases: S3 input, full `unstructured`
  office-format extraction (DOCX/XLSX/PPTX/ODT/etc.), and crates.io
  publishability (git deps block `cargo publish` — Track B).
- Cross-process Ladybug locking (Redis-backed coordination) is intentionally
  out of scope for 0.1.0, matching Python's default single-process model.

[Unreleased]: https://github.com/topoteretes/cognee-rust/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/topoteretes/cognee-rust/releases/tag/v0.1.0
