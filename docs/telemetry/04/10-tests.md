# Task 04-10 — Adapter instrumentation tests

**Status**: ⬜ unimplemented
**Owner**: _unassigned_
**Depends on**:
- [Task 04-03](03-span-capture-test-helper.md) — needs the `SpanCapture` helper.
- [Task 04-04 — Qdrant](04-qdrant-instrumentation.md) — needs the Qdrant spans.
- [Task 04-05 — Ladybug](05-ladybug-instrumentation.md) — needs the Ladybug span.
- [Task 04-06 — OpenAI](06-openai-llm-fields.md) — needs the LLM fields.
- [Task 04-08 — PG](08-pg-adapters.md) — pgvector-side tests skip without DB_PROVIDER=postgres. (pg_graph instrumentation is deferred — no test suite for it in this task.)
- [Task 04-09 — SeaORM](09-seaorm-ops-instrumentation.md) — needs the relational spans.
**Blocks**:
- [Task 04-11 — Docs + CI](11-docs-and-ci.md) — depends on green tests.

**Parent doc**: [04 — DB-Adapter Span Instrumentation](../04-db-adapter-instrumentation.md)
**Locked decisions**: #6 (Approach B test strategy via `SpanCapture`).

---

## 1. Goal

Add structured-attribute integration tests for every adapter
instrumented in this gap. Each test:

1. Installs `cognee_test_utils::SpanCapture::install()`.
2. Drives the adapter through a representative call.
3. Asserts the captured span has the expected name + field values.

Test files (one per adapter crate):

| File | Crate | Adapters / ops covered |
|---|---|---|
| `crates/vector/tests/qdrant_span_instrumentation.rs` | `cognee-vector` | `QdrantAdapter::{search_similar, index_points, delete_points, delete_collection}` |
| `crates/graph/tests/ladybug_span_instrumentation.rs` | `cognee-graph` | `LadybugAdapter::execute_query` (via public methods) + redaction round-trip |
| `crates/llm/tests/openai_span_instrumentation.rs` | `cognee-llm` | `OpenAIAdapter::{call_api, call_transcription_api}` (via httpmock) |
| `crates/vector/tests/pgvector_span_instrumentation.rs` | `cognee-vector` | `PgVectorAdapter::*` — gated on `pg_test_url()` |
| `crates/database/tests/relational_ops_span_instrumentation.rs` | `cognee-database` | One representative op per file (15 files → 15 assertions) |

> **Deferred:** `pg_graph_span_instrumentation.rs` is **out of scope for this
> task**. Per the user decision recorded in [04-08](08-pg-adapters.md), the
> `pg_graph_adapter` is not yet instrumented — its public `query` is a stub
> returning `QueryError("not supported")` and meaningful coverage requires a
> ~22-method fan-in refactor. The pg_graph test suite will land in the
> follow-up task that introduces that refactor.

The OpenAI test uses **`httpmock`** to bind a fake server on `127.0.0.1`;
no outbound network calls. `httpmock = "0.8"` is already a dev-dep on
[`crates/llm/Cargo.toml`](../../crates/llm/Cargo.toml), so no Cargo
changes are required for the LLM crate. (Note: `mockito` is *not* a
workspace dev-dep — it is declared per-crate where used. The original
draft of this sub-doc cited `mockito`; the LLM crate already standardises
on `httpmock`, so we follow that.)

## 2. Rationale

Locked decision 6 picked Approach B (custom `SpanCapture` Layer)
over `tracing-test`. Per-adapter tests close the loop by asserting
that:

- Every span the adapter emits has the **right name** (matches the
  string literal in the `#[instrument]` macro).
- Every required attribute is **set with the right value**, byte-for-byte.
- Truncation + redaction works end-to-end (Ladybug + pg_graph).
- Error paths still produce a span (with `err`-recorded error
  message) and skip the count attribute.

These tests run in debug mode, in parallel, on the host CI lane.
The pg-side tests skip without a Postgres URL, mirroring the
existing pattern in
[`crates/test-utils/src/lib.rs::pg_test_url`](../../crates/test-utils/src/lib.rs).

