# Task 33: Add OpenTelemetry spans for observability

**Priority:** P3 (low)
**Status:** Not started

## Summary

Python uses OpenTelemetry-based tracing throughout the search pipeline (`cognee/modules/observability/`), with semantic attribute constants and a `new_span()` context manager. The Rust codebase uses `tracing` crate spans via `tracing::debug!` in a few places but lacks structured OpenTelemetry-compatible spans with cognee-specific semantic attributes. This task adds OTEL-compatible spans to the Rust search and cognify pipelines.

## Current Rust State

The Rust workspace uses the `tracing` crate for logging (e.g., `tracing::debug` in `graph_completion_retriever.rs`). There is no OpenTelemetry integration, no semantic attribute constants, and no span instrumentation in the search orchestrator or retriever pipeline.

The `tracing` crate has first-class OpenTelemetry integration via `tracing-opentelemetry`, which maps `tracing` spans directly to OTEL spans.

## Python Reference

Python defines semantic constants in `/tmp/cognee-python/cognee/modules/observability/tracing.py`:

```python
COGNEE_DB_SYSTEM = "cognee.db.system"
COGNEE_DB_QUERY = "cognee.db.query"
COGNEE_LLM_MODEL = "cognee.llm.model"
COGNEE_SEARCH_TYPE = "cognee.search.type"
COGNEE_PIPELINE_TASK_NAME = "cognee.pipeline.task_name"
COGNEE_VECTOR_COLLECTION = "cognee.vector.collection"
COGNEE_RESULT_SUMMARY = "cognee.result.summary"
COGNEE_RESULT_COUNT = "cognee.result.count"
# ...
```

The `new_span(name)` context manager in `/tmp/cognee-python/cognee/modules/observability/__init__.py` creates an OTEL span if tracing is enabled, or yields a no-op otherwise.

Usage in `/tmp/cognee-python/cognee/modules/search/methods/get_retriever_output.py`:

```python
with new_span("cognee.retrieval.get_objects") as span:
    span.set_attribute("cognee.retrieval.retriever", retriever_class)
    span.set_attribute(COGNEE_SEARCH_TYPE, query_type.value)
    retrieved_objects = await retriever_instance.get_retrieved_objects(query=query_text)
    span.set_attribute(COGNEE_RESULT_COUNT, obj_count)
```

## Step-by-Step Changes

### Step 1: Add `tracing-opentelemetry` and `opentelemetry` as optional dependencies

In the workspace `Cargo.toml`, add under `[workspace.dependencies]`:

```toml
tracing-opentelemetry = { version = "0.27", optional = true }
opentelemetry = { version = "0.27", optional = true }
opentelemetry_sdk = { version = "0.27", optional = true }
```

Add a `telemetry` feature flag to `crates/search/Cargo.toml` and `crates/lib/Cargo.toml` that enables these.

### Step 2: Define semantic attribute constants

Create `crates/search/src/observability.rs` (or `crates/utils/src/observability.rs` for sharing across crates):

```rust
pub const COGNEE_SEARCH_TYPE: &str = "cognee.search.type";
pub const COGNEE_RESULT_COUNT: &str = "cognee.result.count";
pub const COGNEE_RESULT_SUMMARY: &str = "cognee.result.summary";
pub const COGNEE_RETRIEVER: &str = "cognee.retrieval.retriever";
pub const COGNEE_DB_SYSTEM: &str = "cognee.db.system";
pub const COGNEE_LLM_MODEL: &str = "cognee.llm.model";
pub const COGNEE_VECTOR_COLLECTION: &str = "cognee.vector.collection";
pub const COGNEE_PIPELINE_TASK_NAME: &str = "cognee.pipeline.task_name";
```

### Step 3: Add `#[instrument]` attributes to key methods

Use `tracing::instrument` on retriever and orchestrator methods:

In `crates/search/src/orchestration/search_orchestrator.rs`:

```rust
#[tracing::instrument(
    name = "cognee.search",
    skip(self, request),
    fields(
        cognee.search.type = %format!("{:?}", request.search_type),
        cognee.search.query = %request.query_text,
    )
)]
pub async fn search(&self, request: &SearchRequest) -> Result<SearchResponse, SearchError> {
```

In each retriever's `get_context`:

```rust
#[tracing::instrument(name = "cognee.retrieval.get_context", skip(self))]
async fn get_context(&self, query: &str) -> Result<SearchContext, SearchError> {
```

### Step 4: Add setup utility

Create a setup function that configures the `tracing-opentelemetry` layer when the `telemetry` feature is enabled:

```rust
pub fn setup_tracing(service_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Configure OTEL exporter (stdout or OTLP based on env)
    // Layer with tracing-opentelemetry
    // Set as global subscriber
}
```

### Step 5: Wire into CLI

In `crates/cli/`, call `setup_tracing` if `OTEL_EXPORTER_OTLP_ENDPOINT` or a `--trace` flag is set.

**Files to modify/create:**
- `Cargo.toml` (workspace deps)
- `crates/search/Cargo.toml` (feature flag)
- `crates/search/src/observability.rs` (new file)
- `crates/search/src/orchestration/search_orchestrator.rs` (instrument)
- `crates/search/src/retrievers/*.rs` (instrument key methods)
- `crates/cli/src/main.rs` (setup call)

## Test Verification

1. **Compile test:** `cargo check --all-targets --features telemetry` succeeds.
2. **Compile test:** `cargo check --all-targets` (without feature) still compiles -- no hard dependency.
3. **Unit test:** When the `telemetry` feature is disabled, no OTEL overhead is added (zero-cost).
4. **Integration test (manual):** With `OTEL_EXPORTER_OTLP_ENDPOINT` set, verify spans appear in a local collector (e.g., Jaeger).

## Dependencies

- `tracing` (already used)
- `tracing-opentelemetry` (new optional dependency)
- `opentelemetry`, `opentelemetry_sdk` (new optional dependencies)
- No blocking dependencies from other tasks.
