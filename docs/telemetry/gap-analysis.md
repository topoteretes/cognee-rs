# Telemetry Gap Analysis: Rust vs Python Cognee

> **Status (2026-06): RESOLVED.** All telemetry pillars analysed here have since
> been implemented. **OpenTelemetry/OTLP export** now lives in the
> `cognee-observability` crate (`crates/observability/src/init.rs`,
> `guard.rs`, `settings.rs`), wired to the existing `OTEL_*` config fields.
> **Product-analytics event tracking** (`send_telemetry`) is implemented in the
> `cognee-telemetry` crate (`crates/telemetry/src/`: `lib.rs`, `real.rs`,
> `client.rs`, `ids.rs`, `payload.rs`, `env.rs`), including the `prometh.ai`
> proxy, anon/persistent ID files, PBKDF2 api-key tracking ID, and
> `TELEMETRY_DISABLED`/`ENV` opt-out. Sections 1 and 2 below describe the
> *pre-implementation* state and are retained for historical context; the
> "Completed work" list near the bottom tracks the closures.

## Summary

The Rust port had solid **structured tracing infrastructure** (51 instrumented spans, semantic attributes, in-memory ring buffer, redaction layer, observability HTTP API) — in some ways the span scaffolding is *cleaner* than Python's. At the time of this analysis it was missing two whole telemetry pillars that Python ships with: **OpenTelemetry/OTLP export** and **product-analytics event tracking** (the `send_telemetry` proxy). Both — and pipeline-status persistence — have since been implemented (see status note above and the "Completed work" section).

---

## 1. OpenTelemetry / OTLP Export — RESOLVED (was biggest gap)

> **Implemented** in the `cognee-observability` crate. `init_telemetry()`
> (`crates/observability/src/init.rs`) builds an `SdkTracerProvider`, installs
> a `tracing-opentelemetry` bridge layer, and exports via OTLP gRPC/HTTP using
> the `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`,
> `opentelemetry-semantic-conventions`, and `tracing-opentelemetry` deps
> (see `crates/observability/Cargo.toml`). The `OTEL_*` config fields are now
> wired end-to-end (no longer a no-op). The table below reflects the original
> pre-implementation snapshot.

