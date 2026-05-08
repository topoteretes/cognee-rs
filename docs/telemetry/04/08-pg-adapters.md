# Task 04-08 — Instrument `pgvector_adapter` (pg_graph deferred)

**Status**: ✅ implemented in commit 16ecc16 (pgvector only — pg_graph deferred per user decision)
**Owner**: _unassigned_
**Depends on**:
- [Task 04-02](02-tracing-constants-dedupe.md) — `cognee_utils::tracing_keys::*`.
- [Task 04-04](04-qdrant-instrumentation.md) — pattern source; this task copies the Qdrant shape with `system="pgvector"`.
**Blocks**:
- [Task 04-10 — Tests](10-tests.md) — pgvector-side test cases (run only when `DB_PROVIDER=postgres`).

**Parent doc**: [04 — DB-Adapter Span Instrumentation](../04-db-adapter-instrumentation.md)
**Locked decisions**: #2 (omit `cognee.db.query` on pgvector), #3 (PG adapters are in scope — pgvector covered here, pg_graph deferred), #5 (INFO level), #8 (no feature gate).

---

## 1. Goal

Mirror the Qdrant ([04-04](04-qdrant-instrumentation.md)) instrumentation onto
the `pgvector_adapter` so cloud users get the same vector-span coverage on the
PostgreSQL/pgvector backend.

**Scope was narrowed on 2026-05-08** by user direction: `pg_graph_adapter` is
deferred to a future fan-in refactor. Locked decision 3 (PG adapters in scope)
remains a *goal*; this task delivers the pgvector half of it. See §4.5 below
for why pg_graph is deferred.

### `pgvector_adapter` — `cognee.db.system = "pgvector"`

