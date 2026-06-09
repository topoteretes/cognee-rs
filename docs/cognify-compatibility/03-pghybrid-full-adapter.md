# Item 3 — Full `PgHybridAdapter` + unified-engine wiring

Parent: [../cognify-compatibility-implementation-plan.md](../cognify-compatibility-implementation-plan.md)
Effort: **large (multi-PR milestone)** · Depends on [Item 1](01-wire-pggraph-component-manager.md) + [Item 2](02-postgres-graph-credential-fallback.md)
Status: 📋 Planned

> **Decision D3 (resolved):** implement a real hybrid adapter, not just a config
> shim. Match Python's `PostgresHybridAdapter`: one Postgres connection backing
> both graph and vector, with combined write/search paths. This is the largest
> item in the ticket and is effectively its own milestone — break it into the PRs
> below.

---

## Python reference

- **Adapter:** `PostgresHybridAdapter(GraphDBInterface, VectorDBInterface)`
  ([hybrid/postgres/adapter.py](/tmp/cognee-python/cognee/infrastructure/databases/hybrid/postgres/adapter.py)) — holds a
  `PostgresAdapter` (graph) + `PGVectorAdapter` (vector) over the **same**
  database. Most methods delegate; the value-add is:
  - **Combined writes:** `add_nodes_with_vectors`, `add_edges_with_vectors`,
    `delete_nodes_with_vectors`, `delete_edges_with_vectors`, `prune_all` — graph
    + vector mutations in one transaction.
  - **Combined search:** `search_graph_with_distances` — a single SQL query that
    `JOIN`s `graph_node`/`graph_edge` against the vector collection table (one
    round-trip instead of vector-search-then-graph-fetch).
- **Facade:** `UnifiedStoreEngine` ([unified/unified_store_engine.py](/tmp/cognee-python/cognee/infrastructure/databases/unified/unified_store_engine.py))
  wraps a graph engine + vector engine with `EngineCapability` flags
  (`GRAPH | VECTOR | HYBRID_WRITE | HYBRID_SEARCH`). For hybrid backends both
  `.graph` and `.vector` point at the *same* adapter instance.
- **Selection:** `get_unified_engine()` ([unified/get_unified_engine.py](/tmp/cognee-python/cognee/infrastructure/databases/unified/get_unified_engine.py))
  — `USE_UNIFIED_PROVIDER=pghybrid` ⇒ build `PostgresHybridAdapter` from the
  relational connection string + the cached `PGVectorAdapter`.
- **Consumers:** `get_unified_engine()` is used across ~8 retrievers
  (`graph_completion`, `chunks`, `temporal`, `summaries`,
  `graph_completion_decomposition`, `brute_force_triplet_search`) and the storage
  task `add_data_points.py`. They call `has_capability()` / `is_hybrid` to decide
  whether to take the optimized single-round-trip path.

## Rust gap

Rust has **no** `UnifiedStoreEngine` concept. The pipeline carries separate
`Arc<dyn GraphDBTrait>` and `Arc<dyn VectorDB>` everywhere. There is no hybrid
adapter and no capability flag. Achieving D3 means introducing the hybrid adapter
**and** a minimal unified-engine concept, then teaching at least the write path
(and optionally the read path) to exploit it.

