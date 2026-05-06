# Gap 04 — DB-Adapter Span Instrumentation (vector / graph / relational / LLM)

## Overview

Python `cognee` wraps every vector and graph adapter query in an OpenTelemetry
span carrying the canonical attributes `cognee.db.system`, `cognee.db.query`
(redacted), and `cognee.db.row_count`. LLM adapters add `cognee.llm.model` and
`cognee.llm.provider` to the surrounding span. The Rust port has the **constant
names defined** in [`crates/utils/src/tracing_keys.rs`](../../crates/utils/src/tracing_keys.rs)
(and a parallel set in [`crates/search/src/observability.rs`](../../crates/search/src/observability.rs))
but **zero call sites** outside the http-server's redaction utility. As a
result, traces emitted today have no rows, no row counts, no DB system labels
and no LLM model/provider attribution — Python parity tests will fail and OTLP
consumers see only span names.

This gap is closed by

1. instrumenting the four adapters (Qdrant, Ladybug, SeaORM, OpenAI) at the
   right method boundaries,
2. promoting the existing `redact()` helper from `crates/http-server/` into a
   shared crate (`cognee-utils::redact`) so adapters can scrub query text
   before recording it on a span,
3. confirming `cognee.llm.model` / `cognee.llm.provider` are recorded as
   **fields**, not just present in the span name.

---

## Python instrumentation pattern

`new_span` is a thin context manager around the OTEL tracer. When tracing is
disabled it yields a `_NullSpan` whose attribute setters are no-ops, which lets
adapters always wrap their hot path without paying for tracing in default
configs.

```python
# cognee/modules/observability/__init__.py
@contextmanager
def new_span(name: str):
    if is_tracing_enabled():
        tracer = get_tracer()
        if tracer is not None:
            with tracer.start_as_current_span(name) as span:
                yield span
                return
    yield _NullSpan()
```

The canonical adapter pattern (Neo4j, Ladybug, LanceDB) is:

```python
# cognee/infrastructure/databases/graph/neo4j_driver/adapter.py:147-161
with new_span("cognee.db.graph.query") as otel_span:
    otel_span.set_attribute(COGNEE_DB_SYSTEM, "neo4j")
    otel_span.set_attribute(COGNEE_DB_QUERY, redact_secrets(query[:500]))

    try:
        async with self.get_session() as session:
            result = await session.run(query, parameters=params)
            data = await result.data()
            otel_span.set_attribute(COGNEE_DB_ROW_COUNT, len(data))
            return data
    except Neo4jError as error:
        otel_span.set_status(StatusCode.ERROR, str(error))
        otel_span.record_exception(error)
        logger.error("Neo4j query error: %s", error, exc_info=True)
        raise error
```

Key invariants:

- Span is **named once** per logical operation: `cognee.db.graph.query`,
  `cognee.db.vector.search`. This is the span *name*, not an attribute value.
- `cognee.db.system` carries the backend identifier
  (`neo4j` / `ladybug` / `lancedb` / `qdrant` / `sqlite` / …).
- `cognee.db.query` is **truncated to 500 chars** before being passed through
  `redact_secrets`.
- `cognee.db.row_count` is set **after** the query completes; on every return
  branch (including the early-return `limit <= 0` guard in LanceDB) the count
  is still recorded.
- Errors are reported via `set_status(StatusCode.ERROR, …)` *and*
  `record_exception(e)` before re-raising.

### `redact_secrets` regex set

```python
# cognee/modules/observability/tracing.py:63-78
_SECRET_PATTERNS = [
    re.compile(r"(sk-[A-Za-z0-9]{20,})"),
    re.compile(r"(api[_-]?key\s*[=:]\s*)['\"]?[A-Za-z0-9\-_]{16,}['\"]?", re.IGNORECASE),
    re.compile(r"(bearer\s+)[A-Za-z0-9\-_\.]{20,}", re.IGNORECASE),
    re.compile(r"(password\s*[=:]\s*)['\"]?[^\s'\"]{8,}['\"]?", re.IGNORECASE),
]


def redact_secrets(text: str) -> str:
    if not text:
        return text
    result = text
    for pattern in _SECRET_PATTERNS:
        result = pattern.sub(lambda m: m.group(0)[:6] + "***REDACTED***", result)
    return result
```

