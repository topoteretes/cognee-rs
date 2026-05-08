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

Each item below has a dedicated implementation sub-document under
[`04/`](04/) with rationale, prerequisites, step-by-step source-level
changes, verification commands, files modified, and risks. **The
sub-docs are authoritative**: where they refine details based on the
locked design decisions, follow the sub-doc rather than this
high-level summary.

| #  | Action item | Sub-doc | Depends on | Status |
|----|---|---|---|---|
| 01 | Relocate `redact()` from `crates/http-server/src/observability/redaction.rs` to a new module `cognee_utils::redact`, leaving the JSON walker (`redact_attributes`) in http-server. Adds `regex` direct dep to `cognee-utils`. | [04/01-redact-relocate.md](04/01-redact-relocate.md) | — | ✅ 21f10e8 |
| 02 | Make `crates/utils/src/tracing_keys.rs` the single source of truth for `cognee.*` semantic-attribute key constants; replace `crates/search/src/observability.rs` body with `pub use cognee_utils::tracing_keys::*;`. Adds `cognee-utils` dep on `cognee-search` (if not already). | [04/02-tracing-constants-dedupe.md](04/02-tracing-constants-dedupe.md) | — | ✅ 1e03ac9 |
| 03 | Add `SpanCapture` test helper to `cognee-test-utils` (`tracing::Layer` capturing structured fields into `Mutex<Vec<CapturedSpan>>`). Used by every adapter integration test in 04-10. | [04/03-span-capture-test-helper.md](04/03-span-capture-test-helper.md) | — | ✅ 0578c1f |
| 04 | Instrument `QdrantAdapter::{search_similar, index_points, delete_points, delete_collection}` with `cognee.db.vector.*` spans (`cognee.db.system="qdrant"`, `cognee.vector.collection`, `cognee.vector.result_count` / `cognee.db.row_count`). Adds `cognee-utils` dep on `cognee-vector`. | [04/04-qdrant-instrumentation.md](04/04-qdrant-instrumentation.md) | 02 | ✅ 1d3fda1 |
| 05 | Instrument `LadybugAdapter::execute_query` (the private fan-in helper) with `cognee.db.graph.query` (`cognee.db.system="ladybug"`, `cognee.db.query` truncate-then-redact, `cognee.db.row_count`). Covers all 24 public methods transitively. Adds `cognee-utils` dep on `cognee-graph`. | [04/05-ladybug-instrumentation.md](04/05-ladybug-instrumentation.md) | 01, 02 | ✅ 2683ba1 |
| 06 | Add `cognee.llm.model` / `cognee.llm.provider="openai"` fields to the existing `llm.api_call` and `llm.transcription_api_call` `#[instrument]` blocks on `OpenAIAdapter`. Adds `cognee-utils` dep on `cognee-llm`. | [04/06-openai-llm-fields.md](04/06-openai-llm-fields.md) | 02 | ⬜ |
| 07 | Add `#[instrument]` with `cognee.llm.model` / `cognee.llm.provider="litert"` on `LiteRtAdapter::generate` and `create_structured_output_with_messages_raw`. Feature-gated behind `android-litert`. | [04/07-litert-llm-fields.md](04/07-litert-llm-fields.md) | 02 | ⬜ |
| 08 | Mirror the Qdrant / Ladybug instrumentation onto `pgvector_adapter` (`cognee.db.system="pgvector"`) and `pg_graph_adapter::query` (`cognee.db.system="postgres"`). Decision 3 puts these in scope for cloud users. | [04/08-pg-adapters.md](04/08-pg-adapters.md) | 01, 02, 04, 05 | ⬜ |
| 09 | Ops-level instrumentation of every public function in `crates/database/src/ops/*.rs` (~80 functions across 14 files) with `cognee.db.relational.<file_stem>.<fn>` spans. Adds a `database_system_label(&db)` helper that maps SeaORM backend → `"sqlite"` / `"postgres"` / `"mysql"`. | [04/09-seaorm-ops-instrumentation.md](04/09-seaorm-ops-instrumentation.md) | 02 | ⬜ |
| 10 | Adapter span-instrumentation integration tests using the `SpanCapture` helper from 03. Six new test files: `qdrant_span_instrumentation`, `ladybug_span_instrumentation`, `openai_span_instrumentation` (mockito), `pgvector_span_instrumentation` (skip-on-no-pg), `pg_graph_span_instrumentation` (same), `relational_ops_span_instrumentation` (15 smoke tests). | [04/10-tests.md](04/10-tests.md) | 03, 04, 05, 06, 08, 09 | ⬜ |
| 11 | Docs: extend `docs/observability/opentelemetry.md` with the canonical span-name / attribute reference. Update `docs/telemetry/gap-analysis.md` section 4 to mark the LLM/DB span gap closed. CI: add a Postgres lane for the pg-side span tests if not already present. Closure summary at the bottom of this doc. | [04/11-docs-and-ci.md](04/11-docs-and-ci.md) | 01–10 | ⬜ |

