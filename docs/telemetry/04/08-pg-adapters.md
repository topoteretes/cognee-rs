# Task 04-08 — Instrument `pgvector_adapter` and `pg_graph_adapter`

**Status**: ⬜ unimplemented
**Owner**: _unassigned_
**Depends on**:
- [Task 04-01](01-redact-relocate.md) — `cognee_utils::redact::redact` (used by `pg_graph_adapter::query`).
- [Task 04-02](02-tracing-constants-dedupe.md) — `cognee_utils::tracing_keys::*`.
**Blocks**:
- [Task 04-10 — Tests](10-tests.md) — pg-side test cases (run only when `DB_PROVIDER=postgres`).

**Parent doc**: [04 — DB-Adapter Span Instrumentation](../04-db-adapter-instrumentation.md)
**Locked decisions**: #2 (omit `cognee.db.query` on pgvector), #3 (PG adapters are in scope), #5 (INFO level), #8 (no feature gate), #9 (truncate-then-redact for `pg_graph_adapter`).

---

## 1. Goal

Mirror the Qdrant ([04-04](04-qdrant-instrumentation.md)) and Ladybug
([04-05](05-ladybug-instrumentation.md)) instrumentation onto the two
PostgreSQL-backed adapters so cloud users get the same span coverage:

### `pgvector_adapter` — `cognee.db.system = "pgvector"`

