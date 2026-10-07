# Pluggable backends

cognee-rust is built on trait abstractions so each storage/compute backend can be
swapped via configuration. Pick providers with the env vars / config keys in
[configuration.md](../configuration.md); the trait + adapter detail is in rustdoc
(`cargo doc -p <crate> --no-deps --open`).

| Concern | Trait (crate) | Providers | Selected by |
|---|---|---|---|
| **LLM** | `Llm` ([`cognee-llm`](../../crates/llm/)) | `OpenAIAdapter` (OpenAI/Ollama/vLLM/llama.cpp), `MockLlm` (`testing`) | `LLM_PROVIDER`, `LLM_MODEL`, `LLM_ENDPOINT` |
| **Embeddings** | `EmbeddingEngine` ([`cognee-embedding`](../../crates/embedding/)) | `OnnxEmbeddingEngine` (local BGE-Small), `OpenAICompatibleEmbeddingEngine`, `OllamaEmbeddingEngine`, `MockEmbeddingEngine` | `EMBEDDING_PROVIDER` (+ `MOCK_EMBEDDING`) |
| **Vector DB** | `VectorDB` ([`cognee-vector`](../../crates/vector/)) | `LanceDbAdapter` (on disk, feature `lancedb`, default provider), `BruteForceVectorDB` (in-memory), `PgVectorAdapter` (feature `pgvector`), `MockVectorDB` (`testing`) | `VECTOR_DB_PROVIDER` (`lancedb`/`brute-force`/`pgvector`) |
| **Graph DB** | `GraphDBTrait` ([`cognee-graph`](../../crates/graph/)) | `LadybugAdapter` (embedded), `PgGraphAdapter` (feature `postgres`) | `GRAPH_DATABASE_PROVIDER` (`ladybug`/`kuzu`/`postgres`) |
| **Relational DB** | `IngestDb`/`SearchHistoryDb`/`DeleteDb` ([`cognee-database`](../../crates/database/)) | `DatabaseConnection` — SQLite / Postgres via SeaORM | `DB_PROVIDER`, `DATABASE_URL` |
| **File storage** | `StorageTrait` ([`cognee-storage`](../../crates/storage/)) | `LocalStorage` (`file://`), `MockStorage` | `STORAGE_BACKEND` |
| **Session store** | `SessionStore` ([`cognee-session`](../../crates/session/)) | `FsSessionStore`, `RedisSessionStore`, `SeaOrmSessionStore` | `COGNEE_SESSION_STORE` (server) |
| **Ontology** | `OntologyResolver` ([`cognee-ontology`](../../crates/ontology/)) | `RdfLibOntologyResolver`, `NoOpOntologyResolver` | `ONTOLOGY_RESOLVER` |
| **Tokenizer** (chunking) | `TokenCounter` ([`cognee-chunking`](../../crates/chunking/)) | `WordCounter`, `HuggingFaceTokenCounter` (feature), `TikTokenCounter` (feature) | `COGNEE_TOKEN_COUNTER` |

Notes:

- **Embedded by default.** A plain build runs entirely locally: in-memory
  brute-force vector index, embedded Ladybug (graph), SQLite (relational),
  local file storage. No native vector-store dependencies are required.