## 3. Pre-conditions

- All adapter tasks (04-04 through 04-09) are complete.
- `cognee-test-utils` exports `SpanCapture` (task 04-03).
- `httpmock = "0.8"` is already a dev-dep on `crates/llm/Cargo.toml` (per-crate, not workspace). No `mockito` is required.
- A clean `cargo check --all-targets` on `main`.

## 4. Step-by-step

### 4.1 Add `cognee-test-utils` as dev-dep where missing

Each crate that gains a new test file needs `cognee-test-utils` in
`[dev-dependencies]`. Verify and add to:

- [`crates/vector/Cargo.toml`](../../crates/vector/Cargo.toml)
- [`crates/graph/Cargo.toml`](../../crates/graph/Cargo.toml)
- [`crates/llm/Cargo.toml`](../../crates/llm/Cargo.toml)
- [`crates/database/Cargo.toml`](../../crates/database/Cargo.toml)

```toml
[dev-dependencies]
cognee-test-utils = { path = "../test-utils" }
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
# cognee-llm already has `httpmock = "0.8"` for HTTP mocking — no add needed.
```

`crates/database/Cargo.toml` already lists
`cognee-test-utils = { path = "../test-utils" }` (line 35); confirm before
re-adding.

### 4.2 Qdrant tests — `crates/vector/tests/qdrant_span_instrumentation.rs`

```rust
//! Span attribute integration tests for the Qdrant adapter.

use cognee_test_utils::SpanCapture;
use cognee_vector::{QdrantAdapter, VectorPoint, VectorDB};
use std::sync::Arc;
use tempfile::tempdir;
use uuid::Uuid;

async fn make_adapter() -> Arc<QdrantAdapter> {
    let dir = tempdir().expect("temp dir");
    Arc::new(
        QdrantAdapter::open(dir.path().to_path_buf(), 4)
            .expect("open qdrant adapter"),
    )
}

#[tokio::test]
async fn search_emits_cognee_db_vector_search_span() {
    let capture = SpanCapture::install();
    let adapter = make_adapter().await;

    // Seed the collection.
    let pid = Uuid::new_v4();
    let point = VectorPoint {
        id: pid,
        vector: vec![0.1, 0.2, 0.3, 0.4],
        payload: serde_json::json!({ "text": "hi" }),
    };
    adapter
        .index_points("DocumentChunk", "text", &[point])
        .await
        .expect("seed upsert");

    // Search.
    let results = adapter
        .search_similar("DocumentChunk", "text", &[0.1, 0.2, 0.3, 0.4], 5)
        .await
        .expect("search");
    assert_eq!(results.len(), 1);

    let spans = capture.spans();
    let s = spans
        .iter()
        .find(|s| s.name == "cognee.db.vector.search")
        .expect("expected vector search span");
    assert_eq!(s.field_str("cognee.db.system").as_deref(), Some("qdrant"));
    assert_eq!(
        s.field_str("cognee.vector.collection").as_deref(),
        Some("DocumentChunk_text"),
    );
    assert_eq!(s.field_i64("cognee.vector.result_count"), Some(1));
}

#[tokio::test]
async fn upsert_emits_cognee_db_vector_upsert_span_with_row_count() {
    let capture = SpanCapture::install();
    let adapter = make_adapter().await;

    let points: Vec<VectorPoint> = (0..3)
        .map(|i| VectorPoint {
            id: Uuid::new_v4(),
            vector: vec![i as f32, 0.0, 0.0, 0.0],
            payload: serde_json::json!({}),
        })
        .collect();
    adapter
        .index_points("Entity", "name", &points)
        .await
        .expect("upsert");

    let spans = capture.spans();
    let s = spans
        .iter()
        .find(|s| s.name == "cognee.db.vector.upsert")
        .expect("expected upsert span");
    assert_eq!(s.field_str("cognee.db.system").as_deref(), Some("qdrant"));
    assert_eq!(
        s.field_str("cognee.vector.collection").as_deref(),
        Some("Entity_name"),
    );
    assert_eq!(s.field_i64("cognee.db.row_count"), Some(3));
}

#[tokio::test]
async fn delete_emits_cognee_db_vector_delete_span_with_row_count() {
    let capture = SpanCapture::install();
    let adapter = make_adapter().await;

    // Seed first so the delete has something to remove.
    let pid = Uuid::new_v4();
    adapter
        .index_points(
            "DocumentChunk",
            "text",
            &[VectorPoint {
                id: pid,
                vector: vec![0.1, 0.0, 0.0, 0.0],
                payload: serde_json::json!({}),
            }],
        )
        .await
        .expect("seed");

    adapter
        .delete_points("DocumentChunk", "text", &[pid])
        .await
        .expect("delete");

    let spans = capture.spans();
    let s = spans
        .iter()
        .find(|s| s.name == "cognee.db.vector.delete")
        .expect("expected delete span");
    assert_eq!(s.field_i64("cognee.db.row_count"), Some(1));
}

#[tokio::test]
async fn empty_upsert_short_circuits_but_still_emits_span() {
    let capture = SpanCapture::install();
    let adapter = make_adapter().await;
    adapter
        .index_points("DocumentChunk", "text", &[])
        .await
        .expect("empty upsert");

    let spans = capture.spans();
    assert!(spans.iter().any(|s| s.name == "cognee.db.vector.upsert"));
}
```

