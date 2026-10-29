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

- **Legacy binary office formats** — text, PDF, CSV, HTML, image, audio, and the modern XML-based
  office formats (DOCX, XLSX/XLS/ODS, PPTX, EPUB, EML, ODT, ODP) are all extracted end-to-end (each
  behind its own feature flag; the umbrella `unstructured` feature is on by default in `cognee`).
  The extractors live in [`crates/ingestion/src/loaders/unstructured/`](../../crates/ingestion/src/loaders/unstructured/)
  and dispatch by extension in
  [`mod.rs`](../../crates/ingestion/src/loaders/unstructured/mod.rs). What is **not** supported is the
  set of legacy binary container formats — `.doc`, `.ppt`, `.rtf`, `.msg` — which are rejected with an
  error pointing the user at the Python SDK or a format conversion. Porting these would mean
  reimplementing OLE/CFB binary parsers; deferred.
- **S3 support** — `DataInput::S3Path` returns an error stub.
- **Direct URL streaming in `DataInput::process_by_chunks()`** — calling `process_by_chunks()`
  directly on `DataInput::Url` returns an unsupported error because URLs must first be fetched and
  canonicalized. Core ingestion is wired: `AddPipeline::add()` resolves HTTP(S) URL inputs, stores
  content and URL metadata, and leaves graph provenance to cognify.
- **Tokenizer features under `--no-default-features`** — `HuggingFaceTokenCounter` and
  `TikTokenCounter` are behind optional feature flags (`hf-tokenizer`, `tiktoken`). Both are in
  `cognee`'s **default** feature set, so a standard build (and CI) includes them; they are only
  absent in `--no-default-features` builds or when the `cognee-chunking` crate is used directly
  without enabling them. Not a gap for default builds — noted only for downstream crates that strip
  defaults.

## Python SDK parity gaps (capabilities not ported)

Surfaced by comparing against the Python cognee documentation (docs.cognee.ai) as of 2026-06-20.
These are features the Python SDK ships that the Rust port does not yet implement (so their absence
from the docs is correct). The four memory-API CLI verbs (`remember` / `recall` / `forget` /
`improve`) were a gap here and are **now implemented** — see [../tools/cli.md](../tools/cli.md).

- **MCP server** — Python ships `cognee-mcp` (usable from Cursor, Claude Code, and other MCP
  clients). There is no MCP crate in the Rust workspace.
- **Local UI** — Python offers a web UI (`cognee-cli -ui` + the `cognee-frontend` app). Rust has only
  the single-file d3.js graph export (`cognee-cli visualize`) — interactive in the browser, but a
  static artifact with no server behind it.
- **LLM / embedding / backend provider breadth** — Python documents many more backends than Rust
  implements:
  - LLM: Rust covers OpenAI-compatible (OpenAI/Ollama/Mistral/Gemini/vLLM/llama.cpp),
    Anthropic (native Messages API), Azure, AWS Bedrock (Converse, `bedrock` feature)
    + `LiteRtAdapter` (Android) — i.e. every provider Python names is now covered,
    though Python reaches the wider LiteLLM catalogue that Rust does not.
  - Embeddings: Python adds Gemini and Mistral; Rust covers ONNX (also reachable as
    `fastembed`), OpenAI-compatible, Ollama, AWS Bedrock (InvokeModel, `bedrock`
    feature), mock.
  - Vector stores: Python adds ChromaDB, Neptune Analytics, Redis, FalkorDB; Rust covers
    LanceDB (embedded, default on non-Android), pgvector, and an in-memory brute-force store
    (Android default + `vector_db_url = ":memory:"` escape hatch). An embedded Qdrant adapter
    is not part of this repository.
  - Graph stores: Python adds Neo4j, Neptune, Memgraph; Rust covers Ladybug/Kuzu (embedded) and
    Postgres (`pggraph`).
- **Structured-output backends** — Python lets you choose LiteLLM + Instructor or BAML for structured
  extraction; Rust uses its own structured-output path in [`cognee-llm`](../../crates/llm/).
- **Entity disambiguation / canonicalization** — Python documents entity disambiguation,
  post-extraction canonicalization, and entity-description consolidation (guides/POCs). Not ported.

## improve() — deferred stages (partial implementations)

`improve()` runs Python's nine stages in order and reports one `StageResult`
each. Not yet ported — they report a skip reason instead of running:

- **`review_conflicts`** — skipped `opt_in_disabled` unless opted in with
  `improve.review_conflicts` (there is no per-call argument yet), then `no_llm_configured` or
  `not_ported` (SDK-1038).

Improve settings that exist but are not read by any stage yet: `improve.trace_window_steps`,
`improve.trace_windows_per_run` (SDK-1047), `improve.distill_max_batches_per_run` (SDK-1049) and
`improve.effective_date_key` (SDK-1038). Python's host admission check for the automatic improve
(`register_auto_improve_admission`) is not ported.

Partial stages:

