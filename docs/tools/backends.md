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
| **Relational DB** | `RelationalDb` (supertrait of `IngestDb`/`DeleteDb`/`SearchHistoryDb`/`ProvenanceDb`/`SessionQaDb`/…; [`cognee-database`](../../crates/database/)) | `DatabaseConnection` — SQLite / Postgres via SeaORM | `DB_PROVIDER`, `DATABASE_URL` (`relational.*`; `register_relational`) |
| **File storage** | `StorageTrait` ([`cognee-storage`](../../crates/storage/)) | `LocalStorage` (`file://`), `MockStorage` | `STORAGE_BACKEND` (`storage.backend`; `register_storage`) |
| **Session store** | `SessionStore` ([`cognee-session`](../../crates/session/)) | `RelationalSessionStore` (the relational DB's `SessionQaDb`; default), `FsSessionStore` (feature `session-fs`), `RedisSessionStore` (feature `session-redis`) | `COGNEE_SESSION_STORE` (`session.store`; `register_session_store`) |
| **Ontology** | `OntologyResolver` ([`cognee-ontology`](../../crates/ontology/)) | `RdfLibOntologyResolver`, `NoOpOntologyResolver` | `ONTOLOGY_RESOLVER` (`ontology.resolver`; `register_ontology_resolver`) |
| **Graph extraction** (cognify) | `ChunkGraphExtractor` ([`cognee-cognify`](../../crates/cognify/)) | built-in LLM extractor; external backends (GLiNER, …) | `GRAPH_EXTRACTOR` (`graph_extraction.backend`, default `auto`; `register_graph_extractor`), per-call `extractor` / `graphBackend` |
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
  is met over dead tuples and tightly clustered vectors. Neither is a guarantee:
  over near-duplicate vectors the HNSW graph can close into a component with no
  edge out, and a query that descends into one gets only that component's rows
  at any `ef_search` (49 of 100 measured). So an index scan that returns fewer
  than `top_k` rows is discarded and the search is answered by the exact scan
  instead, in the same statement — a `LIMIT` is never silently unmet while the
  collection holds that many rows. On a full answer the exact branch is never
  started. The fallback's answer is the exact scan's, so zero-norm stored
  vectors (which the cosine index does not store) come back last with a `NaN`
  score, as they already do on every exact path; a zero-norm *query* never
  falls back, because it cannot rank anything. How many queries of a search
  fell back is recorded on its span as `cognee.vector.exact_fallbacks`.
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
  Four exceptions, and in all four the candidate ordering drops back to
  full-precision `vector` too, because there is no fp16 index to match and the
  scan is one the adapter declares exact: collections wider than pgvector can
  index are not indexed (4000 dimensions for the half-precision index; 2000 on a
  pgvector below 0.7, whose full-precision opclass stops there); a `top_k` above
  1000 exceeds the largest `ef_search` pgvector accepts; a collection whose
  index is dropped for a bulk-load scope (see *bulk loads* below) is indexless
  until the scope ends; and a collection found **without a usable index** the
  first time the adapter touches it (see below) is indexless until it has one.
  The ceiling is one predicate (`PgVectorAdapter::is_indexable_dimension`) that
  every gate reads, so it moves in one place or not at all.
  **`text-embedding-3-large` (3072-d) is indexed.** The ceiling used to be
  2000, on the strength of a benchmark of uniform random vectors in which the
  index lost; at production size the exact scan reads every stored vector on
  every search, and the BYOC 100 MB run measured a hybrid-search median of
  8.4 s against ~3 s for the indexed Python store (ENG-33). Over 12 000
  clustered 3072-d rows a top-100 search now takes 3.2-7.6 ms on the index
  against 96 ms exact, with 100/100 recall. A vector this wide does not fit in
  its heap row, so it lives in TOAST, which the planner does not cost: it rates
  a sequential scan of a few thousand such rows far below the index scan, and
  picks a scan that casts every stored vector to fp16 — 6x the exact scan.
  Searches of collections wider than 2000 dimensions therefore run with
  `enable_seqscan = off` (in a transaction, so the setting stays on that
  statement). Narrower collections keep their vectors in the row, where the
  planner's costs are right, and keep the single-statement fast path. Indexing
  makes writing such a collection slower — every row is an HNSW insert of a
  3072-d vector — which the bulk-load deferral below absorbs for a cognify
  run.
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
  A collection whose index was *never* built — a store written before its width
  was indexed, a failed build — is in the catalog rather than in memory, so
  the adapter checks it once, on the first `has_collection` of each collection
  (the round trip that call makes anyway), warns naming `vector-reindex`, and
  searches it exactly from then on. It asks the catalog again a minute later, so
  an index built by another process is picked up without a restart. A
  collection a search reaches without that first `has_collection` is still
  ordered in fp16 — correct, slower. So an interrupted
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
  searches no longer use. Results stay correct — those searches run the exact
  scan, which returns the true top *k* — but **every search on such a store is
  that scan**, with no error and no index missing from the catalog to notice it
  by. The adapter therefore warns once per collection, the first time it
  touches one, naming the collection and `vector-reindex`. **Upgrading to a
  version that indexes 3072-d collections is the same situation:** existing
  3072-d collections have no index until `cognee-cli vector-reindex` builds
  one, and are searched exactly — as fast as before the upgrade, no faster —
  until then.
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
