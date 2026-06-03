# HTTP Server — Observability

Specification for the Rust HTTP server's tracing, span buffering, and access-log strategy. Drives the implementation of `/api/v1/activity/spans`, the structured logs ingested by deployments, and the per-handler tracing that feeds both. Pipeline-run history (the *durable* observability tier) is covered in [pipelines.md](pipelines.md); this doc is the *live* tier.

Companion docs: [architecture.md](architecture.md), [pipelines.md](pipelines.md).

## 1. Goals & non-goals

### Goals

- **Wire-compatible `/api/v1/activity/spans` endpoint** — same JSON shape as Python's [`get_activity_router.get_spans`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py): grouped by `trace_id`, with `root_name`, `duration_ms`, `span_count`, `status`, and a `spans` array.
- **In-memory ring buffer** that bounds memory use to the last 50 traces (matches Python's [`_MAX_TRACES = 50`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/tracing.py)).
- **Single tracing stack**: one `tracing::Subscriber` setup powers structured stdout logs *and* the in-memory span buffer. No parallel "logging vs tracing" stacks.
- **Secret redaction** at recording time so API keys, bearer tokens, and passwords never leave the process in any log or span.
- **Access logging** via `tower_http::trace::TraceLayer` with the same field conventions Python's uvicorn logs use, so existing log dashboards work unchanged.
- **OTEL exportability deferred but unblocked**: the buffer implementation is wrapped in a trait so a future OTLP exporter can be slotted in without rewiring the call sites.

### Non-goals

- **Distributed tracing across services**: not in phase 1. The buffer is process-local; we do not propagate `traceparent` headers or push to an OTEL collector. Phase 2 lights this up via `tracing-opentelemetry` once a collector lands in the deployment story.
- **Metrics endpoint** (Prometheus, OpenMetrics): out of scope. Add a `/metrics` route in a follow-up doc when there's a clear deployment need.
- **Remote sinks** (Datadog, Honeycomb, Sentry events): out of scope. Sentry is already optional in Python's `client.py`; we'll port that as a feature-gated layer later.

## 2. Two tiers of observability

| Tier | Storage | TTL | Endpoint | Purpose |
|---|---|---|---|---|
| **Durable** — pipeline runs | Relational DB (`pipeline_runs` table) | Forever (or app-driven cleanup) | `GET /api/v1/datasets/status`, `GET /api/v1/activity/pipeline-runs` | History, audit, dashboards |
| **Live** — request/handler spans | In-memory ring buffer in the server process | Last 50 traces (LRU) | `GET /api/v1/activity/spans` | Trace viewer, recent-failure debugging |

This doc covers the *live* tier. The durable tier is in [pipelines.md §5](pipelines.md#5-database-persistence--pipeline_runs-table).

## 3. Tracing stack — `tracing` + custom layer

### 3.1 Decision: stay on `tracing`, not OTEL SDK

Python uses the OpenTelemetry SDK directly: a `TracerProvider` with a `SimpleSpanProcessor` plus a custom in-memory `SpanExporter`. The Rust ecosystem's idiomatic equivalent — and what every other crate in this workspace already uses — is the **`tracing`** crate. We layer on top of it instead of bringing in `opentelemetry-sdk`:

| Option | Pros | Cons | Decision |
|---|---|---|---|
| **`tracing` + custom `Layer`** | Zero new deps; aligns with existing logging; fast; trivial to test. | Manual span-attribute extraction; future OTEL bridge needs `tracing-opentelemetry`. | **Chosen** |
| `opentelemetry-sdk` + `tracing-opentelemetry` bridge | Wire-compatible with Python's exporter conventions; future-proof for OTLP export. | Heavy dep; double-bookkeeping (every span is recorded twice — once by `tracing` and once by OTEL); complicates testing. | Deferred to phase 2 (additive, not replacement). |
| OTEL SDK only (no `tracing`) | Direct port of Python's design. | The rest of the cognee Rust workspace uses `tracing`; bypassing it splits the log story. | Rejected. |

### 3.2 Subscriber composition

The `cognee-http-server` binary builds a single `tracing_subscriber::registry` with three layers, in this order:

1. **`fmt::Layer`** for stdout. JSON in `prod`, pretty in `dev` (matches the existing CLI pattern in [`crates/cli/src/main.rs`](../../crates/cli/src/main.rs)).
2. **`SpanBufferLayer`** (Rust port of `CogneeSpanExporter`) — captures span metadata into the in-memory ring buffer. See §4.
3. **`EnvFilter`** at the top of the chain so `RUST_LOG=info,ort=warn` controls both fmt output and buffer captures.

Embedders who consume the *library* do not get this subscriber wired automatically — they install their own. The library never calls `set_global_default`. This is consistent with [architecture.md §12](architecture.md#12-logging--observability).

### 3.3 Span instrumentation conventions

Handlers and pipeline tasks are instrumented with `#[tracing::instrument]`. We use a shared list of attribute keys derived from Python's [`tracing.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/tracing.py) so spans surfaced in `/spans` look the same in both stacks:

| Constant | Key | Where set |
|---|---|---|
| `COGNEE_DB_SYSTEM` | `cognee.db.system` | `cognee-database` repository methods |
| `COGNEE_DB_QUERY` | `cognee.db.query` | Same |
| `COGNEE_DB_ROW_COUNT` | `cognee.db.row_count` | Same |
| `COGNEE_LLM_MODEL` | `cognee.llm.model` | `cognee-llm` adapters |
| `COGNEE_LLM_PROVIDER` | `cognee.llm.provider` | Same |
| `COGNEE_SEARCH_TYPE` | `cognee.search.type` | `cognee-search` orchestrator |
| `COGNEE_PIPELINE_NAME` | `cognee.pipeline.name` | `cognee-cognify`, `cognee-ingestion`, etc. |
| `COGNEE_PIPELINE_TASK_NAME` | `cognee.pipeline.task_name` | Per-task instrumentation |
| `COGNEE_OPERATION_MODE` | `cognee.operation.mode` | `remember()` / `improve()` (`session` vs `permanent`) |
| `COGNEE_RECALL_SCOPE` | `cognee.recall.scope` | `recall()` query router |
| `COGNEE_FORGET_TARGET` | `cognee.forget.target` | `forget()` |
| `COGNEE_DATASET_NAME` | `cognee.dataset.name` | Anywhere a dataset is in scope |
| `COGNEE_SESSION_ID` | `cognee.session.id` | Session-aware operations |

The full list lives in [`crates/utils/src/tracing_keys.rs`](../../crates/utils/src/) (new file). Source of truth is Python's [`tracing.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/tracing.py); cross-SDK parity tests assert each Rust constant matches its Python counterpart.

### 3.4 Span name conventions

Match Python's structure: `cognee.<area>.<operation>`. Examples:

- `cognee.api.add` — handler for `POST /api/v1/add`.
- `cognee.api.cognify` — handler for `POST /api/v1/cognify`.
- `cognee.api.recall` — already implemented (commit 598d553); the recall parity attributes emit through this span.
- `cognee.cognify.extract_graph` — pipeline task.
- `cognee.search.graph_completion` — retriever.
- `cognee.db.query` — repository method.

These names appear as `root_name` in `/api/v1/activity/spans` results.

## 4. In-memory span buffer

### 4.1 Type & API

```rust
// crates/http-server/src/observability/span_buffer.rs

#[derive(Clone, Default)]
pub struct SpanBuffer {
    inner: Arc<Mutex<BufferInner>>,   // lock poison is unrecoverable
    config: Arc<BufferConfig>,
}

struct BufferInner {
    traces: HashMap<TraceId, Vec<RecordedSpan>>,
    trace_order: VecDeque<TraceId>,   // oldest at front
}

pub struct BufferConfig {
    pub max_traces: usize,             // default 50; matches Python _MAX_TRACES
    pub max_spans_per_trace: usize,    // default 1024; safety net
}

impl SpanBuffer {
    pub fn new(config: BufferConfig) -> Self;
    pub fn record(&self, span: RecordedSpan);
    pub fn all_traces(&self) -> Vec<TraceSummary>;     // shape used by /spans
    pub fn last_trace(&self) -> Option<TraceSummary>;
    pub fn clear(&self);
}
```

### 4.2 `RecordedSpan`

```rust
pub struct RecordedSpan {
    pub trace_id:        String,    // 32-char lowercase hex
    pub span_id:         String,    // 16-char lowercase hex
    pub parent_span_id:  Option<String>,
    pub name:            String,
    pub start_time_ns:   u64,
    pub end_time_ns:     u64,
    pub duration_ms:     f64,
    pub status:          SpanStatus,
    pub attributes:      serde_json::Map<String, serde_json::Value>,
}

pub enum SpanStatus { Unset, Ok, Error }
```

The fields **must** match Python's [`CogneeSpanExporter.export` dict shape](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/tracing.py) byte-for-byte so a frontend trace viewer renders identically against either backend.

### 4.3 `SpanBufferLayer` — the `tracing` integration

```rust
impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for SpanBufferLayer {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) { … }
    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) { … }
    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        // Build RecordedSpan from the span extension data, redact attributes,
        // push into SpanBuffer.
    }
}
```

Trace ids are synthesized: `tracing` doesn't have OTEL trace ids natively, so we generate one per *root span* (a span with no parent) and propagate it through child spans via a `tracing::Span::current().extensions_mut()` slot. This is conventional `tracing-opentelemetry` behavior, lifted into a small in-house helper to avoid the `opentelemetry-sdk` dep.

### 4.4 Eviction

LRU on `trace_order`. When a new trace pushes the count past `max_traces`, the oldest is evicted whole (not partially — partial eviction would split a trace across windows). Same as Python.

A safety cap on `max_spans_per_trace` prevents pathological producers from blowing memory on a single trace.

## 4.5. File logging

The HTTP server inherits the same file-logging behaviour as the CLI
(see the project README's "Logging" section). When deploying behind
a process supervisor (systemd, supervisord, Docker), prefer setting
`COGNEE_LOGS_DIR` to a host-mounted volume so logs persist across
restarts.

The in-memory `SpanBufferLayer` that powers `/spans` is **not**
mirrored to disk. To archive spans, scrape the `/spans` endpoint or
configure the OTEL exporter (see [`observability.md`](observability.md)).

Multi-process deployments: avoid running multiple HTTP-server
instances with the same `LOG_FILE_NAME` env var. The rotation is
not coordinated across processes and can corrupt the shared file.
Either set distinct `COGNEE_LOGS_DIR` per worker or `unset
LOG_FILE_NAME` in each worker's environment.

## 5. Secret redaction

Python redacts known secret patterns at *export* time (regex pass over span attributes — see [`redact_secrets`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/tracing.py)). We redact at *record* time inside `SpanBufferLayer::on_close` so:

- The buffered span is already redacted when read by `/spans`.
- Stdout logs (`fmt::Layer`) are *not* redacted by default — they're considered trusted (admin-only access). Operators who run multi-tenant log shipping can opt into redaction by enabling `RedactingFmtLayer` (a wrapper layer that runs the same regex pass before formatting).

### Redaction patterns (mirror Python)

```rust
const SECRET_PATTERNS: &[(&str, &str)] = &[
    (r"sk-[A-Za-z0-9]{20,}", "$0"),                          // OpenAI-style keys
    (r"(?i)(api[_-]?key\s*[=:]\s*)['\"]?[A-Za-z0-9\-_]{16,}['\"]?", "$1"),
    (r"(?i)(bearer\s+)[A-Za-z0-9\-_\.]{20,}", "$1"),
    (r"(?i)(password\s*[=:]\s*)['\"]?[^\s'\"]{8,}['\"]?", "$1"),
];

fn redact(value: &str) -> Cow<str> {
    // For each match: keep first 6 chars, replace the rest with "***REDACTED***".
}
```

Library: `regex` 1.x (already in workspace). Compile once into `OnceCell<Vec<Regex>>`.

Test fixtures cover each pattern with a known-bad string; cross-SDK parity test asserts the redacted output matches Python byte-for-byte.

## 6. Activity router endpoints

The activity router exposes 5 endpoints. Full per-router contracts will live in `routers/activity.md`; this doc nails the observability-relevant ones.

### 6.1 `GET /api/v1/activity/spans`

- **Auth**: required ([`get_authenticated_user`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/methods/get_authenticated_user.py)).
- **Source**: `state.spans.all_traces()`.
- **Response shape** (matches [Python](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py)):
  ```json
  [
    {
      "trace_id":     "0193…",
      "root_name":    "cognee.api.cognify",
      "duration_ms":  4823.7,
      "span_count":   42,
      "status":       "OK",
      "spans": [
        {
          "name": "cognee.api.cognify",
          "trace_id": "0193…",
          "span_id": "abcd…",
          "parent_span_id": null,
          "start_time_ns": 1730000000000000000,
          "end_time_ns":   1730000004823700000,
          "duration_ms":   4823.7,
          "status":        "OK",
          "attributes":    { "cognee.pipeline.name": "cognify_pipeline", … }
        },
        …
      ]
    },
    …
  ]
  ```
- **Errors**: Python catches all and returns `{"error": str}` (sic — not a list). We do the same for compat, but log the underlying error at `error!` level so it's visible.

### 6.2 `GET /api/v1/activity/pipeline-runs`

Belongs to the *durable* tier — joins `pipeline_runs` ⨝ `datasets` ⨝ `users`. See [pipelines.md §5](pipelines.md#5-database-persistence--pipeline_runs-table) for the table; per-router doc for the JOIN shape.

### 6.3 `GET /api/v1/activity/users`, `GET /api/v1/activity/agents`, `GET /api/v1/activity/export/{dataset_id}`

Not observability-specific. Covered in `routers/activity.md`.

## 7. Access logging

`tower_http::trace::TraceLayer::new_for_http()` wraps the router. Default fields per request:

| Field | Source |
|---|---|
| `method` | `Request::method` |
| `uri` | `Request::uri` (path + query) |
| `status` | Response status |
| `latency_ms` | Wall time between request received and response started |
| `user_id` | Set by the `AuthenticatedUser` extractor on success (added via `tracing::Span::current().record(...)`) |
| `pipeline_run_id` | Set by handlers that initiate or look up runs |
| `request_id` | Generated UUID v4 per request; emitted as `X-Request-ID` response header |

In `prod` (`ENV=prod`), the `fmt::Layer` is configured with `.json()` so each line is a JSON object that ingests into Datadog / Loki / Splunk without further parsing.

In `dev`, pretty multi-line output for human readability.

`/health`, `/`, and `/openapi.json` use a `tower::filter::FilterLayer` to drop log lines below `warn` — these endpoints get hammered by load balancers and would otherwise drown the access log. Errors and slow responses still log normally.

## 8. Configuration

Env vars consumed by the binary's tracing setup:

| Var | Default | Purpose |
|---|---|---|
| `RUST_LOG` | `info,ort=warn` | `tracing-subscriber` env filter |
| `ENV` | `prod` | `prod` → JSON logs; `dev` → pretty |
| `COGNEE_SPAN_BUFFER_MAX_TRACES` | `50` | LRU cap on the in-memory buffer |
| `COGNEE_SPAN_BUFFER_MAX_SPANS_PER_TRACE` | `1024` | Safety cap |
| `COGNEE_LOG_REDACT_FMT` | `false` | If `true`, redact stdout logs as well as the buffer |

Library consumers do not see these — they construct a `BufferConfig` directly.

## 9. Trait abstraction for future OTLP export

```rust
#[async_trait]
pub trait SpanSink: Send + Sync {
    async fn record(&self, span: RecordedSpan);
}
```

`SpanBuffer` is the default `SpanSink`. A future `OtlpSpanSink` (hidden behind a `tracing-otlp` feature) can be plugged in alongside. The `SpanBufferLayer` will fan out `record()` to every registered sink.

This trait is **not** exposed in phase 1 — we ship a concrete `SpanBuffer`. The trait gets pulled out of `cognee-http-server` and into a public API the day we add OTLP. No call sites change.

## 10. Testing strategy

| Layer | Tests |
|---|---|
| Unit | `SpanBuffer::record` LRU eviction; `SpanBuffer::all_traces` JSON shape; redaction regex matrix; trace-id propagation through `tracing` parent/child spans. |
| Layer | Drive a small `tracing` workload through `SpanBufferLayer`; assert the buffered traces match the expected hierarchy. |
| Integration | Hit `/api/v1/activity/spans` after invoking a real handler; assert response shape; assert `cognee.api.<name>` root span is present. |
| Cross-SDK | For a fixed sequence of operations, snapshot both Python's and Rust's `/spans` JSON and assert structural equality (modulo timestamps + ids). |
| Redaction | Span attribute containing `Authorization: Bearer eyJ…` → `/spans` returns the value with the bearer token redacted; same for `sk-…`, `password=…`, `api_key=…`. |

Test fixtures: `crates/http-server/tests/fixtures/spans/`.

## 11. Open questions

1. **Per-tenant span buffer**: the buffer is global. A tenant viewing `/spans` sees other tenants' traces too. Python has the same issue (it's an admin debug endpoint). Should we filter by `user_id` extracted from span attributes? Defer until the multi-tenant story (see [tenants.md](tenants.md)) lands.
2. **Span sampling**: every span is recorded. For high-traffic endpoints (the load-balancer health probe is filtered out, but `/datasets/status` polling can be heavy) we may want adaptive sampling. Defer; trivial to add as a `tracing-subscriber` filter.
3. **Buffer persistence across restarts**: currently the buffer is volatile. Do we want to flush to disk on shutdown so post-mortem debugging works? Probably no — durable observability is what the `pipeline_runs` table is for. Keep volatile.
4. **OTLP export timing**: when does the deployment story justify pulling in `opentelemetry-sdk`? Likely tied to the multi-replica deployment doc. Track in [pipelines.md §15](pipelines.md#15-open-questions).
5. **Span ID format**: Python uses lowercase hex (16 chars span, 32 chars trace). Rust `tracing` has no native ids; we synthesize them. Confirm the format is identical so frontend trace viewers render correctly with both backends — covered by cross-SDK parity tests.
6. **`status` enum representation**: Python uses `"OK" | "ERROR" | "UNSET"` strings. Rust serializes the same enum with `#[serde(rename_all = "UPPERCASE")]` to match.

## 12. References

- Python tracing module: [`cognee/modules/observability/tracing.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/tracing.py).
- Python trace context: [`cognee/modules/observability/trace_context.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/observability/trace_context.py).
- Activity router: [`cognee/api/v1/activity/routers/get_activity_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py).
- Architectural reference: [architecture.md §12](architecture.md#12-logging--observability).
- Durable observability tier: [pipelines.md](pipelines.md).
- Existing CLI tracing setup: [`crates/cli/src/main.rs`](../../crates/cli/src/main.rs).
- `tracing` crate: [https://docs.rs/tracing/](https://docs.rs/tracing/).
- `tracing-subscriber`: [https://docs.rs/tracing-subscriber/](https://docs.rs/tracing-subscriber/).