### 4.3 Ladybug tests — `crates/graph/tests/ladybug_span_instrumentation.rs`

```rust
//! Span attribute integration tests for the Ladybug adapter.

use cognee_graph::LadybugAdapter;
use cognee_test_utils::SpanCapture;
use tempfile::tempdir;

async fn make_adapter() -> LadybugAdapter {
    let dir = tempdir().expect("temp dir");
    LadybugAdapter::open(dir.path().join("graph.lbug"))
        .await
        .expect("open ladybug")
}

#[tokio::test]
async fn query_emits_cognee_db_graph_query_span() {
    let capture = SpanCapture::install();
    let adapter = make_adapter().await;
    // A query that returns zero rows is the cleanest assertion.
    adapter
        .query("MATCH (n:Doesnotexist) RETURN n", None)
        .await
        .expect("query");

    let spans = capture.spans();
    let s = spans
        .iter()
        .find(|s| s.name == "cognee.db.graph.query")
        .expect("expected graph query span");
    assert_eq!(s.field_str("cognee.db.system").as_deref(), Some("ladybug"));
    assert_eq!(s.field_i64("cognee.db.row_count"), Some(0));
    let recorded_query = s
        .field_str("cognee.db.query")
        .expect("query attr present");
    assert!(recorded_query.contains("MATCH (n:Doesnotexist)"));
}

#[tokio::test]
async fn query_redacts_secret_in_recorded_attribute() {
    let capture = SpanCapture::install();
    let adapter = make_adapter().await;

    // OpenAI-style key embedded in a Cypher literal. The query is
    // intentionally invalid; we just need the span to fire on the
    // path before the engine errors.
    let q = "MATCH (n) WHERE n.token = 'sk-ABCDEFGHIJKLMNOPQRSTUVWXYZ12345' RETURN n";
    let _ = adapter.query(q, None).await; // ignore result

    let spans = capture.spans();
    let s = spans
        .iter()
        .find(|s| s.name == "cognee.db.graph.query")
        .expect("expected graph query span");
    let recorded = s
        .field_str("cognee.db.query")
        .expect("query attr present");
    assert!(
        recorded.contains("sk-ABC***REDACTED***"),
        "expected redaction marker in: {recorded}"
    );
    assert!(!recorded.contains("DEFGHIJKLMNOP"));
}

#[tokio::test]
async fn long_query_truncated_to_500_chars_before_redaction() {
    let capture = SpanCapture::install();
    let adapter = make_adapter().await;
    let long_query = format!("MATCH (n) WHERE n.x = '{}' RETURN n", "a".repeat(800));
    let _ = adapter.query(&long_query, None).await;

    let spans = capture.spans();
    let s = spans
        .iter()
        .find(|s| s.name == "cognee.db.graph.query")
        .expect("expected graph query span");
    let recorded = s
        .field_str("cognee.db.query")
        .expect("query attr present");
    // Truncation is byte-len <= 500. (Char-boundary walk may shave a few.)
    assert!(
        recorded.len() <= 500,
        "recorded len {} exceeded 500",
        recorded.len()
    );
}
```

