# Router: improve

The improve router is a memory-oriented alias for `/api/v1/memify`. The HTTP surface and the underlying pipeline are nearly identical to memify; the difference lives one layer down in the Rust delegate (`cognee_lib::api::improve::improve`), which adds three optional session-bridging stages on top of the memify enrichment when `session_ids` is supplied. (Note: the Python HTTP router does not currently expose `session_ids` — it forwards to `cognee_improve(...)` without it. The capability is reserved for future extension and parity with the Python `cognee.improve()` SDK.)

It distinguishes itself from `/api/v1/memify` (same enrichment pipeline, no session bridging) by:

1. Returning **`420`** on `PipelineRunErrored` instead of the standard `500` (Python parity quirk — see §2.1 error responses).
2. Calling the `improve` library function rather than `memify` directly.

Companion docs: [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../pipelines.md](../pipelines.md), [memify.md](memify.md), [../tenants.md](../tenants.md), [../observability.md](../observability.md).

## 1. Mount & file
- Mount prefix: `/api/v1/improve`
- Router file: `crates/http-server/src/routers/improve.rs`
- Python source: [`cognee/api/v1/improve/routers/get_improve_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/improve/routers/get_improve_router.py)
- Mounted in [Python `client.py:297`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L297) with `tags=["improve"]`.

## 2. Endpoints

### 2.1 `POST /api/v1/improve` — run the improve pipeline (memify + optional session bridging)

- **Auth**: `required` (`AuthenticatedUser`). Python uses `Depends(get_authenticated_user)` at [line 36](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/improve/routers/get_improve_router.py#L36).
- **Path params**: none.
- **Query params**: none.
- **Request body**: `application/json`, DTO `ImprovePayloadDTO`. Fields are identical to `MemifyPayloadDTO` minus the (Python) absent-from-router-but-present-in-lib `session_ids` field. Field-by-field mapping:

  | Python field | Python type | Rust field | Rust type | Default | Notes |
  |---|---|---|---|---|---|
  | `extraction_tasks` | `Optional[List[str]]` | `extraction_tasks` | `Option<Vec<String>>` | `None` | Same as memify. |
  | `enrichment_tasks` | `Optional[List[str]]` | `enrichment_tasks` | `Option<Vec<String>>` | `None` | Same as memify. |
  | `data` | `Optional[str]` | `data` | `Option<String>` | `Some("")` | Same as memify. |
  | `dataset_name` | `Optional[str]` | `dataset_name` | `Option<String>` | `None` | One of `dataset_name` / `dataset_id` is required. |
  | `dataset_id` | `Union[UUID, Literal[""], None]` | `dataset_id` | `Option<DatasetIdRef>` | `None` | Same Python `Union` quirk as memify; reuse `DatasetIdRef`. |
  | `node_name` | `Optional[List[str]]` | `node_name` | `Option<Vec<String>>` | `None` | Filter graph to specific named entities; only used when `data` is empty. |
  | `run_in_background` | `Optional[bool]` | `run_in_background` | `Option<bool>` | `Some(false)` | See [pipelines.md §9](../pipelines.md#9-sync-vs-background-dispatch-http-wire-shapes). |

  **Not exposed** (Phase 3): `session_ids: Option<Vec<String>>`. The Rust port keeps the field absent for parity. Track as open question §6.

- **Response body**:

  - **Success — blocking (`run_in_background=false`)** — `200 OK`, `application/json`. Body is the raw return value of `cognee_lib::api::improve::improve` (Python returns it directly at [line 88](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/improve/routers/get_improve_router.py#L88)). When `session_ids` is not provided, this is structurally identical to `/memify`'s response — a single `PipelineRunInfoDTO`:

    ```json
    {
      "pipeline_run_id":  "<uuid>",
      "status":           "PipelineRunCompleted",
      "dataset_id":       "<uuid>",
      "dataset_name":     "<str>",
      "payload":          [...]
    }
    ```

    When `session_ids` is provided (future), the response carries an aggregated multi-stage shape — out of scope for Phase 3.

  - **Success — background (`run_in_background=true`)** — `200 OK`. Single `PipelineRunInfoDTO` with `status="PipelineRunStarted"` and `payload=[]`. Per [pipelines.md §9.2](../pipelines.md#92-background-runinbackgroundtrue).

- **Error responses**:

  | Status | Body | Condition | Source |
  |---|---|---|---|
  | `400` | `{"detail": "Either datasetId or datasetName must be provided."}` | Both `dataset_id` and `dataset_name` are missing/empty. Note: Python uses `HTTPException(400, detail=...)`, so the body is `{"detail": "..."}`. | [Python lines 67–71](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/improve/routers/get_improve_router.py#L67-L71) |
  | `400` | `{"detail": [{...}]}` | Body fails JSON validation. | Custom `Json` extractor |
  | `401` | `{"detail": "Unauthorized"}` | No JWT/cookie/API key. | `AuthenticatedUser` |
  | `403` | `{"detail": "..."}` | User lacks `write` permission on the target dataset. | `cognee_lib::permissions` |
  | `409` | `{"error": "An error occurred during graph improvement."}` | Any other exception during processing — the router-level catch-all. **Note**: Python returns `409`, not `500`, here ([lines 89–94](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/improve/routers/get_improve_router.py#L89-L94)). The body uses `error`, no `detail`. | Python parity |
  | **`420`** | `<PipelineRunErrored object as dict>` | `improve_run` returns a `PipelineRunErrored`. **The status code is 420 (Enhance Your Calm), unique to this router.** Python encodes the entire `PipelineRunErrored` object as the response body — see parity note below. | [Python line 87](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/improve/routers/get_improve_router.py#L87) |
  | `422` | `{"detail": [...]}` | Pydantic-level type errors. | |

  **Why 420 specifically**: this is a Python codebase quirk — `JSONResponse(status_code=420, content=improve_run)` is what the handler returns when the underlying `cognee_improve` produces a `PipelineRunErrored`. `420` is the unofficial "Enhance Your Calm" status from the Twitter API era; HTTP/1.1 reserves the code as "unassigned". There is no semantic justification for it in cognee — it's idiosyncratic and likely a leftover from an earlier refactor — but **parity requires we reproduce it**. Cross-SDK clients that branch on status code will break if we use `500` instead.

  Implementation: return `ApiError::PipelineErrored("improve")` from the handler and have the `improve` variant of the error mapper emit `420` (the cognify/memify dispatcher emits `500`). See [architecture.md §9](../architecture.md#9-error-handling) — the `PipelineErrored(String)` variant carries the originating endpoint name so the mapper can branch.

- **Side effects**:
  - Identical to `/memify` (see [memify.md §2.1](memify.md#21-post-apiv1memify--run-the-memify-enrichment-pipeline)) when no session bridging is requested:
    - Writes `pipeline_runs` rows for `DATASET_PROCESSING_INITIATED → STARTED → COMPLETED|ERRORED`.
    - Emits `RunEvent`s on the broadcast channel.
    - Writes triplet embeddings into the `Triplet:text` vector collection.
  - **Future (with `session_ids`)**: also reads/updates `feedback_weight` on graph nodes/edges, cognifies session Q&A into the graph tagged with `node_set="user_sessions_from_cache"`, and incrementally syncs new graph relationships back into the session cache. See [Python `improve.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/improve/improve.py) for the full description.

