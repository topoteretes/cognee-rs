# Implementation: P6 — Observability

## 1. Goal

Land the observability surface for the HTTP server: the in-process tracing fan-out (`SpanBufferLayer`, redaction, span-attribute key constants), the live read endpoint that exposes the buffer (`GET /api/v1/activity/spans`), the four sibling activity endpoints that surface recent pipeline runs, tenant users, agent metadata, and a Markdown export of a dataset, the cloud-sync orchestration router (`POST /api/v1/sync` + `GET /api/v1/sync/status`) with its own `SyncRegistry` for per-user concurrency, and the cloud connection check (`POST /api/v1/checks/connection`). At the end of P6 the server emits structured spans into a bounded ring buffer alongside its stdout logs, every routed request appears in the buffer with secrets redacted, and the three new routers are mounted at `/api/v1/activity`, `/api/v1/sync`, and `/api/v1/checks`.

## 2. References (read these before starting)

- Live observability tier (the buffer + layer + redaction): [../observability.md §3–§5](../observability.md#3-tracing-stack--tracing--custom-layer).
- Durable observability tier (powers `/activity/pipeline-runs`): [../pipelines.md §5](../pipelines.md#5-database-persistence--pipeline_runs-table) and the `PipelineRunRepository` trait at [§5.2](../pipelines.md#52-the-pipelinerunrepository-trait).
- Tenant scoping for `/users` and `/agents`: [../tenants.md §9](../tenants.md#9-repository-surface).
- Activity router contracts (one §2.x per endpoint): [../routers/activity.md §2.1–§2.5](../routers/activity.md#2-endpoints).
- Sync router contracts and concurrency model: [../routers/sync.md §2 / §3.1 / §3.3](../routers/sync.md#2-endpoints).
- Checks router (cloud probe + Python typo): [../routers/checks.md §2.1](../routers/checks.md#21-post-apiv1checksconnection--validate-a-cloud-api-key).
- Error envelope deviations (`{"error": ...}`, `{"detail", "name"}`): [../architecture.md §9](../architecture.md#9-error-handling) and [../routers/README.md §3.1](../routers/README.md#31-error-envelope).
- Phase summary: [../plan.md §4 P6](../plan.md#4-implementation-phases).
- Implementation invariants (atomic steps, no rationale duplication): [README.md](README.md).

## 3. Prerequisites

- **P0–P5 Done.** P6 depends on `AppState`, `ApiError` (incl. envelope deviations), the `Json<T>` validating extractor, the `ComponentHandles` struct (`crates/http-server/src/components.rs` — carries `database`, `storage`, `permissions: Option<Arc<dyn PermissionsRepository>>`, `formatted_graph_data` stub, etc.), the `AuthenticatedUser` extractor, and the `users` / `user_api_key` / `principals` / `tenants` / `acls` schemas.
- **No `cognee-lib` dependency from `cognee-http-server`** — the dependency goes the other way (cognee-lib has a `server` feature that pulls in cognee-http-server). All handler delegation in P6 calls into `state.components()` (returning `Option<&ComponentHandles>`) and from there into the database / repository / storage handles directly. The "facade" prose in older drafts (`state.lib.users()`, `state.lib.datasets()`, `state.lib.permissions()`) is shorthand for `handles.<field>` calls — see Step 7+ for the corrected call shapes.
- **`PipelineRunRepository` exists** — landed in P3-pre per [../pipelines.md §5.2](../pipelines.md#52-the-pipelinerunrepository-trait). P6 extends this trait with one new method (`list_recent_with_attribution`); do not redesign the trait.
- **`AppState::spans` placeholder** — declared in P0 as `pub spans: Option<Arc<()>>` with a `// TODO(P6): wire Arc<SpanBuffer> here` comment (`crates/http-server/src/state.rs:60–62`). P6 retypes the slot to `Arc<SpanBuffer>` and fills it. ([../architecture.md §6](../architecture.md#6-application-state--dependency-injection))
- **`AppState::sync` placeholder** — declared in P0 as `pub sync: Option<Arc<()>>` with a `// TODO(P7): wire Arc<SyncRegistry> here` comment (`crates/http-server/src/state.rs:64–67`). The TODO was authored as P7 but the slot is filled in P6 per this doc and [../routers/sync.md §3.1](../routers/sync.md#31-concurrency-one-running-sync-per-user); update the comment to `TODO(P6)` (or just delete it once the slot is wired).
- **`cognee_cloud::CloudClient` exists** — landed alongside the serve/disconnect work (commits `99d9b1a`, `9230c07`, `ab18925`). P6 adds two new entry points to `cognee-cloud` (`check_api_key` + the `sync` submodule); it does not touch the device-flow or management API surfaces.
- **`tower_http::trace::TraceLayer` is wired** — installed in P0 at `crates/http-server/src/middleware/tracing.rs`; this phase only adjusts the redacted-headers list.

## 4. Step-by-step

### Step 1: Add span-attribute key constants in `cognee-utils`

- **File(s)**: `crates/utils/src/tracing_keys.rs` (new), `crates/utils/src/lib.rs` (add `pub mod tracing_keys;`).
- **Action**: Define `pub const` `&'static str` constants for every key listed in [../observability.md §3.3](../observability.md#33-span-instrumentation-conventions): `COGNEE_DB_SYSTEM`, `COGNEE_DB_QUERY`, `COGNEE_DB_ROW_COUNT`, `COGNEE_LLM_MODEL`, `COGNEE_LLM_PROVIDER`, `COGNEE_SEARCH_TYPE`, `COGNEE_PIPELINE_NAME`, `COGNEE_PIPELINE_TASK_NAME`, `COGNEE_OPERATION_MODE`, `COGNEE_RECALL_SCOPE`, `COGNEE_FORGET_TARGET`, `COGNEE_DATASET_NAME`, `COGNEE_SESSION_ID`. Values are exactly the dotted strings in the spec table. Include a single doc-comment that points at the Python source-of-truth `cognee/modules/observability/tracing.py`.
- **Spec reference**: [../observability.md §3.3](../observability.md#33-span-instrumentation-conventions).
- **Verify**: `cargo check -p cognee-utils`. Cross-SDK parity assertion (every constant matches the Python value byte-for-byte) lives in P8.

### Step 2: `RecordedSpan`, `SpanStatus`, `BufferConfig`, `SpanBuffer`

- **File(s)**: `crates/http-server/src/observability/mod.rs` (new), `crates/http-server/src/observability/span_buffer.rs` (new).
- **Action**: Add the data types from [../observability.md §4.1 / §4.2](../observability.md#41-type--api):
  - `pub enum SpanStatus { Unset, Ok, Error }` with `#[serde(rename_all = "UPPERCASE")]` so the wire serializes `"UNSET" | "OK" | "ERROR"` — locks down the parity quirk in [../observability.md §11.6](../observability.md#11-open-questions).
  - `pub struct RecordedSpan` with `trace_id` (32-char lowercase hex), `span_id` (16-char lowercase hex), `parent_span_id: Option<String>`, `name`, `start_time_ns: u64`, `end_time_ns: u64`, `duration_ms: f64`, `status: SpanStatus`, `attributes: serde_json::Map<String, serde_json::Value>`.
  - `pub struct BufferConfig { pub max_traces: usize, pub max_spans_per_trace: usize }` with `Default` returning `50` and `1024` respectively. Read from `COGNEE_SPAN_BUFFER_MAX_TRACES` / `COGNEE_SPAN_BUFFER_MAX_SPANS_PER_TRACE` env in `BufferConfig::from_env()`.
  - `pub struct SpanBuffer { inner: Arc<Mutex<BufferInner>>, config: Arc<BufferConfig> }`. `BufferInner` holds `traces: HashMap<String, Vec<RecordedSpan>>` and `trace_order: VecDeque<String>` (oldest at front). `Mutex` lock failure is poison; comment `// lock poison is unrecoverable` and `.unwrap()` per project convention.
  - `pub struct TraceSummary { trace_id, root_name: Option<String>, duration_ms: f64, span_count: usize, status: Option<SpanStatus>, spans: Vec<RecordedSpan> }`. Building it from a `Vec<RecordedSpan>` is its own private helper: pick the parentless span as root, fall back to `spans[0]`; `duration_ms = max of every span.duration_ms` (matches Python L86 `max((... for s in spans), default=0)`); `status = root.status` (mapped to `Option<SpanStatus>` so `Unset` becomes `None` only if the root is missing — keep `Unset` itself as `Some(Unset)` to match Python's exporter).
  - `impl SpanBuffer`: `new(config) -> Self`, `record(&self, span: RecordedSpan)` (push to `traces[trace_id]`, evict via §4.4 LRU when a *new* trace pushes count past `max_traces`; per-trace cap discards new spans silently after `max_spans_per_trace`), `all_traces(&self) -> Vec<TraceSummary>` (snapshot under one lock acquisition, ordered most-recent first), `last_trace(&self) -> Option<TraceSummary>`, `clear(&self)`.
- **Spec reference**: [../observability.md §4](../observability.md#4-in-memory-span-buffer).
- **Verify**: `cargo test -p cognee-http-server --lib observability::span_buffer::tests` — cover record/snapshot, LRU eviction (push 51 distinct trace ids, assert oldest gone), per-trace cap, and `TraceSummary` shape (`duration_ms = max`, root selection rules).

### Step 3: Secret-redaction helper

- **File(s)**: `crates/http-server/src/observability/redaction.rs` (new), `crates/http-server/src/observability/mod.rs` (add `pub(crate) mod redaction;`).
- **Action**: Per [../observability.md §5](../observability.md#5-secret-redaction):
  - Lazy-compile the four regex patterns into `static SECRET_PATTERNS: OnceLock<Vec<Regex>>`. Use `OnceLock` (stdlib) so the redaction module has no extra dependency. Initialization happens on first call; failure of any regex compile is a build-time guarantee — write a unit test that exercises the patterns module so a typo surfaces in CI.
  - `pub fn redact(value: &str) -> Cow<str>` — single sweep: when any pattern matches, replace with `"<first 6 chars>***REDACTED***"` per the spec. Chain replacements so a single string with multiple secrets still ends up fully redacted.
  - `pub fn redact_attributes(attrs: &mut serde_json::Map<String, serde_json::Value>)` — recursive walk; on every string leaf, run `redact`; replace the leaf in place. Object keys are not redacted (only values), matching Python's behavior.
  - Tests: known-bad strings for each of the four patterns (`sk-…`, `api_key=…`, `Authorization: Bearer …`, `password=…`) all become `"<prefix>***REDACTED***"`; inert strings pass through unchanged; nested object with a leaf bearer token is redacted in place.
- **Spec reference**: [../observability.md §5](../observability.md#5-secret-redaction).
- **Verify**: `cargo test -p cognee-http-server --lib observability::redaction::tests`.

### Step 4: `SpanBufferLayer` — the `tracing` integration

- **File(s)**: `crates/http-server/src/observability/span_buffer_layer.rs` (new), `crates/http-server/src/observability/mod.rs` (re-export `SpanBufferLayer`).
- **Action**: Implement `pub struct SpanBufferLayer { buffer: Arc<SpanBuffer> }` and `impl<S> Layer<S> for SpanBufferLayer where S: Subscriber + for<'a> LookupSpan<'a>` per [../observability.md §4.3](../observability.md#43-spanbufferlayer--the-tracing-integration). Three lifecycle hooks:
  - `on_new_span` — assign a fresh 16-char lowercase-hex `span_id`. If the span has no parent (root), generate a fresh 32-char lowercase-hex `trace_id`; otherwise look up the parent's trace id via `ctx.span(parent).extensions().get::<TraceCtx>()`. Stash a `TraceCtx { trace_id, span_id, parent_span_id, start_time_ns: now_ns(), attributes: serde_json::Map::new(), status: SpanStatus::Unset }` on the span via `extensions_mut().insert(...)`. The `tracing::Span::current().extensions_mut()` slot is the only carrier — there is no thread-local trace state. `now_ns()` uses `std::time::SystemTime::UNIX_EPOCH.elapsed()` so timestamps line up with Python's exporter.
  - `on_record` — pull `TraceCtx` from the span; fold the recorded fields into `attributes` via a tiny `Visit` impl (string, i64, u64, bool, f64, debug fallback). Skip the special key `error` for now; let `on_event` flip status if a `tracing::error!` lands inside.
  - `on_close` — pull `TraceCtx`, set `end_time_ns = now_ns()`, compute `duration_ms = (end - start) as f64 / 1_000_000.0`, run `redact_attributes(&mut attrs)`, build a `RecordedSpan`, push to `self.buffer.record(span)`. Handle the case where `TraceCtx` is missing (span created by a sibling layer) by silently dropping.
  - Optional: `on_event` — if the event level is `ERROR`, mark `TraceCtx.status = SpanStatus::Error`. This keeps the `status` field meaningful for the `/spans` viewer.
- **Spec reference**: [../observability.md §4.3](../observability.md#43-spanbufferlayer--the-tracing-integration).
- **Verify**: `cargo test -p cognee-http-server --lib observability::span_buffer_layer::tests` — drive a small workload with `tracing::subscriber::with_default` plus the layer, emit a parent span with two children, assert (a) all three spans share the same `trace_id`, (b) child `parent_span_id` matches parent's `span_id`, (c) `attributes` carry recorded fields, (d) bearer-token attribute comes back redacted via the §3 helper.

### Step 5: Wire the buffer + layer into `AppState` and `init_tracing`

- **File(s)**: `crates/http-server/src/state.rs`, `crates/http-server/src/main.rs`.
- **Action**:
  - In `state.rs`: change `pub spans: Option<Arc<()>>` (current placeholder, lines 60–62) to `pub spans: Arc<SpanBuffer>` (no longer optional). `AppState::build` (and `AppState::build_with_db`) constructs it from `BufferConfig::from_env()`; both `build` paths must populate the field. The handler in step 8 reads `state.spans` directly; if a future embedder wants to disable the buffer, they pass `BufferConfig { max_traces: 0, .. }` (the layer becomes a no-op).
  - In `main.rs`: extend the existing `init_tracing()` (currently a `tracing_subscriber::fmt().with_env_filter(filter).try_init()` call at lines 90–98) per [../observability.md §3.2](../observability.md#32-subscriber-composition). Build the registry as `Registry::default().with(env_filter).with(fmt_layer).with(SpanBufferLayer::new(buffer.clone()))`. The `SpanBuffer` is constructed once in `main()` (return it from a refactored `init_tracing()` that now takes a `&SpanBuffer` arg, or construct the buffer in `main()` and hand it to both `init_tracing` and `AppState::build`). The library's `cognee_http_server::run` does **not** install the subscriber — embedders own that, per [../architecture.md §12](../architecture.md#12-logging--observability).
- **Spec reference**: [../observability.md §3.2](../observability.md#32-subscriber-composition), [../architecture.md §6](../architecture.md#6-application-state--dependency-injection).
- **Verify**: `cargo build -p cognee-http-server --features bin --bin cognee-http-server`. Run the binary, hit `/health`, then `/api/v1/activity/spans` (after step 9 lands) — the spans for that very request appear in the buffer.

### Step 6: Activity DTOs

- **File(s)**: `crates/http-server/src/dto/activity.rs` (new), `crates/http-server/src/dto/mod.rs` (add `pub mod activity;`).
- **Action**: Define every DTO struct from [../routers/activity.md §4](../routers/activity.md#4-dto-definitions): `PipelineRunListItemDTO`, `TraceSummaryDTO`, `RecordedSpanDTO`, `TenantUserDTO`, `AgentDTO`, `SpansErrorEnvelopeDTO`. All carry `#[derive(Debug, Clone, Serialize, ToSchema)]`. ISO-8601 timestamp fields stay `Option<String>` (Python parity — Python emits `.isoformat()` strings, not OpenAPI `date-time`); document the choice inline. `RecordedSpanDTO::status: String` (not the enum) so it matches Python's already-stringified shape.
- **Spec reference**: [../routers/activity.md §4](../routers/activity.md#4-dto-definitions).
- **Verify**: `cargo check -p cognee-http-server`.

### Step 7: Extend `PipelineRunRepository` with `list_recent_with_attribution`

- **File(s)**: `crates/database/src/pipelines/repository.rs` (extend trait), `crates/database/src/pipelines/sea_orm_impl.rs` (extend impl), `crates/database/src/pipelines/mod.rs` (re-export the new row type if needed).
- **Action**: Add a new trait method per [../routers/activity.md §2.1](../routers/activity.md#21-get-apiv1activitypipeline-runs--list-recent-pipeline-runs):
  ```rust
  async fn list_recent_with_attribution(
      &self,
      dataset_id: Option<Uuid>,
      limit: u32,
  ) -> Result<Vec<PipelineRunWithAttributionRow>, DbError>;
  ```
  `PipelineRunWithAttributionRow` is a new row type — superset of `PipelineRunRow` plus `dataset_name: Option<String>`, `owner_id: Option<Uuid>`, `owner_email: Option<String>`. SeaORM impl: `pipeline_runs LEFT JOIN datasets ON pipeline_runs.dataset_id = datasets.id LEFT JOIN users ON datasets.owner_id = users.id ORDER BY pipeline_runs.created_at DESC LIMIT $limit`. When `dataset_id.is_some()`, add the `WHERE pipeline_runs.dataset_id = $1` clause.
  - Optional `dataset_id` matches Python's `WHERE` toggle in [Python L37–L42](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L37-L42).
  - **No tenant filter** — see [../routers/activity.md §3](../routers/activity.md#3-cross-cutting-behavior). The query is intentionally cross-tenant for Python parity.
- **Spec reference**: [../pipelines.md §5.2](../pipelines.md#52-the-pipelinerunrepository-trait) (existing trait), [../routers/activity.md §2.1](../routers/activity.md#21-get-apiv1activitypipeline-runs--list-recent-pipeline-runs).
- **Verify**: `cargo test -p cognee-database --test pipeline_run_attribution` — seed two `pipeline_runs` rows with attached `datasets` + `users`, one orphaned row (NULL dataset_id), assert all three come back, the orphan has `dataset_name = None` / `owner_email = None`, the dataset filter narrows correctly, ordering is DESC by `created_at`.

### Step 8: `GET /api/v1/activity/pipeline-runs`

- **File(s)**: `crates/http-server/src/routers/activity.rs` (new — first handler), `crates/http-server/src/routers/mod.rs` (add `pub mod activity;`).
- **Action**: Add handler `pub async fn get_pipeline_runs(State(state): State<AppState>, _user: AuthenticatedUser, Query(filter): Query<PipelineRunsQuery>) -> Result<Json<Vec<PipelineRunListItemDTO>>, ApiError>`. `PipelineRunsQuery` has one optional `dataset_id: Option<Uuid>` field. Body:
  1. Resolve a `&dyn PipelineRunRepository` from `state.components().ok_or_else(|| ApiError::Internal(...))?` — call `SeaOrmPipelineRunRepository::new(handles.database.clone()).list_recent_with_attribution(filter.dataset_id, 50).await?` (or store an `Arc<dyn PipelineRunRepository>` directly on `ComponentHandles` if it grows worth caching). The pre-existing `state.pipelines: Arc<dyn PipelineRunRegistry>` is the **registry** (lifecycle dispatcher), not the repository, so it cannot satisfy this query — go through `handles.database` per the actual `ComponentHandles` shape.
  2. Map each row to `PipelineRunListItemDTO`. `created_at` → `Some(t.to_rfc3339())` matching Python's `.isoformat()` (with `+00:00` suffix; `chrono::DateTime<Utc>::to_rfc3339_opts(SecondsFormat::AutoSi, true)` gives the right shape — verify with a unit test).
  3. Return `Json(rows)`.
  - Map the underlying `DbError` to `ApiError::Internal` (Python parity — Python lets exceptions propagate via FastAPI's default handler).
- **Spec reference**: [../routers/activity.md §2.1](../routers/activity.md#21-get-apiv1activitypipeline-runs--list-recent-pipeline-runs).
- **Verify**: `cargo test -p cognee-http-server --test test_activity_pipeline_runs` (added in §5).

### Step 9: `GET /api/v1/activity/spans`

- **File(s)**: `crates/http-server/src/routers/activity.rs` (extend with handler).
- **Action**: Handler `pub async fn get_spans(State(state): State<AppState>, _user: AuthenticatedUser) -> impl IntoResponse`. Body:
  1. Wrap the entire buffer read in a `match std::panic::catch_unwind(...)` *or* a guard closure returning `Result<Vec<TraceSummary>, anyhow::Error>` — the Python try/except catches *everything* and returns `{"error": str(e)}` at status 200 ([Python L99](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L99)). Most realistic failures are mutex poisoning; we treat them the same.
  2. On success: map `TraceSummary` → `TraceSummaryDTO` 1:1. Each `RecordedSpan` → `RecordedSpanDTO` (the `status` enum becomes a `String` via `format!("{status:?}")` — already uppercase via the rename); attributes pass through.
  3. On failure: log `tracing::error!(error = %e, "spans buffer read failed")` and return `(StatusCode::OK, Json(SpansErrorEnvelopeDTO { error: e.to_string() })).into_response()`. Status stays 200; do NOT use `ApiError`.
  4. **Self-referential**: this handler emits a `cognee.api.activity.spans` span via `#[tracing::instrument]`. The span itself lands in the buffer and shows up in the *next* call's response. Document with a `// SELF-REFERENTIAL` comment so the apparent "leak" is explicit (also called out in [../routers/activity.md §6.6](../routers/activity.md#6-open-questions)).
- **Spec reference**: [../routers/activity.md §2.2](../routers/activity.md#22-get-apiv1activityspans--read-the-in-memory-span-buffer), [../observability.md §6.1](../observability.md#61-get-apiv1activityspans).
- **Verify**: `cargo test -p cognee-http-server --test test_activity_spans` (added in §5).

### Step 10: `GET /api/v1/activity/users`

- **File(s)**: `crates/http-server/src/routers/activity.rs` (extend). No `cognee-lib` adapter — `cognee-http-server` does not depend on `cognee-lib` (the dep goes the other way; see §3 Prerequisites). Call the database / `PermissionsRepository` directly.
- **Action**: Handler `pub async fn get_users(State(state): State<AppState>, _user: AuthenticatedUser) -> Json<Vec<TenantUserDTO>>`. Body — mirror the Python quirk in [../routers/activity.md §2.3 Python parity notes](../routers/activity.md#23-get-apiv1activityusers--list-users-in-the-callers-tenant):
  1. **Critical**: fetch `default_user.tenant_id`, **not** the authenticated user's. Python L109–L113 uses the default user; we replicate. Resolve the well-known default-user id via `crate::lifecycle::default_user_id()` and read `users.tenant_id` from the DB (the lifecycle bootstrap sets it; see `lifecycle.rs:114–123`).
  2. Call `handles.permissions.as_ref()` (or fall back to a SeaORM helper when `None`) — use the existing `PermissionsRepository::list_users_in_tenant(tenant_id)` (`crates/database/src/permissions/mod.rs:169`). On any error, swallow and return `Json(vec![])` (Python parity, [Python L125–L126](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L125-L126)). Log at `tracing::warn!` so the swallowed error is visible.
  3. Map the returned `User` rows → `TenantUserDTO` (`id`, `email`, `is_superuser`, `created_at` via the same `to_rfc3339_opts` recipe as step 8).
- The Python `get_users_in_tenant` function takes a permission check we drop here for parity (see [../routers/activity.md §2.3 quirk note](../routers/activity.md#23-get-apiv1activityusers--list-users-in-the-callers-tenant)). `PermissionsRepository::list_users_in_tenant` is already permission-free (it's a flat `users ⨝ user_tenants` query); the gating happens (or in Python's case, doesn't) at the repository layer.
- **Spec reference**: [../routers/activity.md §2.3](../routers/activity.md#23-get-apiv1activityusers--list-users-in-the-callers-tenant), [../tenants.md §9](../tenants.md#9-repository-surface).
- **Verify**: `cargo check -p cognee-http-server`. Behavioral coverage in §5 (`test_activity.rs` if tested in the same file, otherwise add inline).

### Step 11: `GET /api/v1/activity/agents`

- **File(s)**: `crates/http-server/src/routers/activity.rs` (extend with `get_agents` handler + `classify_agent` helper).
- **Action**: Per [../routers/activity.md §2.4](../routers/activity.md#24-get-apiv1activityagents--list-users-with-agent-metadata):
  1. Add `fn classify_agent(email: &str) -> AgentClassification { ... }` exactly as written in the spec (rsplit_once on `-`, dash-and-underscore replacement only on agent prefix, `"Human User"` literal for the seed default, raw local part for non-agents). Inline unit tests over the truth table: `researcher-bot-abc123@cognee.agent` → `("researcher bot", "abc123")`; `myagent@cognee.agent` (no dash) → `("myagent", "")`; `default_user@example.com` → `("Human User", "")`; `alice@corp.io` → `("alice", "")`.
  2. Handler `pub async fn get_agents(State(state): State<AppState>, _user: AuthenticatedUser) -> Result<Json<Vec<AgentDTO>>, ApiError>`. Body:
     - Run **three** queries — even though the third is unused, replicate it for byte-for-byte side-effect parity per [../routers/activity.md §6.3](../routers/activity.md#6-open-questions): (a) call the new `UserRepository::list_active_with_api_key_counts(handles.database.as_ref())` repository method (see below), (b) inside the same function, run a `SELECT dataset_id, COUNT(*) FROM pipeline_runs WHERE created_at > NOW() - INTERVAL '24 hours' GROUP BY dataset_id` against `handles.database` and bind the result to `let _recent_q = ...;` with a `// sic — Python L153-L159 computes this and never reads it; replicated for parity.` comment. Both queries hit the relational DB; the second's result is discarded.
     - Add the new method `list_active_with_api_key_counts() -> Result<Vec<(User, u64)>, DbError>` to a new or existing user-side repository surface. Note: there is no pre-existing `crates/database/src/users/` module; either create one (`crates/database/src/users/repository.rs` + `crates/database/src/users/mod.rs`) or extend `PermissionsRepository` (which already exposes `list_users_in_tenant` and other user-shaped methods). Pick whichever keeps the trait surface coherent — both options are open. SQL: `users LEFT JOIN user_api_key GROUP BY users.id WHERE users.is_active = true`.
     - Map each `(User, u64)` to `AgentDTO`: classify via `classify_agent(email)`, `status = if api_key_count > 0 { "LIVE" } else { "INACTIVE" }`, `is_default` per the literal `"default_user@example.com"`. ISO-8601 `created_at` via the same recipe as step 8.
  3. Errors propagate as `ApiError::Internal` (Python does **not** swallow here, unlike `/users` — see [../routers/activity.md §2.4 Error responses](../routers/activity.md#24-get-apiv1activityagents--list-users-with-agent-metadata)).
- **Spec reference**: [../routers/activity.md §2.4](../routers/activity.md#24-get-apiv1activityagents--list-users-with-agent-metadata).
- **Verify**: `cargo test -p cognee-http-server --lib routers::activity::tests::classify_agent_truth_table`.

### Step 12: `GET /api/v1/activity/export/{dataset_id}` + Markdown renderer

- **File(s)**: `crates/http-server/src/routers/activity.rs` (extend with `get_export` handler + `render_markdown` private fn). Optionally split the renderer into `crates/http-server/src/routers/activity/export.rs` if §11 grows past the 300-line atomic-step ceiling — this is the recommended split per [../routers/activity.md §5 task 6](../routers/activity.md#5-implementation-tasks).
- **Action**:
  1. `fn render_markdown(ds_name: &str, docs: &[Data], nodes: &[GraphNode], edges: &[GraphEdge], now: DateTime<Utc>) -> String` — a pure function so it round-trips against the snapshot fixture in §5. Build a `Vec<String>` of lines and `lines.join("\n")` at the end. Section gating, label/description pipe escaping (`|` → `\|`), `\n` → `" "` in entity descriptions, `"related_to"` edge fallback, `"?"` source/target fallback (first 12 chars), date format `"%b %d, %Y %H:%M UTC"` for the header / `"%b %d, %Y"` for documents — all per the spec rules in [../routers/activity.md §2.5 reproduction rules](../routers/activity.md#25-get-apiv1activityexportdataset_id--export-dataset-as-markdown). Type categorization: `"Entity"` → entities table, `"TextSummary"` → summaries blockquote, others (excluding `"DocumentChunk"` and `"TextDocument"` which are silently dropped) → "Other Nodes" bullets.
  2. Handler `pub async fn get_export(State(state): State<AppState>, _user: AuthenticatedUser, Path(dataset_id): Path<Uuid>) -> impl IntoResponse`. Body:
     - Resolve `let handles = state.components().ok_or(...)?;` then look up the dataset via the existing dataset accessor on `handles.database` (the same path the P2 datasets router uses; see `crates/http-server/src/routers/datasets.rs` for the pattern). Return `(StatusCode::NOT_FOUND, "Dataset not found").into_response()` on miss — the 404 body is **`text/plain`**, not the canonical `ApiError::NotFound` JSON envelope, per [Python L217](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L217).
     - List the dataset's data rows via the existing `IngestDb::list_data_for_dataset` (or equivalent) on `handles.database` — same source the P2 dataset/raw download uses.
     - `let (nodes, edges) = handles.formatted_graph_data(Some(dataset_id), user.id).await.unwrap_or_else(|e| { tracing::warn!(error = %e, "graph fetch failed during export"); (vec![], vec![]) });` — graph errors are silently swallowed per [Python L228–L233](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L228-L233). The `unwrap_or_else` is permitted because we explicitly observed the error and chose to fall back; do not use `.unwrap()`. **Note**: `ComponentHandles::formatted_graph_data` is currently a stub returning `{"nodes": [], "edges": []}` (see `components.rs:64–82` and the comment "blocking gap"); the export will produce mostly empty Markdown until the underlying Python `get_formatted_graph_data` port lands. Do not block P6 on this — the wire shape (sections gated on emptiness) is still correct.
     - `let body = render_markdown(&ds.name, &docs, &nodes, &edges, Utc::now());`
     - Build the response: `(StatusCode::OK, [(CONTENT_TYPE, "text/markdown; charset=utf-8"), (CONTENT_DISPOSITION, format!("attachment; filename=\"{}-memory-export.md\"", sanitize_filename(&ds.name)))], body).into_response()`. `sanitize_filename` strips CR/LF and replaces `"` with `'` — matches the RFC 6266 minimal sanitization decision in [../routers/activity.md §2.5 filename rule](../routers/activity.md#25-get-apiv1activityexportdataset_id--export-dataset-as-markdown).
- **Spec reference**: [../routers/activity.md §2.5](../routers/activity.md#25-get-apiv1activityexportdataset_id--export-dataset-as-markdown).
- **Verify**: unit tests on `render_markdown` over a small fixed graph fixture (`crates/http-server/tests/fixtures/activity/expected_export.md`); pipe-escaping test (`"a|b"` cell becomes `"a\|b"`); section-gating test (no entities → no `## Entities` header).

### Step 13: Mount the activity router

- **File(s)**: `crates/http-server/src/routers/activity.rs` (router constructor), `crates/http-server/src/lib.rs` (`build_router`).
- **Action**: Add `pub fn router() -> axum::Router<AppState> { Router::new().route("/pipeline-runs", get(get_pipeline_runs)).route("/spans", get(get_spans)).route("/users", get(get_users)).route("/agents", get(get_agents)).route("/export/{dataset_id}", get(get_export)) }`. In `build_router`, mount as `.nest("/api/v1/activity", activity::router())`. Add `#[utoipa::path(...)]` annotations on each handler with tag `["Activity"]`; register them on the OpenAPI doc per the existing pattern from earlier phases.
- **Spec reference**: [../routers/activity.md §1](../routers/activity.md#1-mount--file).
- **Verify**: `cargo check -p cognee-http-server` and the integration tests in §5.

### Step 14: SeaORM migration for `sync_operations`

- **File(s)**: `crates/database/src/migrator/m_<timestamp>_sync_operations.rs` (new), `crates/database/src/migrator/mod.rs` (register).
- **Action**: SeaORM migration creating the `sync_operations` table with every column listed in [../routers/sync.md §3.3](../routers/sync.md#33-persistence--sync_operations-table). Notes:
  - `run_id` is `String` (TEXT) NOT NULL UNIQUE INDEX — matches Python's `str` annotation, *not* a UUID column.
  - `dataset_ids` and `dataset_names` are `JSON` (`json` on PostgreSQL, stored as TEXT on SQLite via SeaORM's portable JSON type).
  - `user_id` is UUID INDEX, **not** an FK — Python deliberately avoids the FK so user deletion does not break sync history.
  - `dataset_sync_hashes` is `JSON` with the nested `{dataset_id_str: {"uploaded": [...], "downloaded": [...]}}` shape. Document inline.
  - Idempotent: when run against a Python-seeded DB, the migration's "create" is skipped via SeaORM's `if_not_exists` builder method.
  - Index on `user_id` so the `WHERE user_id = ? AND status IN (...)` query is O(log n).
- **Spec reference**: [../routers/sync.md §3.3](../routers/sync.md#33-persistence--sync_operations-table).
- **Verify**: `cargo test -p cognee-database --test sync_operations_migration` — apply against an in-memory SQLite, assert table presence and column types.

### Step 15: `SyncOperationRepository` + cognee-cloud sync submodule

- **File(s)**: `crates/database/src/sync/repository.rs` (new), `crates/database/src/sync/mod.rs` (new), `crates/database/src/lib.rs` (re-export). For the cloud-side flow: `crates/cloud/src/sync.rs` (new), `crates/cloud/src/lib.rs` (`pub mod sync;`).
- **Action**:
  - `SyncOperationRepository` trait with `create_operation`, `mark_started`, `mark_completed`, `mark_failed`, `update_progress`, `running_for_user(user_id) -> Vec<SyncOperationRow>`, `get_by_run_id(run_id: &str) -> Option<SyncOperationRow>`. Mirror Python's [`methods/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/sync/methods) module 1:1. SeaORM-backed default impl. `running_for_user` filters `WHERE user_id = ? AND status IN ('started', 'in_progress') ORDER BY created_at DESC`.
  - `crates/cloud/src/sync.rs`: this is the **new submodule** the design doc anticipated ([../routers/sync.md §6 Open Question 1, leaning (a)](../routers/sync.md#6-open-questions)). Add `pub async fn run_background(client: Arc<CloudClient>, run_id: String, datasets: Vec<DatasetInfo>, user_id: Uuid, repo: Arc<dyn SyncOperationRepository>) -> Result<(), CloudError>` — port Python's `_perform_background_sync` from [sync.py L167–L229](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/sync.py#L167-L229). Sub-helpers `_check_hashes_diff`, `_upload_missing_files`, `_download_missing_files`, `_trigger_remote_cognify` follow the structure in [../routers/sync.md §3.2](../routers/sync.md#32-background-task-model). Progress updates (0% → 80% → 90% → 95% → 100%) call `repo.update_progress(...)` *and* `registry.update_progress(...)` — both layers are kept in sync.
  - **`SyncRegistry::update_progress` callback wiring**: the background task gets a `progress_callback: Arc<dyn Fn(u32) + Send + Sync>` so the http-server can pass `move |pct| registry.update_progress(user_id, pct)` without `cognee-cloud` needing to depend on `cognee-http-server`.
- **Spec reference**: [../routers/sync.md §3.2 / §3.3](../routers/sync.md#32-background-task-model), [../routers/sync.md §6 Open Question 1](../routers/sync.md#6-open-questions).
- **Verify**: `cargo check -p cognee-database -p cognee-cloud`. Behavioral tests for `run_background` are out of scope for P6 (they require a live cloud or a `mockito` fixture); cover via the integration test in §5 with a stubbed `CloudClient`.

### Step 16: `SyncRegistry` (in-memory)

- **File(s)**: `crates/http-server/src/sync/mod.rs` (new), `crates/http-server/src/sync/registry.rs` (new).
- **Action**: Per [../routers/sync.md §3.1](../routers/sync.md#31-concurrency-one-running-sync-per-user):
  - `pub struct SyncRegistry { inner: Arc<DashMap<Uuid, RunningSync>> }` keyed by `user_id` (one running sync per user).
  - `pub struct RunningSync { run_id: String, user_id: Uuid, dataset_ids: Vec<Uuid>, dataset_names: Vec<String>, created_at: DateTime<Utc>, progress_percentage: AtomicU32, abort: AbortHandle }`.
  - `try_register(user_id, run) -> Result<(), AlreadyRunning>` — uses `DashMap::entry(...)` with a `match` so insertion is atomic; on `Entry::Occupied`, returns `Err(AlreadyRunning(snapshot))` (snapshot holds the existing run's id, dataset names, progress, created_at) — the handler in step 17 turns that snapshot into the `SyncConflictDTO` body.
  - `snapshot_for(user_id) -> Option<RunningSyncSnapshot>` — used by `GET /sync/status`.
  - `complete(user_id)` — drop the entry; called by the background task on success/failure.
  - `update_progress(user_id, pct)` — relaxed atomic store on the slot's `progress_percentage`.
  - **Separate from `PipelineRunRegistry`** (the cognify/memify dispatcher) — different schema, different progress model, no WebSocket subscription path. See [../routers/sync.md §3.2](../routers/sync.md#32-background-task-model).
  - `AppState::sync = Arc<SyncRegistry>` — fill the slot in `AppState::build`. Construct once at boot.
  - **Shutdown**: `cognee_http_server::lifecycle::on_shutdown` (already present from earlier phases) iterates `SyncRegistry::inner` and `.abort()`s each handle, then calls `repo.mark_failed(run_id, "server_shutdown")` for each — analogous to the pipeline-run registry's shutdown sweep ([../pipelines.md §12](../pipelines.md#12-crash--restart-recovery)). Document the parallel inline.
- **Spec reference**: [../routers/sync.md §3.1](../routers/sync.md#31-concurrency-one-running-sync-per-user).
- **Verify**: `cargo test -p cognee-http-server --lib sync::registry::tests` — `try_register` is atomic under N concurrent threads (only one wins, the rest see `AlreadyRunning`); `snapshot_for` returns `None` after `complete`.

### Step 17: Sync DTOs + `POST /api/v1/sync` + `GET /api/v1/sync/status`

- **File(s)**: `crates/http-server/src/dto/sync.rs` (new), `crates/http-server/src/routers/sync.rs` (new), `crates/http-server/src/routers/mod.rs` (add `pub mod sync;`).
- **Action**:
  1. Add every DTO from [../routers/sync.md §4](../routers/sync.md#4-dto-definitions): `SyncRequestDTO`, `SyncResponseDTO`, `SyncConflictDTO`, `SyncConflictDetailsDTO`, `SyncStatusOverviewDTO`, `LatestRunningSyncDTO`, `SyncErrorDTO`. `run_id` stays `String` (Python `str` parity). `dataset_ids` in `SyncResponseDTO` is `Vec<String>` (stringified UUIDs). All `#[derive(Serialize, ToSchema)]`; request DTO additionally `Deserialize`.
  2. `POST /` handler `pub async fn post_sync(State(state): State<AppState>, user: AuthenticatedUser, Json(req): Json<SyncRequestDTO>) -> Result<Response, ApiError>`. Body sequence per [../routers/sync.md §2.1 side effects](../routers/sync.md#21-post-apiv1sync--start-a-cloud-sync). All of "delegation target" prose below uses `state.lib.X()` as shorthand; the actual call shape goes through `let handles = state.components().ok_or(...)?` and then into `handles.database` / `handles.permissions` / a new `handles.sync_ops: Arc<dyn SyncOperationRepository>` (add to `ComponentHandles` in this phase). The `SyncRegistry` is read off `state.sync` (top-level on `AppState`, not under `lib`).
     - DB-side concurrency check first: call the new `SyncOperationRepository::running_for_user(user.id)` against `handles.database`. If non-empty, build a `SyncConflictDTO` from the most-recent row and return `(StatusCode::CONFLICT, Json(conflict)).into_response()` — note this bypasses `ApiError::Conflict` because the body shape is `{error, details}`, not `{detail}`.
     - Permission gate: when `req.dataset_ids` is `Some(ids)`, filter via `PermissionsRepository::user_can(user.id, id, "write")` per id (silently dropping non-permitted ids — Python parity, [routers/sync.md §2.1 quirk](../routers/sync.md#21-post-apiv1sync--start-a-cloud-sync)). When `req.dataset_ids` is `None`/empty, call the existing `PermissionsRepository::visible_datasets(user.id, "write")` (`crates/database/src/permissions/mod.rs:95`) to enumerate writable datasets. There is **no** dedicated `datasets_for_user(.., perms, ids)` method on the trait; build the call site out of `visible_datasets` + `user_can` to match Python's `get_specific_user_permission_datasets` semantics.
     - Empty resolved list → `(StatusCode::BAD_REQUEST, Json(SyncErrorDTO { error: "At least one dataset must be provided for sync operation".into() }))` ([Python L147–L148](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py#L147-L148)).
     - Generate `run_id = Uuid::new_v4().to_string()`, `now = Utc::now()`. `SyncOperationRepository::create_operation(&run_id, &dataset_ids, &dataset_names, user.id).await?;`.
     - In-memory registry `try_register` — if collision (race against the DB check), build `SyncConflictDTO` from the registry snapshot and return 409. Tests in §5 cover the race.
     - `tokio::spawn(cognee_cloud::sync::run_background(...))` with a progress callback that calls back into both the repo and the registry.
     - Return `(StatusCode::OK, Json(SyncResponseDTO { run_id, status: "started".into(), dataset_ids: stringify_uuids(...), dataset_names, message: format!("Sync operation started in background for {} datasets. Use run_id '{}' to track progress.", dataset_ids.len(), run_id), timestamp: now.to_rfc3339(), user_id: user.id.to_string() })).into_response()`.
  3. `GET /status` handler `pub async fn get_status(State(state): State<AppState>, user: AuthenticatedUser) -> Response`. Body: `let running = SyncOperationRepository::running_for_user(handles.database.as_ref(), user.id).await;` — on error return `(StatusCode::INTERNAL_SERVER_ERROR, Json(SyncErrorDTO { error: "Failed to get sync status overview".into() }))` ([Python L240–L242](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/sync/routers/get_sync_router.py#L240-L242)) — note the body is `{"error": "..."}`, not the canonical `{"detail": "..."}` envelope, so the response is built directly, not via `ApiError`. On success, build `SyncStatusOverviewDTO` from the rows; latest is `running.first()` (DESC ordering); if `running.is_empty()` set `latest_running_sync: None`.
  4. **OpenAPI**: tag `["Cloud Sync"]`. Declare 200 / 400 / 403 / 409 responses on `POST /`. The `409` shape varies — `SyncConflictDTO` on the in-progress path, `SyncErrorDTO` on cloud-unavailable/other; declare both via `#[utoipa::path(... responses(... oneOf(...)))]`.
- **Mount**: `.nest("/api/v1/sync", sync::router())` in `build_router`.
- **Spec reference**: [../routers/sync.md §2](../routers/sync.md#2-endpoints).
- **Verify**: `cargo test -p cognee-http-server --test test_sync` (added in §5).

### Step 18: `cognee_cloud::operations::check_api_key` + checks router

- **File(s)**: `crates/cloud/src/operations.rs` (new) or extend `crates/cloud/src/cloud_client.rs` with a free function (`pub async fn check_api_key(api_key: &str) -> CloudResult<()>`). `crates/cloud/src/lib.rs` (re-export under `pub mod operations { pub use ...::check_api_key; }`). `crates/http-server/src/dto/checks.rs` (new), `crates/http-server/src/routers/checks.rs` (new).
- **Action**:
  1. **`check_api_key`**: build a `reqwest::Client` (or reuse `CloudClient::http()` if exposed), POST `{cloud_url}/api/api-keys/check` with header `X-Api-Key: <api_key>` and an empty body. On status 200, `Ok(())`. On any other status, `Err(CloudError::ManagementApi { status: code.as_u16(), body: error_text })`. On reqwest errors (DNS, TLS, refused), map to `CloudError::Http(...)`. The cloud URL comes from `cognee_cloud::config::cloud_url()` (already env-driven).
  2. **`CloudConfigErrorDTO`**: per [../routers/checks.md §4](../routers/checks.md#4-dto-definitions). `pub struct CloudConfigErrorDTO { pub detail: String, pub name: String }`. `name` carries either `"CloudApiKeyMissingError"` or `"CloudConnnectionError"` (sic — three n's, [Python typo](https://github.com/topoteretes/cognee/blob/main/cognee/modules/cloud/exceptions/CloudConnectionError.py#L11)). Add a `// sic — Python typo replicated for wire parity` comment on the literal in the handler.
  3. **`POST /connection`** handler `pub async fn post_connection(State(_state): State<AppState>, _user: AuthenticatedUser, headers: HeaderMap) -> Response`. Body:
     - Read `X-Api-Key` from `headers`. Missing or empty → `(StatusCode::BAD_REQUEST, Json(CloudConfigErrorDTO { detail: "Failed to connect to the cloud service. Please add your API key to local instance.".into(), name: "CloudApiKeyMissingError".into() })).into_response()`.
     - `match cognee_cloud::operations::check_api_key(api_key).await { Ok(()) => (StatusCode::OK, Json(serde_json::Value::Null)).into_response(), Err(e) => { let detail = format!("Failed to connect to cloud instance: {e}"); (StatusCode::SERVICE_UNAVAILABLE, Json(CloudConfigErrorDTO { detail, name: "CloudConnnectionError".into() })).into_response() } }`. The `200 null` body is Python parity per [../routers/checks.md §6 Open Question 1](../routers/checks.md#6-open-questions); do NOT switch to 204.
  4. **Span attribute hygiene**: the handler runs under `#[tracing::instrument(skip(headers))]` so the header map (which carries the secret) is not auto-recorded. Set `cognee.cloud.url` and `cognee.cloud.status` explicitly via `tracing::Span::current().record(...)`. Do NOT set `cognee.cloud.api_key`. Even if a future contributor adds it, the redaction layer from §3 catches it as a backstop.
  5. **Access-log redaction**: extend `crates/http-server/src/middleware/tracing.rs` (or wherever the `TraceLayer` is configured) to add `X-Api-Key` to the redacted-headers list per [../observability.md §7](../observability.md#7-access-logging) — the existing list lives in P0. This is a one-line addition; do not refactor.
- **Mount**: `.nest("/api/v1/checks", checks::router())`. `checks::router()` registers the single `POST /connection` route.
- **Spec reference**: [../routers/checks.md §2.1 / §4 / §5](../routers/checks.md#21-post-apiv1checksconnection--validate-a-cloud-api-key).
- **Verify**: `cargo test -p cognee-http-server --test test_checks` (added in §5).

## 5. Tests

All integration tests sit under `crates/http-server/tests/` and follow the P0 pattern (`tower::ServiceExt::oneshot` against `build_router(state)`, no socket binding).

- `crates/http-server/tests/test_activity_pipeline_runs.rs` — seed three `pipeline_runs` rows (two with attached datasets/users, one orphan with `dataset_id = NULL`); assert: response is `Vec<PipelineRunListItemDTO>`, ordered DESC by `created_at`, the orphan has `dataset_name = None` / `owner_email = None`; `?dataset_id=...` filters to a single row; cross-tenant (rows from a different tenant) are visible — locks down the Python-parity leakage.
- `crates/http-server/tests/test_activity_spans.rs` — emit a parent + two children via `tracing::info_span!(...).in_scope(...)` while a `SpanBufferLayer` is installed in the test subscriber; hit `/api/v1/activity/spans`; assert one trace, three spans, child `parent_span_id` matches parent's `span_id`, root `name` is the parent's. Then call again and assert the *second* call's `cognee.api.activity.spans` span is now in the buffer (self-referential parity).
- `crates/http-server/tests/test_activity_export.rs` — fixture dataset with two documents, three entities, two summaries, three edges; call `/api/v1/activity/export/{id}`; assert `Content-Type: text/markdown; charset=utf-8`, `Content-Disposition: attachment; filename="<name>-memory-export.md"`, body matches `tests/fixtures/activity/expected_export.md` (after `now`-line normalization). Then call with a UUID that does not exist; assert 404 with `text/plain` body `"Dataset not found"` (NOT the JSON envelope).
- `crates/http-server/tests/test_sync.rs` — (1) empty body `{}` resolves to "all writable datasets" and returns 200 with `status: "started"`; (2) immediately calling again returns 409 with `SyncConflictDTO { error, details: { run_id, status: "already_running", ... } }` whose `details.run_id` matches the first call; (3) `GET /sync/status` while a sync is running returns `has_running_sync: true` with `latest_running_sync.run_id` matching; (4) after the background task completes (use a stub `CloudClient` that returns immediately), `/sync/status` returns `has_running_sync: false`. Use a per-test-process `mockito::Server` to fake the cloud upstream.
- `crates/http-server/tests/test_checks.rs` — happy path: `mockito` returns 200 → handler returns `(200, application/json, "null")`; missing header → 400 with `CloudConfigErrorDTO { name: "CloudApiKeyMissingError" }`; mockito 401 → 503 with `CloudConfigErrorDTO { name: "CloudConnnectionError" }` (verify the typo is preserved); mockito connection-refused → 503 with `detail` mentioning the underlying error.
- `crates/http-server/tests/test_span_redaction.rs` — install the buffer + layer; emit a span with attribute `auth = "Authorization: Bearer eyJabc.def.ghi-very-long-jwt-1234567890"`; hit `/api/v1/activity/spans`; assert the captured attribute value is `"Authoriz***REDACTED***"` (or whatever the spec rule produces — verify against the §3 redaction unit tests so the integration and unit tests agree on the output shape).

Inline unit tests (covered by their respective steps): `SpanBuffer` LRU eviction (§2), redaction matrix (§3), trace-id propagation (§4), `classify_agent` truth table (§11), `render_markdown` snapshot + pipe-escape + section-gating (§12), `SyncRegistry::try_register` atomicity (§16), `SyncErrorDTO` round-trips with `{"error": ...}` (§17).

## 6. Acceptance criteria

- [x] `cargo check --all-targets -p cognee-http-server` succeeds.
- [x] `cargo check --all-targets -p cognee-database -p cognee-cloud` succeeds (both crates gain new public surface this phase).
- [x] `cargo test -p cognee-http-server` passes — every test listed in §5 plus all inline unit tests.
- [x] `scripts/check_all.sh` passes (fmt, clippy `-D warnings`, capi/python/js binding checks unchanged).
- [x] Hitting `/api/v1/activity/spans` after any other request returns at least one trace whose root span is `cognee.api.activity.spans` (self-referential parity, [../routers/activity.md §6.6](../routers/activity.md#6-open-questions)) — verified manually against a running binary and by the second part of `test_activity_spans.rs`.
- [x] The Python typo `CloudConnnectionError` (three n's) is present verbatim in the `name` field of the 503 body emitted by `/api/v1/checks/connection` — verified by `test_checks.rs`.
- [x] A bearer token planted in a span attribute does not appear unredacted in `/api/v1/activity/spans` output — verified by `test_span_redaction.rs`.
- [x] Two concurrent `POST /api/v1/sync` calls for the same user resolve to one 200 + one 409, not two 200s — verified by `test_sync.rs`'s race section.
- [x] Status row for **P6** in [README.md](README.md) flips **Draft → In Progress → Done** in the PR that lands this work.
- [x] Status rows for **activity**, **sync**, and **checks (cloud)** in [../routers/README.md](../routers/README.md) all flip **Draft → In Progress → Done** in the same PR.

## 7. Files touched

New (under `crates/http-server/`):

- `src/observability/mod.rs`
- `src/observability/span_buffer.rs`
- `src/observability/span_buffer_layer.rs`
- `src/observability/redaction.rs`
- `src/dto/activity.rs`
- `src/dto/sync.rs`
- `src/dto/checks.rs`
- `src/routers/activity.rs` (and optionally `src/routers/activity/export.rs` if Step 12 splits)
- `src/routers/sync.rs`
- `src/routers/checks.rs`
- `src/sync/mod.rs`
- `src/sync/registry.rs`
- `tests/test_activity_pipeline_runs.rs`
- `tests/test_activity_spans.rs`
- `tests/test_activity_export.rs`
- `tests/test_sync.rs`
- `tests/test_checks.rs`
- `tests/test_span_redaction.rs`
- `tests/fixtures/activity/expected_export.md`

New (outside `crates/http-server/`):

- `crates/utils/src/tracing_keys.rs`
- `crates/database/src/migrator/m_<timestamp>_sync_operations.rs`
- `crates/database/src/sync/mod.rs`
- `crates/database/src/sync/repository.rs`
- `crates/cloud/src/sync.rs`
- `crates/cloud/src/operations.rs` (or extension of `cloud_client.rs`)

Modified:

- `crates/http-server/src/lib.rs` — `build_router` mounts `/api/v1/activity`, `/api/v1/sync`, `/api/v1/checks`; OpenAPI registers the new handlers.
- `crates/http-server/src/main.rs` — `init_tracing` composes `Registry → EnvFilter → fmt::Layer → SpanBufferLayer`; constructs the `SpanBuffer` and threads it through `AppState::build`.
- `crates/http-server/src/state.rs` — `spans: Arc<SpanBuffer>` (no longer optional), `sync: Arc<SyncRegistry>` populated.
- `crates/http-server/src/dto/mod.rs` — `pub mod activity; pub mod sync; pub mod checks;`.
- `crates/http-server/src/routers/mod.rs` — `pub mod activity; pub mod sync; pub mod checks;`.
- `crates/http-server/src/middleware/tracing.rs` — add `X-Api-Key` to the access-log redacted-headers list.
- `crates/http-server/src/lifecycle.rs` — extend `on_shutdown` to abort `SyncRegistry` entries and mark their DB rows `failed("server_shutdown")`.
- `crates/utils/src/lib.rs` — `pub mod tracing_keys;`.
- `crates/database/src/pipelines/repository.rs` — extend trait with `list_recent_with_attribution`.
- `crates/database/src/pipelines/sea_orm_impl.rs` — implement the new method.
- `crates/database/src/users/repository.rs` — add `list_active_with_api_key_counts`.
- `crates/database/src/migrator/mod.rs` — register the new sync migration.
- `crates/database/src/lib.rs` — re-export `sync` module.
- `crates/cloud/src/lib.rs` — `pub mod sync;` and `pub mod operations;` (or `pub use cloud_client::check_api_key;`).
- `crates/http-server/src/components.rs` — extend `ComponentHandles` with the new `sync_ops: Option<Arc<dyn SyncOperationRepository>>` slot (parallel to the existing `permissions: Option<Arc<dyn PermissionsRepository>>` slot, lines 53–60). No changes under `crates/lib/` — `cognee-http-server` does not depend on `cognee-lib` (the dep goes the other way).
- `docs/http-server/implementation/README.md` — flip P6 status row.
- `docs/http-server/routers/README.md` — flip the `activity`, `sync`, `checks (cloud)` rows' status.

Out of scope (do NOT touch in this phase):

- OTLP / `opentelemetry-sdk` integration — deferred to phase 2 per [../observability.md §1 non-goals](../observability.md#1-goals--non-goals).
- PostHog telemetry events emitted by Python's sync router — deferred per [../routers/sync.md §6 Open Question 2](../routers/sync.md#6-open-questions).
- `/sync/subscribe/{run_id}` WebSocket endpoint — not in scope; if added later, evaluate unifying `SyncRegistry` with `PipelineRunRegistry` per [../routers/sync.md §6 Open Question 6](../routers/sync.md#6-open-questions).
- `/metrics` (Prometheus / OpenMetrics) endpoint — deferred per [../observability.md §1 non-goals](../observability.md#1-goals--non-goals).
- Per-tenant span-buffer filtering — see [../observability.md §11.1](../observability.md#11-open-questions); the buffer is intentionally global for P6 to match Python.
- Cross-SDK parity tests for activity / sync / checks — those land in P8 ([p8-e2e-parity.md](p8-e2e-parity.md)), not P6.
