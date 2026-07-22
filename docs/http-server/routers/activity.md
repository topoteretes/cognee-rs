# Router: activity

Activity & telemetry endpoints. Powers the frontend's activity timeline, trace viewer, agent registry, and dataset memory export. Five endpoints with very different shapes: a JOIN query against `pipeline_runs` (durable observability), a read of the in-memory span buffer (live observability), a tenant-scoped user listing, an agent listing derived from `@cognee.agent` email suffixes, and a Markdown report builder for a single dataset.

Companion docs: [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../pipelines.md](../pipelines.md), [../observability.md](../observability.md), [../tenants.md](../tenants.md).

## 1. Mount & file
- Mount prefix: `/api/v1/activity`
- Router file: `crates/http-server/src/routers/activity.rs`
- Python source: [`cognee/api/v1/activity/routers/get_activity_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py)

## 2. Endpoints

### 2.1 `GET /api/v1/activity/pipeline-runs` — list recent pipeline runs

Reads the **durable** observability tier — the `pipeline_runs` relational table — and joins it onto `datasets` and `users` so the frontend can show "who/what/which dataset" attribution. See [../pipelines.md §5](../pipelines.md#5-database-persistence--pipeline_runs-table) for the underlying table.

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params**:
  - `dataset_id: Option<Uuid>` — filter to a single dataset. Default: `None` (all datasets).
- **Request body**: none.
- **Response body**: `application/json`, `200 OK`, `Vec<PipelineRunListItemDTO>`.
  - `id: Uuid` — `pipeline_runs.id` (per-transition row PK; serialized as string).
  - `pipeline_name: String` — e.g. `"cognify_pipeline"`, `"memify_pipeline"`, `"add_pipeline"`.
  - `status: Option<String>` — one of the `DATASET_PROCESSING_*` enum strings ([../pipelines.md §3.1](../pipelines.md#32-durable-status--written-to-pipeline_runsstatus)). `None` if NULL in the row (Python: `run.status.value if run.status else None`, [Python L55](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L55)).
  - `dataset_id: Option<Uuid>` — `pipeline_runs.dataset_id`. `None` if NULL.
  - `dataset_name: Option<String>` — joined from `datasets.name`. `None` when the OUTER JOIN matches no dataset row (orphaned runs).
  - `owner_id: Option<Uuid>` — joined from `datasets.owner_id`.
  - `owner_email: Option<String>` — joined from `users.email`.
  - `created_at: Option<String>` — ISO-8601, e.g. `"2026-04-24T18:30:00+00:00"`. `None` if NULL.
  - `pipeline_run_id: Option<Uuid>` — deterministic from `(pipeline_id, dataset_id)`; multiple rows can share this value across status transitions ([../pipelines.md §4.2](../pipelines.md#42-pipeline_run_id-deterministic-derived)).
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `ApiError::Unauthorized` | Missing/invalid credential. |
  | `400` | `ApiError::BadRequest("invalid uuid: <e>")` | `dataset_id` is not a valid UUID — caught by the `Query<Uuid>` extractor. |
  | `500` | `ApiError::Internal(e)` | Underlying DB query failure. |

  Python parity: Python does not catch DB errors here; they propagate as 500 via FastAPI's default handler. We do the same via `ApiError::Internal`.
- **Side effects**: read-only. Single SELECT against `pipeline_runs` ⨝ `datasets` ⨝ `users`.
- **Delegation target**: a new repository method `PipelineRunRepository::list_recent_with_attribution(dataset_id: Option<Uuid>, limit: u32)` returning `Vec<PipelineRunRow>`. Lives alongside [`PipelineRunRepository::list_recent`](../pipelines.md#52-the-pipelinerunrepository-trait). The handler delegates to it, then maps rows to DTOs.
- **Validation rules**: none beyond UUID parsing on `dataset_id`.
- **Rate / size limits**: `LIMIT 50` baked into the query. Matches Python ([`get_activity_router.py:44`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L44)). Pagination is not yet supported — see Open Questions.
- **Permission gate**: **none**. Python returns runs across all datasets the *server* has, not just the caller's. This is a known leakage of cross-tenant observability data. Rust replicates Python verbatim — no tenant filter, no permission check, no application-level gate. Operators wanting tenant scoping must apply it at a reverse-proxy layer.
- **OpenAPI**: tag `["Activity"]`. Response: `application/json` array of `PipelineRunListItemDTO`. Security: `[BearerAuth, ApiKeyAuth, CookieAuth]`.
- **Telemetry**: span `cognee.api.activity.pipeline_runs`. Attributes: `cognee.dataset.id` (when filter is set), `cognee.db.row_count`. Recorded in the durable `pipeline_runs` itself? **No** — this is a read endpoint and emits no `pipeline_runs` rows. The handler's own span is captured by the live span buffer ([../observability.md §3.3](../observability.md#33-span-instrumentation-conventions)).
- **Python parity notes**:
  - Python uses `outerjoin(PipelineRun, Dataset)` then `outerjoin(User)` ([Python L37–L42](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L37-L42)) — we do `LEFT JOIN datasets ON … LEFT JOIN users ON …` so orphaned runs (dataset deleted) still appear.
  - `ORDER BY created_at DESC LIMIT 50` — preserved.
  - Python casts every UUID to string in the response; Rust serializes `Uuid` directly via `serde` (which produces the same string form).

### 2.2 `GET /api/v1/activity/spans` — read the in-memory span buffer

Reads the **live** observability tier — the in-process `SpanBuffer` ring — and returns traces grouped by `trace_id`. This is the one endpoint in the entire server that is *self-referentially* observed: the call itself emits a span (`cognee.api.activity.spans`) which then appears in subsequent calls' results.

See [../observability.md §4](../observability.md#4-in-memory-span-buffer) for the buffer's storage model and [../observability.md §6.1](../observability.md#61-get-apiv1activityspans) for the wire shape.

- **Auth**: `required` (`AuthenticatedUser`). Matches Python ([Python L67](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L67)).
- **Path params**: none.
- **Query params**: none.
- **Request body**: none.
- **Response body**: `application/json`, `200 OK`. Two possible shapes — match Python:
  - On success: `Vec<TraceSummaryDTO>`.
  - On error: `{ "error": "<msg>" }` (a JSON object, not an array). Python's `except` block returns this verbatim ([Python L99–L100](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L99-L100)). We match for compat. The status code stays `200` in both cases (sic — Python does not raise here).

  `TraceSummaryDTO` fields:
  - `trace_id: String` — 32-char lowercase hex.
  - `root_name: Option<String>` — name of the first span in the trace; usually `cognee.api.<verb>`.
  - `duration_ms: f64` — `max(s.duration_ms for s in spans)` (Python L86: `max((s.get("duration_ms", 0) for s in spans), default=0)`).
  - `span_count: usize` — `spans.len()`.
  - `status: Option<String>` — root span's `status` field — `"OK" | "ERROR" | "UNSET"` ([../observability.md §11.6](../observability.md#11-open-questions)).
  - `spans: Vec<RecordedSpanDTO>` — full ordered list.

  `RecordedSpanDTO` (mirrors `RecordedSpan` from [../observability.md §4.2](../observability.md#42-recordedspan)):
  - `name: String`
  - `trace_id: String` — 32-char hex.
  - `span_id: String` — 16-char hex.
  - `parent_span_id: Option<String>`
  - `start_time_ns: u64`
  - `end_time_ns: u64`
  - `duration_ms: f64`
  - `status: String` — `"OK" | "ERROR" | "UNSET"`.
  - `attributes: serde_json::Map<String, serde_json::Value>` — already redacted by `SpanBufferLayer::on_close` ([../observability.md §5](../observability.md#5-secret-redaction)).
- **Error responses**: see Python parity above — Python wraps the entire body in a `try/except` and returns `{"error": str(e)}` at `200`. We match. The handler still logs the underlying error at `tracing::error!`.

  | Status | Body | Condition |
  |---|---|---|
  | `200` | `{"error": "<msg>"}` | Buffer read panicked or threw. **Python parity** ([Python L99](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L99)). |
  | `401` | `ApiError::Unauthorized` | Missing/invalid credential. |
- **Side effects**: read-only. Acquires the `SpanBuffer`'s `Mutex`, snapshots, releases. No DB / file IO.
- **Delegation target**: `state.spans.all_traces() -> Vec<TraceSummary>` ([../observability.md §4.1](../observability.md#41-type--api)). The handler maps the in-house `TraceSummary` → DTO with no further transformation (the fields already match Python).
- **Validation rules**: none.
- **Rate / size limits**: bounded by `BufferConfig::max_traces` (default 50) and `max_spans_per_trace` (default 1024) so a single response is at worst `50 * 1024 = 51,200` spans. Practical responses are far smaller.
- **Permission gate**: **none** — Python's endpoint is admin-debug; any authenticated user sees all traces, including other tenants'. Documented as a leakage in [../observability.md §11.1](../observability.md#11-open-questions). P6 keeps Python parity.
- **OpenAPI**: tag `["Activity"]`. Response: `application/json` with `oneOf: [array of TraceSummaryDTO, ErrorDTO]`. Security: same as 2.1.
- **Telemetry**: span `cognee.api.activity.spans`. **Self-referential** — this span itself lands in the buffer and will appear in subsequent calls. Attributes: `cognee.activity.trace_count = result.len()`. We deliberately do *not* set the `error` attribute on the catch-all path because Python silently swallows the error.
- **Python parity notes**:
  - Python lazily initializes the OTEL exporter on first call ([Python L70–L80](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L70-L80)). Rust does not need this — the `SpanBufferLayer` is installed at server startup. If the layer is somehow not present, `state.spans` is `None` and we return `[]` (matches Python's `if exporter is None: return []`).
  - The `status` field on a recorded span is already a string in Python's exporter (it converts `SpanKind` / `StatusCode`). Ours uses `#[serde(rename_all = "UPPERCASE")]` on `SpanStatus` to produce `"OK" | "ERROR" | "UNSET"` directly.

### 2.3 `GET /api/v1/activity/users` — list users in the caller's tenant

Returns users in the current tenant (the *frontend's* default). The list includes any "agent" users provisioned for API-key-only access.

See [../tenants.md](../tenants.md) for the multi-tenant schema.

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params**: none.
- **Request body**: none.
- **Response body**: `application/json`, `200 OK`, `Vec<TenantUserDTO>`.
  - `id: Uuid` — `users.id`.
  - `email: String`
  - `is_superuser: bool`
  - `created_at: Option<String>` — ISO-8601 (Python: `u.created_at.isoformat() if hasattr(u, "created_at") and u.created_at else None`, [Python L119–L121](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L119-L121)).

  Python returns `[]` on any exception ([Python L125–L126](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L125-L126)). We match: `try { … } catch { return [] }`.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `200` | `[]` | DB error / no default user / no tenant. **Python parity** — silently swallows errors. |
  | `401` | `ApiError::Unauthorized` | Missing/invalid credential. |
- **Side effects**: read-only. One join query: `users ⨝ user_tenants` filtered by `tenant_id`.
- **Delegation target**: `state.lib.users().list_in_tenant(tenant_id)` — wraps `get_users_in_tenant` ([Python module](https://github.com/topoteretes/cognee/blob/main/cognee/modules/users/tenants/methods/get_users_in_tenant.py)). **Note**: the Python `get_users_in_tenant` actually checks `has_user_management_permission` on the caller; the activity router calls it via `default_user.tenant_id` ([Python L113](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L113)) using the *default user's* tenant, **not** the authenticated user's. This is a quirk — see Python parity notes.
- **Validation rules**: none.
- **Rate / size limits**: unbounded — returns every user in the tenant. Tenants are typically small, but for very large deployments add `LIMIT 500` as a safety cap. Open Question.
- **Permission gate**: technically `user_management:tenant_id`, but only because Python's `get_users_in_tenant` checks it internally. The router itself does not gate. We keep this layering: the repository method enforces the gate, and a permission denial bubbles back as the `try/except`-swallowed empty list.
- **OpenAPI**: tag `["Activity"]`. Response: `application/json` array of `TenantUserDTO`. Security: same as 2.1.
- **Telemetry**: span `cognee.api.activity.users`. Attributes: `cognee.tenant.id`, `cognee.db.row_count`.
- **Python parity notes**:
  - **Quirk**: Python uses `default_user.tenant_id`, not the authenticated user's tenant ([Python L109–L113](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L109-L113)). This is almost certainly a Python bug — multi-tenant deployments will always show the default user's tenant regardless of who's calling. Rust replicates this verbatim for strict wire parity. The implementor must explicitly fetch `default_user.tenant_id` (not `auth_user.tenant_id`) at this point.
  - The Python source does **not** import `get_users_in_tenant` with the `user` permission check — it uses an older signature `get_users_in_tenant(tenant_id)` (single argument). Looking at the current Python source, `get_users_in_tenant` requires a `user` second argument; this means the activity endpoint as written may be broken upstream. We mirror the *intent* — list users by tenant — and either (a) drop the permission check or (b) thread the authenticated user through. Decision: drop the check for parity, log a warning. Re-evaluate when Python fixes it.

### 2.4 `GET /api/v1/activity/agents` — list users with agent metadata

Returns *all active users* annotated with agent metadata derived from their email. A user is considered an "agent" if their email ends with `@cognee.agent`; the local part of the email encodes the agent type and a short id (split on the last `-`).

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: none.
- **Query params**: none.
- **Request body**: none.
- **Response body**: `application/json`, `200 OK`, `Vec<AgentDTO>`.
  - `id: Uuid` — `users.id`.
  - `email: String`
  - `agent_type: String` — see filter logic below.
  - `agent_short_id: String` — see filter logic below.
  - `is_agent: bool` — `email.ends_with("@cognee.agent")`.
  - `is_default: bool` — `email == "default_user@example.com"` (cognee's seed user).
  - `status: String` — `"LIVE"` if the user has at least one API key, else `"INACTIVE"`. (Python: `has_recent = api_key_count > 0`, [Python L177–L178](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L177-L178).)
  - `api_key_count: u64`
  - `created_at: Option<String>` — ISO-8601.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `401` | `ApiError::Unauthorized` | Missing/invalid credential. |
  | `500` | `ApiError::Internal(e)` | DB query failed. Python lets exceptions propagate here (no `try/except`). |

#### Filter / parsing logic (the "agent metadata" part)

For each active user, the handler computes `agent_type` and `agent_short_id` according to a small state machine. This logic is the load-bearing part of the endpoint — we reproduce it byte-for-byte from [Python L162–L194](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L162-L194):

```rust
fn classify_agent(email: &str) -> AgentClassification {
    let is_agent   = email.ends_with("@cognee.agent");
    let is_default = email == "default_user@example.com";

    let (agent_type, agent_short_id) = if is_agent {
        // Split on last '-': "researcher-bot-abc123" → ("researcher-bot", "abc123")
        let local = email.split('@').next().unwrap_or(email);
        match local.rsplit_once('-') {
            Some((prefix, suffix)) => (
                prefix.replace('-', " ").replace('_', " "),
                suffix.to_string(),
            ),
            None => (local.replace('-', " ").replace('_', " "), String::new()),
        }
    } else if is_default {
        ("Human User".to_string(), String::new())
    } else {
        // Non-agent, non-default: agent_type = local part of email.
        let local = email.split('@').next().unwrap_or(email);
        (local.to_string(), String::new())
    };

    AgentClassification { is_agent, is_default, agent_type, agent_short_id }
}
```

Notes:
- `rsplit_once('-')` matches Python's `parts = local_part.rsplit("-", 1)` (split from the right at most once) ([Python L170](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L170)).
- The dash-and-underscore replacement runs only on the agent prefix, not on non-agent emails — Python L171–L174.
- "Human User" is a special string Python uses for the seed default user; it is not internationalized.

- **Side effects**: read-only. Three queries: (1) `SELECT * FROM users WHERE is_active = true`, (2) `SELECT user_id, COUNT(*) FROM user_api_keys GROUP BY user_id`, (3) `SELECT dataset_id, COUNT(*) FROM pipeline_runs WHERE created_at > now() - interval '24 hours' GROUP BY dataset_id`. Note: the third query is computed but **never used** in Python ([Python L155–L159](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L155-L159)). We may or may not replicate the unused query — see Open Questions.
- **Delegation target**: a new repository method `UserRepository::list_with_api_key_counts() -> Vec<(User, u64)>`. Plus reuse of the existing `PipelineRunRepository::recent_dataset_activity(since: DateTime)` if we replicate the unused query. Alternatively, keep this query in the handler since it's not standard repository surface.
- **Validation rules**: none.
- **Rate / size limits**: unbounded — returns every active user in the system (not tenant-scoped). Same caveat as 2.3.
- **Permission gate**: **none**. Python applies no permission gate (and there's no tenant filter either). Cross-tenant data leakage applies; same Open Question as 2.1 / 2.3.
- **OpenAPI**: tag `["Activity"]`. Response: `application/json` array of `AgentDTO`. Security: same as 2.1.
- **Telemetry**: span `cognee.api.activity.agents`. Attributes: `cognee.agents.total`, `cognee.agents.is_agent_count`.
- **Python parity notes**:
  - The "Live" determination uses *only* API-key presence, not actual recent activity. Python's variable name `has_recent` is misleading.
  - Python computes the unused recent-pipeline-run query for what looks like future work. We will not replicate the unused query in Rust unless parity tests demand it.
  - The agent email format is undocumented in the Python codebase; we treat it as a stable convention based on the parsing code.

### 2.5 `GET /api/v1/activity/export/{dataset_id}` — export dataset as Markdown

Returns a `text/markdown` document summarizing a dataset: documents, entities, summaries, relationships, and other graph nodes. Sets `Content-Disposition: attachment; filename="<dataset_name>-memory-export.md"` so browsers offer it as a download.

- **Auth**: `required` (`AuthenticatedUser`).
- **Path params**: `dataset_id: Uuid`.
- **Query params**: none.
- **Request body**: none.
- **Response body**: `text/markdown; charset=utf-8`, `200 OK`. Body is a UTF-8 string built per the structure below. Set `Content-Disposition: attachment; filename="<safe-name>-memory-export.md"`.
- **Error responses**:

  | Status | Body | Condition |
  |---|---|---|
  | `400` | `{"detail": "invalid uuid"}` | `dataset_id` is not a valid UUID. |
  | `401` | `ApiError::Unauthorized` | Missing/invalid credential. |
  | `404` | `text/plain` body `"Dataset not found"`, status 404 | **Python parity**: Python returns a `text/plain` `Response(content="Dataset not found", status_code=404)` ([Python L217](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L217)) — *not* the JSON `ApiError` envelope. We match. |
  | `500` | `ApiError::Internal(e)` | Repository / graph query failure outside the swallowed graph block (see below). |

  Note the graph-fetch is wrapped in `try/except` ([Python L228–L233](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L228-L233)): a graph error yields empty `nodes`/`edges` and the export still succeeds. We match.

#### Markdown structure (must match Python byte-for-byte)

The handler builds a list of lines with `lines.append(...)` and joins them with `\n`. The order and section gating come straight from [Python L248–L319](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L248-L319):

```text
# Dataset: {ds_name}

Exported: {now} | {len(docs)} documents | {len(entities)} entities | {len(edges)} relationships

## Summaries                          ← only if any summaries exist

> {summary.properties.text}
> {summary2.properties.text}
…

## Entities                            ← only if any entities exist

| Entity | Description |
|--------|-------------|
| {label} | {description} |
…

## Relationships                       ← only if any edges exist

| Source | Relationship | Target |
|--------|-------------|--------|
| {src_label} | {edge.label or "related_to"} | {tgt_label} |
…

## Documents                           ← only if any documents exist

- **{doc.name or "unnamed"}** ({doc.extension.upper(), or ""}, {created or "?"})
…

## Other Nodes                         ← nodes whose type is not in {Entity, TextSummary, DocumentChunk, TextDocument}

- [{type}] {label}
…
```

Specific reproduction rules:

- **Date format**: `now.strftime("%b %d, %Y %H:%M UTC")` — chrono `format("%b %d, %Y %H:%M UTC")` produces the same ([Python L236](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L236)).
- **Document date**: `doc.created_at.strftime("%b %d, %Y")` ([Python L306](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L306)). Use `"?"` when `created_at` is None.
- **Pipe escaping**: replace `|` with `\|` in label, description, source, target, relationship cells (markdown table delimiters). Also replace `\n` with `" "` in entity descriptions ([Python L277](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L277)).
- **Entity categorization**: a node is an "entity" iff `node.type == "Entity"`; "summary" iff `node.type == "TextSummary"`; "other" iff its type is *not* in `{"Entity", "TextSummary", "DocumentChunk", "TextDocument"}`. Note `DocumentChunk` and `TextDocument` are silently dropped ([Python L242–L246](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L242-L246)).
- **Edge label fallback**: if `edge.label` is missing, use `"related_to"` ([Python L292](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L292)).
- **Source/target fallback**: when a node's id isn't in the lookup, take the first 12 chars of the raw id (`e.get("source", "?")[:12]`, [Python L290](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L290)). Use `"?"` when even that's missing.
- **Filename**: `"{ds_name}-memory-export.md"` — Python does not URL-encode the dataset name. If the name contains characters disallowed in HTTP headers (CR/LF/quote), our header serializer must reject or sanitize. **Decision**: replace `"` with `'` and strip CR/LF in the filename header (RFC 6266 minimal). Document the difference; raise as Open Question.

- **Side effects**: read-only.
  1. `SELECT * FROM datasets WHERE id = ?` — 404 if not found.
  2. `SELECT data.* FROM data JOIN dataset_data ON data.id = dataset_data.data_id WHERE dataset_data.dataset_id = ?`.
  3. `state.lib.formatted_graph_data(dataset_id, &user)` — wrapped in `try/except`; any error yields empty `nodes`/`edges`.
- **Delegation target**:
  - `state.lib.datasets().get_by_id(dataset_id)` — existing in `cognee`.
  - `state.lib.datasets().list_data(dataset_id)` — existing.
  - `state.lib.formatted_graph_data(dataset_id, user)` — existing (used by the WebSocket frame builder, [../pipelines.md §10](../pipelines.md#10-websocket-integration)).
  - The Markdown rendering itself is **handler-local** — it doesn't belong in `cognee` because it's HTTP-presentation logic. Lives in `crates/http-server/src/routers/activity/export.rs` as a free `fn render_markdown(...) -> String`.
- **Validation rules**: none beyond UUID parsing.
- **Rate / size limits**: response body is bounded only by graph size. A 100k-node graph produces a multi-MiB Markdown blob. Configurable via `HttpServerConfig::activity_export_max_bytes` (default 10 MiB); if exceeded, truncate with `...truncated...` footer. Open Question: should the limit be a hard 413 instead?
- **Permission gate**: **none**. Python does not check permission on this endpoint — it relies on the graph fetch to fail when the user can't read. Rust matches verbatim: no `PermissionsRepository` call, no application-level gate. The graph-read path produces an empty result for an unauthorized caller, which the handler renders to an effectively empty Markdown report.
- **OpenAPI**: tag `["Activity"]`. Response: `text/markdown` (utoipa supports custom media types via `#[utoipa::path(... responses(... content_type = "text/markdown" ...))]`). Security: same as 2.1.
- **Telemetry**: span `cognee.api.activity.export`. Attributes: `cognee.dataset.id`, `cognee.dataset.name`, `cognee.export.bytes` (final size), `cognee.export.nodes`, `cognee.export.edges`, `cognee.export.docs`.
- **Python parity notes**:
  - Python uses `Response(content=markdown, media_type="text/markdown", headers={"Content-Disposition": ...})`. Our axum response: `(StatusCode::OK, [(CONTENT_TYPE, "text/markdown; charset=utf-8"), (CONTENT_DISPOSITION, format!("attachment; filename=\"{filename}\""))], markdown).into_response()`.
  - Python returns `Response("Dataset not found", status_code=404)` — `text/plain` body, *not* the `ApiError` JSON shape. We replicate.
  - The endpoint takes `Depends(get_authenticated_user)` explicitly ([Python L199](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L199)) instead of the bare default. This matches our `AuthenticatedUser` extractor.

## 3. Cross-cutting behavior

- **Tenant scoping**: 2.1, 2.3, 2.4 all leak across tenants in Python. P6 preserves this for parity; flag as Open Questions.
- **Empty list vs error**: 2.2 (`/spans`) and 2.3 (`/users`) silently swallow internal errors and return a fallback shape. 2.1 and 2.4 propagate errors. 2.5 propagates *most* errors but swallows graph-fetch failures.
- **Read-only**: every endpoint is read-only. None of them write to `pipeline_runs`, the graph, or any other tier of storage.
- **No fix-on-ports anywhere**: every endpoint replicates Python's behavior verbatim, including the cross-tenant data exposure on `/pipeline-runs`, `/users`, `/agents`, the unused query in `/agents`, the `default_user.tenant_id` quirk in `/users`, and the missing permission gate on `/export`. Strict wire and side-effect parity.

## 4. DTO definitions

```rust
// crates/http-server/src/dto/activity.rs

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// One row of `GET /api/v1/activity/pipeline-runs`.
///
/// Mirrors Python's dict at
/// https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L51-L62
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct PipelineRunListItemDTO {
    pub id: Uuid,
    pub pipeline_name: String,
    pub status: Option<String>,        // DATASET_PROCESSING_* enum string
    pub dataset_id: Option<Uuid>,
    pub dataset_name: Option<String>,
    pub owner_id: Option<Uuid>,
    pub owner_email: Option<String>,
    /// ISO-8601, e.g. "2026-04-24T18:30:00+00:00". String to match Python's
    /// `created_at.isoformat()`.
    pub created_at: Option<String>,
    pub pipeline_run_id: Option<Uuid>,
}

/// One trace returned by `GET /api/v1/activity/spans`.
///
/// Wire-compat with the dict produced at
/// https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L88-L96
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct TraceSummaryDTO {
    pub trace_id: String,
    pub root_name: Option<String>,
    pub duration_ms: f64,
    pub span_count: usize,
    pub status: Option<String>,        // "OK" | "ERROR" | "UNSET"
    pub spans: Vec<RecordedSpanDTO>,
}

/// One span inside a `TraceSummaryDTO.spans`. Mirrors `RecordedSpan`
/// from observability.md §4.2 (which itself mirrors Python's exporter dict).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct RecordedSpanDTO {
    pub name: String,
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub start_time_ns: u64,
    pub end_time_ns: u64,
    pub duration_ms: f64,
    pub status: String,                // already redacted; "OK" | "ERROR" | "UNSET"
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

/// One row of `GET /api/v1/activity/users`. Pythoп returns a dict of
/// the same shape minus tenant scoping.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct TenantUserDTO {
    pub id: Uuid,
    pub email: String,
    pub is_superuser: bool,
    pub created_at: Option<String>,    // ISO-8601
}

/// One row of `GET /api/v1/activity/agents`.
///
/// Mirrors the dict at Python L181-L194.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct AgentDTO {
    pub id: Uuid,
    pub email: String,
    pub agent_type: String,
    pub agent_short_id: String,
    pub is_agent: bool,
    pub is_default: bool,
    pub status: String,                // "LIVE" | "INACTIVE"
    pub api_key_count: u64,
    pub created_at: Option<String>,    // ISO-8601
}

/// Body returned by `GET /api/v1/activity/spans` on the `try/except`
/// fallback path. **Python parity**: status is 200, body is this object.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct SpansErrorEnvelopeDTO {
    pub error: String,
}
```

## 5. Implementation tasks

1. Add DTO structs in `crates/http-server/src/dto/activity.rs`.
2. Extend `PipelineRunRepository` (in `cognee-database`) with `list_recent_with_attribution(dataset_id: Option<Uuid>, limit: u32) -> Result<Vec<PipelineRunRow>, DbError>` joining on `datasets` ⨝ `users`. Include indexes on `pipeline_runs.dataset_id` (already present per [../pipelines.md §5](../pipelines.md#5-database-persistence--pipeline_runs-table)).
3. Extend `UserRepository` with `list_active_with_api_key_counts() -> Result<Vec<(User, u64)>, DbError>` for `/agents`.
4. Add `state.lib.users().list_in_tenant(tenant_id)` adapter for `/users` (re-export of `cognee::users::tenants::get_users_in_tenant`).
5. Add `crates/http-server/src/routers/activity.rs` with five handlers wired via `Router::new().route(...).route(...)...`.
6. Add `crates/http-server/src/routers/activity/export.rs` with `pub fn render_markdown(...) -> String` and unit tests around it (line-by-line snapshots vs Python output for a fixed graph fixture).
7. Add `#[utoipa::path(...)]` annotations and `#[derive(ToSchema)]` on DTOs.
8. Unit tests: `classify_agent` truth table; `render_markdown` snapshot vs `tests/fixtures/activity/expected_export.md`; pipe-escaping test; date-format test.
9. Integration tests in `crates/http-server/tests/test_activity.rs`:
   - `pipeline_runs` returns rows in DESC order, joins owner email, respects `dataset_id` filter.
   - `spans` returns the self-referential `cognee.api.activity.spans` span on the second call.
   - `users` returns the tenant's users; returns `[]` when default user has no tenant.
   - `agents` parses `researcher-bot-abc123@cognee.agent` correctly.
   - `export` returns 404 for missing dataset; returns `text/markdown` with attachment header for valid dataset.
10. Cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_activity.py`:
    - Ingest a fixed corpus, run cognify, hit `/pipeline-runs` against both Python and Rust, assert structural equality (status / pipeline_name / counts).
    - Hit `/export/{id}` against both, normalize timestamps, diff the resulting Markdown.

## 6. Open questions

1. **Cross-tenant data leakage in `/pipeline-runs`, `/users`, `/agents`**: Python returns rows from all tenants — no tenant filter. Rust matches verbatim. Operators wanting tenant scoping must implement it at a reverse-proxy layer or via a downstream view; no application-level flag.
2. **`/users` uses `default_user.tenant_id`** ([Python L109–L113](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L109-L113)) — likely a Python bug, but it's the wire contract. Rust replicates verbatim: fetch `default_user.tenant_id` and use it for the lookup, even when a different authenticated user makes the request.
3. **`/agents` computes an unused `recent_q`** ([Python L153–L159](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py#L153-L159)). Rust replicates verbatim — the same SELECT runs against the relational DB and the result is discarded. Same DB-side cost, same observable behavior.
4. **`/export` Markdown size limit**: Python has no cap. Rust matches — no application-level cap on report size. Operators wanting a cap configure it at the reverse-proxy response-size limit.
5. **`/export` permission gate**: Python does not gate this endpoint by `read:dataset`. Rust matches: any authenticated user can export any dataset they can name. No application-level permission check.
6. **`/spans` self-referential span**: `cognee.api.activity.spans` is included in its own buffer (matches Python's behavior). Polling clients amplify buffer churn; document in user-facing docs.

## 7. References

- Python source: [`cognee/api/v1/activity/routers/get_activity_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/activity/routers/get_activity_router.py).
- Durable observability tier (powers 2.1): [../pipelines.md](../pipelines.md).
- Live observability tier (powers 2.2): [../observability.md](../observability.md).
- Tenant model (powers 2.3): [../tenants.md](../tenants.md).
- Auth extractor: [../auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution).
- `formatted_graph_data` (used by 2.5): [`cognee/modules/graph/methods/`](https://github.com/topoteretes/cognee/tree/main/cognee/modules/graph/methods).
- Per-router README and template: [README.md](README.md).
