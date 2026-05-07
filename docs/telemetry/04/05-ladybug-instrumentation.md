# Task 04-05 — Instrument `LadybugAdapter::execute_query`

**Status**: ⬜ unimplemented
**Owner**: _unassigned_
**Depends on**:
- [Task 04-01](01-redact-relocate.md) — needs `cognee_utils::redact::redact`.
- [Task 04-02](02-tracing-constants-dedupe.md) — needs `cognee_utils::tracing_keys::COGNEE_DB_SYSTEM`/`COGNEE_DB_QUERY`/`COGNEE_DB_ROW_COUNT`.
**Blocks**:
- [Task 04-10 — Tests](10-tests.md) — Ladybug-side test cases.

**Parent doc**: [04 — DB-Adapter Span Instrumentation](../04-db-adapter-instrumentation.md)
**Locked decisions**: #5 (INFO level), #8 (no feature gate), #9 (truncate-then-redact).

---

## 1. Goal

Wrap [`LadybugAdapter::execute_query`](../../crates/graph/src/ladybug.rs#L156)
— the single fan-in helper called by every public `LadybugAdapter`
method (~24 callers including `has_node`, `add_node_raw`,
`add_nodes_raw`, `delete_node`, `delete_nodes`, `get_node`,
`get_nodes`, `has_edge`, `has_edges`, `add_edge`, `add_edges`,
`get_edges`, `get_neighbors`, `get_connections`, `get_graph_data`,
`get_graph_metrics`, `get_filtered_graph_data`, `get_nodeset_subgraph`,
`update_node_property`, `update_edge_property`, `get_node_feedback_weights`,
`set_node_feedback_weights`, `get_edge_feedback_weights`,
`set_edge_feedback_weights`) — with a `cognee.db.graph.query` span.

By instrumenting the helper, all 24 public methods get coverage for
free; we don't need to annotate each one.

Required attributes:

| Attribute | Value |
|---|---|
| `cognee.db.system` | `"ladybug"` |
| `cognee.db.query` | `redact(query[..query.len().min(500)])` (truncate-then-redact, locked decision 9) |
| `cognee.db.row_count` | `rows.len() as i64`, set after the query returns |

Errors are recorded automatically via `#[instrument(... err)]`.

The public `LadybugAdapter::query` method (the trait method)
delegates to `execute_query` after a `params.is_some()` short-circuit
— it is covered transitively.

## 2. Rationale

The Ladybug adapter is the default graph backend for cognee-rust and
mirrors Python's Ladybug instrumentation
(`cognee/infrastructure/databases/graph/ladybug/adapter.py:269`). The
Python adapter wraps `_run_query` (its equivalent of `execute_query`)
in a `with new_span("cognee.db.graph.query"):` block; we do the same
via `#[tracing::instrument]`.

Wrapping at the helper level — not at each of the 24 public
methods — is the load-bearing decision. It:

- Cuts diff size from ~24 method annotations to **1**.
- Keeps the call chain traceable: the public method (e.g.
  `has_node`) shows up as a *caller* of `cognee.db.graph.query` in
  the span tree, so consumers can still see what high-level
  operation triggered the query.
- Matches Python, which also wraps the inner helper rather than
  every public method.

## 3. Pre-conditions

- Tasks 04-01 (redact relocation) and 04-02 (constants dedupe) are
  complete.
- A clean `cargo check --all-targets` on `main`.
- `cognee-graph` does **not** currently depend on `cognee-utils`.
  This task adds that edge.

## 4. Step-by-step

### 4.1 Add `cognee-utils` and `tracing` deps to `cognee-graph`

Edit [`crates/graph/Cargo.toml`](../../crates/graph/Cargo.toml). Add:

```toml
[dependencies]
# ... existing ...
cognee-utils = { path = "../utils" }
tracing = { workspace = true }   # confirm — likely already present
```

### 4.2 Instrument `execute_query`

Replace the function body at
[`crates/graph/src/ladybug.rs:156`](../../crates/graph/src/ladybug.rs#L156)
with:

```rust
use cognee_utils::redact::redact;
use cognee_utils::tracing_keys::{COGNEE_DB_QUERY, COGNEE_DB_ROW_COUNT};
use tracing::{Span, instrument};

// ...

/// Execute a query and convert results to JSON values.
///
/// Helper method that executes a Cypher query and converts the QueryResult
/// to a Vec of Vec of JSON values for easier processing.
#[instrument(
    name = "cognee.db.graph.query",
    level = "info",
    skip_all,
    fields(
        cognee.db.system = "ladybug",
        cognee.db.query = tracing::field::Empty,
        cognee.db.row_count = tracing::field::Empty,
    ),
    err,
)]
fn execute_query(&self, query: &str) -> GraphDBResult<Vec<Vec<serde_json::Value>>> {
    // Truncate-then-redact (locked decision 9). The 500-char
    // truncation must come BEFORE redact() so a redacted form
    // longer than 500 chars cannot be re-truncated and split the
    // literal `***REDACTED***` marker.
    let truncated = &query[..query.len().min(500)];
    Span::current().record(COGNEE_DB_QUERY, redact(truncated).as_ref());

    let conn = Connection::new(&self.db).map_err(|e| {
        GraphDBError::ConnectionError(format!("Failed to create connection: {}", e))
    })?;

    let result = conn
        .query(query)
        .map_err(|e| GraphDBError::QueryError(format!("Query failed: {}", e)))?;

    let rows: Vec<Vec<serde_json::Value>> = result
        .map(|row| row.into_iter().map(Self::lbug_value_to_json).collect())
        .collect();

    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}
```

Notes:

- `skip_all` keeps `query` (potentially long) out of the span's
  default field list. We record a redacted, truncated version
  manually.
- `truncated` is `&str` slicing on a UTF-8 boundary issue: if the
  500th byte falls mid-codepoint, the indexing panics. **Use the
  char-aware variant** below instead:

  ```rust
  let truncated = if query.len() > 500 {
      // Walk to the last char boundary at-or-before byte 500.
      let mut end = 500;
      while !query.is_char_boundary(end) {
          end -= 1;
      }
      &query[..end]
  } else {
      query
  };
  ```

  This matches Python's `query[:500]` semantics (which slices on
  unicode codepoints, not bytes) closely enough for our purposes
  (Cypher is overwhelmingly ASCII; user-provided strings inside
  Cypher are quoted and therefore short). The grace path costs at
  most three iterations per query.

- `redact(truncated).as_ref()` produces a `&str` (via `Cow`'s
  `as_ref`), which routes through `tracing`'s `record_str` visitor
  cleanly. Do **not** pass `&redact(...)` (a `&Cow<'_, str>`) — that
  goes through `record_debug` and adds the `"…"` quoting noise the
  test helper documents in [04-03](03-span-capture-test-helper.md#7-risks).

### 4.3 Imports

Add at the top of [`crates/graph/src/ladybug.rs`](../../crates/graph/src/ladybug.rs):

```rust
use cognee_utils::redact::redact;
use cognee_utils::tracing_keys::{COGNEE_DB_QUERY, COGNEE_DB_ROW_COUNT};
use tracing::{Span, instrument};
```

### 4.4 Verify the public `query` method is transitively covered

The public `LadybugAdapter::query` (trait method) at
[`crates/graph/src/ladybug.rs:453`](../../crates/graph/src/ladybug.rs#L453)
delegates to `execute_query` after a `params.is_some()` short-circuit.
After this task, calling `query()` produces a `cognee.db.graph.query`
span automatically — no separate annotation.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. cognee-graph compiles in isolation.
cargo check -p cognee-graph

# 3. Existing graph tests still pass.
cargo test -p cognee-graph

# 4. Clippy.
cargo clippy --all-targets -- -D warnings

# 5. Full check.
scripts/check_all.sh
```

Real coverage of the span attributes lands in
[task 04-10](10-tests.md), where
`crates/graph/tests/ladybug_span_instrumentation.rs` will assert via
the `SpanCapture` helper that `cognee.db.system="ladybug"`,
`cognee.db.query` is redacted/truncated, and `cognee.db.row_count`
matches `len()` of the returned rows.

## 6. Files modified

- [`crates/graph/Cargo.toml`](../../crates/graph/Cargo.toml) — add
  `cognee-utils = { path = "../utils" }`. Confirm `tracing` is
  present.
- [`crates/graph/src/ladybug.rs`](../../crates/graph/src/ladybug.rs)
  — add `#[instrument]` to `execute_query`; add the truncate-then-redact
  step on `cognee.db.query`; record `cognee.db.row_count` after the
  query; add imports.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Naive `&query[..500]` panics on a non-ASCII char boundary | Real but rare (Cypher is mostly ASCII). | Use the `is_char_boundary` walk shown in 4.2. |
| `redact()` allocates on every query — perceived perf regression | Negligible — regex passes over ≤500 chars are < 10 µs. The Ladybug query itself takes orders of magnitude longer. | If profiling later shows hot-path overhead, gate behind `if !tracing::Span::current().is_disabled()` (the `tracing` analogue of Python's `is_recording()`). |
| Span fires at the wrong granularity for batch operations like `add_nodes_raw` (one span per batched query — currently one batched UNWIND statement per call) | Acceptable — `execute_query` is invoked once per logical operation; batched DB ops produce one span. Matches Python. | Document in test comments. |
| The `Connection::new` failure path leaves `cognee.db.row_count = Empty` | Acceptable — the `err` annotation captures the error message; row_count being unset on the error path is consistent with Python's behaviour (Python only sets `row_count` after `await result.data()`). | n/a |
| Manual `Span::current().record` calls in a `skip_all` instrument block silently no-op when the field was not declared in `fields(...)` | Real footgun — the field must be declared as `tracing::field::Empty` up front. | Verified in 4.2; sub-agent C should grep for any `record(...)` lines that reference fields not in the macro. |

## 8. Out of scope

- Instrumenting `LadybugAdapter::initialize`, `is_empty`, `delete_graph`.
  These bypass `execute_query` (or do trivial setup); their cost-to-value
  is low. Revisit if cross-SDK parity tests demand it.
- Mirroring Python's `set_status` + `record_exception` pair manually.
  `#[instrument(... err)]` already records the error message and marks
  the span failed — equivalent for OTLP consumers.
- Replacing `LadybugAdapter` with a different graph backend. Out of
  scope for telemetry.
- Touching `pg_graph_adapter`. That is [task 04-08](08-pg-adapters.md).
- Touching the `MockGraphDB`. The mock is for tests; no spans needed.