### 4.4 OpenAI tests — `crates/llm/tests/openai_span_instrumentation.rs`

The LLM crate already pins `httpmock = "0.8"` as a dev-dep, so we use that
(not `mockito`) to mock the chat-completions endpoint. The implementor
should confirm `OpenAIAdapter::new` (or builder) signature, the chat
completions URL path the adapter posts to, and the smallest valid response
shape that `OpenAIResponse` deserialises — these tend to drift over time.

```rust
//! Span attribute integration tests for the OpenAI adapter using
//! httpmock (no real API calls).

use cognee_llm::adapters::openai::OpenAIAdapter;
use cognee_test_utils::SpanCapture;
use httpmock::prelude::*;

#[tokio::test]
async fn call_api_records_cognee_llm_model_and_provider() {
    let server = MockServer::start_async().await;
    let _m = server.mock_async(|when, then| {
        when.method(POST).path("/chat/completions");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{
                "choices": [{"message": {"content": "hi"}, "finish_reason": "stop", "index": 0}],
                "model": "gpt-4o-mini"
            }"#);
    }).await;

    let capture = SpanCapture::install();
    let adapter = /* construct OpenAIAdapter pointing at server.base_url() with model "gpt-4o-mini" */;
    let _resp = /* drive a call_api / generate path */;

    let spans = capture.spans();
    let s = spans
        .iter()
        .find(|s| s.name == "llm.api_call")
        .expect("expected llm.api_call span");
    assert_eq!(s.field_str("cognee.llm.model").as_deref(), Some("gpt-4o-mini"));
    assert_eq!(s.field_str("cognee.llm.provider").as_deref(), Some("openai"));
}
```

### 4.5 PG tests — `crates/vector/tests/pgvector_span_instrumentation.rs`

`pg_graph_span_instrumentation.rs` is **deferred** (see the table note in
§1) and is not created in this task.

The pgvector test starts with:

```rust
let Some(url) = cognee_test_utils::pg_test_url() else { return };
```

— so the test silently skips on developer machines without a
Postgres provider configured. CI runs Postgres in a sidecar container
when `DB_PROVIDER=postgres` is set.

The body mirrors the Qdrant tests with `cognee.db.system` asserted as
`"pgvector"`.

### 4.6 Relational ops test — `crates/database/tests/relational_ops_span_instrumentation.rs`