Two enablers already exist:
- `PgGraphAdapter::from_connection(db: DatabaseConnection)`
  ([pg_graph_adapter.rs:150](../../crates/graph/src/pg_graph_adapter.rs#L150)) — wrap a shared SeaORM connection.
- `PgVectorAdapter` ([crates/vector/src/pg_vector_adapter.rs](../../crates/vector/src/pg_vector_adapter.rs)) — confirm it has (or add)
  an analogous `from_connection`/shared-pool constructor so both adapters reuse
  one `DatabaseConnection`/pool.

---

## Phased plan

### PR 1 — `PgHybridAdapter` skeleton (delegating)

Create `crates/graph/src/pg_hybrid_adapter.rs` (or a new `crates/hybrid/` crate —
decide based on dependency direction; graph already owns `PgGraphAdapter`, but the
hybrid type needs `cognee-vector` too, so a small dedicated crate or placing it in
`cognee-vector` with a dep on `cognee-graph` may avoid a cycle — **resolve the
crate-graph cycle question before coding**).

```rust
pub struct PgHybridAdapter {
    graph: PgGraphAdapter,
    vector: PgVectorAdapter,
}

impl PgHybridAdapter {
    pub async fn new(database_url: &str, dim: usize) -> Result<Self, _> {
        let db = Database::connect(database_url).await?;   // ONE connection
        let graph = PgGraphAdapter::from_connection(db.clone()).await?;
        let vector = PgVectorAdapter::from_connection(db, dim).await?; // add if missing
        Ok(Self { graph, vector })
    }
}

#[async_trait] impl GraphDBTrait for PgHybridAdapter { /* delegate to self.graph */ }
#[async_trait] impl VectorDB     for PgHybridAdapter { /* delegate to self.vector */ }
```

All trait methods delegate in this PR — no combined queries yet. Feature-gate
behind `pggraph`+`pgvector` (or a new `pghybrid` feature that enables both).
Mirror the existing per-method instrumentation/span pattern from the two adapters.

**Crate-cycle note:** `cognee-graph` and `cognee-vector` must not form a
dependency cycle. Confirm the current direction (likely neither depends on the
other) and place `PgHybridAdapter` where it can `use` both — most cleanly a new
leaf crate `cognee-hybrid` depending on both, re-exported by `cognee-lib`.

### PR 2 — Unified-engine concept + `ComponentManager` wiring

Introduce a minimal Rust analogue of `UnifiedStoreEngine`. Given the
separate-`Arc` architecture, the lightest design is:

- `ComponentManager` gains an internal notion of "the graph and vector `Arc`s may
  be the same object." When `USE_UNIFIED_PROVIDER=pghybrid`:
  - Build one `Arc<PgHybridAdapter>`.
  - Hand the **same** `Arc` to both the graph slot (`Arc<dyn GraphDBTrait>`) and
    the vector slot (`Arc<dyn VectorDB>`).
- Add `Settings` handling for `USE_UNIFIED_PROVIDER` (read in config
  construction). `pghybrid` ⇒ force `graph_database_provider=postgres` +
  `vector_db_provider=pgvector` and route both through the hybrid adapter, using
  the relational `db_*` creds via [Item 2](02-postgres-graph-credential-fallback.md)'s resolver.
- Decide precedence: Python lets `USE_UNIFIED_PROVIDER` **override** explicit
  providers — match that.

At this point the full Postgres stack runs through one shared connection, even
though writes/reads still go through the separate trait methods (correct, just not
yet single-round-trip optimized). **This already satisfies the user-visible "one
flag → full Postgres stack" goal.**

### PR 3 — Combined write path (`HYBRID_WRITE`)

Add the `*_with_vectors` combined-write methods to `PgHybridAdapter` as inherent
methods, issuing graph + vector mutations in a single SeaORM transaction
(mirroring Python `add_nodes_with_vectors` / `add_edges_with_vectors`). Then
expose a capability check so the `add_data_points` stage
([crates/cognify/src/tasks.rs](../../crates/cognify/src/tasks.rs)) can downcast/detect the hybrid adapter and take
the combined path. Options for detection:
- A capability enum returned by a new optional trait method
  (e.g. `fn capabilities(&self) -> EngineCapability { GRAPH }` default), or
- An `Any`-based downcast to `PgHybridAdapter`.
Prefer the capability-method approach for cleanliness; document the choice.

This is an **optimization** — the PR 2 path is already correct. Gate the combined
write behind the capability so non-hybrid backends are unaffected.

### PR 4 — Combined search path (`HYBRID_SEARCH`) — optional / stretch

Port `search_graph_with_distances` (the JOIN query). This requires a hybrid-aware
search entry point; the Rust retrievers currently call vector search and graph
fetch separately. Adding a single-round-trip path touches the retriever layer and
is the most invasive part. **Recommend deferring PR 4 to a follow-up ticket**
unless a measured latency win justifies it — PRs 1–3 deliver functional + write
parity; PR 4 is a read-latency optimization.

### PR 5 — Tests

Extend the Item 5 E2E ([05-postgres-full-stack-e2e-test.md](05-postgres-full-stack-e2e-test.md)) with a hybrid
variant (`USE_UNIFIED_PROVIDER=pghybrid`) and add adapter-level tests for the
combined write path (graph + vector consistency after `add_nodes_with_vectors`).

---

## Files touched (across PRs)

- New: `crates/hybrid/` (or `crates/vector/src/pg_hybrid_adapter.rs`) — `PgHybridAdapter`
- [crates/vector/src/pg_vector_adapter.rs](../../crates/vector/src/pg_vector_adapter.rs) — `from_connection` constructor (if missing)
- [crates/lib/src/component_manager.rs](../../crates/lib/src/component_manager.rs) — pghybrid construction + same-`Arc` wiring
- [crates/lib/src/config.rs](../../crates/lib/src/config.rs) — `USE_UNIFIED_PROVIDER` handling
- [crates/graph/src/lib.rs](../../crates/graph/src/lib.rs) / [crates/cognify/src/tasks.rs](../../crates/cognify/src/tasks.rs) — capability hook + combined-write path (PR 3)
- Cargo manifests — `pghybrid` feature plumbing through `cognee-lib`/`cognee-cli` defaults
- README — `USE_UNIFIED_PROVIDER=pghybrid` documentation

## Acceptance criteria

- **PR 1:** `PgHybridAdapter` compiles, implements both traits, passes a delegating
  smoke test over a shared connection.
- **PR 2:** `USE_UNIFIED_PROVIDER=pghybrid` + relational `db_*` Postgres creds runs
  full add→cognify→search through one shared connection; graph and vector are the
  same underlying instance.
- **PR 3:** the `add_data_points` stage uses the combined transactional write when
  the hybrid adapter is active; non-hybrid backends are byte-for-byte unchanged.
- **PR 4 (if done):** combined JOIN search returns results equivalent to the
  separate path within tolerance.
- `scripts/check_all.sh` passes at each PR.

## Risks / notes

- **Biggest risk is the crate dependency graph.** Resolve where `PgHybridAdapter`
  lives before writing code; a wrong choice forces a cycle. A new `cognee-hybrid`
  leaf crate is the safest.
- The Rust trait surface has no hybrid method today; PR 3/PR 4 require adding a
  capability/detection hook that all backends tolerate. Keep the default a no-op so
  Ladybug/Qdrant are unaffected.
- PR 4 (combined search) is genuinely optional for parity-of-behavior; only the
  write atomicity and the shared connection are semantically meaningful. Be
  explicit in the PR description that deferring PR 4 does not break parity, only
  forfeits a latency optimization.
- This is the one item where it is reasonable to ship incrementally and mark the
  ticket's pghybrid requirement "functionally complete" after PR 2–3, with PR 4 as
  a tracked follow-up.
