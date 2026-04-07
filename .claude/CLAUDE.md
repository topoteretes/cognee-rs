# Cognee-Rust Project Guide

## Project Overview

Cognee-Rust is a Rust port of the Python [cognee](../cognee/) library — an AI memory pipeline that transforms raw data into persistent, queryable knowledge graphs. The goal is to run on edge devices (Android, embedded) with small local models (Phi4-class), while maintaining 90%+ correctness parity with the Python SDK.

**Core pipeline:** `add (ingest)` → `cognify (knowledge graph extraction)` → `search (context retrieval)`

## Python Reference Codebase

The Python implementation lives at `/home/dmytro/dev/cognee/cognee/` and serves as the reference for all Rust ports. Key directories:

| Python Path | Purpose |
|---|---|
| `cognee/api/v1/add/` | Data ingestion — resolves inputs, stores in dataset |
| `cognee/api/v1/cognify/` | KG generation — classify, chunk, extract graph via LLM, summarize, store |
| `cognee/api/v1/search/` | Query — multiple search types (GRAPH_COMPLETION, RAG, CHUNKS, CYPHER, etc.) |
| `cognee/tasks/` | Individual pipeline tasks (ingestion, documents, graph, storage, summarization) |
| `cognee/tasks/chunks/` | Chunking hierarchy: `chunk_by_word` → `chunk_by_sentence` → `chunk_by_paragraph` |
| `cognee/modules/pipelines/` | Task orchestration framework |
| `cognee/modules/chunking/` | Text chunking strategies (TextChunker, LangchainChunker, CsvChunker) |
| `cognee/modules/retrieval/` | Search retrievers (chunks, triplets, graph completion, COT, lexical) |
| `cognee/infrastructure/llm/` | LLM provider abstraction + structured output via `instructor` |
| `cognee/infrastructure/llm/prompts/` | All LLM prompts (graph extraction, classification, search completion) |
| `cognee/infrastructure/databases/graph/` | Graph DB adapters (Kuzu, Neo4j, Neptune) |
| `cognee/infrastructure/databases/vector/` | Vector DB adapters (LanceDB, ChromaDB, pgvector) |
| `cognee/shared/data_models.py` | Core models: Node, Edge, KnowledgeGraph, content classification enums |

**Python cognify pipeline (default task sequence):**
1. `classify_documents` — Document type classification by mime_type/extension
2. `extract_chunks_from_documents` — Text segmentation (TextChunker default)
3. `extract_graph_from_data` — LLM extracts Node/Edge/KnowledgeGraph (structured output)
4. `summarize_text` — Hierarchical summaries
5. `add_data_points` — Store nodes+edges in graph DB, embeddings in vector DB

**Python chunking hierarchy (3 levels):**
- `chunk_by_word` — character-level tokenizer yielding `(text, word_type)` where word_type is `word`, `sentence_end`, or `paragraph_end`
- `chunk_by_sentence` — aggregates words into sentences, tracks paragraph IDs, counts tokens
- `chunk_by_paragraph` — batches sentences until overflow, supports `batch_paragraphs` flag
- `TextChunker` — top-level class, further batches paragraph chunks into `DocumentChunk` output