One test per file, asserting only the **span name + system attribute**
(row counts are exercised in 04-09's existing tests indirectly):

```rust
//! Smoke tests for span emission across `crates/database/src/ops/*`.

use cognee_database::{connect, initialize};
use cognee_test_utils::SpanCapture;
use std::sync::Arc;

async fn make_db() -> Arc<cognee_database::DatabaseConnection> {
    let db = connect("sqlite::memory:").await.expect("connect");
    initialize(&db).await.expect("init");
    Arc::new(db)
}

#[tokio::test]
async fn datasets_list_emits_relational_span() {
    let capture = SpanCapture::install();
    let db = make_db().await;
    let owner = uuid::Uuid::new_v4();
    let _ = cognee_database::ops::datasets::list_datasets_by_owner(&db, owner).await;

    let spans = capture.spans();
    assert!(
        spans
            .iter()
            .any(|s| s.name == "cognee.db.relational.datasets.list_datasets_by_owner"
                && s.field_str("cognee.db.system").as_deref() == Some("sqlite")),
        "expected datasets list span; got {:?}",
        spans.iter().map(|s| &s.name).collect::<Vec<_>>(),
    );
}

// ... one #[tokio::test] per ops file (15 total) ...
```

15 tests is verbose but each is a 5-line smoke. The implementor can
factor a helper:

```rust
fn assert_relational_span(spans: &[CapturedSpan], expected: &str) {
    assert!(
        spans
            .iter()
            .any(|s| s.name == expected && s.field_str("cognee.db.system").as_deref() == Some("sqlite")),
        "missing span {expected}; got {:?}",
        spans.iter().map(|s| &s.name).collect::<Vec<_>>(),
    );
}
```

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. Run the new test files individually.
cargo test -p cognee-vector --test qdrant_span_instrumentation
cargo test -p cognee-graph --test ladybug_span_instrumentation
cargo test -p cognee-llm --test openai_span_instrumentation
cargo test -p cognee-database --test relational_ops_span_instrumentation

# 3. Run pg-side tests with DB_PROVIDER set (skip otherwise).
DB_PROVIDER=postgres DB_HOST=localhost DB_PORT=5432 DB_NAME=cognee_test \
    DB_USERNAME=postgres DB_PASSWORD=postgres \
    cargo test -p cognee-vector --test pgvector_span_instrumentation

# 4. Clippy.
cargo clippy --all-targets -- -D warnings

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/vector/Cargo.toml`](../../crates/vector/Cargo.toml) — add
  `cognee-test-utils` dev-dep if missing.
- [`crates/graph/Cargo.toml`](../../crates/graph/Cargo.toml) — same.
- [`crates/llm/Cargo.toml`](../../crates/llm/Cargo.toml) — add
  `cognee-test-utils` dev-dep if missing (`httpmock = "0.8"` is already pinned).
- [`crates/database/Cargo.toml`](../../crates/database/Cargo.toml) — same.
- [`crates/vector/tests/qdrant_span_instrumentation.rs`](../../crates/vector/tests/qdrant_span_instrumentation.rs)
  — NEW. ~120 lines, four `#[tokio::test]` functions.
- [`crates/graph/tests/ladybug_span_instrumentation.rs`](../../crates/graph/tests/ladybug_span_instrumentation.rs)
  — NEW. ~80 lines, three `#[tokio::test]` functions.
- [`crates/llm/tests/openai_span_instrumentation.rs`](../../crates/llm/tests/openai_span_instrumentation.rs)
  — NEW. ~80 lines, two `#[tokio::test]` functions (call_api +
  call_transcription_api with Whisper-shaped mock body).
- [`crates/vector/tests/pgvector_span_instrumentation.rs`](../../crates/vector/tests/pgvector_span_instrumentation.rs)
  — NEW. Skip-on-no-pg mirror of qdrant_span_instrumentation.
- [`crates/database/tests/relational_ops_span_instrumentation.rs`](../../crates/database/tests/relational_ops_span_instrumentation.rs)
  — NEW. ~15 small `#[tokio::test]` functions.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `OpenAIAdapter::new` signature drift | Real — verify constructor at task time before writing the test. | Sub-agent A re-reads. |
| Tests run in parallel and share global `tracing` state | None — `SpanCapture::install()` uses thread-local `set_default`, so each test owns its own subscriber. | Already verified in 04-03 self-tests. |
| Char-boundary truncation rounds the recorded query length below 500 (e.g. to 498) on byte-edge cases | Real and intentional — the test asserts `<= 500`, not `== 500`. | Already in the assertion in 4.3. |
| `httpmock` body doesn't match `OpenAIResponse` deserialization | Real — confirm with the actual `OpenAIResponse` struct in `crates/llm/src/adapters/openai.rs`. | Sub-agent B reads the deserialize impl before writing the mock body. |
| Postgres tests run in CI without a sidecar and fail | Already handled — skip when `pg_test_url()` returns `None`. | n/a |

## 8. Out of scope

- LiteRT span tests. The adapter is feature-gated to Android; host
  CI cannot exercise it. Future work in `android/`.
- Cross-SDK byte-parity tests. The OTEL exporter parity work belongs
  in [`e2e-cross-sdk/`](../../e2e-cross-sdk/) as a follow-up
  (already listed in [`gap-analysis.md`](../gap-analysis.md) as
  "Cross-SDK OTEL parity test").
- Performance benchmarks for redaction overhead. Future tuning.
- Wiring the new tests into the `lib-tests.yml` CI lane — that
  happens in [task 04-11](11-docs-and-ci.md).