| Method | File | Line | Span name | Required attributes |
|---|---|---|---|---|
| `search_similar` | [`crates/vector/src/pgvector_adapter.rs:300`](../../crates/vector/src/pgvector_adapter.rs#L300) | 300 | `cognee.db.vector.search` | `cognee.db.system="pgvector"`, `cognee.vector.collection`, `cognee.vector.result_count` |
| `index_points` | [`crates/vector/src/pgvector_adapter.rs:231`](../../crates/vector/src/pgvector_adapter.rs#L231) | 231 | `cognee.db.vector.upsert` | `cognee.db.system="pgvector"`, `cognee.vector.collection`, `cognee.db.row_count` |
| `delete_points` | [`crates/vector/src/pgvector_adapter.rs:385`](../../crates/vector/src/pgvector_adapter.rs#L385) | 385 | `cognee.db.vector.delete` | `cognee.db.system="pgvector"`, `cognee.vector.collection`, `cognee.db.row_count` |
| `delete_collection` | [`crates/vector/src/pgvector_adapter.rs:359`](../../crates/vector/src/pgvector_adapter.rs#L359) | 359 | `cognee.db.vector.delete_collection` | `cognee.db.system="pgvector"`, `cognee.vector.collection` |

### `pg_graph_adapter` — `cognee.db.system = "postgres"`

| Method | File | Line | Span name | Required attributes |
|---|---|---|---|---|
| `query` | [`crates/graph/src/pg_graph_adapter.rs:317`](../../crates/graph/src/pg_graph_adapter.rs#L317) | 317 | `cognee.db.graph.query` | `cognee.db.system="postgres"`, `cognee.db.query` (truncated 500 + redacted), `cognee.db.row_count` |

`pg_graph_adapter` does not have a single private fan-in helper like
Ladybug's `execute_query`; the trait method `query` is itself the
boundary. Per locked decision 1, we instrument **only** the trait
`query` method here; the per-method graph operations
(`add_node_raw`, `add_edges`, etc.) build SQL via SeaORM and would
be over-instrumented if we wrapped each one. Their high-level
counterparts get coverage via the SeaORM-ops task ([04-09](09-seaorm-ops-instrumentation.md))
or via the graph-domain layer above.

> **Note on scope**: this task instruments the seven methods above
> only. The other ~22 public methods on `pg_graph_adapter`
> (`has_node`, `add_nodes_raw`, `delete_nodes`, `get_neighbors`, …)
> compose multiple SQL statements per call. Wrapping them would
> require a fan-in refactor; that is *not* part of this gap. If
> users want per-domain spans, they get them from cognee-search /
> cognify wrappers further up the stack.

## 2. Rationale

Locked decision 3 puts PG adapters in scope. Although Python does
not instrument them (Python's pgvector / pg_graph are not even
mentioned in `cognee/infrastructure/databases/`), the Rust adapters
follow the same span-shape as Qdrant/Ladybug, so this task is
mechanically a copy-paste from 04-04 and 04-05 with two attribute
changes (`system` value and the `pg_graph_adapter` location).

The Rust port is the system of record for PG instrumentation; if
Python ever ships PG adapters, Python should adopt these names.

## 3. Pre-conditions

- Tasks 04-01, 04-02 are complete.
- Tasks 04-04 and 04-05 are landed and merged — this task copies
  their patterns and uses them as cross-checks for shape.
- `cognee-vector` and `cognee-graph` already depend on `cognee-utils`
  (added by 04-04 / 04-05).
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
intentional differences are the `system` value and any internal
helper-name shifts.

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

### 4.5 Instrument `pg_graph_adapter::query`

At [`crates/graph/src/pg_graph_adapter.rs:317`](../../crates/graph/src/pg_graph_adapter.rs#L317):

```rust
use cognee_utils::redact::redact;
use cognee_utils::tracing_keys::{COGNEE_DB_QUERY, COGNEE_DB_ROW_COUNT};
use tracing::{Span, instrument};

// ...

#[instrument(
    name = "cognee.db.graph.query",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = "postgres",
        cognee.db.query = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
async fn query(
    &self,
    query: &str,
    /* params arg signature — verify at task time */
) -> GraphDBResult<Vec<Vec<serde_json::Value>>> {
    // Truncate-then-redact (locked decision 9, same as Ladybug).
    let truncated = {
        let mut end = query.len().min(500);
        while !query.is_char_boundary(end) {
            end -= 1;
        }
        &query[..end]
    };
    Span::current().record(COGNEE_DB_QUERY, redact(truncated).as_ref());

    // ... existing body ...

    let rows: Vec<Vec<serde_json::Value>> = /* ... */;
    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}
```

The shape mirrors the Ladybug instrumentation
([04-05 §4.2](05-ladybug-instrumentation.md#42-instrument-execute_query))
with `system = "postgres"` and the trait-method signature.

### 4.6 Imports

For [`crates/vector/src/pgvector_adapter.rs`](../../crates/vector/src/pgvector_adapter.rs):

```rust
use cognee_utils::tracing_keys::{
    COGNEE_DB_ROW_COUNT, COGNEE_VECTOR_COLLECTION, COGNEE_VECTOR_RESULT_COUNT,
};
use tracing::{Span, instrument};
```

For [`crates/graph/src/pg_graph_adapter.rs`](../../crates/graph/src/pg_graph_adapter.rs):

```rust
use cognee_utils::redact::redact;
use cognee_utils::tracing_keys::{COGNEE_DB_QUERY, COGNEE_DB_ROW_COUNT};
use tracing::{Span, instrument};
```

The crates already depend on `cognee-utils` after 04-04 and 04-05;
no `Cargo.toml` changes are needed.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. Existing PG-adapter tests still pass (skipped without DB_PROVIDER=postgres).
cargo test -p cognee-vector pgvector
cargo test -p cognee-graph pg_graph

# 3. Clippy.
cargo clippy --all-targets -- -D warnings

# 4. Full check.
scripts/check_all.sh
```

Real attribute coverage tests land in [task 04-10](10-tests.md),
gated on `DB_PROVIDER=postgres` via `cognee_test_utils::pg_test_url()`.

## 6. Files modified

- [`crates/vector/src/pgvector_adapter.rs`](../../crates/vector/src/pgvector_adapter.rs)
  — four `#[instrument]` annotations (search, upsert, delete, delete_collection)
  + `Span::current().record(...)` calls + imports.
- [`crates/graph/src/pg_graph_adapter.rs`](../../crates/graph/src/pg_graph_adapter.rs)
  — one `#[instrument]` annotation on `query` + truncate-then-redact
  + row_count recording + imports.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `pg_graph_adapter::query`'s parameter list has changed (extra args) | Low. | Sub-agent A re-greps. |
| The 22 uninstrumented methods on `pg_graph_adapter` produce traces with no attribution | Acceptable — locked decision 1 says SeaORM ops-level only; per-method adapter wrapping is over-instrumentation. | Document on the PR. |
| `cognee.db.system = "postgres"` vs `"pgvector"` confuses consumers (same physical DB, different adapters) | Real. | Mirror the existing convention: pgvector adapter is its own `system` because it's a different *interface* (vector ops over pgvector extension), distinct from the graph adapter's CRUD-on-rows interface. Document in the PR. |
| Tests in 04-10 depend on a live Postgres at `DB_HOST` | Real — those tests skip gracefully if `DB_PROVIDER != postgres`. | Established `pg_test_url()` pattern handles this. |
| `Self::collection_name(...)` returns `VectorDBResult<String>` (vs the unwrapped `String` in Qdrant) | Already noted in the snippets via `?`. | n/a |

## 8. Out of scope

- Instrumenting all ~22 public `pg_graph_adapter` methods. Locked
  decision 1 keeps SeaORM at ops-level only; the same logic applies
  here.
- `pg_graph_adapter::initialize`, `is_empty`, `delete_graph`. These
  are setup paths; instrumentation cost-to-value is low.
- Mirroring the QdrantAdapter `collection_size` / `list_collections`
  decision: same reasoning.
- Replacing `cognee.db.system = "postgres"` with a more granular
  identifier (e.g. `"postgres-graph"`). Future cleanup if cross-SDK
  parity tests demand it.