**Python search types:** GRAPH_COMPLETION (default), RAG_COMPLETION, CHUNKS, SUMMARIES, CYPHER, TRIPLET_COMPLETION, GRAPH_COMPLETION_COT, TEMPORAL, FEELING_LUCKY, and more.

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
│   ├── embedding/              # ONNX-based embedding engine (BGE-Small etc.)
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
├── examples/                   # Usage examples (add, cognify, embeddings, fact extraction, qdrant, ladybug, etc.)
└── .github/workflows/          # CI: lib-tests.yml, lint.yml, capi-check.yml, js-check.yml, python-check.yml
```

### Crate Details

**cognee-models** — Core data structures shared across crates
- `Data` — Ingested file/text record with metadata (id, name, raw_data_location, content_hash, owner_id, mime_type)
- `Dataset` — Named collection of Data items, scoped by owner_id
- `DataInput` — Input enum: `Text(String)`, `FilePath(String)`, `Url(String)` with `process_by_chunks()` for streaming
- `Document` — Classified document derived from a Data item (id, name, raw_data_location, mime_type, extension, data_id)
- `DocumentChunk` — Chunk of text from a document (id, text, chunk_size, chunk_index, cut_type, document_id)
- `classify_documents(&[Data]) -> Vec<Document>` — Maps Data items to Documents by mime_type (text/* only currently)

**cognee-storage** — Pluggable file storage
- `StorageTrait` — async trait: store, store_stream, retrieve, exists, delete, create_writer
- `LocalStorage` — Filesystem impl with UUID-based directory distribution (`{base}/{uuid[0:2]}/{uuid[2:4]}/{filename}`)
- `MockStorage` — In-memory HashMap impl for tests (behind `testing` feature)
- `StorageWriter` — Chunk-based streaming writer

**cognee-database** — Pluggable metadata database
- `DatabaseTrait` — async trait: CRUD for Data and Dataset, attach_data_to_dataset, initialize
- `SqliteDatabase` — SQLite impl via sqlx. Schema: `datasets`, `data`, `dataset_data` (junction table)
- `MockDatabase` — In-memory HashMap impl for tests

**cognee-ingestion** — Ingestion pipeline orchestration
- `IngestPipeline<S: StorageTrait, D: DatabaseTrait>` — Generic pipeline. `add()` method: get/create dataset → stream each input with hashing+storage → deduplicate by content hash → create Data record → attach to dataset
- `ContentHasher` — MD5 hashing (content-only, matching Python). Deterministic UUID v5 from hash.
- `url_crawler/` — `UrlFetcher` (reqwest + config), `HtmlParser` (scraper crate), integrated into pipeline for URL inputs

**cognee-chunking** — Text chunking (port of Python chunking hierarchy)
- `chunk_by_word(data: &str) -> Vec<WordChunk>` — Character-level tokenizer using `Peekable<CharIndices>`. Detects sentence endings (`.;!?…。！？`) and paragraph endings (sentence ending + `\n`/`\r`). Zero-copy: `WordChunk.text` is `&str` borrowing from input.
- `chunk_by_sentence(data, maximum_size, counter) -> Vec<SentenceChunk>` — Aggregates words into sentences, tracks paragraph IDs (new UUID v4 on paragraph boundaries), counts tokens via `TokenCounter` trait. Zero-copy: `SentenceChunk.text` is `&str`.
- `chunk_by_paragraph(data, max_chunk_size, batch_paragraphs, counter) -> Vec<ParagraphChunk>` — Batches sentences until token overflow. `batch_paragraphs=true` accumulates across paragraph boundaries; `false` yields at each boundary. Zero-copy: `ParagraphChunk.text` is `&str`.
- `chunk_text(document_id, text, max_chunk_size, counter) -> Vec<DocumentChunk>` — Top-level API (port of Python `TextChunker`). Further batches paragraph chunks, joins with space on emit. `DocumentChunk.text` is owned `String` since it crosses async/crate boundaries.
- `CutType` enum — `ParagraphEnd`, `SentenceEnd`, `SentenceCut`, `Word` (type-safe boundary markers)
- `TokenCounter` trait + `WordCounter` — Pluggable token counting. `WordCounter` uses whitespace-split word count; swap in HuggingFace tokenizers later.
- `ChunkingError` — Error enum: `InvalidChunkSize`, `StorageError`, `InvalidUtf8`

**cognee-cognify** — Full cognify pipeline (port of Python cognify task sequence)
- `CognifyPipeline` — Orchestrates all 6 stages with configurable `CognifyConfig`
- **Stage 1: `classify_documents`** — Maps Data items to typed Documents by mime_type
- **Stage 2: `extract_chunks_from_documents`** — Hierarchical text chunking via cognee-chunking. Supports `ChunkStrategy::Paragraph` (default, sentence-aware) and `ChunkStrategy::Recursive` (character-based with overlap). Writes `token_count` back to Data record.
- **Stage 3: `extract_graph_from_data`** — LLM-based entity/relationship extraction via `FactExtractor`. Batched (`chunks_per_batch`, default 100) with semaphore-controlled concurrency (`max_parallel_extractions`, default 20). DB-aware deduplication of edges. Supports custom extraction prompts. Alternative path: `extract_custom_graph_from_data` for arbitrary `GraphModel` types.
- **Stage 4: `summarize_text`** — LLM-based chunk summarization via `SummaryExtractor`. Conditional (`enable_summarization`, default true). Deterministic UUID for each summary linked to source chunk via `made_from`.
- **Stage 5: `add_data_points`** — Stores all data points to graph DB (nodes: DocumentChunk, TextSummary, Entity, EntityType, EdgeType; edges: LLM-extracted + structural). Generates embeddings and indexes in vector DB across 5 collections: `DocumentChunk:text`, `Entity:name`, `TextSummary:text`, `EdgeType:relationship_name`, `Triplet:embeddable_text` (optional). Writes provenance records to relational DB. Rich metadata per vector point (type, field, dataset_id, user_id, tenant_id, domain-specific fields).
- **Stage 6: `extract_dlt_fk_edges`** — Deterministic foreign-key edge creation for DLT-sourced tabular data (table-level FK edges, row-level is_row_of edges, row-level FK reference edges).
- `CognifyConfig` — Single source of truth for all pipeline parameters. Builder pattern with sensible defaults matching Python. Key params: `max_chunk_size` (1500), `chunk_overlap` (10), `embed_triplets` (false), `enable_summarization` (true), `embedding_batch_size` (100).
- `triplet_creation` — Creates embeddable triplets from knowledge graph edges: `"source: desc → relationship → target: desc"`. Indexed in vector DB when `embed_triplets=true`.

**cognee-search** — Search pipeline with multiple retrieval strategies
- `SearchOrchestrator` — Coordinates retrieval across graph and vector DBs
- `SearchType` enum — 15 search types: GraphCompletion (default), GraphCompletionCot, GraphCompletionContextExtension, GraphSummaryCompletion, TripletCompletion, RagCompletion, Chunks, Summaries, Temporal, Cypher, NaturalLanguage, FeelingLucky, Feedback, CodingRules, ChunksLexical
- Retrievers: `GraphCompletionRetriever`, `ChunksRetriever`, `SummariesRetriever`, `TripletRetriever`, `TemporalRetriever`, `CompletionRetriever`, `LexicalRetriever`, `CypherNlRetriever`, advanced graph retrievers (CoT, context extension)

**cognee-embedding** — Embedding engine
- ONNX Runtime-based embedding via `ort` + `tokenizers`
- `EmbeddingEngine` trait with `OnnxEmbeddingEngine` impl
- Tested with BGE-Small-v1.5 (384 dimensions), supports batch processing and long text truncation

**cognee-llm** — LLM provider abstraction
- `Llm` trait for structured output extraction
- `OpenAiAdapter` — OpenAI-compatible API adapter (works with Ollama, vLLM, etc.)
- `FactExtractor`, `SummaryExtractor` — Typed extraction wrappers with JSON schema validation

**cognee-graph** — Graph database abstraction
- `GraphDb` trait — async trait for node/edge CRUD, neighbor queries
- `LadybugGraphDb` — Embedded graph database (Ladybug)
- Used by cognify for entity/relationship storage and by search for graph traversal

**cognee-vector** — Vector database abstraction
- `VectorDb` trait — async trait for collection management, upsert, search
- `QdrantVectorDb` — Embedded Qdrant vector storage
- Dynamic collection creation, metadata filtering, cosine similarity search

**cognee-delete** — Deletion across all backends
- Scoped deletion (by dataset, by data, by owner)
- Cascading cleanup: relational DB → graph DB → vector DB → file storage
- Dry-run support

**cognee-core** — Task pipeline orchestration
- Generic task pipeline framework for chaining async operations
- `TaskContext` with cancellation support and progress tracking

**cognee-ontology** — Ontology resolution
- `OntologyResolver` trait — entity type matching and subgraph extraction
- `OntologyLoader` — RDF, JSON-LD, Turtle format parsing
- `NoOpOntologyResolver` — pass-through when no ontology is configured

## Architecture Patterns

- **Trait-based abstraction** — `StorageTrait`, `DatabaseTrait`, `TokenCounter` enable backend swapping and mock testing
- **Generics** — `IngestPipeline<S, D>` parameterized on storage/database implementations; `CognifyPipeline` takes `Arc<dyn Trait>` for storage, graph DB, vector DB, LLM, and embedding engine
- **Zero-copy chunking** — `WordChunk<'a>`, `SentenceChunk<'a>`, `ParagraphChunk<'a>` borrow `&str` slices from input text using byte offset tracking; no intermediate String allocations in the chunking hierarchy
- **Arc for shared ownership** — `Arc<S>`, `Arc<D>` in pipeline; `Arc<Mutex<T>>` in mocks
- **Async-first** — All I/O via tokio; `#[async_trait]` for trait objects
- **Streaming-first** — `DataInput::process_by_chunks()`, `StorageTrait::store_stream()`, `ContentHasher::hash_content_stream()` to avoid loading full files into memory
- **Deterministic hashing** — Same content + same owner = same UUID (content-addressed deduplication)
- **Deterministic chunk IDs** — `uuid5(NAMESPACE_OID, "{document_id}-{chunk_index}")` for reproducible chunk identity
- **Error types per crate** — `StorageError`, `DatabaseError`, `ChunkingError`, `UrlFetcherError` via `thiserror`

