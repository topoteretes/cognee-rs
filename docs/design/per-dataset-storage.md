# ADR: per-dataset storage under `ENABLE_BACKEND_ACCESS_CONTROL`

- **Status:** accepted (SDK-1162, stage S4a)
- **Date:** 2026-10-23
- **Python reference:** `cognee/context_global_variables.py`,
  `cognee/infrastructure/databases/dataset_database_handler/*`,
  `cognee/infrastructure/databases/utils/get_or_create_dataset_database.py`,
  `cognee/modules/users/models/DatasetDatabase.py`,
  `cognee/modules/search/methods/search.py` (`search_in_datasets_context`),
  `cognee/modules/data/methods/delete_dataset.py`,
  `cognee/modules/data/deletion/prune_system.py` (cognee `0d118e8cb`).

## Context

cognee-rs keeps every dataset in **one shared graph store and one shared
vector store**. When an `AclDb` is wired, search is scoped by **post-filtering**
the retrieved context on each item's `dataset_id`/`dataset_ids` payload
(cognee-rs #290). A re-verification on 2026-10-19 found that this model cannot
be made tight:

- hybrid entities carry unscoped one-hop neighbour edges, so an entity shared
  between datasets exposes an unreadable dataset's edges and names;
- the graph-completion family (the default `GraphCompletion` and the summary,
  context-extension and CoT variants) scopes triplets by approximate
  membership and can admit edges from unreadable datasets;
- scoping runs after a global top-k, which starves results;
- six search types (Temporal, Cypher, NaturalLanguage, ChunksLexical,
  FeelingLucky, CodingRules) carry no membership and are refused (422);
- visualize, the dataset graph and the activity export render the whole shared
  graph once the `read` check passes;
- `/v1/search` returns one entry under a derived scope, where Python returns
  one per dataset.

Python does not post-filter. Under `ENABLE_BACKEND_ACCESS_CONTROL` every
dataset gets **its own graph and vector database**, recorded in a relational
`dataset_database` table and provisioned by pluggable *dataset database
handlers*. Every operation that touches a dataset's knowledge runs inside that
dataset's database context, so another dataset's data is not in the store at
all.

## Decision

Adopt Python's model. Per-dataset storage replaces post-filter scoping. Without
the flag the shared store stays exactly as today (Python non-ACL parity).

### 1. The switch

`enable_access_control` (`ENABLE_BACKEND_ACCESS_CONTROL`) is a real setting in
OSS. It no longer needs `ConfigManager::set_application_access_control`.

- **OSS default: `false`.** An embedded SDK user opts in.
- **cognee-cloud-rs default: on when the configured backends support it**
  (Python `backend_access_control_enabled()`, `context_global_variables.py:98-107`).
  Unset means "on if both configured handlers support the configured
  providers". Python raises in the unsupported case. The cloud server instead
  turns the flag off and logs one warning, so an existing qdrant or evokoa
  deployment keeps booting on upgrade. An explicit `true` with an unsupported
  backend is a startup error, as in Python.
- `true` with a provider that has no handler fails at the first component
  build with Python's message (`multi_user_support_possible`): the selected
  handler, the provider and the supported handler names.

### 2. Mechanism: a task-local dataset scope and routing engines

Python binds per-dataset engine configs in `ContextVar`s that
`get_graph_engine()` / `get_vector_engine()` read. Rust threads engines
explicitly: `Arc<dyn GraphDBTrait>` and `Arc<dyn VectorDB>` are captured when
`SearchOrchestrator`, `AddPipeline`, `DeleteService`, `TaskContext`, the
retrievers and so on are built, and changing every one of them to look an
engine up per dataset would touch most of the code base.

Instead:

- `cognee_models::dataset_scope` holds a `tokio::task_local!` with the
  **current dataset id**. It lives in `cognee-models` because the graph and
  vector crates already depend on it, and `cognee-database` would pull SeaORM
  into every graph and vector build. `with_dataset(id, fut)` runs `fut` inside it, and
  `current_dataset()` reads it. A scope set around a future travels with that
  future, including into `tokio::spawn`, so the pipeline-run registry's
  background tasks keep it. Spawns *inside* a scoped future that touch an
  engine must re-enter it (`dataset_scope::propagate`). Today only the LLM
  extraction spawns in cognify sit inside, and they never touch an engine.
- In per-dataset mode the `graph_db` / `vector_db` handles that the component
  layer hands out are **routing engines**: `cognee_graph::DatasetRoutedGraphDB`
  and `cognee_vector::DatasetRoutedVectorDB`. Every trait method resolves the
  current dataset's engine through a `DatasetEngineResolver` and delegates to
  it.
- **Fail closed.** A routing engine called outside a dataset scope returns an
  error (`no dataset scope: per-dataset storage (ENABLE_BACKEND_ACCESS_CONTROL)
  requires every graph/vector access to run inside a dataset`). It never falls
  back to a shared store. This is Python's "A dataset must be provided when
  backend access control is enabled." A missed call site therefore fails
  loudly and never leaks another dataset's data.

Operations that span datasets do not use the routing engines. They go through
the resolver directly: prune, health probes, closing at teardown and the
per-dataset search fan-out (each branch enters its own scope).

### 3. The `dataset_database` table

The table matches Python's columns exactly, so a relational database is
interchangeable between the SDKs (`DatasetDatabase.py`):

| column | type | notes |
|---|---|---|
| `dataset_id` | uuid PK, FK `datasets.id` ON DELETE CASCADE | one row per dataset, shared by the owner and every grantee |
| `owner_id` | uuid, FK `principals.id` ON DELETE CASCADE, indexed | the dataset's owner, never the caller |
| `graph_database_name`, `vector_database_name` | text not null | |
| `graph_database_provider`, `vector_database_provider` | text not null | |
| `graph_dataset_database_handler`, `vector_dataset_database_handler` | text not null | handler name, resolved again on every bind |
| `graph_database_url`, `vector_database_url` | text null | |
| `graph_database_key`, `vector_database_key` | text null | |
| `graph_database_connection_info`, `vector_database_connection_info` | json not null default `{}` | no plaintext credentials (§5) |
| `cognee_version` | text null | audit only |
| `migration_revision`, `migration_last_error` | text null | written as NULL: cognee-rs has no per-dataset data-migration chain |
| `created_at`, `updated_at` | timestamptz | |

The migration creates the table only if it is missing, because a database
Python already migrated has it.

### 4. Dataset database handlers

```rust
#[async_trait]
pub trait DatasetDatabaseHandler: Send + Sync {
    fn name(&self) -> &str;                          // "ladybug", "pgvector_shared", …
    fn kind(&self) -> HandlerKind;                   // Graph | Vector
    fn providers(&self) -> &[&'static str];          // compatible provider ids
    async fn create_dataset(&self, base, dataset_id, owner_id) -> Result<HandlerRecord>;
    fn dataset_context(&self, base, record: &HandlerRecord, owner_id) -> Result<BackendBuildContext>;
    async fn delete_dataset(&self, base, record: &HandlerRecord, owner_id) -> Result<()>;
}
```

`dataset_context` is the Rust-specific step, and it also does Python's
`resolve_dataset_connection_info` (credentials from the live configuration). Python feeds the row into
`create_graph_engine` / `create_vector_engine` kwargs. Here the handler derives
a `BackendBuildContext` from the base one (graph file path, vector URL,
resolved Postgres URL, schema), and the **existing provider factory** builds
the engine. Handlers therefore never construct adapters themselves.

Handlers are registered on `ComponentRegistry` like the factories
(`register_dataset_database_handler`), so a closed adapter crate adds its own.
The built-ins mirror the part of Python's handler set that has a Rust backend:

| handler | kind | providers | storage per dataset |
|---|---|---|---|
| `ladybug` | graph | `ladybug`, `kuzu` | `{system_root}/databases/{owner_id}/{dataset_id}.lbug` (`.pkl` for `kuzu`) |
| `lancedb` | vector | `lancedb` | `{system_root}/databases/{owner_id}/{dataset_id}.lance.db` |
| `pgvector` | vector | `pgvector` | `CREATE DATABASE "{dataset_id}"` on the vector server |
| `pgvector_shared` | vector | `pgvector` | schema `ds_{dataset_id_hex}` in the relational database, `search_path = ds_…, public` |
| `postgres_graph` | graph | `postgres`, `postgresql` | `CREATE DATABASE "{dataset_id}"` on the graph server |
| `postgres_graph_shared` | graph | `postgres`, `postgresql` | schema `ds_{dataset_id_hex}` in the relational database |

`graph_dataset_database_handler` / `vector_dataset_database_handler`
(`GRAPH_DATASET_DATABASE_HANDLER` / `VECTOR_DATASET_DATABASE_HANDLER`) select
the handler. Their defaults follow Python's `graph/config.py:77-90` and
`vector/config.py:49-54`: `ladybug` (`kuzu` when the provider is `kuzu`),
`postgres_graph` for a Postgres graph, `lancedb`, and `pgvector` for pgvector.

The closed `qdrant` and `evokoa` providers have no handler yet, so per-dataset
mode refuses them with the error from §1. Adding their handlers is a follow-up.
`mock` and `brute-force` are in-memory backends. They get the `memory` test
handler (one fresh in-memory engine per dataset), so the mode is testable
without Docker.

### 5. Lifecycle (Python parity)

- **Provision lazily** on the first bind of a dataset: read the row by
  `dataset_id`, not by owner (#4829). If it is missing, run both handlers'
  `create_dataset` and insert the row. A primary-key conflict means another
  caller won the race. In that case re-read and use the winner's row. Both
  callers created the same names, because names derive from the dataset id.
- **Namespace by owner.** Storage paths and the row's `owner_id` come from the
  dataset's owner (looked up from `datasets.owner_id`), never from the caller.
  A grantee's search therefore lands in the owner's databases.
- **No credentials at rest.** The Postgres handlers store host, port, database
  and schema only. `resolve_connection` fills the username and password from the
  live configuration on every bind (graph or vector config for the dedicated
  handlers, relational config for the shared ones).
- **Embedding model guard.** The vector connection info records the embedding
  model and dimensions at provisioning time. Binding a dataset under a
  different width fails with a clear error (Python
  `ensure_embedding_model_matches`) instead of failing inside the vector
  store.
- **Engine cache.** Engines are cached per dataset id and built once per bind
  key. Deleting a dataset evicts and closes its engines before the storage is
  dropped. Teardown closes every cached engine.
- **Delete a dataset:** graph handler `delete_dataset`, then vector handler
  `delete_dataset`, then the relational rows. The row goes with the dataset
  (FK cascade, plus an explicit delete for SQLite connections without FK
  enforcement). Deleting **data inside** a dataset runs the existing
  provenance-driven cleanup inside the dataset's scope.
- **Prune** (`prune_system`) drops every per-dataset database through its
  handler, then the rows.
- **No data migration.** Turning the flag on starts every dataset empty: the
  shared store is not split. This is a documented breaking change, and the
  data must be re-ingested and re-cognified.

### 6. Search

| flag | ACL wired | behaviour |
|---|---|---|
| on | either | permission check (explicit ids/names must be readable, and no filter means every readable dataset), then **one retriever run per dataset inside its scope**, one result per dataset. No post-filter, and every search type works. |
| off | yes | permission check, then **one unscoped search over the shared store**, even for explicit `dataset_ids` (Python `search.py:459-505`). |
| off | no | ownership/tenant check of explicit ids (unchanged), then one unscoped search. |

The #290 membership post-filter (`filter_context_to_datasets`, the per-retriever
`filter_context_to_params` follow-ups) and the six-type refusal are removed.
`DatasetScoping` stays only to mark retrievers whose results are not dataset
data (`Feedback`). Cypher and NaturalLanguage stay gated by `ALLOW_CYPHER_QUERY`
in every mode.

Python's per-dataset result shape is `SearchResultPayload` with `dataset_id`,
`dataset_name` and `dataset_tenant_id` (`search.py:330-420`, and
`_collect_dataset_results`). `SearchOrchestrator::search_datasets` returns one
`DatasetSearchResult` per dataset. `search` keeps its single-response shape for
callers that only ever name one dataset. A per-dataset `NoData` is tagged with
its dataset, and the whole search fails only when every dataset is empty
(Python `DatasetNoDataError`, `_collect_dataset_results`).

### 7. Flag truth table (cloud)

`REQUIRE_AUTHORIZATION` (Rust-only, `auth.require_authorization`) decides
whether an `AclDb` is wired. `ENABLE_BACKEND_ACCESS_CONTROL` decides storage
isolation, the search fan-out, the wire shape and the authentication default:

| `ENABLE_BACKEND_ACCESS_CONTROL` | `REQUIRE_AUTHENTICATION` | effective authentication |
|---|---|---|
| on (default) | unset | required |
| on | `false` | **required**: forced on, with a warning (Python `_resolve_auth_posture`) |
| on | `true` | required |
| off | unset | not required (default user) |
| off | `true`/`false` | as set |

Permission checks run in both storage modes (`search.py:174`, `:433`).
Per-dataset storage is isolation, not authorization. It never replaces the
`read`/`write`/`delete` gates.

### 8. Out of scope

- Python's `DatasetQueue` (`DATASET_QUEUE_MAX_CONCURRENT`): cognee-rs
  serialises per dataset with `DatasetLocks` and does not bound concurrent
  datasets.
- Per-dataset data migrations (`migration_revision` stays NULL).
- Handlers for neo4j, turso, qdrant and evokoa.
- Splitting an existing shared store into per-dataset stores.

## Consequences

- With the flag on, every leak in the Context list goes away by construction,
  because the store a dataset is searched in holds only that dataset's data.
  The six refused types work again, and top-k is per dataset.
- Every code path that touches a graph or vector engine must run inside a
  dataset scope. The fail-closed routing engine turns a missed path into an
  error, and the tests run the pipelines in per-dataset mode to find them.
- An embedded ladybug or LanceDB deployment opens one store per dataset. The
  engine cache keeps each one open while it is in use.
- The shared-store code paths stay, and are the only ones used with the flag
  off.

## Delivery

| stage | where | content |
|---|---|---|
| S4a | cognee-rs | this ADR |
| S4b-1 | cognee-rs | `dataset_database` table and repository; the dataset scope; routing engines; the handler trait and registry; the `DatasetStorage` resolver (provision, cache, delete, prune); the `ladybug`, `lancedb` and `memory` handlers; the flag and handler settings |
| S4b-2 | cognee-rs | `pgvector`, `pgvector_shared`, `postgres_graph` and `postgres_graph_shared` handlers; schema-pinned Postgres adapters |
| S4b-3 | cognee-rs | operations run in the dataset scope: add, cognify, memify/improve, remember, update, rollback and startup recovery, visualize; delete, forget and prune lifecycle |
| S4b-4 | cognee-rs | search: per-dataset fan-out, per-dataset results, post-filter and six-type refusal removed |
| S5 | cognee-cloud-rs | adopt: flag default, wiring, routers (visualize, dataset graph, activity export, cognify WebSocket, `/v1/search` shape), recall auto-route interim removed, qdrant/evokoa refusal |
