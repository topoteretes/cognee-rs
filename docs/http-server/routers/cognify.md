# Router: cognify

The cognify router transforms previously-ingested data into a structured knowledge graph and exposes a live progress feed for the resulting pipeline run. It is the entry point to Cognee's "intelligence layer": classification, chunking, LLM-driven entity/relationship extraction, summarisation, vector indexing, and (optionally) DLT foreign-key edge extraction. It distinguishes itself from `/api/v1/memify` (which enriches an existing graph) and `/api/v1/remember` (which combines `add` + `cognify` in one call) by operating only on already-ingested datasets.

The router exposes two endpoints:

1. `POST /api/v1/cognify` — kicks off the cognify pipeline (blocking or background).
2. `GET /api/v1/cognify/subscribe/{pipeline_run_id}` — WebSocket upgrade that streams `RunEvent` frames for an in-flight or just-finished run.

Companion docs: [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../pipelines.md](../pipelines.md), [../websocket.md](../websocket.md), [../tenants.md](../tenants.md), [../observability.md](../observability.md).

## 1. Mount & file
- Mount prefix: `/api/v1/cognify`
- Router file: `crates/http-server/src/routers/cognify.rs`
- Python source: [`cognee/api/v1/cognify/routers/get_cognify_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py)
- Mounted in [Python `client.py:222`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L222) with `tags=["cognify"]`.

## 2. Endpoints

### 2.1 `POST /api/v1/cognify` — run the cognify pipeline

- **Auth**: `required` (`AuthenticatedUser`). Python uses `Depends(get_authenticated_user)` at [line 75](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L75).
- **Path params**: none.
- **Query params**: none.
- **Request body**: `application/json`, DTO `CognifyPayloadDTO`. Field-by-field mapping:

  | Python field | Python type | Rust field | Rust type | Default | Notes |
  |---|---|---|---|---|---|
  | `datasets` | `Optional[List[str]]` | `datasets` | `Option<Vec<String>>` | `None` | Resolved by name within the user's tenant. |
  | `dataset_ids` | `Optional[List[UUID]]` | `dataset_ids` | `Option<Vec<Uuid>>` | `None` | Bypasses name lookup; allows cognifying datasets the user has write permission to but does not own. |
  | `run_in_background` | `Optional[bool]` | `run_in_background` | `Option<bool>` | `Some(false)` | See §3.6 of [routers/README.md](README.md#36-background-job-endpoints). |
  | `graph_model` | `Optional[dict]` | `graph_model` | `Option<serde_json::Value>` | `None` | JSON Schema describing a Pydantic-shaped graph model; converted via `graph_schema_to_graph_model` in Python ([line 203](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L203)). Rust port stores the raw schema and feeds it into `CognifyConfig::graph_schema`. |
  | `custom_prompt` | `Optional[str]` | `custom_prompt` | `Option<String>` | `Some("")` | Replaces the default graph-extraction prompt. |
  | `ontology_key` | `Optional[List[str]]` | `ontology_key` | `Option<Vec<String>>` | `None` | One or more keys returned by `POST /api/v1/ontologies/upload`. Resolved through `cognee_ontology::OntologyManager::get_contents` (Python's `OntologyService.get_ontology_contents`) and turned into a `RDFLibOntologyResolver` over the concatenated streams ([Python lines 147–164](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L147-L164)). |
  | `chunks_per_batch` | `Optional[int]` | `chunks_per_batch` | `Option<u32>` | `None` | When set, overrides `CognifyConfig::chunks_per_batch`. The default lives on `CognifyConfig` itself (no separate constant — `cognee_cognify::DEFAULT_CHUNKS_PER_BATCH` is not exposed); when this field is `None` the config's own default applies. |

- **Response body**:

  - **Success — blocking (`run_in_background=false`)** — `200 OK`, `application/json`. Body shape: `Map<dataset_id_str, PipelineRunInfoDTO>` where each value is the terminal `PipelineRunInfo` for that dataset (Python returns whatever `cognee_cognify` returned, see [line 270](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L270)). For each dataset:

    ```json
    {
      "<dataset-uuid>": {
        "pipeline_run_id":  "<uuid>",
        "status":           "PipelineRunCompleted",
        "dataset_id":       "<uuid>",
        "dataset_name":     "<str>",
        "payload":          [/* graph data summary */]
      }
    }
    ```

  - **Success — background (`run_in_background=true`)** — `200 OK` (Python returns the dict directly with no wrapping). Body shape:

    ```json
    {
      "<dataset-uuid>": {
        "pipeline_run_id":  "<uuid>",
        "status":           "PipelineRunStarted",
        "dataset_id":       "<uuid>",
        "dataset_name":     "<str>",
        "payload":          []
      }
    }
    ```

    Per [pipelines.md §9.2](../pipelines.md#92-background-run_in_backgroundtrue), `payload` is **always** an empty list in the background dispatch; the formatted-graph data is exposed only via the WebSocket subscription.

- **Error responses**:

  | Status | Body | Condition | Source |
  |---|---|---|---|
  | `400` | `{"error": "No datasets or dataset_ids provided"}` | Both `datasets` and `dataset_ids` are missing/empty. | [Python lines 132–138](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L132-L138) |
  | `400` | `{"detail": [{...}]}` | Body fails JSON validation (handled by the custom `Json` extractor — see [architecture.md §10](../architecture.md#10-request-validation)). | serde validation |
  | `401` | `{"detail": "Unauthorized"}` | No JWT/cookie/API key. | `AuthenticatedUser` extractor |
  | `403` | `{"detail": "..."}` | User lacks `write` permission on a target dataset (raised inside `cognee_cognify`). Maps to `ApiError::Forbidden`. | `cognee::permissions` |
  | `409` | `{"error": "<msg>"}` | `OntologyManager::get_contents` returns an unknown-key error (Python: `OntologyService.get_ontology_contents` raises `ValueError`). | [Python lines 271–278](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L271-L278) |
  | `500` | `{"error": "Pipeline run errored", "detail": "<msg>"}` | Any of the per-dataset runs returned a `PipelineRunErrored`. The `detail` is the first errored run's `error` string (Python uses `next(...isinstance(v, PipelineRunErrored))` at [lines 255–269](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L255-L269)). |
  | `500` | `{"error": "Internal server error", "detail": "<msg>"}` | Any other exception during pipeline execution. | [Python lines 280–288](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L280-L288) |
  | `422` | `{"detail": [...]}` | Pydantic-level type errors (e.g. `dataset_ids` not a list of UUIDs). |

  Note: cognify is **not** the `/improve` quirk — `PipelineRunErrored` returns `500`, not `420`.

- **Side effects**:
  - Writes `pipeline_runs` rows for `DATASET_PROCESSING_INITIATED → STARTED → COMPLETED|ERRORED` per dataset (see [pipelines.md §5](../pipelines.md#5-database-persistence--pipeline_runs-table)).
  - Emits `RunEvent`s on the in-memory broadcast channel registered against the deterministic `pipeline_run_id` (see [pipelines.md §6](../pipelines.md#6-cognee_corepipelinerunregistry--the-new-component)). WebSocket subscribers receive these.
  - Writes nodes/edges into the configured graph DB (Ladybug by default).
  - Writes `DocumentChunk:text`, `Entity:name`, `EntityType:name`, `TextSummary:text`, `EdgeType:relationship_name`, and `Triplet:text` collections into the vector DB.
  - Writes `Data → DocumentChunk` provenance into the relational DB.
  - Persists `graph_model` and `custom_prompt` into `DatasetConfiguration` for the **first dataset** in `datasets`/`dataset_ids` ([Python lines 215–252](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L215-L252)). On lookup-then-write the row is created if absent. Failures are logged at `WARN` and **do not** fail the request.
  - On startup the router reads `DatasetConfiguration` for the first dataset to fill missing `graph_model`/`custom_prompt` from a previous cognify ([Python lines 171–198](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L171-L198)). Failures here are logged at `DEBUG` and ignored.

- **Delegation target**: `cognee::cognify::cognify(datasets, user, CognifyConfig { graph_model, ontology_resolver, custom_prompt, chunks_per_batch, run_in_background, .. })`. The handler does not duplicate dataset resolution, classification, chunking, or LLM logic — it constructs the `CognifyConfig` and delegates. The handler **does** own the `DatasetConfiguration` round-trip, the ontology resolver assembly, and the `PipelineRunErrored` aggregation step (these are router-side concerns that Python's HTTP layer also owns).

- **Validation rules**:
  - At least one of `datasets` or `dataset_ids` must be non-empty (Python rejects empty lists with the same 400 message).
  - `dataset_ids` strings must parse as UUID v4/v5; reject with `400 {"detail": "..."}` from the custom `Json` extractor.
  - `chunks_per_batch`, when set, must be `> 0` — Rust adds this guard (Python does not; we choose `400` here for safety, document in open questions).
  - `ontology_key` items must be non-empty strings; empty list (Python `[]`) is treated as "no ontology".

- **Permission gate**: per dataset, the user must have `write` permission via `state.lib.permissions().user_can(user.id, dataset_id, "write")` (see [../tenants.md §9](../tenants.md#9-repository-surface)). Cognify mutates the graph; `read` is insufficient. The check is enforced inside `cognee::cognify::cognify` via the internal `resolve_authorized_user_datasets` helper (which calls the same `PermissionsRepository::user_can` underneath). The handler does not pre-check; permission errors surface as `ApiError::Forbidden`.

- **Rate / size limits**: standard JSON body limit (default 1 MiB for non-multipart endpoints); cognify payloads are tiny.

- **OpenAPI**:
  - `tags = ["cognify"]`.
  - `security = [{BearerAuth: []}, {ApiKeyAuth: []}]`.
  - Documented responses: 200, 400, 403, 409, 422, 500.

- **Telemetry**:
  - Span name: `cognee.api.cognify.post`.
  - Attributes: `user.id`, `dataset.count`, `dataset.ids` (joined), `run_in_background`, `ontology_key.count`, `custom_prompt.present`, `graph_model.present`.
  - Sub-spans inherited from `cognee_cognify` (per-dataset `cognee.cognify.run`, per-batch `cognee.cognify.extract_graph`, etc.) — see [observability.md §3.3](../observability.md#33-span-instrumentation-conventions).
  - Emits the Python-equivalent telemetry event `"Cognify API Endpoint Invoked"` via `send_telemetry` once per request ([Python lines 123–130](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L123-L130)).

- **Python parity notes**:
  - `graph_model_schema` lookup-from-DB happens *before* the run, and *also* the persist step happens *after* — both are best-effort and never fail the request. Reproduce both.
  - When `dataset_ids` is provided, Python uses it instead of `datasets` (see [line 144](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L144) — `dataset_ids if payload.dataset_ids else payload.datasets`). Rust does the same; do not merge them.
  - The `DatasetConfiguration` lookup is gated on "the first dataset can be parsed as UUID". If `datasets` is `["my-dataset-name"]` the lookup is silently skipped because `UUID(...)` raises. Match exactly.
  - `PipelineRunErrored` aggregation always picks the **first** errored run when emitting the 500 detail.
  - Python's `OntologyService.get_ontology_contents` raises `ValueError` on unknown key; Python catches that as `409`. Rust's `OntologyManager::get_contents` returns an `Err` that the handler maps to `409`. Other ontology errors still hit the 500 catch-all. Reproduce.

### 2.2 `WS /api/v1/cognify/subscribe/{pipeline_run_id}` — live pipeline progress

This endpoint is the WebSocket surface for the cognify pipeline. The full protocol — auth handshake, frame shape, close codes, reconnect rules — is specified in [websocket.md](../websocket.md). This section covers only the cognify-specific bindings.

- **Auth**: `required` (cookie-only). Python's WS handler reads `websocket.cookies.get(AUTH_TOKEN_COOKIE_NAME)` and rejects with `WS_1008_POLICY_VIOLATION` if absent or invalid — see [websocket.md §4](../websocket.md#4-authentication) for the full handshake. Bearer/API-key are not supported on the WS upgrade in either Python or Rust.
- **Path params**: `pipeline_run_id: Uuid` (string in path).
- **Method**: `GET` upgraded to WebSocket per RFC 6455.
- **Body / response**: see [websocket.md §5](../websocket.md#5-frame-format). Each frame is JSON:

  ```json
  {
    "pipeline_run_id": "<uuid>",
    "status":          "PipelineRunYield",
    "payload":         { "nodes": [...], "edges": [...] }
  }
  ```

  The cognify-specific bit: **`payload` is the formatted graph snapshot for the run's dataset**, recomputed on every event by calling `state.lib.formatted_graph_data(dataset_id, &user)`. Python does the same on every yield ([Python line 338](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L338)). This is wasteful for `PipelineRunYield` but **matches Python behavior** and is what the existing frontend expects; do not optimise away.

- **Close codes**:

  | Code | Reason | When |
  |---|---|---|
  | `1000` Normal Closure | (none) | `PipelineRunCompleted` forwarded. **Errored / AlreadyCompleted do NOT close** — Python parity; see [websocket.md §6](../websocket.md#6-status-semantics--terminal-close). |
  | `1008` Policy Violation | `"Unauthorized"` | Auth fails (no cookie / bad signature / expired / unknown user). |
  | `1011` Internal Error | `"channel lagged"` | Subscriber falls behind the broadcast capacity (default 64). |
  | `1011` Internal Error | `"<error message>"` | Unhandled exception during forward / payload computation. |

- **Side effects**:
  - Calls `cognee_core::PipelineRunRegistry::subscribe(run_id)`. If the run id is unknown, the registry returns an empty `Stream` attached to a placeholder slot ([pipelines.md §10](../pipelines.md#10-websocket-integration), Python's `initialize_queue` parity). This lets clients connect *before* the producer's first event lands.
  - On `PipelineRunCompleted` the registry tears down the per-run subscriber slot via the close path. On `PipelineRunErrored` / `PipelineRunAlreadyCompleted` the subscriber slot is **left in place** (Python parity); cleanup happens via the registry's TTL sweep ([pipelines.md §11](../pipelines.md#11-eviction--resource-budget)) or when the client disconnects.

- **Delegation target**:
  - Auth: `crates/http-server/src/auth/cookie.rs::authenticate_from_cookie`.
  - Subscription: `cognee_core::PipelineRunRegistry::subscribe(run_id)` (held in `AppState::pipelines` as `Arc<dyn cognee_core::PipelineRunRegistry>`).
  - Payload: `cognee::graph::formatted_graph_data(dataset_id, user)`.

- **Validation rules**:
  - `pipeline_run_id` parses as `Uuid` (path-level coercion via axum's `Path<Uuid>`). Bad UUID → 400 from the framework before upgrade.
  - **Authorisation gap (Python parity)**: any authenticated user can subscribe to any `pipeline_run_id`, regardless of dataset ownership. Python does not enforce ownership ([websocket.md §4 verification step 5](../websocket.md#verification-details-matches-authmd)). Document the gap; do not fix in Phase 3. Tracked as open question 1 in [websocket.md §12](../websocket.md#12-open-questions).

- **Permission gate**: only authentication. No dataset-level permission check (Python parity). Open question.

- **Rate / size limits**: each broadcast channel has capacity 64 (`cognee_core::pipeline_run_registry::RegistryConfig::channel_capacity`). Slow subscribers are closed with 1011 rather than backpressuring the producer.

- **OpenAPI**: `#[utoipa::path(get, ...)]` documenting the WS upgrade as a `GET` returning `101 Switching Protocols` with `tags = ["cognify"]`.

- **Telemetry**:
  - Span name: `cognee.api.cognify.subscribe`.
  - Attributes: `pipeline_run_id`, `user.id`, `dataset.id` (resolved from the run after subscription), `subscriber.lagged` (set on close 1011), `subscriber.frames_sent`.
  - Each forwarded frame is **not** a sub-span (too noisy); we record a counter `cognee.api.cognify.subscribe.frames_total`.

- **Python parity notes**:
  - Python accepts the WebSocket *before* auth and only sends the close frame after a failed cookie check ([Python lines 292–315](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L292-L315)). Reproduce.
  - Python polls the per-run queue via `asyncio.sleep(2)` between empty reads ([Python line 327](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L327)). Rust uses tokio's `broadcast::Receiver` directly — no polling, lower latency. Document the divergence; the wire shape is unchanged.
  - Python sends one final TEXT frame *before* the Close frame on `PipelineRunCompleted`; we do the same so clients never need to infer state from the close code alone ([websocket.md §6](../websocket.md#6-status-semantics--terminal-close)).
  - Python only closes on `PipelineRunCompleted` ([line 342](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L342)), not `PipelineRunErrored` — i.e. a run that fails leaves the queue alive and the WS open until the client disconnects (or the registry TTL sweeps the slot). **Rust replicates this verbatim** to preserve wire compatibility with existing SDK / frontend clients that don't have a disconnect handler for non-Completed terminals. The error is conveyed via the `status` field in the forwarded JSON frame; the client must inspect it and disconnect on its own. Cross-SDK parity tests assert the connection stays open after `PipelineRunErrored` and `PipelineRunAlreadyCompleted` — see [websocket.md §6](../websocket.md#6-status-semantics--terminal-close).
  - Python ignores any `WebSocketDisconnect` exception by silently calling `remove_queue(...)` and breaking out of the read loop ([Python lines 346–348](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L346-L348)). Rust does the same when `socket.send(...)` returns an error, by tearing down the per-subscriber subscription without aborting the underlying pipeline run.

### 2.2.1 WebSocket worked example

Concrete sequence for a successful background cognify run, illustrating how the WebSocket integrates with the deterministic id and the broadcast channel:

```
T+0  ms  POST /api/v1/cognify {datasets: ["docs"], run_in_background: true}
         server: pid  = uuid5(NAMESPACE_OID, "<user>cognify_pipeline<dataset>")
                 prid = uuid5(NAMESPACE_OID, "<pid>_<dataset>")
                 INSERT pipeline_runs (status=DATASET_PROCESSING_INITIATED, ...);
                 spawn background task -> emits PipelineRunStarted on the channel.
         server: respond 200 {"<dataset_id>": {pipeline_run_id: <prid>, status: "PipelineRunStarted", payload: []}}

T+5  ms  GET /api/v1/cognify/subscribe/<prid> Upgrade: websocket
         server: accept upgrade, read cookie, authenticate.
         server: cognee_core::PipelineRunRegistry::subscribe(prid) -> recv side of broadcast channel.

T+5  ms  channel buffer already contains PipelineRunStarted from T+0
         server: forward as TEXT {pipeline_run_id, status: "PipelineRunStarted", payload: <graph snapshot>}

T+800ms  task emits PipelineRunYield (chunk batch 1/3)
         server: forward as TEXT {..., status: "PipelineRunYield", payload: <graph snapshot>}

T+1.6s   task emits PipelineRunYield (chunk batch 2/3)
         server: forward as TEXT
T+2.4s   task emits PipelineRunYield (chunk batch 3/3) + PipelineRunCompleted
         server: forward both
         server: send Close frame 1000

         INSERT pipeline_runs (status=DATASET_PROCESSING_COMPLETED, ...)
```

If the WS subscriber connects **before** the POST handler creates the run (race), the registry creates an empty broadcast slot via `subscribe`; the client waits idle on `recv()` until events flow. This matches Python's `initialize_queue` semantics ([Python line 321](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L321)).

If the run **erroneously fails** mid-stream (e.g. LLM provider 500), the task emits `PipelineRunErrored` on the channel and the WS handler forwards the frame with `status="PipelineRunErrored"` and `payload` containing whatever graph snapshot exists at that moment. The WebSocket **does not close** — it stays open until the client disconnects or the registry's TTL sweeps the run (Python parity, [websocket.md §6](../websocket.md#6-status-semantics--terminal-close)). Clients that observe a `PipelineRunErrored` frame are expected to disconnect on their own.

## 3. Cross-cutting behavior

- **Pipeline name**: `"cognify_pipeline"`. The dispatcher in [pipelines.md §7](../pipelines.md#7-background-task-lifecycle-http-server-side) uses this when computing `pipeline_id = uuid5(NAMESPACE_OID, "{user_id}{pipeline_name}{dataset_id}")` and `pipeline_run_id = uuid5(NAMESPACE_OID, "{pipeline_id}_{dataset_id}")`. Same as [Python `generate_pipeline_id.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/utils/generate_pipeline_id.py).
- **`cognee_core::PipelineRunRegistry` methods called from the POST handler**:
  - `register_inline(spec, work)` for blocking (`run_in_background=false`).
  - `register_background(spec, work)` for background (`run_in_background=true`).
  - The `RunSpec` carries `Some(prid)` (deterministic id), `pipeline_name = "cognify_pipeline"`, `user_id`, `dataset_id`. The registry handles `pipeline_runs` row writes (Initiated → Started → Completed/Errored) automatically via its `PipelineWatcher` impl.
- **`cognee_core::PipelineRunRegistry` methods called from the WS handler**:
  - `subscribe(run_id)` (covers the "subscribe before producer starts" case).
- **Persistence**: every status transition writes a new `pipeline_runs` row (see [pipelines.md §5](../pipelines.md#5-database-persistence--pipeline_runs-table)). `PipelineRunYield` events are channel-only — no DB write, by design.
- **Idempotent re-cognify**: invoking POST twice on the same dataset within the registry's TTL window returns `PipelineRunAlreadyCompleted` for the second call without re-running. This matches Python ([pipelines.md §8](../pipelines.md#8-status-transitions)) and is observable both in the HTTP response and the WS frame stream.
- **Auth**: per [auth.md §2](../auth.md). For HTTP endpoints all three modes (cookie, bearer, API key) work; for the WebSocket only cookies (per [websocket.md §4](../websocket.md#4-authentication)).
- **Tenant scope**: dataset names resolve only against datasets owned by `user.tenant_id` (see [tenants.md §5](../tenants.md#5-permission-resolution)). `dataset_ids` allow cross-tenant access subject to the permission check.

### 3.1 Per-dataset fan-out for the POST handler

Cognify accepts a list of datasets, but the underlying `cognee::cognify::cognify` runs them sequentially (Python parity). For each dataset the dispatcher:

1. Computes `pipeline_id` and `pipeline_run_id` per [pipelines.md §4](../pipelines.md#4-identifiers).
2. Builds a `RunSpec { run_id: Some(prid), pipeline_name: "cognify_pipeline", user_id, dataset_id }`.
3. Calls `cognee_core::PipelineRunRegistry::register_inline(spec, work)` (blocking) or `register_background(spec, work)` (background). The registry writes the `Initiated → Started → Completed/Errored` rows automatically.
4. Aggregates the per-dataset `PipelineRunInfo` into the response dict.

When `run_in_background=true` and one dataset's dispatch errors during *startup* (e.g. permission denied before the pipeline begins), the response shape becomes mixed: that dataset's value is omitted from the dict and the entire response surfaces as `403`/`500` per [Python lines 271–288](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L271-L288). The blocking-mode `PipelineRunErrored` aggregation at [lines 255–269](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L255-L269) handles only the case where the run *started* and *errored*. Reproduce both branches.

### 3.2 Connection between POST `pipeline_run_id` and WS subscription

The POST handler returns the deterministic `pipeline_run_id` for each dataset; clients use it directly in the WS path. There is no opaque session token — `pipeline_run_id` is itself the subscription handle. Because the id is deterministic in `(user, dataset, pipeline_name)`, two clients running cognify on the same dataset see the same id and either:

1. The first run is still in flight — the second client's POST handler observes the existing handle, does *not* spawn a new run, and emits `PipelineRunAlreadyCompleted` once the first finishes.
2. The first run already completed — the second POST emits `PipelineRunAlreadyCompleted` immediately.

Both cases match Python's idempotent-re-cognify semantics. The WS subscription protocol works identically: any client with the id can attach.

## 4. DTO definitions

```rust
// crates/http-server/src/dto/cognify.rs
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Mirrors Python's `CognifyPayloadDTO`.
/// Source: cognee/api/v1/cognify/routers/get_cognify_router.py:41-58
#[derive(Debug, Clone, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct CognifyPayloadDTO {
    /// Dataset names owned by the authenticated user.
    #[serde(default)]
    pub datasets: Option<Vec<String>>,

    /// Dataset UUIDs. Allows cognifying datasets the user doesn't own
    /// but has write permission on (when ENABLE_BACKEND_ACCESS_CONTROL=true).
    #[serde(default)]
    pub dataset_ids: Option<Vec<Uuid>>,

    /// When true, dispatch to the background and return the
    /// `PipelineRunStarted` event immediately. When false (default),
    /// await the run to completion.
    #[serde(default)]
    pub run_in_background: Option<bool>,

    /// JSON Schema describing a custom Pydantic-shaped graph model.
    /// Falls back to `KnowledgeGraph` when None and no `DatasetConfiguration`
    /// row exists for the first dataset.
    #[serde(default)]
    pub graph_model: Option<serde_json::Value>,

    /// Replaces the default graph-extraction prompt for this run.
    /// Persisted into `DatasetConfiguration.custom_prompt` for the first dataset.
    #[serde(default)]
    pub custom_prompt: Option<String>,

    /// One or more keys from `POST /api/v1/ontologies/upload`.
    /// Resolved to `RDFLibOntologyResolver` on the server side.
    #[serde(default)]
    pub ontology_key: Option<Vec<String>>,

    /// Overrides `CognifyConfig::chunks_per_batch` for this run.
    /// `None` means use the configured default.
    #[serde(default)]
    pub chunks_per_batch: Option<u32>,
}

/// Per-dataset response payload — matches the value type of Python's response dict.
/// Source: cognee/modules/pipelines/models/PipelineRunInfo.py
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct PipelineRunInfoDTO {
    pub pipeline_run_id: Uuid,
    pub status: String,            // "PipelineRunStarted" | ... | "PipelineRunErrored"
    pub dataset_id: Uuid,
    pub dataset_name: String,
    /// For background dispatches, always `[]`.
    /// For blocking + WebSocket frames, the formatted graph data.
    pub payload: serde_json::Value,
    /// Present only on `PipelineRunErrored`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response body type alias — Python returns the dict directly.
pub type CognifyResponseDTO = std::collections::HashMap<String, PipelineRunInfoDTO>;

/// WebSocket frame.
/// Source: cognee/api/v1/cognify/routers/get_cognify_router.py:333-340
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct CognifyWsFrameDTO {
    pub pipeline_run_id: Uuid,
    pub status: String,
    pub payload: serde_json::Value,
}
```

Notes:

- We deliberately **do not** define a `CognifyResponseDTO` newtype because `utoipa` handles `HashMap<String, T>` directly and the Python wire shape is a free-form dict keyed by stringified UUIDs.
- `PipelineRunInfoDTO` is shared with `/memify`, `/improve`, `/remember`, `/sync`, and `/add` (background mode). Move the type to `crates/http-server/src/dto/pipeline_run.rs` so all four routers reuse it.
- `CognifyWsFrameDTO` is also reused by any future WS endpoint that exposes pipeline progress; keep it generic.

## 5. Implementation tasks

1. Add DTOs in `crates/http-server/src/dto/cognify.rs` and `crates/http-server/src/dto/pipeline_run.rs` (shared `PipelineRunInfoDTO`, `CognifyWsFrameDTO`).
2. Add the POST handler in `crates/http-server/src/routers/cognify.rs::post_cognify` — should be ≤ 80 lines, delegating to:
   - `cognee::ontology::OntologyManager::get_contents` (existing crate: [crates/ontology/src/manager.rs:263](../../../crates/ontology/src/manager.rs)) to resolve `ontology_key`.
   - `cognee::cognify::cognify` for the pipeline run.
   - `crates/http-server/src/state::AppState::pipelines.dispatch_pipeline(...)` for the registry plumbing.
   - A new `DatasetConfigurationRepository::{find_by_dataset_id, upsert}` (to-be-added in P3 — the existing `crates/database/src/ops/` does not yet expose this; SeaORM entity for `dataset_configuration` will be added with this router) for the schema/prompt persistence (best-effort, never fail the request).
3. Add the WebSocket handler `ws_subscribe` in `crates/http-server/src/routers/cognify.rs`:
   - Accept the upgrade unconditionally (Python parity).
   - Read the cookie and authenticate via `auth::cookie::authenticate`.
   - Subscribe via `state.pipelines.subscribe(run_id)`.
   - Forward each `RunEvent` as a TEXT frame (call `state.lib.formatted_graph_data(...)` per event).
   - Send Close `1000` only after `PipelineRunCompleted`. Forward `PipelineRunErrored` and `PipelineRunAlreadyCompleted` and continue subscribing (Python parity — see [websocket.md §6](../websocket.md#6-status-semantics--terminal-close)).
4. Wire the router into `build_router`:

   ```rust
   .nest("/cognify", cognify::router())
   // where:
   pub fn router() -> Router<AppState> {
       Router::new()
           .route("/",                          post(post_cognify))
           .route("/subscribe/:pipeline_run_id", get(ws_subscribe))
   }
   ```

5. Add OpenAPI annotations: `#[utoipa::path(post, path = "/api/v1/cognify", tag = "cognify", ...)]` and an explicit override for the WS upgrade route.
6. Add unit tests in the same file for:
   - Empty `datasets` AND `dataset_ids` → 400.
   - `datasets=["x"]`, `dataset_ids=["<uuid>"]` → uses the UUID list (Python parity).
   - Unknown ontology key → 409.
   - Single-dataset blocking run, mocked `cognee`, response shape.
   - First-errored aggregation: two datasets, one ok one errored → 500 with the errored dataset's `error` as `detail`.
7. Add integration tests in `crates/http-server/tests/test_cognify.rs`:
   - End-to-end `POST` on a tmpfs workspace with a real cognify run (gated behind `OPENAI_URL`).
   - WebSocket parity test: spawn a background run, attach a WS, capture frames, assert close 1000 after `PipelineRunCompleted`.
   - Unauth WS connect → close 1008.
   - Slow consumer → close 1011 (overrun the broadcast channel deliberately).
8. Add cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_cognify.py`:
   - Same payload, same OpenAPI route, diff response JSON shapes between Python and Rust (excluding free-form `error` text).
   - Same WS path, same frame sequence (count + status field) between Python and Rust runs of the same dataset content.

## 6. Open questions

1. **`graph_model` schema → Rust graph model**: Python uses `graph_schema_to_graph_model` to convert a Pydantic-flavoured JSON Schema into a runtime `BaseModel`. The Rust equivalent already exists in [`crates/llm/src/dynamic_model.rs`](../../../crates/llm/src/dynamic_model.rs) (not in `cognee_cognify` despite the name implying so). The HTTP port stores the raw schema in `CognifyConfig::graph_schema: Option<serde_json::Value>` and feeds it through `cognee_llm::dynamic_model` for runtime compilation; revisit if customers require structured pre-validation at the router boundary.

2. **`chunks_per_batch=0` handling**: Python silently passes `0` through to `cognify` (where it acts as a "no batching" sentinel). Rust matches: pass through verbatim, no application-level rejection. Strict wire parity.

3. **WS authorisation**: Python lets any authenticated user subscribe to any `pipeline_run_id`. Rust matches verbatim — no `user.tenant_id` ↔ `dataset.tenant_id` check, no application-level dataset-ownership gate on the WebSocket subscribe path.

4. **Empty `datasets=[]` with non-empty `dataset_ids`**: Python's `not payload.datasets` evaluates `[]` as falsy and proceeds with `dataset_ids`. Match exactly — do not require both to be non-`None`.

5. **`DatasetConfiguration` race**: between the read and the write, another cognify call could insert a row. Python catches `Exception` broadly and continues. Rust matches verbatim — same broad catch, same race window, no transactional protection.

6. **WebSocket payload recomputation**: `formatted_graph_data` is called on every `PipelineRunYield`. Python does the same. Rust matches — no caching, no optimization, even though large graphs make this expensive. Strict wire and behavior parity.

7. **Request body size limit**: Python applies no per-route limit. Rust matches: only the global middleware cap ([../architecture.md §8](../architecture.md#8-middleware-stack)) applies. Large `graph_model` JSON Schemas embedded in the body are accepted up to that limit.

## 7. References

- Python router: [`cognee/api/v1/cognify/routers/get_cognify_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py).
- Python core function: [`cognee/api/v1/cognify/cognify.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/cognify.py).
- Python pipeline run model: [`cognee/modules/pipelines/models/PipelineRun.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRun.py).
- Python WS handler: [lines 290–349 of `get_cognify_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/cognify/routers/get_cognify_router.py#L290-L349).
- Python ontology resolution: [`cognee/api/v1/ontologies/ontologies.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/ontologies/ontologies.py) — Python `OntologyService.get_ontology_contents`; Rust target is `cognee_ontology::OntologyManager::get_contents`.
- Python `DatasetConfiguration` model: [`cognee/modules/data/models/DatasetConfiguration.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/data/models/DatasetConfiguration.py).
- Pipeline registry & event channel: [pipelines.md](../pipelines.md).
- WebSocket protocol: [websocket.md](../websocket.md).
- Auth extractors: [auth.md §5](../auth.md#2-three-auth-mechanisms--precedence-and-resolution).
- Tenant resolution: [tenants.md §5](../tenants.md#5-permission-resolution).
- Observability spans: [observability.md §3](../observability.md#3-tracing-stack--tracing--custom-layer).