## Current State & Roadmap

### Implemented — full add → cognify → search pipeline
- **Data models** (Data, Dataset, DataInput, Document, DocumentChunk)
  - `DataInput` variants: `Text`, `FilePath`, `Url`, `S3Path` (error stub), `Binary`, `DataItem`
  - `Data` has full 22-column Python-compat schema (label, tenant_id, loader_engine, raw_content_hash, …)
- **File storage** with LocalStorage; `base_path()` on `StorageTrait` for absolute `file://` URIs
- **SQLite metadata database** with SeaORM; migrations include Python-compat columns and tenant_id indexes
- **Ingestion pipeline** — fully Python-compatible `add()`:
  - MD5 hashing (content-only, no owner_id) with configurable `HashAlgorithm` (MD5 default, SHA256 opt-in)
  - Deterministic UUID5 IDs for data and datasets matching Python's `uuid5(NAMESPACE_OID, …)` formula
  - Multi-tenant support (`tenant_id` flows through pipeline, ID generation, and DB queries)
  - `file://` absolute URI storage paths matching Python format
  - Text files stored as `text_<md5>.txt` matching Python's `TextData` naming
  - Loader engine registry (`text_loader`, `pypdf_loader`, `beautiful_soup_loader`, …)
  - URL inputs: fetches HTML via `UrlFetcher`, extracts text via `HtmlParser`, stores as text
  - Deduplication by content hash within owner+tenant scope
