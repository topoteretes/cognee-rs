# Telemetry Gap Analysis: Rust vs Python Cognee

## Summary

The Rust port has solid **structured tracing infrastructure** (51 instrumented spans, semantic attributes, in-memory ring buffer, redaction layer, observability HTTP API) — in some ways the span scaffolding is *cleaner* than Python's. But it is missing two whole telemetry pillars that Python ships with: **OpenTelemetry/OTLP export** and **product-analytics event tracking** (the `send_telemetry` proxy). Pipeline-status persistence is also incomplete.

---

## 1. OpenTelemetry / OTLP Export — biggest gap

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

## 2. Product Analytics (`send_telemetry`) — completely missing

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

| | Python | Rust |
|---|---|---|
| `pipeline_runs` table with `PipelineRunStatus` enum | Yes — `cognee/modules/pipelines/models/PipelineRun.py` | Partial — see [crates/core/src/pipeline_run_registry/](../../crates/core/src/pipeline_run_registry/) |
| Statuses: `INITIATED`, `STARTED`, `COMPLETED`, `ERRORED` | Yes — four explicit `log_pipeline_run_*` ops | Verify Rust enum & API parity |
| `run_info` JSON column for flexible metadata | Yes | Verify |
| Provenance stamping per DataPoint (source_pipeline, source_task, source_user, source_node_set, source_content_hash) | Yes — in `run_tasks_base.py` | Not found |

The Rust http-server has `/api/v1/activity/pipeline-runs` reading a DB table, so *some* persistence exists, but the Python four-state lifecycle and provenance stamping should be verified against the Rust pipeline implementation.

---

## 4. LLM / DB Span Coverage

Python instruments **every** LLM adapter via `@observe(as_type="generation"|"transcription")` and sets `cognee.llm.model`/`cognee.llm.provider`. Vector and graph adapters (LanceDB, Neo4j, Ladybug, Kuzu) wrap queries in `cognee.db.{vector,graph}.{search,query}` spans with `cognee.db.system`, `cognee.db.query` (with `redact_secrets`), `cognee.db.row_count`.

**Rust status:**

