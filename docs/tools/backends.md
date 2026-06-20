# Pluggable backends

cognee-rust is built on trait abstractions so each storage/compute backend can be
swapped via configuration. Pick providers with the env vars / config keys in
[configuration.md](../configuration.md); the trait + adapter detail is in rustdoc
(`cargo doc -p <crate> --no-deps --open`).

| Concern | Trait (crate) | Providers | Selected by |
|---|---|---|---|
| **LLM** | `Llm` ([`cognee-llm`](../../crates/llm/)) | `OpenAIAdapter` (OpenAI/Ollama/vLLM/llama.cpp), `LiteRtAdapter` (Android, feature-gated) | `LLM_PROVIDER`, `LLM_MODEL`, `LLM_ENDPOINT` |
| **Embeddings** | `EmbeddingEngine` ([`cognee-embedding`](../../crates/embedding/)) | `OnnxEmbeddingEngine` (local BGE-Small), `OpenAICompatibleEmbeddingEngine`, `OllamaEmbeddingEngine`, `MockEmbeddingEngine` | `EMBEDDING_PROVIDER` (+ `MOCK_EMBEDDING`) |
| **Vector DB** | `VectorDB` ([`cognee-vector`](../../crates/vector/)) | `QdrantAdapter` (embedded), `pgvector` (feature) | `VECTOR_DB_PROVIDER` (`qdrant`/`lancedb`→qdrant/`pgvector`) |
| **Graph DB** | `GraphDBTrait` ([`cognee-graph`](../../crates/graph/)) | `LadybugAdapter` (embedded), `PgGraphAdapter` (feature `postgres`) | `GRAPH_DATABASE_PROVIDER` (`ladybug`/`kuzu`/`postgres`) |
| **Relational DB** | `IngestDb`/`SearchHistoryDb`/`DeleteDb` ([`cognee-database`](../../crates/database/)) | `DatabaseConnection` — SQLite / Postgres via SeaORM | `DB_PROVIDER`, `DATABASE_URL` |
| **File storage** | `StorageTrait` ([`cognee-storage`](../../crates/storage/)) | `LocalStorage` (`file://`), `MockStorage` | `STORAGE_BACKEND` |
| **Session store** | `SessionStore` ([`cognee-session`](../../crates/session/)) | `FsSessionStore`, `RedisSessionStore`, `SeaOrmSessionStore` | `COGNEE_SESSION_STORE` (server) |
| **Ontology** | `OntologyResolver` ([`cognee-ontology`](../../crates/ontology/)) | `RdfLibOntologyResolver`, `NoOpOntologyResolver` | `ONTOLOGY_RESOLVER` |
| **Tokenizer** (chunking) | `TokenCounter` ([`cognee-chunking`](../../crates/chunking/)) | `WordCounter`, `HuggingFaceTokenCounter` (feature), `TikTokenCounter` (feature) | `COGNEE_TOKEN_COUNTER` |

Notes:

- **Embedded by default.** A plain build runs entirely locally: embedded Qdrant
  (vector), embedded Ladybug (graph), SQLite (relational), local file storage.
  `lancedb` is accepted as a vector provider alias and maps to the embedded
  Qdrant adapter.
- **Feature gates.** `pgvector`, `pggraph`/`postgres`, `litert`, and the
  `hf-tokenizer`/`tiktoken` counters are cargo features. They are on by default
  in `cognee-lib`/`cognee-cli` except platform-specific ones — see
  [architecture.md §feature strategy](../architecture.md#architecture-patterns).
- **Full Postgres stack** (relational + graph + vector on one Postgres) is the
  one remaining adapter milestone — see [roadmap/](../roadmap/README.md).

`MockEmbeddingEngine`, `MockGraphDB`, `MockVectorDB`, and `MockStorage` (the
`testing` feature) back the test suite — see
[test patterns](../../.claude/CLAUDE.md#test-patterns).