| Method | File | Line | Span name | Required attributes |
|---|---|---|---|---|
| `search_similar` | [`crates/vector/src/pgvector_adapter.rs:300`](../../crates/vector/src/pgvector_adapter.rs#L300) | 300 | `cognee.db.vector.search` | `cognee.db.system="pgvector"`, `cognee.vector.collection`, `cognee.vector.result_count` |
| `index_points` | [`crates/vector/src/pgvector_adapter.rs:231`](../../crates/vector/src/pgvector_adapter.rs#L231) | 231 | `cognee.db.vector.upsert` | `cognee.db.system="pgvector"`, `cognee.vector.collection`, `cognee.db.row_count` |
| `delete_points` | [`crates/vector/src/pgvector_adapter.rs:385`](../../crates/vector/src/pgvector_adapter.rs#L385) | 385 | `cognee.db.vector.delete` | `cognee.db.system="pgvector"`, `cognee.vector.collection`, `cognee.db.row_count` |
| `delete_collection` | [`crates/vector/src/pgvector_adapter.rs:359`](../../crates/vector/src/pgvector_adapter.rs#L359) | 359 | `cognee.db.vector.delete_collection` | `cognee.db.system="pgvector"`, `cognee.vector.collection` |

## 2. Rationale

Locked decision 3 puts PG adapters in scope. Although Python does not
instrument them (Python's pgvector / pg_graph are not even mentioned in
`cognee/infrastructure/databases/`), the Rust pgvector adapter follows the same
span-shape as Qdrant, so this task is mechanically a copy-paste from 04-04
with one attribute change (`system` value). The Rust port is the system of
record for pgvector instrumentation; if Python ever ships a pgvector adapter,
Python should adopt these names.

`pg_graph_adapter` is deferred — see §4.5 — so locked decision 3's "pg_graph in
scope" remains an open goal pending a future fan-in helper refactor.

## 3. Pre-conditions

- Tasks 04-02 is complete.
- Task 04-04 is landed and merged — this task copies its pattern and uses it as
  a cross-check for shape.
- `cognee-vector` already depends on `cognee-utils` (added by 04-04).
- A clean `cargo check --all-targets` on `main`.

## 4. Step-by-step

### 4.1 Instrument `pgvector_adapter::search_similar`

At [`crates/vector/src/pgvector_adapter.rs:300`](../../crates/vector/src/pgvector_adapter.rs#L300):

```rust
use cognee_utils::tracing_keys::{
    COGNEE_DB_ROW_COUNT, COGNEE_VECTOR_COLLECTION, COGNEE_VECTOR_RESULT_COUNT,
};
use tracing::{Span, instrument};

// ...

#[instrument(
    name = "cognee.db.vector.search",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = "pgvector",
        cognee.vector.collection = tracing::field::Empty,
        cognee.vector.result_count = tracing::field::Empty,
    ),
    err,
)]
async fn search_similar(
    &self,
    data_type: &str,
    field_name: &str,
    query_vector: &[f32],
    top_k: usize,
) -> VectorDBResult<Vec<SearchResult>> {
    let collection = Self::collection_name(data_type, field_name)?;
    Span::current().record(COGNEE_VECTOR_COLLECTION, collection.as_str());

    // ... existing body ...

    let mapped: Vec<SearchResult> = /* ... */;
    Span::current().record(COGNEE_VECTOR_RESULT_COUNT, mapped.len() as i64);
    Ok(mapped)
}
```

The shape is identical to [04-04 §4.2](04-qdrant-instrumentation.md#42-instrument-search_similar)
modulo `cognee.db.system = "pgvector"`. Sub-agent A and B should
diff against the Qdrant version after each change to ensure the only
intentional difference is the `system` value.

### 4.2 Instrument `pgvector_adapter::index_points`

At [`crates/vector/src/pgvector_adapter.rs:231`](../../crates/vector/src/pgvector_adapter.rs#L231):

```rust
#[instrument(
    name = "cognee.db.vector.upsert",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = "pgvector",
        cognee.vector.collection = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
async fn index_points(
    &self,
    data_type: &str,
    field_name: &str,
    points: &[VectorPoint],
) -> VectorDBResult<()> {
    if points.is_empty() {
        return Ok(());
    }

    let collection = Self::collection_name(data_type, field_name)?;
    Span::current().record(COGNEE_VECTOR_COLLECTION, collection.as_str());

    // ... existing body ...

    Span::current().record(COGNEE_DB_ROW_COUNT, points.len() as i64);
    Ok(())
}
```

### 4.3 Instrument `pgvector_adapter::delete_points`

At [`crates/vector/src/pgvector_adapter.rs:385`](../../crates/vector/src/pgvector_adapter.rs#L385):

```rust
#[instrument(
    name = "cognee.db.vector.delete",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = "pgvector",
        cognee.vector.collection = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
async fn delete_points(
    &self,
    data_type: &str,
    field_name: &str,
    point_ids: &[Uuid],
) -> VectorDBResult<()> {
    if point_ids.is_empty() {
        return Ok(());
    }

    let collection = Self::collection_name(data_type, field_name)?;
    Span::current().record(COGNEE_VECTOR_COLLECTION, collection.as_str());

    // ... existing body ...

    Span::current().record(COGNEE_DB_ROW_COUNT, point_ids.len() as i64);
    Ok(())
}
```

### 4.4 Instrument `pgvector_adapter::delete_collection`

At [`crates/vector/src/pgvector_adapter.rs:359`](../../crates/vector/src/pgvector_adapter.rs#L359):

```rust
#[instrument(
    name = "cognee.db.vector.delete_collection",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = "pgvector",
        cognee.vector.collection = tracing::field::Empty,
    ),
    err,
)]
async fn delete_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<()> {
    let collection = Self::collection_name(data_type, field_name)?;
    Span::current().record(COGNEE_VECTOR_COLLECTION, collection.as_str());
    // ... existing body ...
    Ok(())
}
```

### 4.5 Why `pg_graph_adapter` is deferred (not part of this task)

The original draft of this task included instrumenting
`pg_graph_adapter::query`. On 2026-05-08 the user directed that we drop
`pg_graph_adapter` entirely from 04-08 because the only public `query`
implementation at
[`crates/graph/src/pg_graph_adapter.rs:317`](../../crates/graph/src/pg_graph_adapter.rs#L317)
is a stub:

```rust
async fn query(
    &self,
    _query: &str,
    _params: Option<HashMap<Cow<'static, str>, Value>>,
) -> GraphDBResult<Vec<Vec<Value>>> {
    Err(GraphDBError::QueryError(
        "The PostgreSQL graph backend does not support raw Cypher queries. \
         Use a graph-native backend (Ladybug, Neo4j) for raw query support, \
         or use the typed adapter methods (add_nodes, get_neighbors, etc.)."
            .into(),
    ))
}
```

Wrapping a method that always returns `QueryError("not supported")` produces
spans with no useful row counts and no real query text — the value is
near-zero. Meaningful instrumentation requires wrapping the ~22 typed methods
(`add_nodes_raw`, `delete_nodes`, `get_neighbors`, …), each of which composes
multiple SQL statements per call. That work needs a fan-in helper similar to
Ladybug's `execute_query` (or one helper per logical op group), and the
refactor is out of scope here.

This deferral does **not** modify locked decision 3. Decision 3 remains a
goal: pgvector is delivered now, pg_graph waits for the fan-in refactor. A
follow-up gap document should track the refactor + instrumentation as a single
unit.

### 4.6 Imports

For [`crates/vector/src/pgvector_adapter.rs`](../../crates/vector/src/pgvector_adapter.rs):

```rust
use cognee_utils::tracing_keys::{
    COGNEE_DB_ROW_COUNT, COGNEE_VECTOR_COLLECTION, COGNEE_VECTOR_RESULT_COUNT,
};
use tracing::{Span, instrument};
```

`cognee-vector` already depends on `cognee-utils` after 04-04; no `Cargo.toml`
changes are needed.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. Existing pgvector tests still pass (skipped without DB_PROVIDER=postgres).
cargo test -p cognee-vector pgvector

# 3. Clippy.
cargo clippy --all-targets -- -D warnings

# 4. Full check.
scripts/check_all.sh
```

Real attribute coverage tests land in [task 04-10](10-tests.md),
gated on `DB_PROVIDER=postgres` via `cognee_test_utils::pg_test_url()`.
The pg_graph test lane noted in earlier drafts is removed alongside the
deferred instrumentation.

## 6. Files modified

- [`crates/vector/src/pgvector_adapter.rs`](../../crates/vector/src/pgvector_adapter.rs)
  — four `#[instrument]` annotations (search, upsert, delete, delete_collection)
  + `Span::current().record(...)` calls + imports.

No `Cargo.toml` changes (the `cognee-utils` dep was added in 04-04). No
changes to `crates/graph/` — `pg_graph_adapter` is deferred per §4.5.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `cognee.db.system = "pgvector"` vs `"postgres"` confuses consumers (same physical DB, different adapters) | Real. | Mirror the existing convention: pgvector adapter is its own `system` because it's a different *interface* (vector ops over pgvector extension), distinct from any future graph adapter's CRUD-on-rows interface. Document in the PR. |
| Tests in 04-10 depend on a live Postgres at `DB_HOST` | Real — those tests skip gracefully if `DB_PROVIDER != postgres`. | Established `pg_test_url()` pattern handles this. |
| `Self::collection_name(...)` returns `VectorDBResult<String>` (vs the unwrapped `String` in Qdrant) | Already noted in the snippets via `?`. | n/a |
| `pg_graph_adapter::query` is a stub (`GraphDBError::QueryError("not supported")`) — wrapping it adds no signal | **Discovered during review on 2026-05-08.** | Defer pg_graph instrumentation entirely (§4.5). A future fan-in refactor wrapping the ~22 typed methods is required to satisfy locked decision 3 for pg_graph. Track in a follow-up. |

## 8. Out of scope

- `pg_graph_adapter` instrumentation in any form (see §4.5 — deferred).
- `pgvector_adapter::collection_size` / `list_collections` if they exist
  — same reasoning as Qdrant: setup/inspection paths, low cost-to-value.
- Replacing `cognee.db.system = "pgvector"` with a more granular
  identifier (e.g. `"postgres-vector"`). Future cleanup if cross-SDK
  parity tests demand it.