- LLM: [openai.rs:138,729](../../crates/llm/src/adapters/openai.rs#L138) has `llm.api_call` / `llm.transcription_api_call`
- Verify `cognee.llm.model` / `cognee.llm.provider` are set as attributes (not just span name)
- **No equivalent spans on QdrantAdapter, LadybugAdapter, SqliteDatabase queries** — Python's `cognee.db.*` instrumentation is not mirrored. [crates/utils/src/tracing_keys.rs](../../crates/utils/src/tracing_keys.rs) defines the constants but no call sites use them.
- No query-text redaction utility (Python has `redact_secrets()` in `cognee/modules/observability/tracing.py`)

---

## 5. Logging

| | Python | Rust |
|---|---|---|
| Framework | structlog + stdlib logging | `tracing` + `tracing-subscriber` |
| File output | `PlainFileHandler`, 50MB rotation, 5 backups | Stdout only |
| Log dir resolution (`COGNEE_LOGS_DIR`, BaseConfig) | Yes | No |
| Suppression of noisy libs (litellm, openai) | Yes | Partial via `RUST_LOG="info,ort=warn"` |

---

## 6. Bindings (capi/python/js/android)

Python SDK auto-initializes telemetry on import. Rust bindings ([capi/](../../capi/), [python/](../../python/), [js/](../../js/)) **do not initialize tracing at all** — caller must wire it. This means a Python user dropping in `cognee-rust` via PyO3 loses telemetry entirely until they configure subscribers themselves.

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

These mirror Python's namespaces but several (`cognee.db.system`, `cognee.db.query`, `cognee.db.row_count`, `cognee.llm.provider`) are defined and unused.

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
| `COGNEE_TRACING_ENABLED` | [crates/lib/src/config.rs:463-465](../../crates/lib/src/config.rs#L463-L465) | false | Parsed, unused |
| `OTEL_SERVICE_NAME` | [crates/lib/src/config.rs:467-468](../../crates/lib/src/config.rs#L467-L468) | empty | Parsed, unused |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | [crates/lib/src/config.rs:470-471](../../crates/lib/src/config.rs#L470-L471) | empty | Parsed, unused |
| `OTEL_EXPORTER_OTLP_HEADERS` | [crates/lib/src/config.rs:473-474](../../crates/lib/src/config.rs#L473-L474) | empty | Parsed, unused |

### Crate coverage

- **Crates with `tracing`:** `core`, `ingestion`, `search`, `delete`, `database`, `http-server`, `cli`, `llm`, `embedding`, `vector`, `graph`, `storage`, `session`, `chunking`, `cognify`, `cloud`, `visualization`
- **Crates without `tracing`:** `models`, `ontology`, `test-utils`, `utils`
- **No `opentelemetry`, `prometheus`, `metrics` deps anywhere**

### Bindings

| Binding | Telemetry init |
|---|---|
| Python (PyO3) | None — caller owns subscriber setup |
| C API | None — caller owns observability |
| JS (Neon) | None |

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

Each gap is broken out into a dedicated sub-document with deep investigation, design, and numbered action items.

1. **Wire OpenTelemetry SDK + OTLP exporter** — makes the existing `OTEL_*` config fields actually do something. Add `tracing-opentelemetry` bridge so all 51 existing `#[instrument]` spans flow out. → [01-otel-otlp-export.md](01-otel-otlp-export.md) (3/12 sub-tasks complete)
2. **Implement `send_telemetry()` analytics client** — proxy URL, anon/persistent ID files, PBKDF2 api-key tracking ID, opt-out semantics, async fire-and-forget HTTP. → [02-send-telemetry-analytics.md](02-send-telemetry-analytics.md)
3. **Emit pipeline & task lifecycle events** — `Pipeline Run Started/Completed/Errored`, per-task variants, and API events (`cognee.recall`, `cognee.improve`, `cognee.forget`). → [03-pipeline-task-api-events.md](03-pipeline-task-api-events.md)
4. **Instrument DB adapters** — Qdrant, Ladybug, SeaORM/SQLite with `cognee.db.{vector,graph}.{search,query}` spans + `cognee.db.system/query/row_count` attributes; promote `redact_secrets()` to `cognee-utils`. → [04-db-adapter-instrumentation.md](04-db-adapter-instrumentation.md)
5. **Provenance stamping on DataPoints** — `source_pipeline`, `source_task`, `source_user`, `source_node_set`, `source_content_hash` attached to every yielded DataPoint with a per-run visited set. → [05-datapoint-provenance.md](05-datapoint-provenance.md)
6. **File logging with rotation** — mirror Python's `COGNEE_LOG_FILE`, `COGNEE_LOGS_DIR`, `COGNEE_LOG_MAX_BYTES`, etc.; rotating non-blocking appender; library noise suppression. → [06-file-logging-rotation.md](06-file-logging-rotation.md)
7. **Auto-init tracing in bindings** — PyO3, Neon, C API entry points so embedders get telemetry without extra setup; avoid double-emission when embedded in the Python SDK. → [07-bindings-auto-init.md](07-bindings-auto-init.md)
8. **Pipeline run status lifecycle** — schema and four-state lifecycle are defined but `INITIATED` is never written, `run_info` content drifts from Python, and library-level pipelines bypass the registry entirely. → [08-pipeline-run-status.md](08-pipeline-run-status.md)

---

## Future work / out of scope

Items intentionally not addressed by the eight gaps above. Captured here so they aren't lost.

- **OpenTelemetry metrics export.** Python's `monitoring` extra ships only OTEL traces, no metric points; the `tracing` crate likewise produces spans only. A future initiative could add `opentelemetry_sdk::metrics::SdkMeterProvider` and emit counters/histograms (e.g. pipeline-run duration, search latency, embedding-batch sizes) via the same OTLP endpoint.
- **OpenTelemetry logs export.** The OTEL log signal is stable in `opentelemetry_sdk` 0.31; bridging `tracing` events (not spans) to `OTEL_EXPORTER_OTLP_ENDPOINT` would let operators consolidate all telemetry on one collector. Not in scope of [01-otel-otlp-export.md](01-otel-otlp-export.md), which covers traces only.
- **Replacing `SpanBufferLayer` with an OTEL in-memory exporter.** Could unify the `/api/v1/activity/spans` endpoint with the OTEL pipeline, but would lose byte-for-byte parity with Python's `CogneeSpanExporter` ring buffer that the current test suite depends on. Not worth it today.
- **Cross-SDK OTEL parity test.** Extend [`e2e-cross-sdk/`](../../e2e-cross-sdk) with an `otel-collector` service in `docker-compose.yml`, point both Python and Rust at it, and assert both SDKs emit comparable span sets for the same operation. Follow-up to [01-otel-otlp-export.md](01-otel-otlp-export.md).
