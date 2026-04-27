# Implementation: P3 — Pipelines + WebSocket

## 1. Goal

Land the four pipeline-style routers — `/cognify` (POST + WebSocket), `/memify`, `/remember`, `/improve` — on top of the `cognee_core::PipelineRunRegistry` shipped in the P3 prerequisite. Every route must match Python wire shape verbatim, including the two acknowledged quirks: `/improve` returns `420` on `PipelineRunErrored`, and the cognify WebSocket only closes on `PipelineRunCompleted` (forwards `Errored` / `AlreadyCompleted` and keeps the socket open). After this phase, an end-to-end `add → cognify → search` round-trip works through the HTTP server, with live progress observable on the WebSocket.

## 2. References

Design rationale lives in the design docs; do not duplicate it.

- Phase template + invariants: [README.md](README.md).
- Phase scope: [../plan.md §4](../plan.md#4-implementation-phases).
- Registry API + dispatcher pattern + eviction + recovery: [../pipelines.md](../pipelines.md). In particular [§3 status taxonomy](../pipelines.md#3-status-taxonomy-and-wire-mapping), [§6 registry](../pipelines.md#6-cognee_corepipelinerunregistry--the-new-component), [§7 dispatch](../pipelines.md#7-background-task-lifecycle-http-server-side), [§9 sync vs background shapes](../pipelines.md#9-sync-vs-background-dispatch-http-wire-shapes), [§12 crash & restart recovery](../pipelines.md#12-crash--restart-recovery).
- WebSocket wire protocol, auth handshake, close codes, terminal-close behavior: [../websocket.md](../websocket.md). In particular [§4 auth](../websocket.md#4-authentication), [§5 frame format](../websocket.md#5-frame-format), [§6 status semantics & terminal close](../websocket.md#6-status-semantics--terminal-close), [§9 server-side implementation](../websocket.md#9-server-side-implementation).
- Per-router specs (read all four):
  - [../routers/cognify.md](../routers/cognify.md) — POST + WS, the largest spec.
  - [../routers/memify.md](../routers/memify.md) — POST only, single-dataset response.
  - [../routers/remember.md](../routers/remember.md) — multipart POST, `409` catch-all (not `500`).
  - [../routers/improve.md](../routers/improve.md) — POST, `420` on `PipelineRunErrored`, `409` catch-all.
- Error model, including `ApiError::PipelineErrored` and the `Improve → 420` quirk: [../architecture.md §9](../architecture.md#9-error-handling).
- Identifier generation: [../pipelines.md §4](../pipelines.md#4-identifiers).

## 3. Prerequisites

- **P0** done — crate skeleton, `AppState`, `ApiError`, OpenAPI bootstrap, `build_router`. P0 declared `AppState::pipelines: Option<Arc<dyn PipelineRunRegistry>>`; this phase removes the `Option` and wires the real registry at startup.
- **P1** done — `AuthenticatedUser` extractor (cookie / bearer / API key), cookie-based auth helper for the WebSocket handshake (`auth::cookie::authenticate_from_cookie`), `users` and `user_api_key` migrations.
- **P2** done — multipart streaming extractor (used by `/remember`); `OntologyManager` is wired through `state.lib` (used by `/cognify` for `ontology_key` resolution); `DatasetConfigurationRepository::{find_by_dataset_id, upsert}` exists (used by `/cognify` for the best-effort `graph_model` / `custom_prompt` round-trip).
- **P3 prereq library refactor** done — see [p3-prereq-library-refactor.md](p3-prereq-library-refactor.md). Without it the registry trait does not exist and `cognee_lib::api::remember::remember()` / `cognee_lib::api::improve::improve()` still carry `run_in_background` on their library signatures. The dispatcher in this phase assumes both library functions are synchronous.

## 3.1 Error-body cheat sheet

Pin the exact wire shape per route+condition before writing handlers. The rest of §4 references this table by name. Source per row: the per-router error tables in `routers/*.md`.

| Route | Condition | Status | Body |
|---|---|---|---|
| `POST /cognify` | both `datasets` and `dataset_ids` empty | 400 | `{"error": "No datasets or dataset_ids provided"}` |
| `POST /cognify` | unknown `ontology_key` | 409 | `{"error": "<ontology error message>"}` |
| `POST /cognify` | per-dataset `PipelineRunErrored` | 500 | `{"error": "Pipeline run errored", "detail": "<first errored run's error string>"}` |
| `POST /cognify` | other internal | 500 | `{"error": "Internal server error", "detail": "<msg>"}` |
| `POST /memify` | `dataset_id` and `dataset_name` both empty | 400 | `{"error": "Either datasetId or datasetName must be provided."}` |
| `POST /memify` | `PipelineRunErrored` | 500 | `{"error": "Pipeline run errored", "detail": "<msg>"}` |
| `POST /remember` | `datasetName` and `datasetId` both empty | 400 | `{"detail": "Either datasetId or datasetName must be provided."}` (note `detail`, not `error`) |
| `POST /remember` | any error (incl. `PipelineRunErrored`) | 409 | `{"error": "An error occurred during remember."}` (no `detail`) |
| `POST /remember` | body too large | 413 | `{"detail": "request body too large"}` |
| `POST /improve` | `dataset_id` and `dataset_name` both empty | 400 | `{"detail": "Either datasetId or datasetName must be provided."}` |
| `POST /improve` | `PipelineRunErrored` | **420** | the raw serialised `PipelineRunInfoDTO` (NOT the canonical envelope) |
| `POST /improve` | any other error | 409 | `{"error": "An error occurred during graph improvement."}` |
| `WS /cognify/subscribe` | auth failure | close 1008 | reason `"Unauthorized"` (UTF-8, not JSON) |
| `WS /cognify/subscribe` | broadcast lag | close 1011 | reason `"channel lagged"` |
| `WS /cognify/subscribe` | `PipelineRunCompleted` | close 1000 | (no reason text) — sent **after** the final TEXT frame |

Note the `error` vs `detail` key inconsistency between routers (and even between conditions on the same router). This is Python's accidental inconsistency from `HTTPException(...)` (produces `detail`) vs `JSONResponse({"error": ...})`. Match per-row, not per-router.

## 4. Step-by-step

Each step is one commit. Steps are ordered so each one compiles on its own (`cargo check --all-targets -p cognee-http-server`). Tests for a new behaviour land in the same step that introduces the behaviour.

### Step ordering rationale

Steps 1–6 are the shared infrastructure that the four routers all consume: registry wiring, status mapping, the shared dataset-id deserializer, the error variant for `PipelineErrored`, the dispatcher, and the `formatted_graph_data` re-export. Steps 7–9 land cognify (POST + WS + mount) — the largest router and the only one with a WebSocket. Steps 10–12 land the three remaining POST-only routers, each of which depends only on Steps 1–5. Steps 13–15 are cross-cutting polish (WS payload computation refinement, graceful shutdown, OpenAPI). Step 16 is doc-only.

A reviewer can land Steps 1–6 as a stack, then any of {cognify, memify, remember, improve} in parallel branches, since the four routers do not depend on each other once the shared infrastructure is in place.

### Step 1 — Wire the registry into `AppState::pipelines`

- Files: `crates/http-server/src/state.rs`, `crates/http-server/src/lib.rs` (`AppState::build`), `crates/http-server/src/main.rs` (startup), `crates/http-server/src/config.rs` (extend `HttpServerConfig` with registry knobs).
- Spec: [../architecture.md §6](../architecture.md#6-application-state--dependency-injection), [../pipelines.md §6.1](../pipelines.md#61-location-and-feature-gating), [../pipelines.md §11](../pipelines.md#11-eviction--resource-budget).
- Action: drop the `Option<…>` wrapper from `AppState::pipelines` (P0 left it as `Option<Arc<dyn PipelineRunRegistry>>` so unit tests could pass without the registry). At construction time, build a `RegistryConfig` from the matching `HttpServerConfig` fields and instantiate the concrete registry from `cognee_core` (`DefaultPipelineRunRegistry::new(repo, cfg)`). Store `Arc::new(registry)` on the state. The `repo: Arc<dyn PipelineRunRepository>` is the SeaORM impl that ships with the P3 prereq.

  Configuration mapping (extend `HttpServerConfig` to expose every knob from [../pipelines.md §6.2 `RegistryConfig`](../pipelines.md#62-public-types) — operators need to reach the strict-Python-parity path):
  - `PIPELINE_REGISTRY_MAX_RUNS` → `max_in_memory_runs` (default 4096; `usize::MAX` for unbounded).
  - `PIPELINE_REGISTRY_FINISHED_RETENTION_SECS` → `finished_retention` (default 3600).
  - `PIPELINE_REGISTRY_CHANNEL_CAPACITY` → `channel_capacity` (default 64).
  - `PIPELINE_REGISTRY_ABORT_WRITES_ERRORED` → `abort_writes_errored_row` (default `true`; `false` for strict Python parity).

  **Run the orphan reset once on startup**: after constructing the registry but before binding the listener, call `repo.reset_orphans("server_restart_orphan")` per [../pipelines.md §12](../pipelines.md#12-crash--restart-recovery). This rewrites any `INITIATED` / `STARTED` rows from a previous unclean shutdown to `ERRORED`. Log the count at `INFO`. Do not block startup on a non-fatal repo error — log at `WARN` and continue.
- Verify: `cargo check --all-targets -p cognee-http-server`. Existing health-only integration tests still pass. Add a unit test that pre-seeds an `INITIATED` row and asserts the startup hook rewrites it to `ERRORED` with the documented `reason`.

### Step 2 — Status enum mapping (`PipelineRunInfoDTO`, wire-string helpers)

- Files: `crates/http-server/src/dto/pipeline_run.rs` (new), re-export from `dto::mod`.
- Spec: [../pipelines.md §3](../pipelines.md#3-status-taxonomy-and-wire-mapping), [../routers/cognify.md §4](../routers/cognify.md#4-dto-definitions).
- **Note**: A `PipelineRunInfoDTO` struct already exists in `crates/http-server/src/dto/add.rs` (used by the P2 add router). The P3 struct in `pipeline_run.rs` is the **shared** version for all four pipeline routers; `dto/add.rs` re-exports it from `pipeline_run.rs` instead of defining it inline. Update `dto/add.rs` and `dto/update.rs` to import from `dto::pipeline_run` to avoid duplication.
- Action: define the shared `PipelineRunInfoDTO` (used by all four routers — it is **not** cognify-only). Add a `pub fn event_kind_to_python_string(kind: &RunEventKind) -> &'static str` and the inverse for the durable-status side. Cover all five wire strings: `PipelineRunStarted`, `PipelineRunYield`, `PipelineRunCompleted`, `PipelineRunErrored`, `PipelineRunAlreadyCompleted`. Add a `pipeline_status_to_db_string(status: PipelineRunStatus) -> &'static str` for the `DATASET_PROCESSING_*` durable-side mapping (the registry's `PipelineWatcher` impl uses this when it writes rows; if the prereq impl already owns the mapping, this step adds only the wire-string helpers).
- Verify: unit test for every enum variant in both directions; assert the output strings match the Python literals from [../pipelines.md §3.2](../pipelines.md#32-durable-status--written-to-pipeline_runsstatus) and [§3.3](../pipelines.md#33-live-event-status--emitted-on-the-registry-channel-and-the-websocket-frame).

### Step 3 — `DatasetIdRef` deserializer + shared util module

- Files: `crates/http-server/src/dto/util.rs` (new), `crates/http-server/src/dto/mod.rs`.
- Spec: [../routers/memify.md §3](../routers/memify.md#3-cross-cutting-behavior) and [§4](../routers/memify.md#4-dto-definitions).
- Action: add the `DatasetIdRef` newtype with a custom `Deserialize` accepting `null`, `""`, or a UUID. Reject any other string with a serde error (which the custom `Json` extractor surfaces as `ApiError::Validation`). Re-export from `dto`. This is reused by `/memify`, `/improve`, and `/remember`.
- Verify: round-trip tests — `null`, `""`, valid UUID, invalid UUID, non-string scalar. Each produces the documented outcome.

### Step 4 — Extend `ApiError` with `PipelineErrored { source, run_info }`

- Files: `crates/http-server/src/error.rs`.
- Spec: [../architecture.md §9](../architecture.md#9-error-handling), [../routers/improve.md §3](../routers/improve.md#3-cross-cutting-behavior).
- Action: introduce a `PipelineErrorSource` enum (`Cognify`, `Memify`, `Improve`, `Remember`, `Sync`). Replace any P0 placeholder `PipelineErrored(String)` with the structured variant `PipelineErrored { source: PipelineErrorSource, run_info: serde_json::Value }`. The `IntoResponse` impl maps `Improve → 420` and every other `source → 500`. The body for `Improve` is the `run_info` value verbatim (Python returns the run object as the body, not the canonical envelope); for cognify/memify the body is `{"error": "Pipeline run errored", "detail": "<error string>"}` per [../routers/cognify.md §2.1 error responses](../routers/cognify.md#21-post-apiv1cognify--run-the-cognify-pipeline). `Remember` does **not** use this variant — its catch-all is `409 {"error": "An error occurred during remember."}` and is encoded via `ApiError::Conflict(...)`.
- Verify: unit tests asserting status codes and body shapes for each `PipelineErrorSource`.

### Step 5 — HTTP-side dispatcher (`crates/http-server/src/pipelines/dispatch.rs`)

- Files: `crates/http-server/src/pipelines/mod.rs` (new), `crates/http-server/src/pipelines/dispatch.rs` (new).
- Spec: [../pipelines.md §7](../pipelines.md#7-background-task-lifecycle-http-server-side).
- Action: implement `dispatch_pipeline(state, user, pipeline_name, dataset_id, run_in_background, work)` returning a `DispatchOutcome::{Blocking { outcome }, Background { handle }}`. Pseudocode lives at [../pipelines.md §7](../pipelines.md#7-background-task-lifecycle-http-server-side); copy it verbatim.

  Concrete steps inside the function:
  1. Compute `pid = pipeline_id(user.id, dataset_id.unwrap_or_default(), pipeline_name)` and `prid = dataset_id.map(|d| pipeline_run_id(pid, d))` via the helpers from [../pipelines.md §4](../pipelines.md#4-identifiers). The prereq exposes both as free functions in `cognee_core::pipeline_run_registry` (the existing `cognee_core::pipeline::deterministic_pipeline_id` is renamed/wrapped — confirm with the prereq landing).
  2. Build `RunSpec { run_id: prid, pipeline_name: pipeline_name.into(), user_id: Some(user.id), dataset_id }` for endpoints that have a deterministic id; pass `None` for `run_id` only on ad-hoc paths (none in P3). The registry auto-generates a `Uuid::new_v4()` when `run_id` is `None`.
  3. Branch on `run_in_background`. `false` → `register_inline(spec, work_box).await` and return `Blocking { outcome }`. `true` → `register_background(spec, work_box).await` and return `Background { handle }`. The work future is boxed via `Box::pin(...)` to satisfy the `PipelineFuture` type alias.
  4. The `work` future receives an `Arc<TaskContext>` whose `pipeline_watcher` slot is bound to the per-run `ScopedRunWatcher` provided by the registry — **do not** construct a fresh watcher here; the registry owns that wiring. The dispatcher's only responsibility is to call `state.lib.task_context_with_watcher(state.pipelines.watcher_for(prid)).await?` before invoking `work(ctx)`.
  5. Map `RegistryError` to `ApiError::Internal` — registry errors are operator-visible only and never client-facing under steady state.
- Verify: unit tests with a mock `PipelineRunRegistry`. The prereq lands a `MockPipelineRunRegistry` in `cognee-test-utils`; if it has not, sketch one inline behind `#[cfg(test)]`. Assert: (a) `run_in_background=true` returns immediately with a `RunHandle`; (b) `run_in_background=false` awaits the future to completion; (c) both call sites pass the deterministic `pipeline_run_id` derived via the helpers. Cover the `dataset_id=None` path (used by future ad-hoc endpoints; no P3 caller exercises it).

### Step 6 — `formatted_graph_data` helper wiring

- Files: `crates/lib/src/lib.rs` (or `crates/lib/src/graph.rs`) — the `cognee-lib` re-export. Possibly a thin wrapper inside `crates/http-server/src/state.rs` if `cognee-lib` does not yet expose this surface.
- Spec: [../websocket.md §5.3](../websocket.md#53-payload-computation), [../routers/cognify.md §2.2](../routers/cognify.md#22-ws-apiv1cognifysubscribepipeline_run_id--live-pipeline-progress).
- Action: confirm `state.lib.formatted_graph_data(dataset_id, &user)` resolves to a function returning `Result<serde_json::Value, _>` shaped `{ "nodes": [...], "edges": [...] }`. Per the audit ([audit-findings.md row on `cognee_lib::modules::graph::methods::get_formatted_graph_data`](../audit-findings.md)), the helper lives in `cognee_graph`. If it's not yet re-exported through `cognee-lib`, add a thin wrapper in this step that calls into the existing `cognee-graph` formatter (do not reimplement). The WS handler must be able to call it on every event without further plumbing.
- Verify: a unit test that constructs a small graph with the in-memory `MockGraphDB` from `cognee-test-utils`, calls `formatted_graph_data`, and asserts the JSON shape matches `{"nodes": [...], "edges": [...]}`. The test does not need real data — empty `[]`/`[]` is sufficient.
- **Implementation note (commit 53b1da0)**: `formatted_graph_data` was stubbed in `ComponentHandles` (always returns `{}`) because `cognee_graph::get_formatted_graph_data` is not yet re-exported through `cognee-lib`. The stub is wire-correct for the WebSocket payload (Python falls back to `{}` when the graph is empty). Full wiring is deferred to **P5** where `ComponentHandles` is fully populated. See `crates/http-server/src/components.rs` for the stub and its `TODO: wire to cognee_graph::get_formatted_graph_data` comment.

### Step 7 — `crates/http-server/src/routers/cognify.rs` — POST handler

- Files: `crates/http-server/src/routers/cognify.rs` (new), `crates/http-server/src/dto/cognify.rs` (new — `CognifyPayloadDTO`, `CognifyWsFrameDTO`).
- Spec: [../routers/cognify.md §2.1](../routers/cognify.md#21-post-apiv1cognify--run-the-cognify-pipeline).
- Action:
  1. Parse the JSON body into `CognifyPayloadDTO` via the custom `Json` extractor.
  2. Validate that at least one of `datasets` / `dataset_ids` is non-empty → otherwise `ApiError::BadRequest("No datasets or dataset_ids provided")` (note: this surfaces as `{"error": "..."}`, not `{"detail": ...}`, per the Python source — extend `ApiError::BadRequest` if needed, or use a dedicated variant; document the choice inline).
  3. When `dataset_ids` is provided, use it instead of `datasets` (Python parity, `dataset_ids if payload.dataset_ids else payload.datasets`). Do not merge.
  4. Resolve `ontology_key` via `state.lib.ontology().get_contents(...)` ([cognify.md row](../routers/cognify.md#21-post-apiv1cognify--run-the-cognify-pipeline)). On `Err` map to `ApiError::Conflict` with the literal Python message. Concatenate the contents and wrap in `cognee_ontology::RDFLibOntologyResolver` (single resolver across all keys).
  5. **Best-effort `DatasetConfiguration` round-trip** for the *first* dataset: if the first identifier parses as UUID, look up the row and fill `graph_model` / `custom_prompt` if the request omitted them. After the run, persist any new values back. Both reads and writes use `tracing::warn!` on failure and **do not** propagate the error. Skip silently if the first identifier is a name (Python parity).
  6. Per-dataset fan-out: for each resolved dataset, call the dispatcher from Step 5 with `pipeline_name = "cognify_pipeline"`. The `work` future invokes `cognee_lib::cognify::cognify(dataset, user, CognifyConfig { graph_schema, ontology_resolver, custom_prompt, chunks_per_batch, .. })`.
  7. Aggregate: build a `Map<dataset_id_str, PipelineRunInfoDTO>` per [cognify.md §2.1 success-blocking shape](../routers/cognify.md#21-post-apiv1cognify--run-the-cognify-pipeline). For background, `payload` is always `[]` ([../pipelines.md §9.2](../pipelines.md#92-background-runinbackgroundtrue)).
  8. **`PipelineRunErrored` aggregation (blocking only)**: walk the per-dataset results, find the **first** `PipelineRunErrored`, and return `ApiError::PipelineErrored { source: Cognify, run_info: serde_json::json!({"error": "Pipeline run errored", "detail": "<msg>"}) }` (note: the body shape for cognify is the plain envelope, not the run object; `Cognify` mapper produces `500`).
- Verify: `cargo check`. The `PipelineRunErrored` aggregation is exercised in the integration test from §5 below; this step must compile and pass the unit test for "empty datasets" and "dataset_ids overrides datasets".

### Step 7.1 — Cognify POST: response-shape unit tests

- Files: `crates/http-server/src/routers/cognify.rs` (extend Step 7 with `#[cfg(test)] mod tests`).
- Action: cover the four pure-router unit tests called out in [../routers/cognify.md §5](../routers/cognify.md#5-implementation-tasks) item 6:
  1. Empty `datasets` AND `dataset_ids` → `400 {"error": "No datasets or dataset_ids provided"}`.
  2. `datasets=["x"]`, `dataset_ids=["<uuid>"]` → handler resolves only the UUID list (Python parity, do not merge).
  3. Unknown ontology key → handler propagates `cognee_ontology` error as `409`.
  4. First-errored aggregation: two datasets, one returns `PipelineRunCompleted` and one returns `PipelineRunErrored`. Expect `500` with `{"error": "Pipeline run errored", "detail": "<the errored run's error string>"}`. Pick the **first** errored entry deterministically (Python uses `next(...isinstance(v, PipelineRunErrored))`).
- Verify: `cargo test -p cognee-http-server --lib routers::cognify`.

### Step 8 — `crates/http-server/src/routers/cognify.rs` — WebSocket handler

- Files: `crates/http-server/src/routers/cognify.rs` (extend Step 7 with `ws_subscribe`).
- Spec: [../websocket.md §9](../websocket.md#9-server-side-implementation), [../routers/cognify.md §2.2](../routers/cognify.md#22-ws-apiv1cognifysubscribepipeline_run_id--live-pipeline-progress).
- Action:
  1. Handler signature: `async fn ws_subscribe(ws: WebSocketUpgrade, State(state): State<AppState>, Path(run_id): Path<Uuid>, cookies: CookieJar) -> impl IntoResponse`. Accept the upgrade unconditionally — Python parity ([../websocket.md §9.2](../websocket.md#92-why-we-accept-the-upgrade-before-auth)). Use `ws.on_upgrade(move |socket| ws_loop(socket, state, run_id, cookies))` so the auth + loop logic runs only after the WebSocket handshake completes.
  2. Inside `ws_loop`: authenticate via `auth::cookie::authenticate_from_cookie(&state, &cookies).await`. On `Err`, send a Close frame with code `1008` and reason `"Unauthorized"` (literal UTF-8, no JSON envelope), then return. Do not retry; do not leak the underlying auth error.
  3. Subscribe via `state.pipelines.subscribe(run_id)` — the registry returns an empty-but-attached `Pin<Box<dyn Stream<Item = RunEvent>>>` for unknown ids (Python's `initialize_queue` parity). The stream is non-fallible for the consumer; the registry maps internal `BroadcastStream::Lagged` errors to a synthetic `RunEvent { kind: Errored { message: "subscriber lagged" } }` that the WS handler maps to a `1011` close per [../pipelines.md §6.4](../pipelines.md#64-channel-implementation).
  4. Resolve `dataset_id` once from the registry's run snapshot if known. For unknown ids the snapshot is `None`; pass `None` through to `formatted_graph_data`, which yields `{}`.
  5. Loop: `while let Some(event) = events.next().await { ... }`. For each event:
     - Compute `payload = state.lib.formatted_graph_data(event.dataset_id.or(dataset_id), &user).await.unwrap_or_else(|_| serde_json::json!({}))`.
     - Build the frame `{ "pipeline_run_id": event.run_id, "status": event_kind_to_python_string(&event.kind), "payload": payload }`.
     - Send as `Message::Text(frame.to_string())`. On send error, the client has disconnected — return immediately. Do **not** abort the underlying pipeline run; the producer continues unaffected.
  6. **Terminal handling — strict Python parity**: only `RunEventKind::Completed` triggers `socket.send(Close frame {code: 1000, reason: ""})` followed by `return`. `RunEventKind::Errored` and `RunEventKind::AlreadyCompleted` are forwarded **but the loop continues** ([../websocket.md §6](../websocket.md#6-status-semantics--terminal-close)). The loop ends naturally when the producer closes the channel or the client disconnects.
  7. On the synthetic Lagged event, close with `1011 "channel lagged"` and return — do not forward the synthetic event to the client.
- Verify: `cargo check`. Behaviour is exercised in `test_cognify_websocket.rs` (§5).

### Step 8.1 — Cookie reading inside `on_upgrade`

- Files: `crates/http-server/src/auth/cookie.rs` (or wherever P1 placed the cookie helper), `crates/http-server/src/routers/cognify.rs`.
- Spec: [../websocket.md §4](../websocket.md#4-authentication).
- Action: P1 ships `authenticate_from_cookie(state, cookies) -> Result<AuthenticatedUser, AuthError>` for HTTP handlers. The WS handler calls the same function inside `on_upgrade`, so confirm the helper accepts a `&CookieJar` (or whatever container the upgrade exposes). If P1 only wired it for the HTTP extractor, factor out the inner verification logic so both call sites share it: read the `auth_token` cookie, decode the HS256 JWT, require `aud == ["fastapi-users:auth"]`, require `exp > now`, look up the user by `sub`. On any failure, the WS handler closes with `1008 "Unauthorized"` (a single, opaque reason — do not leak the underlying cause; matches Python). The reason must be a literal string, not a JSON envelope, since WebSocket close-frame reasons are plain UTF-8.
- Verify: unit test the shared helper with: missing cookie, expired JWT, audience mismatch, unknown user. Each branch returns the same `AuthError` shape.

### Step 9 — Mount the cognify router

- Files: `crates/http-server/src/lib.rs` (`build_router`), `crates/http-server/src/routers/mod.rs`.
- Spec: [../routers/cognify.md §5](../routers/cognify.md#5-implementation-tasks) item 4.
- Action: `pub fn router() -> Router<AppState>` returning `Router::new().route("/", post(post_cognify)).route("/subscribe/{pipeline_run_id}", get(ws_subscribe))`. Mount at `/api/v1/cognify` via `.nest("/cognify", cognify::router())`. Also add `[utoipa::path(...)]` for both endpoints — the WS endpoint is documented as a `GET` returning `101 Switching Protocols` ([../websocket.md §2](../websocket.md#2-endpoint)).
- Verify: integration test `test_cognify_blocking.rs` from Step 17 hits `POST /api/v1/cognify` against the live router.

### Step 10 — `crates/http-server/src/routers/memify.rs`

- Files: `crates/http-server/src/routers/memify.rs` (new), `crates/http-server/src/dto/memify.rs` (new — `MemifyPayloadDTO`).
- Spec: [../routers/memify.md](../routers/memify.md).
- Action:
  1. Parse `MemifyPayloadDTO`. Validate that at least one of `dataset_id` / `dataset_name` is non-empty — otherwise `400 {"error": "Either datasetId or datasetName must be provided."}`.
  2. Resolve the dataset (single, not a list — memify is single-dataset).
  3. Call the dispatcher with `pipeline_name = "memify_pipeline"`. The `work` future is `cognee_lib::cognify::memify::memify(MemifyConfig { … })` — sync after the prereq refactor.
  4. Response: a **single** `PipelineRunInfoDTO`, **not** wrapped in a dict ([memify.md §2.1 parity note](../routers/memify.md#21-post-apiv1memify--run-the-memify-enrichment-pipeline)).
  5. On `PipelineRunErrored`, return `ApiError::PipelineErrored { source: Memify, run_info: <envelope> }` → `500` with `{"error": "Pipeline run errored", "detail": "<msg>"}`.
  6. Mount at `/api/v1/memify` in `build_router`.
- Verify: unit test for the validation branch and the success/error response shapes.

### Step 11 — `crates/http-server/src/routers/remember.rs`

- Files: `crates/http-server/src/routers/remember.rs` (new), `crates/http-server/src/dto/remember.rs` (new — `RememberFormDTO`, `UploadedFilePart`, `RememberResultDTO`).
- Spec: [../routers/remember.md](../routers/remember.md).
- Action:
  1. Multipart extraction: reuse the streaming multipart extractor from P2. Collect string parts into `RememberFormDTO`; accumulate file parts in `Vec<UploadedFilePart>`. Honour the camelCase form names `datasetName` / `datasetId` via `#[serde(rename = "datasetName")]` etc. Apply the `node_set=[""] → None` translation **after** extraction ([remember.md §2.1 Python parity note](../routers/remember.md#21-post-apiv1remember--ingest--cognify-in-one-call)).
  2. Validate that at least one of `datasetName` / `datasetId` is set — otherwise `400 {"detail": "Either datasetId or datasetName must be provided."}` (note: Python uses `HTTPException(400, detail=...)` so the body uses `detail`, not `error`).
  3. Call the dispatcher with `pipeline_name = "cognify_pipeline"` (remember's pipeline run uses the cognify name; the `add` step is not pipeline-tracked). The `work` future is `cognee_lib::api::remember::remember(files, RememberConfig { … }, user)` — sync after the prereq refactor.
  4. Response: serialise the `RememberResult` to JSON via `serde_json::to_value` and return it directly. Match the keys from [remember.md §2.1 success body](../routers/remember.md#21-post-apiv1remember--ingest--cognify-in-one-call).
  5. **Catch-all is `409`, not `500`** ([remember.md §2.1 Python parity note](../routers/remember.md#21-post-apiv1remember--ingest--cognify-in-one-call)). Even a `PipelineRunErrored` from the cognify portion surfaces as `ApiError::Conflict("An error occurred during remember.")` with the literal body — do **not** use `ApiError::PipelineErrored` here. Body shape is `{"error": "An error occurred during remember."}` with **no** `detail`.
  6. Apply `tower_http::limit::RequestBodyLimitLayer::new(state.config.body_limit)` on this nested router (default 100 MiB).
  7. Mount at `/api/v1/remember` in `build_router`.
- Verify: unit tests for the multipart parse, the `node_set=[""] → None` translation, the catch-all `409`, and the camelCase wire names.

### Step 11.1 — Multipart parsing details for `/remember`

- Files: `crates/http-server/src/multipart.rs` (extend the P2 helper if needed — note: this file is at the crate root, **not** inside `middleware/`), `crates/http-server/src/routers/remember.rs` (Step 11 caller).
- Spec: [../routers/remember.md §3](../routers/remember.md#3-cross-cutting-behavior), [../routers/remember.md §5](../routers/remember.md#5-implementation-tasks) item 2.
- Action: confirm the P2 multipart extractor produces a stream of `(name, content_type, AsyncRead)` per part. The remember handler iterates parts and dispatches by name:
  - Name `data` → stream to a per-request tempdir (`dirs::cache_dir()/cognee/uploads/<request-id>/<part-name>`); accumulate `UploadedFilePart` entries with `(filename, content_type, temp_path, byte_count)`.
  - Names `datasetName`, `datasetId`, `node_set`, `run_in_background`, `custom_prompt`, `chunks_per_batch` → collect string bytes and feed into `RememberFormDTO` via per-field setters. `node_set` accumulates across multiple `node_set=...` parts (multipart-array convention).
  - After extraction, apply `if form.node_set == Some(vec!["".into()]) || form.node_set == Some(vec![]) { form.node_set = None }` per [../routers/remember.md §2.1 Python parity note](../routers/remember.md#21-post-apiv1remember--ingest--cognify-in-one-call).
  - Tempfile cleanup: schedule a `tokio::spawn` cleanup hook bound to the handler's exit (success or failure) so disconnected clients do not leak files. The orphan-sweep follow-up is tracked in [../routers/remember.md §6](../routers/remember.md#6-open-questions).
- Verify: a unit test that drives the extractor with a synthetic multipart body containing two `data` parts and the camelCase form fields, asserting the `RememberFormDTO` matches and both files land in the tempdir.

### Step 12 — `crates/http-server/src/routers/improve.rs`

- Files: `crates/http-server/src/routers/improve.rs` (new), `crates/http-server/src/dto/improve.rs` (new — `ImprovePayloadDTO`).
- Spec: [../routers/improve.md](../routers/improve.md).
- Action:
  1. Parse `ImprovePayloadDTO`. Validate that at least one of `dataset_id` / `dataset_name` is non-empty — otherwise `400 {"detail": "Either datasetId or datasetName must be provided."}` (note `detail`, not `error`).
  2. Call the dispatcher with `pipeline_name = "memify_pipeline"` — improve reduces to memify when `session_ids` is absent (Phase 3 does not expose `session_ids`). The `work` future is `cognee_lib::api::improve::improve(ImproveConfig { …, session_ids: None }, user)` — sync after the prereq refactor.
  3. Response: a **single** `PipelineRunInfoDTO`, same shape as memify.
  4. **`PipelineRunErrored` → 420 (parity quirk)** ([improve.md §2.1](../routers/improve.md#21-post-apiv1improve--run-the-improve-pipeline-memify--optional-session-bridging)). Return `ApiError::PipelineErrored { source: Improve, run_info: serde_json::to_value(&pipeline_run_info)? }`. The mapper from Step 4 emits `420` with the run object as the body — **not** the canonical `{"error":..., "detail":...}` envelope.
  5. **Catch-all is `409`, not `500`** ([improve.md §2.1](../routers/improve.md#21-post-apiv1improve--run-the-improve-pipeline-memify--optional-session-bridging)). Use `ApiError::Conflict("An error occurred during graph improvement.")`.
  6. Mount at `/api/v1/improve` in `build_router`. Add the OpenAPI annotation with the 420 response declared explicitly so codegen tools recognise it.
- Verify: unit test asserting the literal `420` status code on `PipelineRunErrored` (this is the headline parity quirk).

### Step 13 — `formatted_graph_data` integration in WS handler

- Files: `crates/http-server/src/routers/cognify.rs` (refine Step 8).
- Spec: [../websocket.md §5.3](../websocket.md#53-payload-computation), [../routers/cognify.md §2.2 Python parity notes](../routers/cognify.md#22-ws-apiv1cognifysubscribepipeline_run_id--live-pipeline-progress).
- Action: ensure the WS loop calls `state.lib.formatted_graph_data(dataset_id, &user)` on **every** event (yes, even on `PipelineRunYield` — wasteful but matches Python). On `Err`, substitute `serde_json::json!({})` and continue. Do **not** cache between events; do **not** skip on yield. This is strict wire parity.
- Verify: integration test `test_cognify_websocket.rs` asserts `payload` is a JSON object with `nodes` / `edges` keys (or `{}` when the dataset is empty) on every frame.

### Step 14 — Graceful shutdown calls `state.pipelines.shutdown()`

- Files: `crates/http-server/src/main.rs` (the binary's `main`), `crates/http-server/src/lib.rs` (`run` helper if it owns the signal handler).
- Spec: [../pipelines.md §12](../pipelines.md#12-crash--restart-recovery), [../architecture.md §15](../architecture.md#15-graceful-shutdown) (the shutdown sketch).
- Action: in the SIGTERM / SIGINT handler attached to `axum::serve(...).with_graceful_shutdown(...)`, await `state.pipelines.shutdown()` **before** dropping `state`. The default `RegistryConfig` writes `DATASET_PROCESSING_ERRORED` rows for each in-flight run with `run_info = {"reason": "server_shutdown"}`. Operators wanting strict Python parity can flip `RegistryConfig::abort_writes_errored_row = false` via `HttpServerConfig`.
- Verify: integration test `test_pipelines_shutdown.rs` (Step 17) drives this end-to-end.

### Step 15 — OpenAPI annotations

- Files: each router file from Steps 7, 10, 11, 12, plus the central `ApiDoc` modifier (P0 wired this — extend the `paths(...)` and `components(schemas(...))` lists).
- Spec: [../architecture.md §13](../architecture.md#13-openapi-generation----utoipa) (utoipa conventions), per-router OpenAPI sections in each spec.
- Action: add `#[utoipa::path(...)]` to every handler from Steps 7–12, declaring full response coverage:
  - **`POST /api/v1/cognify`**: `tag = "cognify"`, `request_body = CognifyPayloadDTO`, responses 200 (`HashMap<String, PipelineRunInfoDTO>`), 400, 401, 403, 409, 422, 500. Document the 409 specifically as the unknown-ontology-key path.
  - **`GET /api/v1/cognify/subscribe/{pipeline_run_id}`**: `tag = "cognify"`, response 101 ("Switching Protocols"). utoipa's idiomatic way is a single `(status = 101, description = "Switching Protocols")` entry — the actual frame schema is not part of the OpenAPI document. Add a description block referencing [../websocket.md §5](../websocket.md#5-frame-format) so SDK generators know where to find the wire shape.
  - **`POST /api/v1/memify`**: `tag = "memify"`, `request_body = MemifyPayloadDTO`, responses 200 (`PipelineRunInfoDTO`), 400, 401, 403, 422, 500.
  - **`POST /api/v1/remember`**: `tag = "remember"`, `request_body(content_type = "multipart/form-data")` with the `data` field annotated as `array of binary`. Responses 200 (`RememberResultDTO`), 400, 401, 403, 409, 413, 422.
  - **`POST /api/v1/improve`**: `tag = "improve"`, `request_body = ImprovePayloadDTO`, responses 200 (`PipelineRunInfoDTO`), 400, 401, 403, 409, **420** (the headline parity quirk — declare it explicitly with `(status = 420, description = "Pipeline run errored", body = PipelineRunInfoDTO)`), 422.
- Verify: `cargo check` against the OpenAPI test harness from P0 (the existing `openapi.json` snapshot test extends to cover the new routes). Confirm utoipa accepts the literal `420` status code; if it rejects, file the workaround in [../observability.md open questions](../observability.md#11-open-questions) per [../routers/improve.md §6](../routers/improve.md#6-open-questions) item 7.

### Step 15.1 — End-to-end smoke test plan

- Files: none (manual verification step).
- Spec: [../routers/cognify.md §2.2.1 worked example](../routers/cognify.md#221-websocket-worked-example).
- Action: with the binary running locally and an OpenAI-compatible LLM reachable, drive the worked example:
  1. `POST /api/v1/auth/login` to acquire a session cookie.
  2. `POST /api/v1/add` (P2) with a small text fixture.
  3. `POST /api/v1/cognify {"datasets": ["..."], "run_in_background": true}` and capture the `pipeline_run_id`.
  4. Open a WS to `/api/v1/cognify/subscribe/{pipeline_run_id}` with the cookie. Assert the frame sequence ends with `PipelineRunCompleted` and a Close frame `1000`.
  5. Re-issue the same `POST /api/v1/cognify` and assert the second response carries `status="PipelineRunAlreadyCompleted"` ([../pipelines.md §3.3](../pipelines.md#33-live-event-status--emitted-on-the-registry-channel-and-the-websocket-frame)).
- Verify: capture the frame transcript and attach to the phase PR. The cross-SDK Python comparison ships in P8 and is out of scope here.

### Step 16 — Update status tables

- Files: `docs/http-server/implementation/README.md` (P3 status row), `docs/http-server/routers/README.md` (rows 13–16: cognify, memify, remember, improve).
- Action: flip the four router rows to **Done** and the P3 row to **Done** **after** all P3 tests pass and `scripts/check_all.sh` is green.
- Verify: doc-only commit; no code change.

## 5. Tests

All test files live under `crates/http-server/tests/`. Integration tests use `axum::serve` on `127.0.0.1:0` and `tokio-tungstenite` for WebSocket clients. LLM-dependent tests are gated behind `OPENAI_URL` / `OPENAI_TOKEN` and skip gracefully when absent (consistent with the project's existing test convention from [.claude/CLAUDE.md](../../../.claude/CLAUDE.md)).

### Shared test harness

Every integration test in this phase needs the same scaffolding:

- A `TestServer` helper (P0 ships this) that builds an in-memory SQLite-backed `AppState`, calls `build_router(state)`, binds `axum::serve` on `127.0.0.1:0`, and exposes the bound `SocketAddr`.
- A `login_and_cookie(server)` helper (P1 ships this) that drives `POST /api/v1/auth/login` and returns a `reqwest::cookie::Jar` plus a JWT bearer string.
- For WS tests, a `connect_ws(server, path, cookies)` helper using `tokio_tungstenite::connect_async` with the cookie header set on the upgrade request.
- For pipeline-error injection, a `MockPipelineRunRegistry` (the P3 prereq lands one in `cognee-test-utils`) that lets tests scripted-emit `RunEvent`s on demand. Use it for `test_improve_420.rs` and `test_pipelines_shutdown.rs` — those tests should not require a real LLM.

Mark LLM-dependent tests with `#[ignore]` *or* a runtime `if std::env::var("OPENAI_URL").is_err() { return; }` early-return per the project's existing convention. The `bash scripts/run_tests_with_openai.sh` script populates the env vars from `.env` and runs all tests serially (single-threaded for LLM isolation).

| File | Coverage |
|---|---|
| `tests/test_cognify_blocking.rs` | `POST /api/v1/cognify` with `run_in_background=false`. Pre-add a tiny dataset, then cognify. Assert response shape: `Map<dataset_id_str, PipelineRunInfoDTO>` with `status="PipelineRunCompleted"` and `payload` being a non-empty graph snapshot. Gated on `OPENAI_URL`. |
| `tests/test_cognify_background.rs` | `POST /api/v1/cognify` with `run_in_background=true`. Assert the response returns immediately with `status="PipelineRunStarted"` and `payload=[]` per [../pipelines.md §9.2](../pipelines.md#92-background-runinbackgroundtrue). The `pipeline_run_id` matches the deterministic `uuid5(NAMESPACE_OID, "{pipeline_id}_{dataset_id}")`. |
| `tests/test_cognify_websocket.rs` | Start a background cognify, attach a WS subscriber via `tokio-tungstenite`, capture frames, assert: (a) every frame has `pipeline_run_id`, `status`, `payload`; (b) `status` sequence ends with `PipelineRunCompleted`; (c) the server sends a Close frame `1000` after `PipelineRunCompleted`; (d) **forced-error variant**: corrupt the dataset to force `PipelineRunErrored` and assert the WS **stays open** after forwarding that frame (Python parity quirk per [../websocket.md §6](../websocket.md#6-status-semantics--terminal-close)). Use `tokio::time::timeout` with a generous bound to detect the no-close behaviour. Also assert that an unauthenticated connect (no cookie) closes with `1008 "Unauthorized"`. |
| `tests/test_cognify_already_completed.rs` | Invoke `POST /api/v1/cognify` twice on the same dataset within the registry's TTL. The second invocation must return `status="PipelineRunAlreadyCompleted"` for that dataset, with no new `pipeline_runs` row written ([../pipelines.md §3.3](../pipelines.md#33-live-event-status--emitted-on-the-registry-channel-and-the-websocket-frame), [../routers/cognify.md §3.2](../routers/cognify.md#32-connection-between-post-pipeline_run_id-and-ws-subscription)). |
| `tests/test_memify.rs` | Blocking and background variants. Assert response shape is a **single** `PipelineRunInfoDTO`, not a dict ([../routers/memify.md §2.1 parity note](../routers/memify.md#21-post-apiv1memify--run-the-memify-enrichment-pipeline)). Validate the `dataset_id="" + dataset_name="foo"` → name fallback path. Assert `dataset_id` and `dataset_name` both empty → `400 {"error": "Either datasetId or datasetName must be provided."}`. |
| `tests/test_remember.rs` | Multipart upload with two files + `datasetName`. Assert response keys per [../routers/remember.md §2.1](../routers/remember.md#21-post-apiv1remember--ingest--cognify-in-one-call). Negative path: induce an inner error and assert `409 {"error": "An error occurred during remember."}` with no `detail` field. Verify the `node_set=[""] → None` translation by spying on the inner `cognee_lib::api::remember` call (use a test harness that captures the config). |
| `tests/test_improve.rs` | Blocking and background. Assert response shape matches `PipelineRunInfoDTO`. Validate the `dataset_id` empty + `dataset_name="foo"` path. The `420` quirk is covered separately in `test_improve_420.rs`. |
| `tests/test_improve_420.rs` | Force `PipelineRunErrored` (e.g. via a mocked library function or a deliberately corrupt dataset) and assert: (a) HTTP status is **literally `420`**; (b) body is the serialised `PipelineRunInfoDTO` with `status="PipelineRunErrored"` and an `error` field; (c) body is **not** wrapped in the canonical `{"error":..., "detail":...}` envelope. This is the headline parity test for the phase. |
| `tests/test_pipelines_shutdown.rs` | Start a background cognify (or any other pipeline), give it ~50 ms to publish `PipelineRunStarted`, then trigger graceful shutdown by signalling the server's shutdown channel. Assert: (a) `state.pipelines.shutdown()` returns `Ok`; (b) the durable `pipeline_runs` table contains a row for that `pipeline_run_id` with `status="DATASET_PROCESSING_ERRORED"` and `run_info` containing `"reason": "server_shutdown"` per [../pipelines.md §12](../pipelines.md#12-crash--restart-recovery); (c) any attached WS subscriber received a final `RunEventKind::Errored` frame. |

Cross-SDK parity tests (`e2e-cross-sdk/harness/test_http_{cognify,memify,remember,improve}.py`) are out of scope for this phase — they ship with [P8](p8-e2e-parity.md). The Rust-side coverage in this phase is sufficient to prove wire compatibility for downstream P8 work.

## 6. Acceptance criteria

- [x] `cargo check --all-targets -p cognee-http-server` is green.
- [x] All P3 test files listed in §5 pass under `bash scripts/run_tests_with_openai.sh` (LLM-dependent ones skip gracefully when env vars are absent).
- [x] `scripts/check_all.sh` passes (fmt, check, clippy, capi/python/js wrapper checks).
- [ ] Manual smoke: a real `POST /api/v1/cognify` with `run_in_background=true` followed by a WebSocket subscription on `/api/v1/cognify/subscribe/{pipeline_run_id}` produces an event stream that ends with a single `PipelineRunCompleted` frame and a Close frame `1000`. The forced-error variant leaves the socket open until the client disconnects.
- [x] `state.pipelines.shutdown()` is called from the SIGTERM / SIGINT handler in the binary, and a manual SIGTERM during an in-flight run results in a `DATASET_PROCESSING_ERRORED` row visible via `/api/v1/datasets/status` (or direct DB query, since the `/datasets/status` endpoint is owned by P2/P5).
- [x] `/improve` returns literal status `420` on `PipelineRunErrored`, with the `PipelineRunInfo` object as the body (not the canonical error envelope). Asserted by `test_improve_420.rs`.
- [x] `/remember` returns literal status `409 {"error": "An error occurred during remember."}` for any error (including `PipelineRunErrored` from the inner cognify). Asserted by `test_remember.rs`.
- [x] The cognify WebSocket forwards `PipelineRunErrored` and `PipelineRunAlreadyCompleted` frames **without** closing the socket. Asserted by `test_cognify_websocket.rs`.
- [x] Status table in [README.md](README.md) and [../routers/README.md](../routers/README.md) updated: rows for cognify, memify, remember, improve flipped to **Done**; P3 row flipped to **Done**.

### Behaviour matrix to assert in tests

The four routers + the WS endpoint share a small set of orthogonal behaviours. Use this matrix when scoping a test file so you do not duplicate coverage.

| Behaviour | Cognify | Memify | Remember | Improve |
|---|---|---|---|---|
| Validation: dataset id/name required | 400 `error` | 400 `error` | 400 `detail` | 400 `detail` |
| `dataset_id=""` translates to `None` | n/a (uses `dataset_ids` array) | yes | yes | yes |
| `node_set=[""]` translates to `None` | n/a | n/a | yes | n/a |
| Per-dataset response shape | dict | single | flat (`RememberResultDTO`) | single |
| Background `payload` is `[]` | yes | yes | yes (cognify portion) | yes |
| `PipelineRunErrored` status code | 500 | 500 | 409 (catch-all) | **420** |
| Error envelope on PipelineErrored | `{"error":..., "detail":...}` | `{"error":..., "detail":...}` | `{"error":...}` (no detail) | raw `PipelineRunInfoDTO` |
| Catch-all status | 500 | 500 | 409 | 409 |
| WebSocket close on `Errored` | **no** (Python parity) | n/a | n/a | n/a |
| WebSocket close on `Completed` | yes (1000) | n/a | n/a | n/a |

Each cell that says "n/a" means the surface is not exposed by that router (no WS, no multipart, no array fan-out, etc.). Cells marked **bold** are the parity quirks that tests must assert literally.

## 6.1 Common pitfalls

A list of things a less-powerful executor is likely to get wrong if not specifically warned. Read this before starting Step 7.

- **Do not close the WebSocket on `PipelineRunErrored`.** Python parity requires that only `PipelineRunCompleted` triggers `socket.close(1000)`. Errored and AlreadyCompleted are forwarded but the loop continues. If your test asserts a clean disconnect on Errored, the test is wrong, not the implementation. See [../websocket.md §6](../websocket.md#6-status-semantics--terminal-close).
- **Do not wrap memify's response in a dict.** Cognify returns `Map<dataset_id_str, PipelineRunInfoDTO>` because it is multi-dataset; memify and improve return a **single** `PipelineRunInfoDTO` because they are single-dataset. The shapes diverge intentionally — match Python's accidental divergence.
- **`/improve` on `PipelineRunErrored` returns `420`, not `500`.** And the body is the **raw `PipelineRunInfoDTO`**, not the canonical `{"error":..., "detail":...}` envelope. This is the headline parity quirk.
- **`/remember` swallows everything as `409`.** Including a `PipelineRunErrored` from the inner cognify. Do **not** route remember errors through `ApiError::PipelineErrored` — use `ApiError::Conflict` with the literal Python message.
- **`HTTPException(400, detail=...)` vs `JSONResponse({"error": ...})`** — Python uses different keys for different errors **on the same router**. Match the literal field names per the per-router error tables (`detail` vs `error`).
- **camelCase form parts on `/remember`**: `datasetName` and `datasetId` use camelCase, not snake_case. Use `#[serde(rename = "...")]` on the form-extractor struct.
- **`node_set=[""]` translation must run *after* extraction**, not as part of the deserializer. The `[""]` default is observable on the wire; the handler-side translation to `None` is the second step.
- **`formatted_graph_data` runs on every event, including yields**. Wasteful but Python-parity. Do not optimize; do not cache.
- **Accept the WebSocket upgrade *before* auth.** Python's `await websocket.accept()` runs unconditionally; cookie verification happens on the established connection. If you reject at the HTTP layer, you diverge from Python.
- **`run_in_background` is an HTTP concern only.** After the prereq refactor, `cognee_lib::api::remember::remember()` and `cognee_lib::api::improve::improve()` are sync. If you find yourself writing `if run_in_background { spawn(...) }` inside the `work` closure, you have the layering wrong — that decision lives in the dispatcher (Step 5).

## 7. Files touched

New files:

- `crates/http-server/src/dto/pipeline_run.rs` — shared `PipelineRunInfoDTO` and wire-string helpers (Step 2).
- `crates/http-server/src/dto/util.rs` — `DatasetIdRef` deserializer (Step 3).
- `crates/http-server/src/dto/cognify.rs` — `CognifyPayloadDTO`, `CognifyWsFrameDTO` (Step 7).
- `crates/http-server/src/dto/memify.rs` — `MemifyPayloadDTO` (Step 10).
- `crates/http-server/src/dto/remember.rs` — `RememberFormDTO`, `UploadedFilePart`, `RememberResultDTO` (Step 11).
- `crates/http-server/src/dto/improve.rs` — `ImprovePayloadDTO` (Step 12).
- `crates/http-server/src/pipelines/mod.rs` — module declaration (Step 5).
- `crates/http-server/src/pipelines/dispatch.rs` — HTTP-side dispatcher (Step 5).
- `crates/http-server/src/routers/cognify.rs` — POST + WS handlers (Steps 7, 8).
- `crates/http-server/src/routers/memify.rs` — POST handler (Step 10).
- `crates/http-server/src/routers/remember.rs` — multipart POST handler (Step 11).
- `crates/http-server/src/routers/improve.rs` — POST handler (Step 12).
- `crates/http-server/tests/test_cognify_blocking.rs`
- `crates/http-server/tests/test_cognify_background.rs`
- `crates/http-server/tests/test_cognify_websocket.rs`
- `crates/http-server/tests/test_cognify_already_completed.rs`
- `crates/http-server/tests/test_memify.rs`
- `crates/http-server/tests/test_remember.rs`
- `crates/http-server/tests/test_improve.rs`
- `crates/http-server/tests/test_improve_420.rs`
- `crates/http-server/tests/test_pipelines_shutdown.rs`

Modified files:

- `crates/http-server/src/state.rs` — drop `Option` from `pipelines`, wire the registry (Step 1).
- `crates/http-server/src/lib.rs` — `AppState::build` constructs the registry; `build_router` mounts the four nested routers (Steps 1, 9, 10, 11, 12).
- `crates/http-server/src/main.rs` — call `state.pipelines.shutdown()` in the graceful-shutdown handler (Step 14).
- `crates/http-server/src/error.rs` — extend `ApiError::PipelineErrored` with `{ source, run_info }` and add `PipelineErrorSource` enum (Step 4).
- `crates/http-server/src/routers/mod.rs` — module declarations for the four new routers (Steps 9, 10, 11, 12).
- `crates/http-server/src/dto/mod.rs` — re-exports for the new DTO modules (Steps 2, 3, 7, 10, 11, 12).
- `crates/lib/src/lib.rs` (or a thin module) — re-export `formatted_graph_data` from `cognee-graph` if not already exposed (Step 6).
- `docs/http-server/implementation/README.md` — flip P3 row status (Step 16).
- `docs/http-server/routers/README.md` — flip cognify / memify / remember / improve rows (Step 16).