- **Delegation target**: `cognee_lib::api::improve::improve(ImproveConfig { extraction_tasks, enrichment_tasks, data, dataset, node_name, run_in_background, user, session_ids: None })`. The Rust port matches Python's `cognee.api.v1.improve.improve` — same task resolution, same fallback to `cognee_lib::cognify::memify::memify` when no session-bridging stages run.

- **Validation rules**:
  - At least one of `dataset_id` / `dataset_name` must be set.
  - `dataset_id=""` → `None` via the `DatasetIdRef` deserializer.
  - Empty `data=""` is treated as "no data — use the existing graph" (Python parity).
  - Unknown task names surface as `409` via the catch-all (Python parity).

- **Permission gate**: `write` on the target dataset via `state.lib.permissions().user_can(user.id, dataset_id, "write")` (see [../tenants.md §9](../tenants.md#9-repository-surface)). Improve mutates the graph and the vector index. The check is enforced inside `cognee_lib::api::improve::improve` via the internal `resolve_authorized_user_datasets` helper (which calls the same `PermissionsRepository::user_can` underneath).

- **Rate / size limits**: standard JSON body limit (1 MiB by default). Same considerations as `/memify`.

- **OpenAPI**:
  - `tags = ["improve"]`.
  - `security = [{BearerAuth: []}, {ApiKeyAuth: []}]`.
  - Documented responses: 200, 400, 403, 409, **420**, 422. Explicitly add the 420 response so OpenAPI consumers don't treat it as an undocumented status.

- **Telemetry**:
  - Span name: `cognee.api.improve.post`.
  - Attributes: `user.id`, `dataset.ref`, `extraction_tasks.count`, `enrichment_tasks.count`, `data.bytes`, `node_name.count`, `run_in_background`, `session_ids.count` (always 0 in Phase 3).
  - Sub-spans inherited from `cognee_improve` (per-task spans `cognee.improve.task.<name>`, plus the session-bridging stage spans `cognee.improve.feedback`, `cognee.improve.persist_session`, `cognee.improve.sync_to_cache` once that surface lands).
  - Emits the Python-equivalent telemetry event `"Improve API Endpoint Invoked"` via `send_telemetry` once per request ([Python lines 58–65](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/improve/routers/get_improve_router.py#L58-L65)).

- **Python parity notes**:
  - **`420` for `PipelineRunErrored`**: as documented above. This is the single most surprising parity behavior in the entire HTTP layer; tests must assert the literal `420` status code.
  - **`PipelineRunErrored` body shape**: Python returns `JSONResponse(status_code=420, content=improve_run)` — i.e. the run object itself, *not* the standard `{"error": "...", "detail": "..."}` envelope. FastAPI's `JSONResponse` serialises the Pydantic model via `jsonable_encoder`, producing a dict shaped like `{"pipeline_run_id": "...", "status": "PipelineRunErrored", "dataset_id": "...", "dataset_name": "...", "payload": [...], "error": "<msg>"}`. The Rust port must match: serialize the `PipelineRunInfoDTO` for the errored run and wrap it in the 420 response — do **not** transform it into the canonical `ApiError` envelope.
  - **`409` catch-all for non-pipeline exceptions**: identical behavior to `/remember` (also `409`) but **different from `/memify` and `/cognify`** (which use `500`). The literal body is `{"error": "An error occurred during graph improvement."}` — no `detail`. Reproduce literally.
  - **`HTTPException(400, ...)` body shape**: `{"detail": "..."}`, not `{"error": "..."}`. Reproduce.
  - The Python router does not invoke any session-bridging behavior from the HTTP surface (it omits `session_ids` from the payload). The lib function signature in [`improve.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/improve/improve.py) accepts `session_ids` but the router does not forward it. Match exactly: **do not** add `session_ids` to `ImprovePayloadDTO` in Phase 3.

## 3. Cross-cutting behavior

- **Pipeline name**: `"memify_pipeline"` (improve delegates to memify under the hood; the registry sees the memify name unless session bridging adds dedicated sub-pipelines). The deterministic `pipeline_run_id` collides intentionally with `/memify` — calling `/api/v1/memify` then `/api/v1/improve` on the same dataset returns `PipelineRunAlreadyCompleted` for the second call (Python parity, since `cognee_improve` reduces to `cognee_memify` when no `session_ids` are present).

- **`cognee_core::PipelineRunRegistry` methods called**:
  - `register_inline(spec, work)` for blocking.
  - `register_background(spec, work)` for background.
  - `RunSpec { run_id: Some(prid), pipeline_name: "memify_pipeline", user_id: Some(user.id), dataset_id }`.
  - When `session_ids` ships in a later phase, additional `pipeline_id`s for `"improve_feedback"`, `"improve_persist_session"`, and `"improve_sync_to_cache"` will be created. Out of scope for Phase 3.

- **Library API note**: `cognee_lib::api::improve::improve()` no longer accepts a `run_in_background` parameter. The current library implementation has a `run_in_background: bool` flag at [crates/lib/src/api/improve.rs:59](../../../crates/lib/src/api/improve.rs#L59) and a `has_sessions && !run_in_background` branch at [:197-198](../../../crates/lib/src/api/improve.rs#L197-L198) — that flag is being removed as a prerequisite of this router landing. After the refactor, `improve()` always runs the full session-bridging path when sessions are present; the HTTP layer wraps it via the registry. See [pipelines.md §2](../pipelines.md#2-library-refactor-prerequisite).

- **No WebSocket**: `/improve` does not expose a WS endpoint; subscribers can attach to the cognify WS at `/api/v1/cognify/subscribe/{pipeline_run_id}` if they know the deterministic id but it's not officially supported (Python parity — see [websocket.md §1](../websocket.md#1-goals--non-goals)).

- **Auth**: per [auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution); cookie / bearer / API key all accepted.

- **Tenant scope**: dataset name resolves only against the user's tenant. `dataset_id` permits cross-tenant access subject to the permission check.

- **`ApiError::PipelineErrored` mapping**: introduce a discriminator so the error mapper can route `420` for `/improve` and `500` for cognify/memify. Sketch:

  ```rust
  pub enum ApiError {
      // ... other variants ...
      /// 500 for most routers; 420 for /improve.
      PipelineErrored {
          source: PipelineErrorSource,
          run_info: serde_json::Value,
      },
  }
  pub enum PipelineErrorSource {
      Cognify,
      Memify,
      Improve,   // -> 420 (Python parity)
      Remember,
      Sync,
  }
  ```

  See [architecture.md §9](../architecture.md#9-error-handling) for the broader `ApiError` discussion.

## 4. DTO definitions

```rust
// crates/http-server/src/dto/improve.rs
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::dto::pipeline_run::PipelineRunInfoDTO;
use crate::dto::util::DatasetIdRef;

/// Mirrors Python's `ImprovePayloadDTO`.
/// Source: cognee/api/v1/improve/routers/get_improve_router.py:21-28
#[derive(Debug, Clone, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct ImprovePayloadDTO {
    /// Names of registered Cognee tasks for the extraction phase.
    /// `None` selects `get_default_memify_extraction_tasks()`.
    #[serde(default)]
    pub extraction_tasks: Option<Vec<String>>,

    /// Names of registered Cognee tasks for the enrichment phase.
    /// `None` selects `get_default_memify_enrichment_tasks()`.
    #[serde(default)]
    pub enrichment_tasks: Option<Vec<String>>,

    /// Optional caller-supplied input. `Some("")` means "no data — use the existing graph".
    #[serde(default)]
    pub data: Option<String>,

    /// Dataset name — resolved within the user's tenant.
    #[serde(default)]
    pub dataset_name: Option<String>,

    /// Dataset UUID. Accepts `""` as "no value" for Python-parity.
    #[serde(default)]
    pub dataset_id: Option<DatasetIdRef>,

    /// Filter graph to specific named entities (only when `data` is empty).
    #[serde(default)]
    pub node_name: Option<Vec<String>>,

    /// When true, dispatch to the background and return immediately.
    #[serde(default)]
    pub run_in_background: Option<bool>,

    // NOTE: `session_ids` exists in the Python *library* function but NOT in the
    // Python *HTTP* DTO. We preserve that gap for Phase 3 parity. Track in §6.
}

/// Response body. Single `PipelineRunInfoDTO` (mirrors Python's behavior for the no-session-bridging path).
/// On 420, the body is a `PipelineRunInfoDTO` with `status="PipelineRunErrored"` and an `error` field.
pub type ImproveResponseDTO = PipelineRunInfoDTO;
```

`PipelineRunInfoDTO` lives in `crates/http-server/src/dto/pipeline_run.rs` — see [cognify.md §4](cognify.md#4-dto-definitions) for its definition. `DatasetIdRef` lives in `crates/http-server/src/dto/util.rs` — see [memify.md §4](memify.md#4-dto-definitions).

## 5. Implementation tasks

1. Add DTO in `crates/http-server/src/dto/improve.rs` (`ImprovePayloadDTO`).
2. Extend `ApiError` in `crates/http-server/src/error.rs` with a `PipelineErrored { source: PipelineErrorSource, run_info: serde_json::Value }` variant where `PipelineErrorSource::Improve` maps to status `420` and all other variants map to `500`. The body in both cases is the serialised `run_info` directly (Python parity).
3. Add the handler `post_improve` in `crates/http-server/src/routers/improve.rs`:
   - Validate at least one of `dataset_id` / `dataset_name`.
   - Resolve the dataset.
   - Call the HTTP-side dispatcher (`crates/http-server/src/pipelines/dispatch.rs`) which builds a `RunSpec { pipeline_name: "memify_pipeline", .. }` and invokes `state.pipelines.register_inline` or `register_background` against the `cognee_core::PipelineRunRegistry`. The `work` future is `cognee_lib::api::improve::improve(...)` (sync — no `run_in_background` parameter; see library refactor note above).
   - On `PipelineRunErrored`, return `ApiError::PipelineErrored { source: Improve, run_info: serde_json::to_value(...)? }`.
   - On any other exception, return `ApiError::Conflict("An error occurred during graph improvement.")` (mapping to 409 with the literal Python message).
4. Wire the router into `build_router`:

   ```rust
   .nest("/improve", improve::router())
   ```

5. Add OpenAPI annotations: `#[utoipa::path(post, path = "/api/v1/improve", tag = "improve", responses(... (status = 420, ...) ...))]`. The 420 response **must** be documented explicitly so codegen tooling treats it as known.
6. Add unit tests in the same file:
   - Both `dataset_id` and `dataset_name` empty → `400` with `{"detail": "Either datasetId or datasetName must be provided."}`.
   - `dataset_id=""` and `dataset_name="foo"` → resolves by name.
   - Mocked `cognee_lib::improve` returning `PipelineRunErrored` → **`420`** with the run object as the body (assert the status code literally — this is the parity quirk).
   - Mocked `cognee_lib::improve` raising any other exception → `409` with the literal `error` body.
   - Mocked success → `200` with `PipelineRunInfoDTO` shape.
7. Add integration tests in `crates/http-server/tests/test_improve.rs`:
   - End-to-end blocking improve on a tmpfs workspace with a real graph (gated behind `OPENAI_URL`).
   - Background dispatch returns immediately with `status="PipelineRunStarted"`.
   - **420 path** — deliberately corrupt the graph to force a `PipelineRunErrored` and assert the `420` status code plus the `status` field in the body equals `"PipelineRunErrored"`.
8. Add cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_improve.py`:
   - Same payload, diff response JSON shapes between Python and Rust (excluding free-form error text).
   - Force a `PipelineRunErrored` in both stacks and assert both return `420` with structurally-equivalent bodies.

## 6. Open questions

1. **Should we expose `session_ids` in Phase 3?** Python's HTTP router does not, but the lib does. Adding it to Rust would be a forward-compatible upgrade — but it would diverge from Python's HTTP wire shape (cross-SDK tests would fail until Python catches up). Proposal: **no** — strict parity in Phase 3; add `session_ids` in lockstep with the Python router upstream.

2. **`420` documentation**: should we surface the parity quirk in user-facing docs (e.g. a callout in `/improve` OpenAPI description), or treat it as an internal compat detail? Proposal: surface it — clients writing error handlers need to know.

3. **`ApiError::PipelineErrored` discriminator**: is `PipelineErrorSource` the right shape, or should we use a string identifier? Proposal: enum for type safety; serialise via `Display` if/when it leaks into telemetry attributes.

4. **`409` vs `500` for the catch-all**: improve uses `409` (matches `/remember`) where memify and cognify use `500`. Should Rust normalise to `500` everywhere? Proposal: keep parity; document the divergence.

5. **`pipeline_name` for improve**: improve currently reuses memify's pipeline name and run id. When session-bridging stages land, each will need its own deterministic id. Proposal: the inner library (`cognee_improve`) will own those ids; the HTTP router reports only the top-level (memify) one.

6. **Body shape on 420**: should we wrap the `PipelineRunInfo` in `{"error": "Pipeline run errored", "detail": ..., "run_info": {...}}` for consistency with the cognify error envelope? Proposal: **no** — Python returns the raw `PipelineRunInfo` object, and that's what cross-SDK clients deserialise.

7. **OpenAPI tooling support for `420`**: not all generators recognise `420`. Verify utoipa allows arbitrary status codes; if not, file a workaround note in the open questions of [observability.md](../observability.md).

## 7. References

- Python router: [`cognee/api/v1/improve/routers/get_improve_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/improve/routers/get_improve_router.py).
- Python core function: [`cognee/api/v1/improve/improve.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/improve/improve.py).
- Memify (the underlying enrichment pipeline): [memify.md](memify.md).
- `PipelineRunErrored` model: [`cognee/modules/pipelines/models/PipelineRunInfo.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/models/PipelineRunInfo.py).
- Pipeline registry & event channel: [pipelines.md](../pipelines.md).
- Auth extractors: [auth.md §5](../auth.md#2-three-auth-mechanisms--precedence-and-resolution).
- Tenant resolution: [tenants.md §5](../tenants.md#5-permission-resolution).
- Error envelope (incl. the `PipelineErrored` discriminator): [architecture.md §9](../architecture.md#9-error-handling).
- HTTP 420 (Twitter API "Enhance Your Calm"): [Wikipedia — List of HTTP status codes](https://en.wikipedia.org/wiki/List_of_HTTP_status_codes#420).