- **Text chunking** — full 3-level hierarchy (word → sentence → paragraph → TextChunker), ported from Python. Two strategies: Paragraph (default, sentence-aware) and Recursive (character-based with overlap).
- **Document classification** — mime_type-based classification (text/* supported)
- **Cognify pipeline** — all 6 stages fully implemented:
  1. `classify_documents` — mime_type-based document typing
  2. `extract_chunks_from_documents` — hierarchical text chunking, writes token_count back to Data
  3. `extract_graph_from_data` — LLM-based entity/relationship extraction with batched concurrency and DB-aware deduplication. Supports custom prompts and custom graph models.
  4. `summarize_text` — LLM-based chunk summarization (conditional via config)
  5. `add_data_points` — stores nodes+edges to Ladybug graph DB, generates embeddings and indexes in Qdrant vector DB across 5 collection types, writes provenance records to relational DB
  6. `extract_dlt_fk_edges` — deterministic foreign-key edge creation for DLT-sourced tabular data
- **Triplet embedding** — optional creation and indexing of `"source → relationship → target"` triplets in vector DB
- **LLM integration** — OpenAI-compatible API adapter (works with Ollama, vLLM, etc.) with structured JSON schema output
- **Embedding engine** — ONNX Runtime-based embeddings (tested with BGE-Small-v1.5, 384 dimensions)
- **Graph storage** — Ladybug embedded graph DB for node/edge CRUD and graph traversal
- **Vector storage** — Embedded Qdrant for similarity search with metadata filtering
- **Search pipeline** — 15 search types including GraphCompletion (default), GraphCompletionCot, GraphCompletionContextExtension, GraphSummaryCompletion, TripletCompletion, RagCompletion, Chunks, Summaries, Temporal, Cypher, NaturalLanguage, FeelingLucky, Feedback, CodingRules, ChunksLexical
- **Ontology resolution** — RDF/JSON-LD/Turtle ontology loading with entity type matching
- **Deletion** — scoped deletion cascading across relational DB, graph DB, vector DB, and file storage (with dry-run support)
- **CLI** — full command set: `add`, `cognify`, `add-and-cognify`, `search`, `delete`, `config`, `run-sequence`
- **Comprehensive test suite** including:
  - Python cross-validated ID tests (`crates/ingestion/tests/python_compat_ids.rs`)
  - Tenant isolation tests, DataItem label tests
  - Schema compatibility tests (`crates/database/tests/schema_compat.rs`)
  - Full E2E search matrix test (add → cognify → search across 9 search types)
  - CLI E2E tests, deletion tests, embedding tests, fact extraction tests

### Not Yet Implemented (next steps)
- **Cross-SDK E2E tests** — Python writes DB, Rust reads; Rust writes, Python verifies
- **Non-text document types** — PDF, CSV, image, audio classification and reading
- **S3 support** — `DataInput::S3Path` currently returns an error stub
- **Real tokenizer** — Replace `WordCounter` with HuggingFace `tokenizers` via `TokenCounter` trait

## Key Dependencies

| Crate | Purpose |
|---|---|
| `tokio` | Async runtime |
| `sqlx` (SQLite) | Relational database |
| `ort` (ONNX Runtime) | Local model inference (embeddings, LLM) |
| `qdrant` (segment, shard, common, edge — git deps) | Vector storage |
| `reqwest` (rustls-tls) | HTTP client for URL fetching |
| `scraper` | HTML parsing |
| `sha2` | Content hashing |
| `uuid` (v4, v5) | ID generation |
| `serde` / `serde_json` | Serialization |
| `chrono` | Timestamps |
| `ndarray` | Tensor operations for embeddings |
| `tokenizers` | Text tokenization |
| `async-trait` | Async trait support |
| `thiserror` | Error type derivation |
| `anyhow` | Error propagation in examples/apps |

## Build & Development

```bash
# Check compilation (all targets including tests and examples)
cargo check --all-targets

# Run tests (debug mode by default, no --release unless explicitly asked)
cargo test

# After making changes, run the full check suite:
scripts/check_all.sh
```

## Test Patterns

- **Async tests:** `#[tokio::test]` for all async test functions
- **Mock objects:** `MockStorage` (HashMap-based, behind `testing` feature) and `MockDatabase` (HashMap-based) for unit testing without I/O
- **Temp files:** `tempfile::NamedTempFile` for file-based test inputs
- **Inline tests:** Each module has `#[cfg(test)] mod tests` with focused unit tests
- **Test coverage areas:** CRUD operations, deduplication, multi-owner isolation, streaming, error handling, chunking hierarchy, isomorphism (reconstructing original text from chunks), deterministic IDs, edge cases (empty input, oversized paragraphs, overflow)

## Coding Conventions

- Use `thiserror` for custom error enums in library crates, `anyhow` in binaries/examples
- Prefer streaming (`AsyncRead + Unpin + Send`) over loading full content into memory
- Prefer `&str` borrows over `String` in intermediate data structures; use byte offset tracking for zero-copy slicing
- All public traits must be `Send + Sync` for multi-threaded async usage
- Use `Arc<T>` for shared ownership in pipeline structs
- UUID v5 for deterministic IDs (content-addressed), UUID v4 for random IDs
- Content hash always includes `owner_id` for per-tenant isolation
- Follow existing patterns: new crates go in `crates/`, expose public API through `lib.rs`
