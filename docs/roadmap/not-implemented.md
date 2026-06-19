# Not Yet Implemented

A running list of capabilities that are **intentionally deferred, out of scope, or stubbed** in
cognee-rust. The big parity tracks (the API-gaps and telemetry gap analyses, and the HTTP-server
P0–P8 port) are all complete and their tracking docs have been removed. What remains below is the
set of known gaps that a future contributor might pick up.

Sourced from the surviving design docs and verified against the code as of 2026-06-17. Cross-cutting
*design decisions* that are still open (as opposed to missing features) live in
[open-questions.md](open-questions.md).

## Core pipeline / SDK

These are tracked in the project guide ([`.claude/CLAUDE.md`](../../.claude/CLAUDE.md) → "Not Yet
Implemented") and restated here for one consolidated view:

- **`unstructured` office-format extraction** — text, PDF, CSV, HTML, image, and audio files are
  extracted end-to-end (each behind its own feature flag). The `unstructured` office formats
  (DOCX/XLSX/PPTX/ODT/etc.) are classified and registered in the loader registry, but full
  extraction parity for them is not yet implemented.
- **S3 support** — `DataInput::S3Path` returns an error stub.
- **Direct URL streaming in `DataInput::process_by_chunks()`** — calling `process_by_chunks()`
  directly on `DataInput::Url` returns an unsupported error because URLs must first be fetched and
  canonicalized. Core ingestion is wired: `AddPipeline::add()` resolves HTTP(S) URL inputs, stores
  content and URL metadata, and leaves graph provenance to cognify.
- **Default tokenizer features in CI** — `HuggingFaceTokenCounter` and `TikTokenCounter` are behind
  optional feature flags (`hf-tokenizer`, `tiktoken`); CI builds may need to enable them explicitly.

## improve() — deferred stages (partial implementations)

Shipped in task 20 as sanctioned partials for 0.1.0:

- **`build_global_context_index` stage (Stage 3b)** — the `build_global_context_index: bool` flag
  and the `"global_context_index"` entry in `stages_run` are wired. The current implementation
  reads all graph edges via `get_graph_data()`, formats them as `"source → relationship → target"`
  lines, and stores the result in the session's graph-context slot (keyed
  `"_global_context_index"`). What is **not** yet implemented:
  - Bucket/root summarization: Python's reference implementation chunks the edge list into buckets
    and runs an LLM summarization pass to produce a condensed global context rather than a raw edge
    dump. The Rust version stores the raw edge list, which is token-inefficient for large graphs.
  - TODO: add `global_context_index_pipeline()` in `cognee-cognify` (mirrors Python
    `session_manager.py: build_global_context_index`) that does bucket summarization with the LLM
    before writing to the session store.

- **`persist_trace_steps` stage (Stage 2b)** — trace steps whose `session_feedback` field is
  non-empty are collected and run through the standard `add → cognify` path (tagged to the
  `"agent_trace_feedbacks"` node set). This is a scoped-down version of the Python reference:
  the full per-step provenance metadata (origin function, status, parameters, return values) is not
  stored as separate graph nodes, only the feedback text is cognified.
  - TODO: add a dedicated `persist_trace_step_metadata()` in `cognee-cognify` that creates a
    per-step graph entity preserving the full `SessionTraceStep` fields for audit/replay.

## HTTP server

All routers ship (see [http-server/routers/README.md](../http-server/routers/README.md)). The
remaining gaps are operational/feature-flagged rather than missing endpoints:

- **Multi-replica WebSocket fan-out** — the pipeline-run registry is process-local and does not fan
  out across replicas. Multi-replica deployments need sticky-session WS routing or a Redis-backed
  channel. Documented and deferred. ([pipelines.md §15](../http-server/pipelines.md))
- **OTLP export from the HTTP-server span buffer** — the in-memory span buffer is trait-wrapped so an
  OTLP exporter can be slotted in, but that exporter is not wired. Deferred to a later phase.
  ([observability.md](../http-server/observability.md))
- **Per-user LLM cost quotas** — any authenticated user can trigger paid LLM calls via `/llm` and
  `/responses`; there is no quota layer. Out of scope. ([routers/llm.md](../http-server/routers/llm.md))
- **Streaming `/llm` custom-prompt output (SSE)** — both `/llm` endpoints are blocking. Streaming is
  out of scope. ([routers/llm.md](../http-server/routers/llm.md))
- **WebSocket dataset-ownership authorization** — the subscribe handshake authenticates the user but
  does not verify the user owns the dataset behind the `pipeline_run_id`. Documented gap, not fixed.
  ([websocket.md](../http-server/websocket.md))
- **WebSocket query-string token auth** — only cookie auth is accepted on the WS handshake. A
  `?token=…` fallback for non-browser clients is a future addition behind a feature flag.
  ([websocket.md](../http-server/websocket.md))

### Notebook execution (`/notebooks/.../run`)

The endpoint is implemented (`SubprocessRunner`, gated by the `notebook_runner_enabled` config
flag), but the surrounding deployment story is not solved:

- **`cognee` Python package in the sandbox** — for `await cognee.add(...)` to work inside a cell, the
  subprocess needs `pip install cognee`. Bundling CPython + the wheel vs. operator-provided is
  undecided.