### Suggested execution order

A clean PR sequence based on the dependency graph:

1. **PR 1** (foundation): tasks 01 + 02 + 03 — `redact()` relocation,
   constants dedupe, `SpanCapture` test helper. No new spans yet.
2. **PR 2** (core adapters): tasks 04 + 05 + 06 — Qdrant + Ladybug +
   OpenAI. Covers the default backends every cognee-rust deployment
   uses.
3. **PR 3** (Android): task 07 — LiteRT. Independent of PR 2; can
   land in parallel.
4. **PR 4** (cloud / postgres): task 08 — pgvector + pg_graph
   adapters. Independent of PR 3.
5. **PR 5** (relational): task 09 — SeaORM ops-level instrumentation.
   Largest mechanical diff but isolated.
6. **PR 6** (validation): task 10 — adapter span tests.
7. **PR 7** (closeout): task 11 — docs + CI + gap closure.

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

## Design decisions (locked)

Approved by the project owner on 2026-05-07. **Do not re-litigate.** Sub-agents
may surface new evidence that contradicts a decision; if so, escalate to the
user before changing course.

| # | Decision | Rationale | Affected tasks |
|---|---|---|---|
| 1 | **SeaORM instrumentation is ops-level only.** Add one span per public function in `crates/database/src/ops/*.rs` (e.g. `data::create_data`, `datasets::list_datasets`). Do **not** instrument every individual SeaORM/sqlx call. | Per-call spans are noisy and partly redundant with SeaORM's own `sqlx::query` events. Python instruments neither layer; ops-level keeps span count manageable while still covering every logical DB op. | [04-09](04/09-seaorm-ops-instrumentation.md) |
| 2 | **Omit `cognee.db.query` on Qdrant spans** (and on the Rust pgvector adapter). | Matches Python's LanceDB instrumentation — there is no SQL string for a vector lookup. The collection name + `cognee.vector.result_count` are sufficient. | [04-04](04/04-qdrant-instrumentation.md), [04-08](04/08-pg-adapters.md) |
| 3 | **PG adapters (`pgvector_adapter`, `pg_graph_adapter`) are in scope.** Same span shape as Qdrant / Ladybug; `cognee.db.system="pgvector"` / `cognee.db.system="postgres"`. | Same code shape, low cost, production-relevant for cloud users. | [04-08](04/08-pg-adapters.md) |
| 4 | **LiteRT lives in its own task.** `cognee.llm.{model,provider}` fields go on `LiteRtAdapter::generate` (and `create_structured_output_with_messages_raw`), with `provider = "litert"`. | The adapter is feature-gated (`android-litert`) and uses a different call shape from `OpenAIAdapter`; bundling would add `#[cfg(feature = "android-litert")]` noise to the OpenAI task. Splitting keeps each diff small and reviewable. | [04-06](04/06-openai-llm-fields.md), [04-07](04/07-litert-llm-fields.md) |
| 5 | **Span level is INFO for all adapter spans.** | Matches Python (`new_span` defaults to INFO) and matches the existing default for `#[tracing::instrument]`. Operators tune verbosity via `RUST_LOG` / OTEL sampler. | All adapter tasks |
| 6 | **Test strategy is Approach B.** Add a `SpanCapture` helper in `cognee-test-utils` implementing `tracing::Layer` and pushing `(name, fields)` to a `Mutex<Vec<…>>`. Each adapter integration test installs it as the test subscriber and asserts span name + structured field values. | Approach A (`tracing-test` + `logs_contain`) only sees the formatted text and is brittle for asserting structured field values byte-for-byte. The cross-SDK parity tests need structured assertions. | [04-03](04/03-span-capture-test-helper.md), [04-10](04/10-tests.md) |
| 7 | **Foundation cleanups are two separate tasks** (not bundled). | `redact()` relocation and the constants dedupe are mechanically independent. Splitting keeps each commit minimal and revertable. | [04-01](04/01-redact-relocate.md), [04-02](04/02-tracing-constants-dedupe.md) |
| 8 | **Adapter instrumentation is unconditional — no feature gate.** Tracing span macros are always compiled and consumed by whichever subscriber the embedder attaches; the cost of an unsubscribed span is negligible. The `telemetry` cargo feature gates analytics events (gap 02/03), not tracing spans. | Mirrors how `#[tracing::instrument]` is already used elsewhere in the workspace (`crates/llm/src/adapters/openai.rs`, `crates/lib/src/api/recall.rs`, etc.). | All adapter tasks |
| 9 | **Truncation order is `redact(query[..min(query.len(), 500)])`** — truncate first, then redact, matching Python's `redact_secrets(query[:500])`. | Reversing the order would let a redacted form longer than 500 chars be re-truncated and split the literal `***REDACTED***` marker. | [04-05](04/05-ladybug-instrumentation.md) |

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
