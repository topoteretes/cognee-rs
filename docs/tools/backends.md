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
  **Two pgvector version floors, both probed.** `hnsw.iterative_scan` needs
  pgvector 0.8+. On a pool the adapter opens itself it is a connection *option*,
  applied before any extension library loads, so an older server turns it into a
  placeholder that is dropped with a warning and simply does not iterate. A
  connection handed in by the caller (`from_connection`) has no such hook: the
  searches apply the setting themselves with `SET LOCAL`, and because pgvector
  marks the `hnsw.` GUC prefix reserved, an older extension answers *that* with
  `unrecognized configuration parameter` — an error that would fail the search
  instead of merely not iterating. So the version decides whether the setting is
  sent at all. `halfvec` is a harder floor still — the *type* arrives in 0.7.0,
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
  Three exceptions, and in all three the candidate ordering drops back to
  full-precision `vector` too, because there is no fp16 index to match and the
  scan is one the adapter declares exact: collections wider than 2000 dimensions
  are not indexed; a `top_k` above 1000 exceeds the largest `ef_search` pgvector
  accepts; and a collection whose index is dropped for a bulk-load scope (see
  *bulk loads* below) is indexless until the scope ends.
  The 2000-dimension ceiling is **this adapter's choice, not pgvector's limit**:
  2000 is the cap for the `vector` opclasses, while the half-precision
  expression index this adapter builds allows 4000, so
  `text-embedding-3-large` at 3072 could be indexed. It is not, because at that
  width the index is not the faster plan — measured on pgvector 0.8.2 over 3 000
  rows of uniform random 3072-d vectors, a top-100 exact scan took 15.4-16.0 ms
  against the HNSW index's 22.5 ms, for a 25.3 s build and 23 MB. Real
  embeddings cluster better than uniform random ones, so that is a floor rather
  than a verdict; raising the ceiling is a benchmark against a real 3072-d
  corpus, and in the code it is one predicate
  (`PgVectorAdapter::is_indexable_dimension`) that every gate reads, so it moves
  in one place or not at all.
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
  written. **A collection without its index is still correct**: because the
  adapter knows per collection that the index is gone, a search of it runs with
  `enable_indexscan` / `enable_bitmapscan` off and orders by the full-precision
  distance rather than the fp16 one the index would have matched — so it is an
  exact scan returning the true top *k*, and entering a bulk-load scope does not
  move rows across the `LIMIT` boundary. Both halves are needed: the
  full-precision ordering is what the *legacy* `<coll>_vector_hnsw` index serves
  (see *upgrading an existing pgvector store* below), so on an upgraded store
  the expression alone would hand the search to an approximate index the adapter
  neither built nor maintains. Turning the index paths off for the statement
  cannot be defeated by a stray index, and costs nothing outside a scope.
  (A collection whose index was *never* built is the one case left outside that:
  it is in the catalog rather than in memory, so a search cannot tell without a
  round trip, and its scan is still ordered in fp16. `vector-reindex` is the
  repair, as below.) So an interrupted
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
  searches no longer use. Results stay correct — those searches fall back to a
  sequential scan and a sort, which returns the true top *k* — but **every
  search on such a store is that scan**, with no error and no index missing from
  the catalog to notice it by. The adapter therefore warns once per collection,
  the first time it touches one, naming the collection and `vector-reindex`.
  `cognee-cli vector-reindex` is the repair: it builds `<coll>_halfvec_hnsw`
  with `CREATE INDEX CONCURRENTLY` and then **drops the superseded
  `<coll>_vector_hnsw`** (`CONCURRENTLY` as well, and only once the new index is
  confirmed valid — on a pgvector below 0.7 that name *is* the active index, so
  there it is left alone). Dropping it matters beyond disk: it is still
  maintained, so until then every upsert pays two HNSW inserts and a bulk-load
  scope that drops `<coll>_halfvec_hnsw` keeps maintaining the legacy one row by
  row, which is the whole cost the scope exists to avoid. The bulk-load path
  does not drop it itself: it rebuilds only its own index shape at the end of
  the load, so removing an index it never created would be permanent, and a
  load is documented as a hint that changes nothing semantically.
  The same name change applies, for the same reason, to a collection whose
  **name is longer than 50 bytes**: index names now reserve room for their
  suffix and trim the collection part, rather than appending and truncating the
  result, so such a collection's index name changes. (Truncating the result let
  the HNSW and the GIN membership index land on the *same* name at 62 and 63
  bytes, where `CREATE INDEX IF NOT EXISTS` reports a taken name as a NOTICE and
  the second index was silently never built.) `cognee-cli vector-reindex` builds
  the index under the new name; an index under an *old trimmed* name is not
  recognised as superseded and is left on disk for the operator to drop.
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
  **`PgGraphAdapter` requires PostgreSQL 13 or newer.** That floor comes from
  this migration and nothing else: `autovacuum_vacuum_insert_scale_factor` is the
  storage parameter for insert-triggered vacuuming, added in PostgreSQL 13, and
  Postgres rejects an unrecognized reloption rather than ignoring it — so on 12
  the migration fails and the adapter refuses to initialise rather than running
  without the setting. PostgreSQL 12 reached end of life in November 2024. The
  vector adapter and the relational store have no such requirement (their
  settings, `plan_cache_mode` among them, are 12+).
  Both constructors run with `plan_cache_mode = force_custom_plan`, by different
  means: `PgGraphAdapter::new` sets it as a connection option on the pool it
  opens, and `from_connection` — the constructor the shared-Postgres layout uses,
  where the pool belongs to the caller — sends it as a `SET LOCAL` with each
  **parameterised** statement, in a transaction so it cannot leak to the next
  borrower of that pooled connection. Only the parameterised statements: a
  generic plan can differ from a custom one only where there is a parameter to be
  costed blind, and the `count(*)`-style statements plan identically either way.
- **Not in OSS.** Embedded Qdrant and on-device LiteRT inference (Android) are
  not part of this repository.
- **Full Postgres stack** (relational + graph + vector on one Postgres) is the
  one remaining adapter milestone — see [roadmap/](../roadmap/README.md).

`MockEmbeddingEngine`, `MockGraphDB`, `MockVectorDB`, and `MockStorage` (the
`testing` feature) back the test suite — see
[test patterns](../../.claude/CLAUDE.md#test-patterns).