Behaviour: the first 6 characters of the **match** survive, the rest is
replaced with `***REDACTED***`. A *non-matching* string is returned unchanged.

### LLM adapter pattern

LLM spans are produced by the surrounding `@observe(...)` litellm decorator;
the model/provider attributes are added inside `acreate_structured_output`
**after** the call returns:

```python
# cognee/infrastructure/llm/structured_output_framework/litellm_instructor/llm/generic_llm_api/adapter.py:38-55
def _enrich_llm_span(model: str, name: str) -> None:
    if not is_tracing_enabled():
        return
    try:
        from opentelemetry import trace as otel_trace
        from cognee.modules.observability.tracing import (
            COGNEE_LLM_MODEL, COGNEE_LLM_PROVIDER,
        )
        current_span = otel_trace.get_current_span()
        if current_span and current_span.is_recording():
            current_span.set_attribute(COGNEE_LLM_MODEL, model)
            current_span.set_attribute(COGNEE_LLM_PROVIDER, name)
    except Exception:
        pass

# called at adapter.py:174 immediately after `await client.chat.completions.create(...)`
```

So Python attaches `cognee.llm.model` / `cognee.llm.provider` to *the current
span* — typically the litellm/instructor wrapper — rather than starting a new
one. Rust's analogue is `tracing::Span::current().record(...)` from inside
`call_api()`.

---

## Per-adapter status

### Python adapters (reference)

| Adapter | File | Span name | Attributes set | Row-count source | Redaction | Error path |
|---|---|---|---|---|---|---|
| LanceDB | `infrastructure/databases/vector/lancedb/LanceDBAdapter.py:612` | `cognee.db.vector.search` | `cognee.db.system="lancedb"`, `cognee.vector.collection`, `cognee.vector.result_count` | `len(results)` (and `0` on the `limit <= 0` and empty-results branches) | not applied (no SQL string) | implicit (raises propagate) |
| Neo4j | `infrastructure/databases/graph/neo4j_driver/adapter.py:147` | `cognee.db.graph.query` | `cognee.db.system="neo4j"`, `cognee.db.query` (truncated 500c, redacted), `cognee.db.row_count` | `len(data)` after `await result.data()` | yes — `redact_secrets(query[:500])` | `set_status(ERROR) + record_exception(error)` |
| Ladybug | `infrastructure/databases/graph/ladybug/adapter.py:269` | `cognee.db.graph.query` | `cognee.db.system="ladybug"`, `cognee.db.query` (truncated 500c, redacted), `cognee.db.row_count` | `len(result)` after blocking executor returns | yes — `redact_secrets(query[:500])` | `set_status(ERROR) + record_exception(e)` |
| Kuzu | `infrastructure/databases/graph/kuzu/` | (none — Kuzu adapter not yet instrumented in Python either) | — | — | — | — |
| QDrant Python | `infrastructure/databases/vector/qdrant/QDrantAdapter.py` | (none — Python QDrant adapter has **no** `new_span` block today) | — | — | — | — |
| Relational | `infrastructure/databases/relational/sqlalchemy/` | (none — relational layer is uninstrumented in Python) | — | — | — | — |
| LLM (generic, openai, anthropic, …) | `infrastructure/llm/structured_output_framework/litellm_instructor/llm/generic_llm_api/adapter.py:174` | (no new span — uses the surrounding observe span) | `cognee.llm.model`, `cognee.llm.provider` | n/a | n/a | — |

> **Implication:** Python only instruments the *graph* adapter that is the
> active default (Neo4j or Ladybug) and the LanceDB *vector* adapter. Qdrant
> and the relational layer have **no spans** there either, so the cognee-rust
> task is to (a) cover Qdrant and SeaORM proactively and (b) match Ladybug
> fully, not to chase Python on the un-instrumented backends.

### Rust adapters (current)

