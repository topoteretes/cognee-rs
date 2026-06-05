# Not Yet Implemented

A running list of capabilities that are **intentionally deferred, out of scope, or stubbed** in
cognee-rust. The big parity tracks (the API-gaps and telemetry gap analyses, and the HTTP-server
P0–P8 port) are all complete and their tracking docs have been removed. What remains below is the
set of known gaps that a future contributor might pick up.

Sourced from the surviving design docs and verified against the code as of 2026-06-02. Cross-cutting
*design decisions* that are still open (as opposed to missing features) live in
[open-questions.md](open-questions.md).

## Core pipeline / SDK

These are tracked in the project guide ([`.claude/CLAUDE.md`](../.claude/CLAUDE.md) → "Not Yet
Implemented") and restated here for one consolidated view:

- **Non-text document extraction** — classification and the loader registry recognize PDF, CSV,
  image, and audio, but only `text/*` files are extracted end-to-end. Actual extraction for the
  other types is not implemented.
- **S3 support** — `DataInput::S3Path` returns an error stub.
- **Direct URL streaming in `DataInput::process_by_chunks()`** — calling `process_by_chunks()`
  directly on `DataInput::Url` returns an unsupported error because URLs must first be fetched and
  canonicalized. Core ingestion is wired: `AddPipeline::add()` resolves HTTP(S) URL inputs, stores
  content and URL metadata, and leaves graph provenance to cognify.
- **Default tokenizer features in CI** — `HuggingFaceTokenCounter` and `TikTokenCounter` are behind
  optional feature flags (`hf-tokenizer`, `tiktoken`); CI builds may need to enable them explicitly.

## HTTP server

All routers ship (see [http-server/routers/README.md](http-server/routers/README.md)). The
remaining gaps are operational/feature-flagged rather than missing endpoints:

- **Multi-replica WebSocket fan-out** — the pipeline-run registry is process-local and does not fan
  out across replicas. Multi-replica deployments need sticky-session WS routing or a Redis-backed
  channel. Documented and deferred. ([pipelines.md §15](http-server/pipelines.md))
- **OTLP export from the HTTP-server span buffer** — the in-memory span buffer is trait-wrapped so an
  OTLP exporter can be slotted in, but that exporter is not wired. Deferred to a later phase.
  ([observability.md](http-server/observability.md))
- **Per-user LLM cost quotas** — any authenticated user can trigger paid LLM calls via `/llm` and
  `/responses`; there is no quota layer. Out of scope. ([routers/llm.md](http-server/routers/llm.md))
- **Streaming `/llm` custom-prompt output (SSE)** — both `/llm` endpoints are blocking. Streaming is
  out of scope. ([routers/llm.md](http-server/routers/llm.md))
- **WebSocket dataset-ownership authorization** — the subscribe handshake authenticates the user but
  does not verify the user owns the dataset behind the `pipeline_run_id`. Documented gap, not fixed.
  ([websocket.md](http-server/websocket.md))
- **WebSocket query-string token auth** — only cookie auth is accepted on the WS handshake. A
  `?token=…` fallback for non-browser clients is a future addition behind a feature flag.
  ([websocket.md](http-server/websocket.md))

### Notebook execution (`/notebooks/.../run`)

The endpoint is implemented (`SubprocessRunner`, gated by the `notebook_runner_enabled` config
flag), but the surrounding deployment story is not solved:

- **`cognee` Python package in the sandbox** — for `await cognee.add(...)` to work inside a cell, the
  subprocess needs `pip install cognee`. Bundling CPython + the wheel vs. operator-provided is
  undecided.
- **Sandbox auth / state propagation** — a cell that calls cognee needs scoped credentials so it
  can't use the operator's keys against another tenant. Not implemented.

See [routers/notebooks.md §6](http-server/routers/notebooks.md).

## Auth

From [http-server/auth.md](http-server/auth.md):

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

From [observability/](observability/) and the logging crate:

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

## Cross-SDK parity harness

The HTTP parity harness ships. Follow-ups noted in its (now-removed) design doc:

- **Per-endpoint OpenAPI snapshots** — only an informational `openapi.python.json` reference snapshot
  is committed; per-endpoint golden snapshots are a follow-up.
- **`--quick` LLM-mock mode** — LLM-dependent parity tests can take 60s+; a mocked fast mode was
  proposed but not built.
- **TLS path testing** — the suite runs over plain HTTP only.