- **Sandbox auth / state propagation** — a cell that calls cognee needs scoped credentials so it
  can't use the operator's keys against another tenant. Not implemented.

See [routers/notebooks.md §6](../http-server/routers/notebooks.md).

## Auth

From [http-server/auth.md](../http-server/auth.md):

- **OAuth2 / OIDC providers** (Google, GitHub, …) — deferred; only cookie + bearer + API-key auth
  ships.
- **JWT denylist / revocation** — logout invalidates the cookie, not the underlying JWT. A leaked
  token can only be revoked by rotating `FASTAPI_USERS_JWT_SECRET`.
- **Multi-key JWT secret rotation** (`kid` header + secret map) — single secret only today.

## Telemetry / observability

The eight telemetry gaps are closed (OTLP export, `send_telemetry`, pipeline-run status, etc.). What
remains for a future initiative:

- **OpenTelemetry metrics export** — only traces are emitted today. Counters/histograms
  (pipeline-run duration, search latency, embedding-batch sizes) via an
  `SdkMeterProvider` over the same OTLP endpoint are not implemented.
- **In-crate search-lifecycle test** — the `cognee.search EXECUTION STARTED/COMPLETED` event pair is
  covered by the cross-SDK byte-parity harness but has no in-crate mockito test. Low-priority
  follow-up.

## Logging

From [observability/](../observability/) and the logging crate:

- **Size-based log rotation** — only daily time-based rotation is implemented
  (`tracing-appender::RollingFileAppender`); Python's 50 MB size-based rotation is not matched.

## Language bindings — TypeScript / Node (`js/`)

The Phase-3 pipeline ops (`cogneeAdd` / `cogneeCognify` / `cogneeAddAndCognify`) accept a
discriminated-union `dataInput` (`{ type, … }`). The supported variants track what
`DataInput` flows end-to-end today:

- **`text` / `file` / `binary`** — fully supported (`binary` requires a `name`, used for MIME
  detection; `bytes` may be a base64 string, a byte array, or a Node `Buffer`).
- **`url`** — accepted and marshalled to `DataInput::Url`. The normal add pipeline resolves HTTP(S)
  URLs; only callers that bypass the pipeline and invoke `DataInput::process_by_chunks()` directly
  hit the direct-streaming gap above.
- **`s3`** — rejected at the boundary with an `UNSUPPORTED` error (`DataInput::S3Path` is a stub).
- **Recursive `dataItem`** (`DataInput::DataItem`) — out of scope for the v1 binding; rejected with
  an `UNSUPPORTED` error.

`cogneeAdd` returns one record per input including duplicates (the pipeline's duplicate branch
returns the pre-existing row), so the binding partitions the result into `added` (newly created) vs
`deduplicated` (already existed) by content-addressed id; there is no in-pipeline "drop duplicates
from the result" behavior to rely on.

## Cloud sync

- **`POST /api/v1/sync` — no-op stub.** `crates/cloud/src/sync.rs::run_background()` marks the
  operation started, ticks a progress counter from 0 → 100 %, and marks it completed with zero
  records/bytes transferred. No data is actually moved. The HTTP wire contract accepts the request
  and returns a well-formed response, but the reported transfer totals are always zero. The full
  diff/upload/download/cognify orchestration is deferred. This gap is documented in the
  `run_background` rustdoc.

## Session stores

- **`FsSessionStore` on-disk container differs from Python.** The Rust `FsSessionStore`
  (`crates/session/src/fs_store.rs`) stores sessions as plain JSON files at
  `{base_dir}/{user_id}/{session_id}.json` — one JSON array of `QaEntry` objects per file. The
  Python `FsSessionStore` uses a `diskcache` SQLite database at `.cognee_fs_cache/sessions_db/`
  with its own key-hashing layout. The *entry shape* is compatible (matching field names and JSON
  types); the *container format* is not. A Rust FS session store and a Python FS session store
  cannot read each other's files. For cross-process or cross-SDK session sharing, use the
  `SeaOrm` or `Redis` session store backends instead.

## Visualization

- **Story / Schema / Inspector multi-view deferred.** The Python `cognee_network_visualization`
  module renders three HTML tabs (Story, Schema, Inspector) backed by four JS modules
  (`ui_chrome`, `schema_view`, `story_view`, `inspector`). The Rust `cognee-visualization` crate
  emits a single self-contained d3.js force-directed graph view. The schema tab is always absent;
  node-type coloring uses the 12-entry hard-coded map in `colors.rs`. The full multi-view rewrite
  is a substantial JS-embedding effort and is deferred to a post-0.1.0 release.

  The 8-key schema-node name fallback from Python's `preprocessor.py:223–237` (`database_type`,
  `primary_key`, `source_table`, `source_column`, `target_table`, `target_column`,
  `relationship_type`, `row_count_estimate`) is implemented in `serialize.rs` so schema-typed
  nodes render with meaningful names in the single-view output.

## Cross-SDK parity harness

The HTTP parity harness ships. Follow-ups noted in its (now-removed) design doc:

- **Per-endpoint OpenAPI snapshots** — only an informational `openapi.python.json` reference snapshot
  is committed; per-endpoint golden snapshots are a follow-up.
- **`--quick` LLM-mock mode** — LLM-dependent parity tests can take 60s+; a mocked fast mode was
  proposed but not built.
- **TLS path testing** — the suite runs over plain HTTP only.