- **`extract_agent_context`** — the improve stage is ported; the trace-write hooks are not.
  Python also extracts on every trace write while automatic session context is on (a no-LLM
  `failure_lessons` lesson per errored step, and an LLM batch every 10 traces), inside
  `SessionManager.add_agent_trace_step`; Rust's `add_agent_trace_step` stores the step only, so
  lessons appear when `improve()` runs. The passes exist as `cognee_session::
  extract_live_agent_context` / `extract_pending_agent_context`. The rendered session guidance
  block that serves the lessons (Python's `build_active_context_block`) is not ported.
- **`update_user_preferences`** — ported. Differences: the graph trait has no edge delete, so a
  retired `prefers` edge is always rewritten as neutral (weight 0.5, turn 0) — Python's fallback
  for backends without `delete_edge_triples` — and the node is always written in full (every
  adapter's node write is an upsert). The feedback rows that infer ratings
  (`referenced_qa_rating`) and the `preferences`-section entries that become the preference
  text are written by Python's session-turn analysis (`prepare_session_turn`), which is not
  ported (SDK-1045); in Rust an explicit `feedback_score` (`add_feedback`) is the rating source,
  and stored entries of either kind are honoured. **The preference-text track is inert on
  Rust-only data until SDK-1045 lands:** nothing in Rust writes a `preferences`-section entry,
  so the node's preference text stays empty unless Python wrote those entries into a shared
  session store; only the `prefers` edge weights (from explicit ratings) take effect.
- **Personalized retrieval** — graph completions (triplet ranking) and RAG completion (chunk
  re-ranking, preference text as guidance) read the preferences. Not yet: the hybrid search's
  chunk-lane factor and guidance (SDK-1045), and the session guidance block's `Preferences`
  section. Rust appends the preference text on session searches too, since it has no session
  guidance block; Python serves it there through that block. RAG scores are similarities in
  Rust; the re-rank takes each back to Python's distance space (`1 - score`) and scales that,
  so the order matches Python's. A RAG completion's turn records its chunks' ids, so ratings
  can weight chunks.

- **`global_context_index`** — opt-in via `build_global_context_index`; builds Python's
  default `from_entities` index. Differences: a document's name comes from its data item
  (`name`, else the basename of `raw_data_location`) because Rust chunks carry no
  `document_name`; the LLM method ports only the default `bucketing_strategy="vector"`
  (the experimental graph bucketing is not ported); index edges are not stamped with a
  dataset source ref (Rust's ownership ledger is relational; the nodes' rows carry the
  dataset). Its `already_completed` watermark: unlike Python, which narrows the
  change check to the dataset under backend access control, every dataset's pipeline runs count.
  A host's `improve_pipeline` rows never count: an improve counts through the `improve` row it
  records after a stage that may have written, so an index-only improve is not a change (Python
  counts every non-noop improve). The hybrid retriever's global-context channel is not wired yet
  (SDK-1045); graph completion's is.

- **`persist_agent_traces`** — trace steps whose `session_feedback` field is non-empty are
  collected and run through the standard `add → cognify` path (tagged to the
  `"agent_trace_feedbacks"` node set), all sessions as one document. There is no per-session
  trace watermark (`windows`, `sessions_stuck`) yet (SDK-1047); a run with no trace feedback reports
  `already_completed: no_new_trace_steps`. Per-step provenance metadata is stored as graph nodes in
  *neither* SDK — a shared future enhancement rather than a parity gap.

- **`persist_session_qa` / `distill_sessions`** — no Q&A or distillation watermark yet
  (SDK-1047 / SDK-1049): the watermark rows exist (`cognee_session::SESSION_PERSIST_WATERMARK`,
  `distill_watermark`, on the session-context store) but no stage reads or advances them yet, so
  every run persists and distills the sessions' full Q&A, and
  `distill_sessions` reports no `entries_uncurated` count.

## HTTP server

`cognee-http-server` is not in this repository; its known gaps (server, notebook
execution, auth) and open design questions left with it.

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

## Language bindings — TypeScript / Node (`ts/`)

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

The multi-view frontend now ships: the Graph / Schema / Memory / Semantic tabs and the inspector
panel are rendered from the same six JS view modules Python uses, vendored verbatim (see
`crates/visualization/assets/README.md` for the resync rule). Three payload gaps remain.

- **Search-event timeline is always empty.** Python passes `search_events or []` into the story
  view from `visualization/session_events.py`; that module has no Rust counterpart, so the
  `__SEARCH_EVENTS__` payload is hard-coded `[]` and the story view's event lane renders empty.

- **Semantic map renders its empty state.** Python computes 2-D node positions and clusters
  best-effort (PCA + seeded k-means over stored embeddings). Rust cannot: `cognee_vector::SearchResult`
  exposes no `vector` field, so persisted embeddings are not readable back, and the workspace has no
  PCA/SVD or seeded k-means. Both payloads are `null` — the same state Python's own blanket
  `except Exception` fallback reaches, so `semantic_map.js` shows its documented empty state rather
  than breaking.

- **The page is not offline-self-contained.** The vendored `template.html` loads d3 v7 and the Inter
  font from a CDN, exactly as Python does. Rendering therefore needs network access on first load.
  This is upstream behaviour, not a Rust-side regression, but it does mean "self-contained HTML" now
  means "single file", not "zero external fetches".

## Cross-SDK parity harness

The HTTP parity harness shipped, then left this repository with
`cognee-http-server`; what remains here is the CLI/DB lane. Follow-ups noted in its
(now-removed) design doc:

- **Per-endpoint OpenAPI snapshots** — an informational `openapi.python.json` reference snapshot
  used to be committed here; it left with the rest of the HTTP parity lane, and
  per-endpoint golden snapshots are no longer tracked here.
- **`--quick` LLM-mock mode** — LLM-dependent parity tests can take 60s+; a mocked fast mode was
  proposed but not built.
- **TLS path testing** — the suite runs over plain HTTP only.