| Crate / file | Method(s) | Instrumentation today | Gap |
|---|---|---|---|
| [`crates/vector/src/qdrant_adapter.rs:250`](../../crates/vector/src/qdrant_adapter.rs#L250) | `index_points`, `search_similar:283`, `delete_points:329`, `collection_size:354`, `delete_collection:315` | none — only a `tracing::warn` import in scope | needs `cognee.db.vector.search` span on `search_similar`; `cognee.db.vector.upsert` on `index_points`; `cognee.db.vector.delete` on `delete_points` and `delete_collection`. Set `cognee.db.system="qdrant"`, `cognee.vector.collection`, `cognee.vector.result_count` (search) / `cognee.db.row_count` (upsert/delete). |
| [`crates/graph/src/ladybug.rs:156`](../../crates/graph/src/ladybug.rs#L156) | `execute_query` (private helper, called from 20+ public methods) and the public `query:453` | none | wrap **`execute_query`** with `cognee.db.graph.query` span; record `cognee.db.system="ladybug"`, redacted truncated query text, row count. Wrapping the helper covers all callers transitively. |
| [`crates/graph/src/pg_graph_adapter.rs`](../../crates/graph/src/pg_graph_adapter.rs) | analogous `query` / `execute_query` paths | none (only `tracing::debug` import) | analogous instrumentation with `cognee.db.system="postgres"` for parity / future `pg_graph` users. Lower priority — Python has no postgres-graph instrumentation. |
| [`crates/vector/src/pgvector_adapter.rs`](../../crates/vector/src/pgvector_adapter.rs) | `search_similar`, `index_points`, … | none (only `tracing::debug` import) | analogous instrumentation with `cognee.db.system="pgvector"`. Lower priority. |
| [`crates/database/src/connection.rs`](../../crates/database/src/connection.rs) | `connect`, `initialize` | none | low signal — SeaORM emits its own `sqlx`/`sea-orm` logs already. **Recommended:** instrument coarser units in `crates/database/src/ops/*.rs` instead of every SeaORM call (see *Open questions*). |
| [`crates/llm/src/adapters/openai.rs:138`](../../crates/llm/src/adapters/openai.rs#L138) | `call_api` — already `#[instrument(name = "llm.api_call", skip(self, request_body), fields(url = tracing::field::Empty))]` | span is created but **`cognee.llm.model` and `cognee.llm.provider` are NOT recorded** as fields | add `model` to the `fields(...)` declaration and `Span::current().record("cognee.llm.model", self.model.as_str())` after the borrow / inside the function. Same for provider (`"openai"`). Re-use the constants from `cognee_utils::tracing_keys`. |
| [`crates/llm/src/adapters/openai.rs:729`](../../crates/llm/src/adapters/openai.rs#L729) | `call_transcription_api` | same gap as `call_api` | record `cognee.llm.model = self.transcription_model`, `cognee.llm.provider = "openai"`. |
| [`crates/llm/src/adapters/litert.rs`](../../crates/llm/src/adapters/) (Android) | `complete`, `transcribe_audio` | unknown — not currently inspected by this task | needs the same `cognee.llm.model` / `cognee.llm.provider="litert"` fields. |

### Existing redaction utility

There is already a Rust port of `redact_secrets` at
[`crates/http-server/src/observability/redaction.rs:43`](../../crates/http-server/src/observability/redaction.rs#L43)
with the four-pattern regex set, the `Cow<'_, str>` allocation-free fast path,
and a JSON-walking `redact_attributes`. The patterns and the `***REDACTED***`
suffix already match Python. **It is in the wrong crate** — adapters in
`cognee-vector` and `cognee-graph` cannot depend on `cognee-http-server`. The
function should be relocated to a shared crate before adapter instrumentation
lands.

---

## Detailed gap analysis (call paths)

The following call paths produce **zero span attributes** today although they
are the canonical observability points used by Python:

1. **`QdrantAdapter::search_similar`** — every retrieval path
   (Chunks/Summaries/Triplet/RagCompletion/GraphCompletion vector lookups)
   funnels through here. No `cognee.db.vector.search` span.
2. **`QdrantAdapter::index_points`** — every cognify run upserts six
   collections (DocumentChunk:text, Entity:name, EntityType:name,
   TextSummary:text, EdgeType:relationship_name, Triplet:text). No
   `cognee.db.row_count`.
3. **`QdrantAdapter::delete_points` / `delete_collection`** — used by
   `cognee-delete`. No span emitted on cascade delete.
4. **`LadybugAdapter::execute_query`** — fan-out point for `has_node`,
   `add_node_raw`, `add_nodes_raw`, `delete_node`, `delete_nodes`, `get_node`,
   `get_nodes`, `has_edge`, `has_edges`, `add_edge`, `add_edges`, `get_edges`,
   `get_neighbors`, `get_connections`, `get_graph_data`, `get_graph_metrics`,
   `get_filtered_graph_data`, `get_nodeset_subgraph`, `update_node_property`,
   `update_edge_property`, `get_node_feedback_weights`,
   `set_node_feedback_weights`, `get_edge_feedback_weights`,
   `set_edge_feedback_weights`. **A single instrumented helper covers all of
   these.**
5. **`LadybugAdapter::query`** (the trait method) currently delegates to
   `execute_query` after a `params.is_some()` short-circuit. Once
   `execute_query` is instrumented, `query` is covered for free.
6. **`OpenAIAdapter::call_api` / `call_transcription_api`** — span exists but
   does not carry the LLM model/provider, so traces cannot answer "which model
   answered this request?" without log scraping.
7. **`SeaOrmDatabase` ops (`crates/database/src/ops/*.rs`)** — `data`,
   `dataset`, `permissions`, `pipelines`, `sync` operations are all
   uninstrumented. Coarser-grained spans here (per ORM op) match what Python
   would do if its relational layer were instrumented.

---

## Proposed design

### Idiomatic Rust shape

Use `#[tracing::instrument(...)]` where the function body is short and the
attributes are known up front, and fall back to `info_span!(...).in_scope(...)`
or `Span::current().record(...)` where the attribute value is computed
mid-function.

For the Ladybug helper:

```rust
// crates/graph/src/ladybug.rs
use cognee_utils::redact::redact;
use cognee_utils::tracing_keys::{COGNEE_DB_QUERY, COGNEE_DB_ROW_COUNT, COGNEE_DB_SYSTEM};
use tracing::{instrument, Span};

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
    let truncated = if query.len() > 500 { &query[..500] } else { query };
    Span::current().record(COGNEE_DB_QUERY, redact(truncated).as_ref());

    let conn = Connection::new(&self.db).map_err(/* ... */)?;
    let result = conn.query(query).map_err(/* ... */)?;
    let rows: Vec<Vec<serde_json::Value>> = result
        .map(|row| row.into_iter().map(Self::lbug_value_to_json).collect())
        .collect();

    Span::current().record(COGNEE_DB_ROW_COUNT, rows.len() as i64);
    Ok(rows)
}
```

Notes on the `instrument` choice:

- `name = "cognee.db.graph.query"` — the span **name**, matching Python.
- `skip_all` — we don't want `query` or `&self` auto-recorded; `query` carries
  secrets, `self` is large.
- `fields(... = tracing::field::Empty)` — declared up front so `record(...)`
  later finds the slot. If we omitted them, `record` would silently drop the
  value (this is the most common footgun in `tracing` instrumentation).
- `err` — automatically records `Err` returns as `error.message` and sets the
  span status. This replaces Python's `set_status(ERROR) + record_exception`.
  The default level is `error`.

For the Qdrant search path the body is short enough to use the same shape:

```rust
#[instrument(
    name = "cognee.db.vector.search",
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
    /* ... */
    let mapped: Vec<SearchResult> = results.iter().map(Self::from_qdrant_result).collect();
    Span::current().record(COGNEE_VECTOR_RESULT_COUNT, mapped.len() as i64);
    Ok(mapped)
}
```

For LLM model/provider on `OpenAIAdapter::call_api`, the field is already a
struct field and known at entry, so we can record it directly in the
declaration:

```rust
#[instrument(
    name = "llm.api_call",
    skip(self, request_body),
    fields(
        url = tracing::field::Empty,
        cognee.llm.model = self.model.as_str(),
        cognee.llm.provider = "openai",
    ),
)]
async fn call_api(&self, request_body: Value) -> LlmResult<OpenAIResponse> { /* ... */ }
```

(`fields()` evaluates the expressions in scope of `&self`, so this compiles.)

### Where the redaction helper lives

Move `pub fn redact(&str) -> Cow<'_, str>` from
`crates/http-server/src/observability/redaction.rs` into a new module
`crates/utils/src/redact.rs` (or `crates/utils/src/redaction.rs`) and re-export
from `cognee_utils`. The `regex` crate is already a workspace dep through the
http-server but should be added as a direct dep on `cognee-utils`. The
existing `redact_attributes` JSON helper can stay in `http-server` (it's
specific to the observability HTTP API) and re-implement on top of the
relocated `redact()`.

### Why `tracing` field values cannot be lazy

`tracing` evaluates field expressions eagerly when the span is created (or when
`record(...)` is called); there is no lazy / closure variant for field values.
So callers **must** pre-redact before passing the field value. Concretely:

```rust
// WRONG — secrets reach the subscriber
Span::current().record(COGNEE_DB_QUERY, query);
// RIGHT
Span::current().record(COGNEE_DB_QUERY, redact(&query[..query.len().min(500)]).as_ref());
```

The 500-char truncation **must come before** `redact()` to match Python's
`redact_secrets(query[:500])` ordering — otherwise a redacted form longer than
500 chars would be re-truncated and split a `***REDACTED***` marker.

### Constant deduplication

`crates/search/src/observability.rs` and `crates/utils/src/tracing_keys.rs`
declare overlapping constants with the same string values. The cleanup is to
make `crates/search/src/observability.rs` re-export from `cognee-utils` rather
than redeclaring. This is a separate cleanup PR but is worth doing in the same
window so adapter call sites only ever import from one place.

---

## Action items

In dependency order:

1. **Move `redact()` to `cognee-utils`.**
   - New file `crates/utils/src/redact.rs` with `pub fn redact(&str) -> Cow<'_, str>`
     and the four-pattern `OnceLock<Vec<Regex>>`.
   - Add `regex = "1"` to `crates/utils/Cargo.toml`.
   - Re-export from `crates/utils/src/lib.rs`.
   - Update `crates/http-server/src/observability/redaction.rs` to call
     `cognee_utils::redact::redact` and keep only the JSON walker.

2. **Deduplicate constants.**
   - Make `crates/search/src/observability.rs` re-export
     `cognee_utils::tracing_keys::*` instead of redeclaring.

3. **Instrument `QdrantAdapter`** ([`crates/vector/src/qdrant_adapter.rs`](../../crates/vector/src/qdrant_adapter.rs)).
   - `search_similar:283` → span name `cognee.db.vector.search`,
     attrs `cognee.db.system="qdrant"`, `cognee.vector.collection`,
     `cognee.vector.result_count`.
   - `index_points:250` → span name `cognee.db.vector.upsert`,
     attrs `cognee.db.system="qdrant"`, `cognee.vector.collection`,
     `cognee.db.row_count = points.len()`.
   - `delete_points:329` → span name `cognee.db.vector.delete`,
     attrs `cognee.db.system="qdrant"`, `cognee.vector.collection`,
     `cognee.db.row_count = point_ids.len()`.
   - `delete_collection:315` and `collection_size:354` → optional but cheap;
     same naming convention.

4. **Instrument `LadybugAdapter::execute_query`** ([`crates/graph/src/ladybug.rs:156`](../../crates/graph/src/ladybug.rs#L156)).
   - Span name `cognee.db.graph.query`,
     attrs `cognee.db.system="ladybug"`, `cognee.db.query` (truncated 500 +
     redacted), `cognee.db.row_count = rows.len()`.
   - All public methods that go through `execute_query` are instrumented
     transitively.

5. **Add LLM model/provider fields** to
   [`crates/llm/src/adapters/openai.rs:138`](../../crates/llm/src/adapters/openai.rs#L138)
   and `:729`.
   - `fields(cognee.llm.model = self.model.as_str(), cognee.llm.provider = "openai")`
     on `call_api`.
   - Same on `call_transcription_api` with `self.transcription_model`.
   - Repeat in `crates/llm/src/adapters/litert.rs` if present, with provider
     label `"litert"`.

6. **Optional / deferred:** instrument `pgvector_adapter`, `pg_graph_adapter`,
   and `crates/database/src/ops/*.rs`. Python has no spans on these paths so
   parity tests won't drive them; they are still useful for OTLP consumers.

7. **Tests** — see *Testing strategy* below. Add at minimum one happy-path test
   per adapter asserting span name + the three required attributes appear with
   correct values.

8. **Update gap-analysis.md** (separate, owner: telemetry tracker) to mark this
   gap as closed once 3-5 land. **This task does not edit gap-analysis.md.**

---

## Redaction utility — integration notes

- **Patterns** (verbatim, single-pass `replace_all` per pattern):

  1. `r"sk-[A-Za-z0-9]{20,}"` — OpenAI-style keys.
  2. `r#"(?i)(api[_-]?key\s*[=:]\s*)['"]?[A-Za-z0-9\-_]{16,}['"]?"#`
  3. `r"(?i)(bearer\s+)[A-Za-z0-9\-_\.]{20,}"`
  4. `r#"(?i)(password\s*[=:]\s*)['"]?[^\s'"]{8,}['"]?"#`

- **Replacement** — first 6 chars of the *match* survive, rest →
  `***REDACTED***`. The Rust port already implements this faithfully.

- **Non-laziness of `tracing` fields** — repeated for emphasis: the Rust
  `tracing` API does **not** evaluate field values lazily. Whatever you pass
  to `record(...)` is what the subscriber sees. The cost of `redact()` on a
  ≤500-char string is a few regex passes; this is acceptable on every query
  call. If profiling later shows hot-path overhead, gate the call behind
  `if tracing::Span::current().is_disabled()` — `is_disabled()` is the
  `tracing` analogue of Python's `is_recording()`.

- **Truncation order** — `redact(query[..query.len().min(500)])`, *not*
  `redact(query)[..500]`. The Python order is the same. This avoids splitting
  the literal `***REDACTED***` marker.

---

## Open questions

1. **SeaORM instrumentation granularity.** Should we instrument every ORM call
   (per-query span, high cardinality, partly redundant with SeaORM's own
   `sqlx::query` events) or only the higher-level `crates/database/src/ops/`
   functions (one span per logical operation: `data::lookup`,
   `permissions::grant`)? Python instruments neither; the Rust answer should
   probably be "ops level only" to keep span count manageable. **Decision
   pending.**
2. **`cognee.db.query` for Qdrant.** Python's LanceDB span omits
   `cognee.db.query` because there is no SQL string. Should Rust set a
   synthetic value (e.g. `"vector_search(top_k=15)"`) for consistency, or
   leave it absent? Recommend: leave absent, matching Python LanceDB.
3. **PG adapters.** Should they get instrumented in the same PR even though
   Python doesn't cover them? Recommend: yes — same code shape, low cost, and
   they're production-relevant for cloud users.
4. **Span level.** Python uses default level (INFO). Rust default for
   `#[instrument]` is also INFO. Confirm: do we want INFO for query spans, or
   bump to DEBUG? Recommend: INFO for parity, with the subscriber's env-filter
   gating noise.
5. **Litert adapter.** Was not inspected by this investigation. Confirm it
   exists and add provider="litert" before closing the gap.

---

## Testing strategy

The Rust workspace already has `tracing-subscriber` with the `registry` and
`fmt` features. There are two practical approaches:

### A. `tracing-test`

Add `tracing-test = "0.2"` (or `"0.3"`) as a dev-dependency on the affected
crate. Annotate the test with `#[traced_test]` and assert the span output via
`logs_contain(...)`. This is the lowest-friction approach but only sees the
*formatted* representation, which makes it brittle for asserting structured
field values.

```rust
#[tokio::test]
#[traced_test]
async fn ladybug_query_emits_span() {
    let adapter = test_adapter().await;
    adapter.execute_query("MATCH (n:Node) RETURN n").unwrap();
    assert!(logs_contain("cognee.db.graph.query"));
    assert!(logs_contain("cognee.db.system=\"ladybug\""));
}
```

### B. Custom `Layer` capturing `SpanData`

Implement a small in-test `tracing_subscriber::Layer` that pushes
`(span_name, fields)` to a `Mutex<Vec<…>>`. This is what Python's
`CogneeSpanExporter` does in spirit. Recommend lifting the existing
`crates/http-server/tests/` span-capture helper (if any) into
`crates/test-utils/` so each adapter crate can `use cognee_test_utils::SpanCapture`.

```rust
let capture = SpanCapture::install();
adapter.search_similar("DocumentChunk", "text", &vec![0.1; 384], 5).await?;
let spans = capture.spans();
let s = spans
    .iter()
    .find(|s| s.name == "cognee.db.vector.search")
    .expect("expected vector search span");
assert_eq!(s.field_str("cognee.db.system").as_deref(), Some("qdrant"));
assert_eq!(s.field_i64("cognee.vector.result_count"), Some(5));
```

Approach B is preferred because it asserts the *structured* attributes
byte-for-byte, which is what the cross-SDK parity tests need.

### Additional cases to cover

- **Redaction round-trip.** `execute_query("MATCH (n) WHERE n.token = 'sk-AAAAAAAAAAAAAAAAAAAAAAAAAAA' RETURN n")`
  → assert `cognee.db.query` field starts with `MATCH (n) WHERE n.token = 'sk-AAA***REDACTED***`.
- **Truncation.** Generate a 1000-char query; assert recorded value is ≤ 500
  characters before redaction expansion is applied.
- **Error path.** Force an invalid Cypher query; assert the span has
  `error = true` (or status ERROR) and the row_count attribute is **not** set.
- **LLM model/provider.** Stub `call_api` against a local mock server; assert
  the `llm.api_call` span carries `cognee.llm.model="gpt-4o-mini"` and
  `cognee.llm.provider="openai"`.

---

## References

- Python:
  - [`cognee/modules/observability/tracing.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/tracing.py) — semantic constants + `redact_secrets`.
  - [`cognee/modules/observability/__init__.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/__init__.py) — `new_span` context manager.
  - [`cognee/infrastructure/databases/graph/neo4j_driver/adapter.py`](https://github.com/topoteretes/cognee/blob/main/cognee/infrastructure/databases/graph/neo4j_driver/adapter.py) lines 147-161.
  - [`cognee/infrastructure/databases/graph/ladybug/adapter.py`](https://github.com/topoteretes/cognee/blob/main/cognee/infrastructure/databases/graph/ladybug/adapter.py) lines 269-328.
  - [`cognee/infrastructure/databases/vector/lancedb/LanceDBAdapter.py`](https://github.com/topoteretes/cognee/blob/main/cognee/infrastructure/databases/vector/lancedb/LanceDBAdapter.py) lines 612-688.
  - [`cognee/infrastructure/llm/structured_output_framework/litellm_instructor/llm/generic_llm_api/adapter.py`](https://github.com/topoteretes/cognee/blob/main/cognee/infrastructure/llm/structured_output_framework/litellm_instructor/llm/generic_llm_api/adapter.py) lines 38-55, 174.

- Rust:
  - [`crates/utils/src/tracing_keys.rs`](../../crates/utils/src/tracing_keys.rs) — constants (no callers).
  - [`crates/search/src/observability.rs`](../../crates/search/src/observability.rs) — duplicate constant set.
  - [`crates/http-server/src/observability/redaction.rs`](../../crates/http-server/src/observability/redaction.rs) — `redact()` impl awaiting relocation.
  - [`crates/vector/src/qdrant_adapter.rs`](../../crates/vector/src/qdrant_adapter.rs) — uninstrumented.
  - [`crates/graph/src/ladybug.rs`](../../crates/graph/src/ladybug.rs) — uninstrumented; `execute_query` at line 156 is the single fan-in helper.
  - [`crates/llm/src/adapters/openai.rs`](../../crates/llm/src/adapters/openai.rs) — span exists at `:138` / `:729` but missing `cognee.llm.{model,provider}` fields.
  - [`crates/database/src/connection.rs`](../../crates/database/src/connection.rs) — relational layer uninstrumented.

- Sibling docs:
  - [`gap-analysis.md`](gap-analysis.md) sections 4 (LLM/DB span coverage) and the broader telemetry pillars.
  - [`../http-server/observability.md`](../http-server/observability.md) — ring-buffer span store and `/api/v1/activity/spans` endpoint that consumes whatever attributes the adapters emit.
