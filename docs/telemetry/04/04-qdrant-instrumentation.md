# Task 04-04 — Instrument `QdrantAdapter` (search / upsert / delete)

**Status**: ⬜ unimplemented
**Owner**: _unassigned_
**Depends on**:
- [Task 04-02](02-tracing-constants-dedupe.md) — needs `cognee_utils::tracing_keys::COGNEE_VECTOR_RESULT_COUNT` and `COGNEE_VECTOR_COLLECTION` consolidated.
**Blocks**:
- [Task 04-10 — Tests](10-tests.md) — Qdrant-side test cases.

**Parent doc**: [04 — DB-Adapter Span Instrumentation](../04-db-adapter-instrumentation.md)
**Locked decisions**: #2 (omit `cognee.db.query` on Qdrant), #5 (INFO level), #8 (no feature gate).

---

## 1. Goal

Add `cognee.db.vector.{search,upsert,delete}` spans to the four
hot-path methods of [`crates/vector/src/qdrant_adapter.rs`](../../crates/vector/src/qdrant_adapter.rs):

| Method | Line | Span name | Required attributes |
|---|---|---|---|
| `search_similar` | [283](../../crates/vector/src/qdrant_adapter.rs#L283) | `cognee.db.vector.search` | `cognee.db.system="qdrant"`, `cognee.vector.collection`, `cognee.vector.result_count` |
| `index_points` | [250](../../crates/vector/src/qdrant_adapter.rs#L250) | `cognee.db.vector.upsert` | `cognee.db.system="qdrant"`, `cognee.vector.collection`, `cognee.db.row_count` |
| `delete_points` | [329](../../crates/vector/src/qdrant_adapter.rs#L329) | `cognee.db.vector.delete` | `cognee.db.system="qdrant"`, `cognee.vector.collection`, `cognee.db.row_count` |
| `delete_collection` | [315](../../crates/vector/src/qdrant_adapter.rs#L315) | `cognee.db.vector.delete_collection` | `cognee.db.system="qdrant"`, `cognee.vector.collection` |

**Decision 2** locks: `cognee.db.query` is **not** set on any of
these spans (matches Python LanceDB).

`collection_size` is intentionally not instrumented — it's a
one-liner used internally for diagnostics; the span overhead would
exceed the value.

## 2. Rationale

`QdrantAdapter` is the default vector backend for cognee-rust and is
on every search retrieval (Chunks/Summaries/Triplet/RagCompletion/
GraphCompletion vector lookups all funnel through `search_similar`)
and every cognify run (six upserts via `index_points`). Without
spans here, `/api/v1/activity/spans` and OTLP consumers see
*zero* vector activity attribution.

Span names follow Python's `cognee.db.<flavour>.<op>` convention
(seen in Neo4j and Ladybug), with the addition of `.upsert` and
`.delete` to disambiguate write paths. Python's LanceDB has no
write-path instrumentation, so this is Rust setting the precedent —
the names match the pattern Python would use if it had them.

## 3. Pre-conditions

- Tasks 04-01 and 04-02 are complete (`cognee_utils::redact::redact`
  and `cognee_utils::tracing_keys::COGNEE_VECTOR_*` are in place).
- A clean `cargo check --all-targets` on `main`.
- `cognee-vector` does **not** currently depend on `cognee-utils`.
  This task adds that edge.

## 4. Step-by-step

### 4.1 Add `cognee-utils` and `tracing` deps to `cognee-vector`

Edit [`crates/vector/Cargo.toml`](../../crates/vector/Cargo.toml).
Confirm `tracing = { workspace = true }` is already present (the
crate has a `tracing::warn` import in scope at
[`qdrant_adapter.rs`](../../crates/vector/src/qdrant_adapter.rs)).

Add:

```toml
[dependencies]
# ... existing ...
cognee-utils = { path = "../utils" }
tracing = { workspace = true }   # confirm — likely already present
```

### 4.2 Instrument `search_similar`

Replace the existing signature at
[`crates/vector/src/qdrant_adapter.rs:283`](../../crates/vector/src/qdrant_adapter.rs#L283)
with:

```rust
use cognee_utils::tracing_keys::{
    COGNEE_DB_SYSTEM, COGNEE_VECTOR_COLLECTION, COGNEE_VECTOR_RESULT_COUNT,
};
use tracing::{Span, instrument};

// ...

#[instrument(
    name = "cognee.db.vector.search",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = "qdrant",
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
    let collection = Self::collection_name(data_type, field_name);
    Span::current().record(COGNEE_VECTOR_COLLECTION, collection.as_str());

    let shard = self.get_or_create_shard(&collection, self.dimension)?;

    let query_vec: VectorInternal = query_vector.to_vec().into();
    let results = shard
        .query(ShardQueryRequest {
            prefetches: vec![],
            query: Some(ScoringQuery::Vector(QueryEnum::Nearest(NamedQuery {
                query: query_vec,
                using: Some("default".to_string()),
            }))),
            filter: None,
            score_threshold: None,
            limit: top_k,
            offset: 0,
            params: None,
            with_vector: WithVector::Bool(false),
            with_payload: WithPayloadInterface::Bool(true),
        })
        .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

    let mapped: Vec<SearchResult> = results.iter().map(Self::from_qdrant_result).collect();
    Span::current().record(COGNEE_VECTOR_RESULT_COUNT, mapped.len() as i64);
    Ok(mapped)
}
```

Notes on the `#[instrument]` shape:

- `level = "info"` — locked decision 5.
- `skip_all` — `data_type`, `field_name`, `top_k`, and `query_vector`
  are not auto-recorded. The collection name is the canonical
  identifier (set explicitly via `record`). `query_vector` is large
  and PII-relevant; never record it.
- `cognee.db.system = "qdrant"` is set in the macro because it's a
  static literal known at entry.
- `cognee.vector.collection` and `cognee.vector.result_count` are
  declared as `tracing::field::Empty` so the span has slots for them;
  `record(...)` later fills the slots. Forgetting `Empty` would
  silently drop the value — this is the most common footgun in
  `tracing` instrumentation.
- `err` — automatically records `Err` returns as `error.message` and
  marks the span as failed. Replaces Python's
  `set_status(ERROR) + record_exception`.

### 4.3 Instrument `index_points`

```rust
#[instrument(
    name = "cognee.db.vector.upsert",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = "qdrant",
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

    let collection = Self::collection_name(data_type, field_name);
    Span::current().record(COGNEE_VECTOR_COLLECTION, collection.as_str());

    let expected_dim = points[0].vector.len();
    for point in points {
        if point.vector.len() != expected_dim {
            return Err(VectorDBError::DimensionMismatch {
                expected: expected_dim,
                actual: point.vector.len(),
            });
        }
    }

    let shard = self.get_or_create_shard(&collection, expected_dim)?;
    let batch = Self::points_to_batch(points);

    shard
        .update(PointOperation(UpsertPoints(PointsBatch(batch))))
        .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

    Span::current().record(COGNEE_DB_ROW_COUNT, points.len() as i64);
    Ok(())
}
```

Use `cognee_utils::tracing_keys::COGNEE_DB_ROW_COUNT` for the upsert
count. Vector "rows" are points here; we use `cognee.db.row_count`
for write paths and `cognee.vector.result_count` for search paths,
matching Python's pattern (LanceDB sets `vector.result_count` on
search; no row count on writes because Python doesn't instrument
LanceDB writes).

**Edge case**: `points.is_empty()` returns early *before* the span
records `row_count`. The span will still close (the `instrument`
attribute wraps the whole function), recording
`cognee.vector.collection = Empty` and `cognee.db.row_count = Empty`.
This is acceptable — the early return represents a no-op, and a
missing slot is cheaper than wiring a separate code path.

### 4.4 Instrument `delete_points`

```rust
#[instrument(
    name = "cognee.db.vector.delete",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = "qdrant",
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

    let collection = Self::collection_name(data_type, field_name);
    Span::current().record(COGNEE_VECTOR_COLLECTION, collection.as_str());

    let shard = self.get_or_create_shard(&collection, self.dimension)?;

    let ids: Vec<ExtendedPointId> = point_ids
        .iter()
        .map(|id| ExtendedPointId::Uuid(*id))
        .collect();

    shard
        .update(PointOperation(DeletePoints { ids }))
        .map_err(|e| VectorDBError::StorageError(e.to_string()))?;

    Span::current().record(COGNEE_DB_ROW_COUNT, point_ids.len() as i64);
    Ok(())
}
```

### 4.5 Instrument `delete_collection`

```rust
#[instrument(
    name = "cognee.db.vector.delete_collection",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = "qdrant",
        cognee.vector.collection = tracing::field::Empty,
    ),
    err,
)]
async fn delete_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<()> {
    let collection = Self::collection_name(data_type, field_name);
    Span::current().record(COGNEE_VECTOR_COLLECTION, collection.as_str());

    let mut shards = self.shards.write().unwrap(); // lock poison is unrecoverable
    shards.remove(&collection);

    let shard_path = self.data_dir.join(&collection);
    if shard_path.exists() {
        std::fs::remove_dir_all(&shard_path)?;
    }

    Ok(())
}
```

`delete_collection` does not have a row count concept (it removes the
whole collection), so we omit `COGNEE_DB_ROW_COUNT`.

### 4.6 Imports

Add at the top of [`crates/vector/src/qdrant_adapter.rs`](../../crates/vector/src/qdrant_adapter.rs):

```rust
use cognee_utils::tracing_keys::{
    COGNEE_DB_ROW_COUNT, COGNEE_VECTOR_COLLECTION, COGNEE_VECTOR_RESULT_COUNT,
};
use tracing::{Span, instrument};
```

The existing `tracing::warn` import (if any) becomes redundant —
collapse the `use tracing::*;` lines into a single grouped import.

`cognee.db.system` is **not** imported as a constant — the
`fields(cognee.db.system = "qdrant")` shorthand uses an inline
literal because the field name is a *path*, not a Rust expression
evaluated to a string.

> ⚠️ Important: `tracing::instrument`'s `fields(name = value)` syntax
> uses the literal **identifier path** `cognee.db.system` as the
> field name. You cannot substitute the constant
> `COGNEE_DB_SYSTEM` here. The constants are still useful for
> `Span::current().record(...)` calls because that takes a `&str`.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. cognee-vector compiles in isolation.
cargo check -p cognee-vector

# 3. Existing vector tests still pass.
cargo test -p cognee-vector

# 4. Clippy.
cargo clippy --all-targets -- -D warnings

# 5. Full check.
scripts/check_all.sh
```

The structural assertions on the new spans land in
[task 04-10](10-tests.md), which adds
`crates/vector/tests/qdrant_span_instrumentation.rs` using the
`SpanCapture` helper from task 04-03. This task only ensures things
compile and existing behaviour is unchanged.

## 6. Files modified

- [`crates/vector/Cargo.toml`](../../crates/vector/Cargo.toml) — add
  `cognee-utils = { path = "../utils" }`. Confirm `tracing` is
  present.
- [`crates/vector/src/qdrant_adapter.rs`](../../crates/vector/src/qdrant_adapter.rs)
  — add four `#[instrument]` annotations (search, upsert, delete,
  delete_collection) and the matching `Span::current().record(...)`
  calls; add imports.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `#[instrument]` attribute syntax `cognee.db.system = "qdrant"` parses as field-path; trailing dots in field names are accepted by `tracing` and rendered as `cognee.db.system="qdrant"` | None — verified against `cognee-search/src/observability.rs` and Python parity. | n/a |
| Empty-points / empty-ids early-return paths leave the span with placeholder `Empty` slots | Acceptable — see 4.3. | Document in test comments. |
| `cognee-vector` build slows from regex pull-in (`cognee-utils` now depends on `regex`) | Negligible. `regex` is already in the workspace tree via http-server etc. | n/a |
| The `qdrant_adapter` file shifts line numbers between writing this doc and execution | Likely. Sub-agent A re-grep before approving. | Doc references `name = ...`/method names, not line numbers, in the steps. |

## 8. Out of scope

- Instrumenting `collection_size`, `list_collections`, `has_collection`.
  These are diagnostic helpers; instrumentation cost-to-value is low.
- Adding `cognee.db.query = "vector_search(top_k=N)"` synthetic
  values. Locked decision 2 says no.
- Routing search-time `top_k` through a new attribute. Out of
  Python parity; revisit only if cross-SDK tests demand it.
- Touching `MockVectorDB`. The mock is exclusively for tests; no
  span instrumentation is needed there.
