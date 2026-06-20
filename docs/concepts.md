# Core Concepts

The vocabulary behind cognee-rust: the stores that hold memory, the building
blocks that produce it, and the terms that show up across the API, CLI, and
config. This page is the conceptual map — API/type detail lives in rustdoc
(`cargo doc --no-deps --open`). For *what the system does* see
[operations.md](operations.md); for *how it fits together* see
[architecture.md](architecture.md).

## Architecture: three stores

Cognee keeps memory in three complementary backends. Every cognify run writes to
all three; search reads across them.

| Store | Role | Crate | Default backend |
|---|---|---|---|
| **Relational** | Document tracking, deduplication, provenance/lineage, sessions | [`cognee-database`](../crates/database/) | SQLite via SeaORM (Postgres supported) |
| **Vector** | Semantic similarity over embeddings (chunks, entities, summaries, triplets) | [`cognee-vector`](../crates/vector/) | Embedded Qdrant |
| **Graph** | Entity relationships — the knowledge graph itself | [`cognee-graph`](../crates/graph/) | Embedded Ladybug |

Backend selection and connection settings are covered in
[tools/backends.md](tools/backends.md) and
[configuration.md](configuration.md); the layering is in
[architecture.md](architecture.md).

## Building blocks

### DataPoints

A **DataPoint** is the base storage-layer unit: a structured record that carries
a stable UUID, timestamps, a `type` discriminator, free-form `metadata`, and
provenance fields (`source_pipeline`, `source_task`, `source_node_set`,
`source_content_hash`). Typed graph nodes — `Entity`, `EntityType`, `EdgeType`,
`DocumentChunk`, etc. — embed a DataPoint as their `base`, exposed through the
`HasDataPoint` trait so provenance stamping can walk any node uniformly. When a
DataPoint is indexed, its serialized form becomes the vector-store payload
(`vector_metadata()`), keeping the on-disk shape comparable to Python's.

Rust: `DataPoint` / `HasDataPoint` in [`cognee-models`](../crates/models/).

### Tasks

A **Task** is one reusable unit of work that transforms data — classify, chunk,
extract, summarize, embed. Tasks come in eight execution flavours (sync/async ×
single/iterator/stream × single-value/batch) so a step can stream, fan out, or
process whole batches. They are composed with optional per-task config
(`TaskInfo`: name, batch size, weight, rate limiter) and the pipeline executor
routes values between them.

Rust: `Task` / `TypedTask` / `TaskInfo` in [`cognee-core`](../crates/core/).

### Pipelines

A **Pipeline** is an orchestrated sequence of Tasks with shared context
(database, graph, vector, cancellation, progress) and a watcher for status
events. The concrete pipelines are:

| Pipeline | What it composes | Crate / entry point |
|---|---|---|
| **add** | ingest → hash → dedup → persist | [`cognee-ingestion`](../crates/ingestion/) (`AddPipeline`) |
| **cognify** | classify → chunk → extract → summarize → index → FK edges | [`cognee-cognify`](../crates/cognify/) (`cognify()`) |
| **memify** | read graph → build triplets → embed → index | [`cognee-cognify`](../crates/cognify/) (`memify()`) |
| **search** | route query → retrieve → (optionally) complete | [`cognee-search`](../crates/search/) (`SearchOrchestrator`) |

Pipeline orchestration primitives (`PipelineWatcher`, `ExecStatusManager`,
thread pool) live in [`cognee-core`](../crates/core/). The end-to-end flow is
described in [operations.md](operations.md).

## Key concepts

### Datasets

A **Dataset** is the organizational scope for memory operations: named, owned by
a user, optionally tenant-scoped. Data and DataPoints belong to one or more
datasets (`DataPoint.belongs_to_set`), and add / cognify / search / delete all
operate within a dataset scope. Dataset IDs are deterministic (UUID5 of name +
owner) for cross-SDK reproducibility.

Rust: `Dataset` in [`cognee-models`](../crates/models/); lifecycle helpers in
`DatasetManager` ([`cognee-lib`](../crates/lib/) `api::datasets`).

### Sessions

A **Session** is a temporary memory context — search/answer history and feedback
for a single conversational thread — distinct from permanent, graph-backed
storage. Passing a `--session-id` to `remember` / `recall` scopes a turn to that
session and lets retrieval reuse prior context; omitting it persists input as
permanent graph memory (see the memory API in [operations.md](operations.md)).
The store backend is pluggable.

Rust: `SessionStore` trait in [`cognee-session`](../crates/session/), with
`FsSessionStore` (feature `fs`), `RedisSessionStore`, and `SeaOrmSessionStore`
backends.

### Node Sets

A **node set** is a tag attached to ingested data and the DataPoints derived from
it, used to categorize and later scope the knowledge base. It is **partially
realized** in Rust today:

- **Tagging at ingest** — `add` accepts a `node_set` (stored on `Data.node_set`
  and propagated to derived DataPoints as `source_node_set`); the pipeline
  executor can attach `node_set` provenance to task outputs (`Tagged` /
  `TaggedMeta` in [`cognee-core`](../crates/core/)).
- **Scoping memify** — memify enrichment can be restricted to a subset of the
  graph by node *type* and node *name* via `--node-type` / `--node-name`
  (`MemifyConfig::with_node_type_filter` / `with_node_name_filter`, backed by the
  graph trait's `get_nodeset_subgraph`). The internal `persist_sessions` step
  tags cached session data with a fixed node set.

Note: this is type/name-based subgraph filtering rather than a fully general
named-node-set query surface; treat node sets as a tagging-and-scoping primitive,
not a finished feature.

### Ontologies

An **ontology** grounds extracted entities in external, structured knowledge.
cognee-rust loads RDF/OWL ontologies (Turtle, RDF/XML, N-Triples, JSON-LD) and
uses them for fuzzy entity matching and subgraph enrichment during cognify. The
default is a no-op resolver (no grounding), matching Python's
`ontology_file=None`.

Rust: `OntologyResolver` trait in [`cognee-ontology`](../crates/ontology/), with
`NoOpOntologyResolver` (default) and `RdfLibOntologyResolver`. Enabled per run
via cognify's `--ontology-file`; see [configuration.md](configuration.md).

### Loaders & Chunkers

**Loaders** handle file-format reading at ingest: a loader registry dispatches by
MIME type / extension to per-format loaders (text, PDF, CSV, HTML, image, audio,
and the `unstructured` office formats), most behind feature flags. **Chunkers**
then segment a document into token-bounded pieces through a word → sentence →
paragraph hierarchy, sizing chunks with a pluggable `TokenCounter`
(`WordCounter`, or the feature-gated HuggingFace / tiktoken counters).

Rust: loaders (`LoaderRegistry`, `DocumentLoader`) in
[`cognee-ingestion`](../crates/ingestion/); chunking (`text_chunker`,
`TokenCounter`) in [`cognee-chunking`](../crates/chunking/). Token-counter
selection is configured in [configuration.md](configuration.md).

## See also

- [operations.md](operations.md) — what the pipelines and memory API actually do
- [configuration.md](configuration.md) — env vars and runtime config for every concept above
- [architecture.md](architecture.md) — crate layering and design patterns
- [tools/backends.md](tools/backends.md) — choosing relational / vector / graph backends
- [roadmap/README.md](roadmap/README.md) — what is partial or not yet implemented