| | Python | Rust |
|---|---|---|
| `opentelemetry-api/sdk` deps | Yes — optional `[tracing]` extra | No crate |
| OTLP gRPC/HTTP exporter | Yes — `_try_add_otlp_exporter()` in `cognee/modules/observability/tracing.py` | None |
| `TracerProvider` w/ `Resource` (service.name, version, env) | Yes | No |
| Console span exporter (debug) | Yes — `ConsoleSpanExporter` | Partial — stdout via `tracing-subscriber::fmt`, *not* OTEL spans |
| Auto-detection of external instrumentation (Datadog, Dash0) | Yes | No |
| `setup_tracing()` / `enable_tracing()` runtime API | Yes — in `trace_context.py` | No |
| Config fields | Used | **Fields exist in [config.rs:467-475](../../crates/lib/src/config.rs#L467-L475) but never wired to anything** |

**Effective gap:** `OTEL_EXPORTER_OTLP_ENDPOINT` is a no-op in Rust today. To match Python you need `opentelemetry`, `opentelemetry-otlp`, `opentelemetry_sdk`, `tracing-opentelemetry` and a bridge layer added to the subscriber.

---

## 2. Product Analytics (`send_telemetry`) — RESOLVED (was completely missing)

> **Implemented** in the `cognee-telemetry` crate. `send_telemetry()` /
> `try_send_telemetry()` (`crates/telemetry/src/lib.rs`, dispatched via
> `real.rs`) fire async fire-and-forget events to the same
> `https://test.prometh.ai` proxy (`env.rs::proxy_url`). The three identity
> layers are in `ids.rs`: `get_anonymous_id` (`<project_root>/.anon_id`),
> `get_persistent_id` (`~/.cognee/.persistent_id`), and `get_api_key_tracking_id`
> (PBKDF2-HMAC-SHA256). Opt-out via `TELEMETRY_DISABLED` and `ENV=test|dev`
> lives in `env.rs::is_disabled`. The `cognee.forget` event is emitted from
> `crates/lib/src/api/forget.rs` (gated behind the `telemetry` feature); the
> event payload shape is in `payload.rs`. The pre-implementation description
> below is retained for historical context.

Python ships a **custom telemetry proxy** at `https://test.prometh.ai` (NOT PostHog — that's a declared-but-unused optional dep). Events:

- Pipeline lifecycle: `Pipeline Run Started/Completed/Errored`
- Per-task: `${task_type} Task Started/Completed/Errored`
- API ops: `cognee.recall`, `cognee.improve`, `cognee.forget`

Identity layers sent with every event:

- `anonymous_id` from `.anon_id` (project-root)
- `persistent_id` from `~/.cognee/.persistent_id` (machine-level)
- `api_key_tracking_id` — PBKDF2-SHA256 of LLM API key with configurable salt
- transient `user_id`

Opt-out: `TELEMETRY_DISABLED=1`, auto-disabled when `ENV=test|dev`.

**Rust:** zero equivalent. No proxy client, no event helper, no anon/persistent ID files, no `TELEMETRY_DISABLED` knob, no PBKDF2 key-tracking ID.

---

## 3. Pipeline Run Status Persistence

✅ **Implemented in [gap 08](08-pipeline-run-status.md).** Rust now
writes the full four-state Python lifecycle (`INITIATED → STARTED →
COMPLETED | ERRORED`) for every cognify / memify / ingestion run,
regardless of whether the run originated from the HTTP server, the
CLI, or an embedded library caller. `pipeline_runs.dataset_id` is
nullable and FK-less to match Python; `run_info` JSON is
byte-identical to Python's shape (`{"data": [...]}` on
`STARTED` / `COMPLETED`, `{"data": [...], "error": "..."}` on
`ERRORED`, `{}` on `INITIATED`). The Rust library exposes
`reset_pipeline_run_status` and `reset_dataset_pipeline_run_status`
helpers under `cognee_lib::api::pipeline_runs`, plus three reader
helpers (`get_pipeline_run`, `get_pipeline_run_by_dataset`,
`get_pipeline_runs_by_dataset`) on `PipelineRunRepository`. The
cognify and memify entry points consult
`check_pipeline_run_qualification` to short-circuit
already-completed datasets and reject already-running ones. Per-
DataPoint provenance (`source_pipeline`, `source_task`,
`source_user`, `source_node_set`, `source_content_hash`) was closed
earlier by [gap 05](05-datapoint-provenance.md). Cross-SDK parity is
asserted by
[`e2e-cross-sdk/harness/test_pipeline_runs_parity.py`](../../e2e-cross-sdk/harness/test_pipeline_runs_parity.py).
See the [gap-08 closure summary](08-pipeline-run-status.md#closure-summary)
for the per-commit audit trail.

---

## 4. LLM / DB Span Coverage — closed by gap 04

Python instruments the active graph adapter (Neo4j or Ladybug) and the
LanceDB vector adapter via `new_span("cognee.db.{graph,vector}.*")`
with `cognee.db.system` / `cognee.db.query` / `cognee.db.row_count`
attributes. LLM adapters add `cognee.llm.{model,provider}` to their
surrounding span.

**Rust:** closed in
[`04-db-adapter-instrumentation.md`](04-db-adapter-instrumentation.md).
Spans are emitted by `QdrantAdapter`, `LadybugAdapter`, `OpenAIAdapter`
(host), `LiteRtAdapter` (Android), `PgVectorAdapter`, and every public
function in `crates/database/src/ops/*.rs`. Per-method
`pg_graph_adapter` spans are deferred (see the
[gap 04 closure summary](04-db-adapter-instrumentation.md#known-follow-ups)
for the rationale). The `redact_secrets` helper now lives at
[`cognee_utils::redact::redact`](../../crates/utils/src/redact.rs) so
adapter crates can call it without depending on `cognee-http-server`.
Constants are consolidated under
[`cognee_utils::tracing_keys`](../../crates/utils/src/tracing_keys.rs);
[`cognee_search::observability`](../../crates/search/src/observability.rs)
is a re-export shim for backwards compatibility with existing search
call sites.

---

## 5. Logging

| | Python | Rust |
|---|---|---|
| Framework | structlog + stdlib logging | `tracing` + `tracing-subscriber` |
| File output | `PlainFileHandler`, 50MB rotation, 5 backups | ✅ Implemented in [gap 06](06-file-logging-rotation.md) — `tracing-appender::RollingFileAppender` with daily time-based rotation (size-based deferred) |
| Log dir resolution (`COGNEE_LOGS_DIR`, BaseConfig) | Yes | ✅ Implemented in [gap 06](06-file-logging-rotation.md) — see [`cognee_logging::resolve_logs_dir`](../../crates/logging/src/paths.rs) |
| Suppression of noisy libs (litellm, openai) | Yes | ✅ Implemented in [gap 06](06-file-logging-rotation.md) — broad library-noise default filter applied when `RUST_LOG`/`LOG_LEVEL` are unset |

---

## 6. Bindings (capi/python/js/android)

Python SDK auto-initializes telemetry on import. ✅ **Implemented in
[gap 07](07-bindings-auto-init.md)** — Rust bindings now ship
auto-init for the default tracing bridge plus explicit
`setup_logging()` (gap 06), `setup_telemetry()` (gap 07),
`setup_telemetry_analytics()` (gap 07) entrypoints. PyO3 bridges
into Python's `logging` via `pyo3-log`; Neon writes a stderr fmt
subscriber by default; C API stays fully explicit (with a panic
hook installed by `cg_init` for FFI debuggability). Auto-init can
be suppressed via `COGNEE_BINDING_SUPPRESS_LOGS=1`.

---

## 7. Things Rust has that Python doesn't

- **In-memory bounded ring buffer with redaction layer** — [span_buffer_layer.rs](../../crates/http-server/src/observability/span_buffer_layer.rs), governed by `COGNEE_SPAN_BUFFER_MAX_TRACES` / `_MAX_SPANS_PER_TRACE`. Python has a similar `CogneeSpanExporter` but the Rust version is hooked directly into the `tracing` Layer trait — closer to OTLP-ready.
- **`/api/v1/activity/spans` HTTP endpoint** to dump live spans. Python doesn't expose this over HTTP.
- **`telemetry` cargo feature flag** for compile-time opt-out (Python is runtime-only).

---

## Detailed Inventory — Rust Side

### Tracing setup

| Component | File | Lines | Notes |
|---|---|---|---|
| Workspace deps | [Cargo.toml](../../Cargo.toml) | 99-100 | `tracing = "0.1"`, `tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }` |
| CLI subscriber init | [crates/cli/src/main.rs](../../crates/cli/src/main.rs#L50-L58) | 50-58 | `tracing_subscriber::fmt()` with `EnvFilter`, stdout |
| HTTP server init | [crates/http-server/src/main.rs](../../crates/http-server/src/main.rs#L100-L118) | 100-118 | Layered Registry + fmt layer + SpanBufferLayer |

### #[tracing::instrument] coverage (51 total)

- HTTP routers: 46 spans across `search`, `settings`, `checks`, `responses`, `configuration`, `llm`, `notebooks`, `activity`, `sessions`, `recall`, `permissions`, `remember`, `visualize`
- Core/search/LLM: 5 spans in retrievers, search orchestrator, OpenAI adapter
- Delete operations: 6 spans in [crates/delete/src/lib.rs](../../crates/delete/src/lib.rs)
- Manual spans: pipeline task ([crates/core/src/pipeline.rs:971](../../crates/core/src/pipeline.rs#L971)), recall ([crates/lib/src/api/recall.rs:128](../../crates/lib/src/api/recall.rs#L128)), HTTP middleware

### Semantic attribute constants

- [crates/search/src/observability.rs](../../crates/search/src/observability.rs) — 18 `cognee.*` constants
- [crates/utils/src/tracing_keys.rs](../../crates/utils/src/tracing_keys.rs) — 13 `cognee.*` constants

These mirror Python's namespaces. Every key is now consumed by at least one call site after gap 04 closure (Qdrant, Ladybug, pgvector, OpenAI, LiteRT, and the relational ops layer in `crates/database/src/ops/*.rs`).

### Telemetry feature flag

- Definition: [crates/lib/Cargo.toml:37-41](../../crates/lib/Cargo.toml#L37-L41) — `telemetry = []` (opt-in)
- Pipeline span recording gated at [crates/core/src/pipeline.rs:970-1069](../../crates/core/src/pipeline.rs#L970-L1069)
- Forget event emission gated at [crates/lib/src/api/forget.rs:103-123](../../crates/lib/src/api/forget.rs#L103-L123)

### In-memory span buffer

| Component | File | Purpose |
|---|---|---|
| Orchestration | [crates/http-server/src/observability/mod.rs](../../crates/http-server/src/observability/mod.rs) | Re-exports |
| Ring buffer | [crates/http-server/src/observability/span_buffer.rs](../../crates/http-server/src/observability/span_buffer.rs) | LRU bounded buffer (default 50 traces, 1024 spans/trace) |
| Tracing layer | [crates/http-server/src/observability/span_buffer_layer.rs](../../crates/http-server/src/observability/span_buffer_layer.rs) | Implements `tracing::Layer` |
| Redaction | [crates/http-server/src/observability/redaction.rs](../../crates/http-server/src/observability/redaction.rs) | Masks PII/secrets |

### Activity API

| Endpoint | Backing | File |
|---|---|---|
| `GET /api/v1/activity/spans` | In-memory ring buffer | [crates/http-server/src/routers/activity.rs](../../crates/http-server/src/routers/activity.rs) |
| `GET /api/v1/activity/pipeline-runs` | DB-backed (pipeline_runs ⨝ datasets ⨝ users) | same |
| `GET /api/v1/activity/users` | DB | same |
| `GET /api/v1/activity/agents` | DB | same |
| `GET /api/v1/activity/export/{dataset_id}` | DB → markdown | same |

### Environment variables

| Var | File | Default | Status |
|---|---|---|---|
| `RUST_LOG` | [crates/cli/src/main.rs:53-54](../../crates/cli/src/main.rs#L53-L54) | `info,ort=warn` | Used |
| `RUST_LOG` | [crates/http-server/src/main.rs:106-107](../../crates/http-server/src/main.rs#L106-L107) | `info,ort=warn` | Used |
| `COGNEE_SPAN_BUFFER_MAX_TRACES` | [crates/http-server/src/observability/span_buffer.rs:79-81](../../crates/http-server/src/observability/span_buffer.rs#L79-L81) | 50 | Used |
| `COGNEE_SPAN_BUFFER_MAX_SPANS_PER_TRACE` | same | 1024 | Used |
| `COGNEE_TRACING_ENABLED` | crates/lib/src/config.rs | false | Used (drives `is_tracing_enabled` in `cognee-observability`) |
| `OTEL_SERVICE_NAME` | crates/lib/src/config.rs | empty | Used (OTEL resource `service.name`) |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | crates/lib/src/config.rs | empty | Used (OTLP exporter endpoint) |
| `OTEL_EXPORTER_OTLP_HEADERS` | crates/lib/src/config.rs | empty | Used (OTLP exporter headers) |
| `TELEMETRY_DISABLED` | crates/telemetry/src/env.rs | unset | Used (product-analytics opt-out) |

### Crate coverage

- **Crates with `tracing`:** `core`, `ingestion`, `search`, `delete`, `database`, `http-server`, `cli`, `llm`, `embedding`, `vector`, `graph`, `storage`, `session`, `chunking`, `cognify`, `cloud`, `visualization`
- **Crates without `tracing`:** `models`, `ontology`, `test-utils`, `utils`
- **`opentelemetry` deps now present** in the `cognee-observability` crate
  (`opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`,
  `opentelemetry-semantic-conventions`, `tracing-opentelemetry`; feature-gated).
  Dedicated `prometheus`/`metrics` exporters remain absent (see "Future work").

### Bindings

> **Updated:** auto-init landed in gap 07 (see section 6). The "None" entries
> below were the pre-implementation snapshot.

| Binding | Telemetry init (current) |
|---|---|
| Python (PyO3) | Auto-init tracing bridge into Python `logging` via `pyo3-log`; explicit `setup_logging` / `setup_telemetry` / `setup_telemetry_analytics` |
| C API | Explicit (`cg_init` installs a panic hook); caller owns observability |
| JS (Neon) | Auto-init stderr fmt subscriber by default |

---

## Detailed Inventory — Python Side

### Core tracing module

`cognee/modules/observability/tracing.py` (373 lines)

- `setup_tracing(console_output)` — auto-detects external auto-instrumentation (Datadog, Dash0); creates `TracerProvider` with `Resource` (service.name, service.version, deployment.environment); attaches `CogneeSpanExporter` + optional OTLP + optional console
- `CogneeSpanExporter` — bounded in-memory buffer (last 50 traces), thread-safe, exposes `get_last_trace_spans()` / `get_all_traces()` / `clear()`
- `redact_secrets()` — masks `sk-xxx` API keys, bearer tokens, passwords; applied to query attributes
- `CogneeTrace.summary()` / `tree()` — operation breakdown and hierarchical view

### Tracing enablement API

`cognee/modules/observability/trace_context.py`

- `enable_tracing(console_output=False)` — initializes provider, sets module flag
- `disable_tracing()` — shuts down provider
- `is_tracing_enabled()` — checks module flag → config → `COGNEE_TRACING_ENABLED` env var
- `get_last_trace()` / `get_all_traces()` / `clear_traces()`

### Decorators / wrappers

`cognee/modules/observability/get_observe.py`

- `@observe(as_type="generation"|"transcription"|...)` — wraps Langfuse `@observe` plus OTEL span; sync + async; sets `cognee.span.category`
- `new_span()` context manager (in `__init__.py`) — yields OTEL span when enabled, else `_NullSpan`

### Observer enum

`cognee/modules/observability/observers.py` — `NONE | LANGFUSE | LLMLITE | LANGSMITH`

### Custom telemetry proxy (`send_telemetry`)

`cognee/shared/utils.py`

- Proxy URL: `https://test.prometh.ai`
- Three identity layers: `anonymous_id` (`.anon_id` file), `persistent_id` (`~/.cognee/.persistent_id`), `api_key_tracking_id` (PBKDF2-SHA256 of LLM key, 100k iterations, configurable salt)
- Async fire-and-forget HTTP via `aiohttp`
- Auto-disabled when `ENV in {test, dev}` or `TELEMETRY_DISABLED` set

### Events sent

Pipeline (`run_tasks_with_telemetry.py`):

- `Pipeline Run Started`, `Pipeline Run Completed`, `Pipeline Run Errored` — pipeline_name, cognee_version, tenant_id, config

Tasks (`run_tasks_base.py:120-193`):

- `${task_type} Task Started/Completed/Errored` — task_name, cognee_version, tenant_id

API (`api/v1/...`):

- `cognee.recall` — query_length, scope, auto_route, top_k, search_type, session_id, datasets, cognee_version
- `cognee.improve` — dataset, session_count, session_ids, run_in_background, cognee_version
- `cognee.forget` — target, dataset, data_id, cognee_version

### Pipeline run status DB

`cognee/modules/pipelines/models/PipelineRun.py`

```python
class PipelineRunStatus(enum.Enum):
    DATASET_PROCESSING_INITIATED  = "DATASET_PROCESSING_INITIATED"
    DATASET_PROCESSING_STARTED    = "DATASET_PROCESSING_STARTED"
    DATASET_PROCESSING_COMPLETED  = "DATASET_PROCESSING_COMPLETED"
    DATASET_PROCESSING_ERRORED    = "DATASET_PROCESSING_ERRORED"
```

Operations: `log_pipeline_run_initiated`, `log_pipeline_run_start`, `log_pipeline_run_complete`, `log_pipeline_run_error`. `run_info` JSON column for flexible metadata.

Provenance per output DataPoint: `source_pipeline`, `source_task`, `source_user`, `source_node_set`, `source_content_hash`.

### Logging

`cognee/shared/logging_utils.py` (630 lines)

- structlog + stdlib logging; custom `PlainFileHandler` (50MB rotation, 5 backups by default, max 10 files)
- Log dir priority: `COGNEE_LOGS_DIR` → `BaseConfig.logs_root_directory` → `/tmp/cognee_logs`
- Env: `COGNEE_LOG_FILE`, `COGNEE_LOG_MAX_BYTES`, `COGNEE_LOG_BACKUP_COUNT`, `LOG_LEVEL`, `COGNEE_LOGS_DIR`, `LOG_FILE_NAME`, `MAX_LOG_FILES`, `COGNEE_CLI_MODE`, `LITELLM_LOG`, `LITELLM_SET_VERBOSE`

### Config / opt-out

`cognee/base_config.py`

```python
cognee_tracing_enabled: bool = ...              # COGNEE_TRACING_ENABLED
otel_service_name: str = ... ("cognee")          # OTEL_SERVICE_NAME
otel_exporter_otlp_endpoint: Optional[str] = ... # OTEL_EXPORTER_OTLP_ENDPOINT
otel_exporter_otlp_headers: Optional[str] = ...  # OTEL_EXPORTER_OTLP_HEADERS
monitoring_tool: object = Observer.NONE
```

### Dependencies (`pyproject.toml`)

- Core: `structlog>=25.2.0,<26`
- Optional `[tracing]`: `opentelemetry-api`, `opentelemetry-sdk`, `opentelemetry-exporter-otlp-proto-grpc`, `opentelemetry-exporter-otlp-proto-http`
- Optional `[monitoring]`: above + `sentry-sdk[fastapi]`, `langfuse`
- Optional `[posthog]`: `posthog>=3.5.0,<4` (declared but never imported)

### Tests

- `cognee/tests/test_telemetry.py`
- `cognee/tests/unit/shared/test_telemetry_tracking.py`
- `cognee/tests/unit/modules/observability/test_tracing.py`
- `cognee/tests/unit/modules/observability/test_get_observe.py`

---

## Prioritized Gap List

> **All resolved.** Every prioritized item below has been closed. Each remains
> linked to its dedicated sub-document for the investigation/design/action-item
> trail. Strikethrough marks completion.

1. ~~**Implement `send_telemetry()` analytics client**~~ — done. Proxy URL,
   anon/persistent ID files, PBKDF2 api-key tracking ID, opt-out semantics, and
   async fire-and-forget HTTP all implemented in the `cognee-telemetry` crate
   (`crates/telemetry/src/`). → [02-send-telemetry-analytics.md](02-send-telemetry-analytics.md)
2. ~~**Emit pipeline & task lifecycle events**~~ — done. `Pipeline Run
   Started/Completed/Errored`, per-task variants, and API events
   (`cognee.recall`, `cognee.improve`, `cognee.forget`) are emitted (e.g.
   `crates/lib/src/api/forget.rs`). → [03-pipeline-task-api-events.md](03-pipeline-task-api-events.md)
3. ~~**File logging with rotation**~~ — done. `COGNEE_LOG_FILE`,
   `COGNEE_LOGS_DIR`, `LOG_LEVEL`, rotating non-blocking appender, and library
   noise suppression in the `cognee-logging` crate. → [06-file-logging-rotation.md](06-file-logging-rotation.md)
4. ~~**Auto-init tracing in bindings**~~ — done. PyO3, Neon, and C API entry
   points auto-init (see section 6). → [07-bindings-auto-init.md](07-bindings-auto-init.md)
5. ~~**Pipeline run status lifecycle**~~ — closed by [gap 08](08-pipeline-run-status.md). Full four-state lifecycle, Python-shaped `run_info`, library-pipeline coverage (cognify / memify / ingestion), qualification gate, and reset helpers all landed. See the [gap-08 closure summary](08-pipeline-run-status.md#closure-summary).

### Completed work

- ✅ **Wire OpenTelemetry SDK + OTLP exporter** — wired the existing `OTEL_*` config fields end-to-end: `init_telemetry`, `tracing-opentelemetry` bridge, OTLP gRPC/HTTP exporters, RAII flush guard, CLI + HTTP server subscriber composition, unit + integration tests, CI lanes, user docs. → [01-otel-otlp-export.md](01-otel-otlp-export.md) (complete — see commits `8cc50bb..0fc9adb`).
- ✅ **Instrument DB / LLM adapters with spans + attributes** — Qdrant,
  Ladybug, pgvector, SeaORM ops, OpenAI, LiteRT now emit
  `cognee.db.{vector,graph,relational}.*` and `cognee.llm.*` spans.
  Redaction helper relocated to `cognee-utils`. Constants consolidated.
  → [04-db-adapter-instrumentation.md](04-db-adapter-instrumentation.md)
  (complete — see the
  [closure summary](04-db-adapter-instrumentation.md#closure-summary)).
- ✅ **Provenance stamping on DataPoints** — every DataPoint
  emitted by the pipeline executor now carries `source_pipeline`,
  `source_task`, `source_user`, `source_node_set`,
  `source_content_hash`, mirroring Python. Vector-store payloads
  carry the full DataPoint dump.
  → [05-datapoint-provenance.md](05-datapoint-provenance.md)
  (complete — see the
  [closure summary](05-datapoint-provenance.md#closure-summary)).
- ✅ **Pipeline run status lifecycle (gap 08).** Full four-state
  `INITIATED → STARTED → COMPLETED | ERRORED` trail for every
  cognify / memify / ingestion run across HTTP, CLI, and library
  surfaces. `pipeline_runs.dataset_id` nullable + FK-less, `run_info`
  JSON byte-identical to Python, reset helpers and reader trio
  exposed under `cognee_lib::api::pipeline_runs`,
  `check_pipeline_run_qualification` gates cognify / memify against
  re-runs. → [08-pipeline-run-status.md](08-pipeline-run-status.md)
  (complete — see the
  [closure summary](08-pipeline-run-status.md#closure-summary)).
- ✅ **Route convenience pipelines through the executor (LIB-06).**
  `cognify::cognify` (standard + temporal branches),
  `cognify::memify::memify`, and `ingestion::AddPipeline::add` now
  call `cognee_core::pipeline::execute` instead of running tasks
  inline. Unblocks `PipelineWatcher` lifecycle events for library
  callers — prerequisite for gap-08 task 07 (`pipeline_runs` audit
  trail) and the LIB-06 payload-event mechanism. →
  [lib-06-executor-routed-convenience.md](lib-06-executor-routed-convenience.md)
  (complete — see the
  [closure summary](lib-06-executor-routed-convenience.md#closure-summary)).

---

## Future work / out of scope

Items intentionally not addressed by the eight gaps above. Captured here so they aren't lost.

> **Operator reference:** The canonical operator-facing documentation for
> tracing configuration and recipes (env vars, Grafana Tempo, Honeycomb,
> Dash0, in-cluster Collector) lives at
> [`../observability/opentelemetry.md`](../observability/opentelemetry.md).
> This `docs/telemetry/` folder remains the engineering gap-analysis space.

- **OpenTelemetry metrics export.** Python's `monitoring` extra ships only OTEL traces, no metric points; the `tracing` crate likewise produces spans only. A future initiative could add `opentelemetry_sdk::metrics::SdkMeterProvider` and emit counters/histograms (e.g. pipeline-run duration, search latency, embedding-batch sizes) via the same OTLP endpoint.
- **OpenTelemetry logs export.** The OTEL log signal is stable in `opentelemetry_sdk` 0.31; bridging `tracing` events (not spans) to `OTEL_EXPORTER_OTLP_ENDPOINT` would let operators consolidate all telemetry on one collector. Not in scope of [01-otel-otlp-export.md](01-otel-otlp-export.md), which covers traces only.
- **Replacing `SpanBufferLayer` with an OTEL in-memory exporter.** Could unify the `/api/v1/activity/spans` endpoint with the OTEL pipeline, but would lose byte-for-byte parity with Python's `CogneeSpanExporter` ring buffer that the current test suite depends on. Not worth it today.
- **Cross-SDK OTEL parity test.** Extend [`e2e-cross-sdk/`](../../e2e-cross-sdk) with an `otel-collector` service in `docker-compose.yml`, point both Python and Rust at it, and assert both SDKs emit comparable span sets for the same operation. Follow-up to [01-otel-otlp-export.md](01-otel-otlp-export.md).
- **Search lifecycle mockito test inside `crates/search/`.** The gap-03 integration suite covers the four pipeline + task lifecycle events ([`crates/core/tests/pipeline_telemetry_events.rs`](../../crates/core/tests/pipeline_telemetry_events.rs)) but does not assert the `cognee.search EXECUTION STARTED` / `EXECUTION COMPLETED` pair from `crates/search/src/orchestration/search_orchestrator.rs`. The cross-SDK byte-parity harness covers `EXECUTION COMPLETED`; adding a small in-crate mockito test for both events is a low-priority follow-up. See [03-pipeline-task-api-events.md → Known follow-ups](03-pipeline-task-api-events.md#known-follow-ups) for context.
- **Wire `Pipeline::telemetry_settings` from production SDK paths.** LIB-06 ([`lib-06-executor-routed-convenience.md`](lib-06-executor-routed-convenience.md)) closed on `b5ccc96` and routed `cognify`, `memify`, `AddPipeline::add` (both standard and temporal cognify branches) through `cognee_core::pipeline::execute()`. The `Pipeline.telemetry_settings` carrier now fires for library paths as part of the `Pipeline Run *` emission inside `execute()`. The companion `DbPipelineWatcher` wiring (so the events actually land in `pipeline_runs`) is gap-08 task 07.