- **Feature gates.** `lancedb`, `pgvector`, `pggraph`/`postgres`, `onnx`,
  `ladybug`, and the `hf-tokenizer`/`tiktoken` counters are cargo features, on by
  default in `cognee`/`cognee-cli` (`pggraph` excepted) — see
  [architecture.md §feature strategy](../architecture.md#architecture-patterns).
- **pgvector indexing.** Each collection gets an HNSW index when it is created,
  over `vector::halfvec(dim)` with `halfvec_cosine_ops` and
  `m = 24, ef_construction = 128`, so similarity search is not a sequential
  scan. The column itself stays `vector` (the Python SDK's schema): only
  candidate *selection* sees fp16, and scores are the full-precision distance,
  re-sorted. Each search raises `hnsw.ef_search` to cover its own `top_k` (an
  HNSW scan returns at most `ef_search` rows, default 40, and then stops — a
  larger `LIMIT` is otherwise silently unmet) and runs with
  `hnsw.iterative_scan = relaxed_order`, which keeps scanning until the `LIMIT`
  is met over dead tuples and tightly clustered vectors.
  **Two pgvector version floors, and only one of them is harmless.**
  `hnsw.iterative_scan` needs pgvector 0.8+, but it is a plain GUC name: an
  older server ignores the unknown placeholder setting and simply does not
  iterate. `halfvec` is not like that — the *type* arrives in pgvector 0.7.0,
  and below it `vector::halfvec(n)` does not parse, so the index build fails
  and every similarity search errors with `type "halfvec" does not exist`.
  `CREATE EXTENSION IF NOT EXISTS vector` does not help, because it is a no-op
  against a database that already carries 0.5.x or 0.6.x. So the adapter probes
  `pg_extension.extversion` once when it connects and, below 0.7.0, logs a
  warning and puts both the index and the search ordering back on
  full-precision `vector` / `vector_cosine_ops` (under the legacy
  `<coll>_vector_hnsw` name) — a larger index and slower scans, identical
  results. To move such a store onto the half-precision index, install a
  pgvector 0.7+ binary, run `ALTER EXTENSION vector UPDATE`, and then
  `cognee-cli vector-reindex`.
  Two exceptions: collections wider than 2000 dimensions cannot be indexed by
  pgvector and keep the exact scan, and a `top_k` above 1000 exceeds the largest
  `ef_search` pgvector accepts and so also falls back to the exact scan.
  `search_similar_filtered` stays **exact** filter-then-limit, but no longer by
  disabling index scans: each collection carries a GIN index over
  `cognee_vector_set_names(metadata)`, an `IMMUTABLE` function with
  `node_filter`'s exact semantics, and the filter is `&&` (OR) / `@>` (AND)
  against it, so the membership predicate is an index lookup while the ordering
  keeps the HNSW post-filtering scan out.
- **pgvector bulk loads.** `VectorDB::begin_bulk_load` / `end_bulk_load` (a
  nestable, no-op-by-default hint that cognify wraps a pipeline run in) let the
  adapter stop maintaining an HNSW index row by row during a load: once a
  collection has taken 25% of its rows inside the scope its index is dropped,
  and it is built once — in parallel, under a sized `maintenance_work_mem` —
  when the last scope ends, followed by an `ANALYZE` of every collection
  written. **A collection without its index is still correct**: the planner
  falls back to an exact scan, which returns the true top k. So an interrupted
  load (a crash, a process killed between `begin` and `end`) leaves every
  written row in place and searchable, and the one repair is
  `cognee-cli vector-reindex` / `create_missing_vector_indexes()`, which is
  idempotent. The knobs are under *Vector database* in
  [configuration.md](../configuration.md).
  Collections created *before* this existed have only their primary key, and so
  does any collection whose index build failed — creation is best-effort (it
  logs a warning and continues, because failing there would leave the table
  created and unregistered), so pgvector too old for the `hnsw` access method, a
  restricted role or too little `maintenance_work_mem` all yield a working,
  silently sequential-scan collection. `cognee-cli vector-reindex` backfills
  both cases with `CREATE INDEX CONCURRENTLY` (online, idempotent, and it
  rebuilds an index left invalid by an interrupted build); embedders can call
  `PgVectorAdapter::create_missing_vector_indexes()` directly, or reach it
  through `VectorDB::create_missing_vector_indexes()` on any backend — it
  defaults to a no-op returning `0` for the ones that have no such index. It is
  not automatic: building HNSW over a large collection is expensive, so the
  operator chooses when.
- **Upgrading an existing pgvector store.** The halfvec index is a new index
  under a new name (`<coll>_halfvec_hnsw`), so a store built by an older version
  carries the old full-precision `<coll>_vector_hnsw`, which the halfvec-ordered
  searches no longer use. Nothing breaks — those searches fall back to an exact
  scan, which returns the true top *k* — but they are slow until
  `cognee-cli vector-reindex` builds the new index. The **old index is not
  dropped automatically**: it stays on disk and keeps costing an HNSW insert per
  upsert, so drop it once the new one is in place:
  `DROP INDEX IF EXISTS "<coll>_vector_hnsw"`. Leaving it is safe, just wasteful.
  The same applies, for the same reason, to a collection whose **name is longer
  than 50 bytes**: index names now reserve room for their suffix and trim the
  collection part, rather than appending and truncating the result, so such a
  collection's index name changes. (Truncating the result let the HNSW and the
  GIN membership index land on the *same* name at 62 and 63 bytes, where
  `CREATE INDEX IF NOT EXISTS` reports a taken name as a NOTICE and the second
  index was silently never built.) `cognee-cli vector-reindex` builds the index
  under the new name; the old one is likewise left on disk for the operator to
  drop.
- **Postgres graph tables.** `PgGraphAdapter` creates `graph_node` /
  `graph_edge` with their key columns `COLLATE "C"` (they are only ever
  compared for equality, and the locale collation costs a `strcoll` per btree
  insert, lookup and FK check) and without the redundant source-side covering
  index, which held exactly the primary key's columns in the primary key's
  order. **Both apply to new tables only** — `CREATE TABLE IF NOT EXISTS` never
  rewrites one that exists — so a store created by an older build keeps the
  locale collation and the extra index, and keeps working; a mixed estate is
  expected and supported. A migration additionally sets per-table autovacuum
  scale factors (`vacuum 0.05`, `insert 0.05`, `analyze 0.02`) on both tables;
  it touches no server setting.
- **Not in OSS.** Embedded Qdrant and on-device LiteRT inference (Android) are
  not part of this repository.
- **Full Postgres stack** (relational + graph + vector on one Postgres) is the
  one remaining adapter milestone — see [roadmap/](../roadmap/README.md).

`MockEmbeddingEngine`, `MockGraphDB`, `MockVectorDB`, and `MockStorage` (the
`testing` feature) back the test suite — see
[test patterns](../../.claude/CLAUDE.md#test-patterns).
