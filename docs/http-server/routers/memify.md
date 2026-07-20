# Router: memify

The memify router runs Cognee's enrichment pipeline on top of an existing knowledge graph. Where `/api/v1/cognify` *creates* a graph from ingested data, `/api/v1/memify` reads the existing graph and re-embeds its triplets. The Rust pipeline embeds existing triplets into the `Triplet:text` vector collection so that `SearchType::TripletCompletion` works end-to-end.

> **Current Rust scope.** The Rust `MemifyPayloadDTO` accepts only dataset selection (`dataset_id` / `dataset_name`) and `run_in_background`. It does **not** accept Python's richer task/data surface (`extraction_tasks`, `enrichment_tasks`, `node_name`, `node_type`, `data`); the handler builds `MemifyConfig::default()` regardless (see the handler comment at `crates/http-server/src/routers/memify.rs:88-93`). Sections below describe only what the Rust code actually does today.

It distinguishes itself from `/api/v1/cognify` (graph *creation* from data) and `/api/v1/improve` (memify-shaped alias with an additional session-bridging dimension — see [improve.md](improve.md)).

Companion docs: [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../pipelines.md](../pipelines.md), [../tenants.md](../tenants.md), [../observability.md](../observability.md).

## 1. Mount & file
- Mount prefix: `/api/v1/memify`
- Router file: `crates/http-server/src/routers/memify.rs`
- Python source: [`cognee/api/v1/memify/routers/get_memify_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/memify/routers/get_memify_router.py)
- Mounted in [Python `client.py:224`](https://github.com/topoteretes/cognee/blob/main/cognee/api/client.py#L224) with `tags=["memify"]`.

## 2. Endpoints

### 2.1 `POST /api/v1/memify` — run the memify enrichment pipeline

- **Auth**: `required` (`AuthenticatedUser`). Python uses `Depends(get_authenticated_user)` at [line 50](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/memify/routers/get_memify_router.py#L50).
- **Path params**: none.
- **Query params**: none.
- **Request body**: `application/json`, DTO `MemifyPayloadDTO`. Wire is camelCase per Decision 10; snake_case accepted via per-field aliases. The DTO has only three fields. Field-by-field mapping:

  | Rust field | Rust type | Default | Notes |
  |---|---|---|---|
  | `dataset_name` | `Option<String>` | `None` | One of `dataset_name` / `dataset_id` is required. |
  | `dataset_id` | `DatasetIdRef` (newtype around `Option<Uuid>`) | `DatasetIdRef(None)` | Accepts `null`, `""`, or a UUID string — see §3 cross-cutting behavior for the deserializer. Not wrapped in `Option`. |
  | `run_in_background` | `Option<bool>` | `None` | See [pipelines.md §9](../pipelines.md#9-sync-vs-background-dispatch-http-wire-shapes). |

- **Response body**:

  - **Success — blocking (`run_in_background=false`)** — `200 OK`. Body is a single `PipelineRunInfoDTO` (the handler builds it directly — see `crates/http-server/src/routers/memify.rs:125-133`). Unlike `/cognify`, memify operates on a **single** dataset, so the body is a flat object, **not** keyed by dataset id:

    ```json
    {
      "pipeline_run_id":  "<uuid>",
      "status":           "PipelineRunCompleted",
      "dataset_id":       "<uuid>",
      "dataset_name":     "<str>",
      "payload":          null,
      "error":            null
    }
    ```

    Match Python: [`memify.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/memify/memify.py) returns the single run info. The router does **not** wrap it in a dict.

  - **Success — background (`run_in_background=true`)** — `200 OK`, single `PipelineRunInfoDTO` with `status="PipelineRunStarted"` and `payload=null`. Per [pipelines.md §9.2](../pipelines.md#92-background-run_in_backgroundtrue).

- **Error responses**:

  | Status | Body | Condition | Source |
  |---|---|---|---|
  | `400` | `{"error": "Either datasetId or datasetName must be provided."}` | Both `dataset_id` and `dataset_name` are missing/empty. | [Python lines 92–98](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/memify/routers/get_memify_router.py#L92-L98) |
  | `400` | `{"detail": [{...}]}` | Body fails JSON validation. | Custom `Json` extractor |
  | `401` | `{"detail": "Unauthorized"}` | No JWT/cookie/API key. | `AuthenticatedUser` |
  | `403` | `{"detail": "..."}` | User lacks `write` permission on the target dataset. | `cognee::permissions` |
  | `422` | `{"detail": [...]}` | Pydantic-level type errors (e.g. `dataset_id` neither UUID nor empty string). | |
  | `500` | `{"error": "Pipeline run errored", "detail": "<msg>"}` | `memify_run` is a `PipelineRunErrored`. | [Python lines 113–122](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/memify/routers/get_memify_router.py#L113-L122) |
  | `500` | `{"error": "Internal server error", "detail": "<msg>"}` | Any other exception. | [Python lines 124–132](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/memify/routers/get_memify_router.py#L124-L132) |

  Note: memify follows the **standard 500** Python parity — `PipelineRunErrored` returns `500`, *not* `420`. The 420 quirk is `/improve`-only.

- **Side effects**:
  - Writes `pipeline_runs` rows for `DATASET_PROCESSING_INITIATED → STARTED → COMPLETED|ERRORED`.
  - Emits `RunEvent`s on the in-memory broadcast channel at `pipeline_run_id` (subscribable in principle, though no WS endpoint currently exposes memify — Python is the same; see [websocket.md §1 non-goals](../websocket.md#1-goals--non-goals)).
  - Writes into the `Triplet:text` vector collection (the default memify enrichment behavior — re-embeds existing triplets).
  - **Idempotent**: re-running memify on an already-enriched graph re-embeds the same triplets without producing duplicates (matches Rust's `cognee-cognify` memify design).

- **Delegation target**: `cognee_cognify::run_memify(...)` (imported via `use cognee_cognify::{MemifyConfig, run_memify};`). The handler reads only `dataset_id`, `dataset_name`, and `run_in_background` from the payload, builds `MemifyConfig::default()` (it does **not** accept or forward task-name strings, and there is no `resolve_task_specs` call), and threads the graph/vector/embedding/thread-pool handles from `ComponentHandles` into `run_memify`. See `crates/http-server/src/routers/memify.rs:88-93` (config construction) and `:224-235` (`run_memify` call in `run_real_memify`).

- **Validation rules**:
  - At least one of `dataset_id` / `dataset_name` must be set (Python rejects with `400` and the literal message above).
  - If `dataset_id` is the empty string `""` (Python's `Literal[""]`), the `DatasetIdRef` deserializer treats it as `None`, so the handler falls back to `dataset_name`. The Rust `DatasetIdRef` deserializer (see §3) accepts `null`, `""`/whitespace, or a UUID.
  - There are no task-name fields to validate: the DTO carries no `extraction_tasks` / `enrichment_tasks`, so the "unknown task name" failure mode does not exist in Rust today.

- **Permission gate**: `write` permission on the target dataset via `state.lib.permissions().user_can(user.id, dataset_id, "write")` (see [../tenants.md §9](../tenants.md#9-repository-surface)). Memify mutates the graph and the vector index. The check is enforced inside `cognee::cognify::memify::memify` via the internal `resolve_authorized_user_datasets` helper (which calls the same `PermissionsRepository::user_can` underneath).

- **Rate / size limits**: standard JSON body limit (the global middleware cap applies; the payload is small — dataset selection plus a boolean).

- **OpenAPI**:
  - `tags = ["memify"]`.
  - `security = [{BearerAuth: []}, {ApiKeyAuth: []}]`.
  - Documented responses: 200, 400, 403, 422, 500.

- **Telemetry**:
  - Emits the Python-equivalent product-analytics event `"Memify API Endpoint Invoked"` via `crate::telemetry::emit` once per request, carrying `user.id` and `{ "endpoint": "POST /v1/memify" }` (`crates/http-server/src/routers/memify.rs:31-35`). Python parity: [Python lines 86–90](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/memify/routers/get_memify_router.py#L86-L90).
  - No per-task or `data`/`node_name` attributes exist, since those payload fields are not accepted by the DTO.

- **Python parity notes**:
  - Python accepts a richer payload (`extraction_tasks`, `enrichment_tasks`, `data`, `node_name`, `node_type`) that the Rust DTO does **not** yet plumb. The Rust handler ignores those dimensions and always runs `MemifyConfig::default()` (`crates/http-server/src/routers/memify.rs:88-93`). Closing this gap is tracked below (§5).
  - Python's `dataset` argument is `payload.dataset_id if payload.dataset_id else payload.dataset_name` ([line 107](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/memify/routers/get_memify_router.py#L107)). Rust mirrors this: the `DatasetIdRef` deserializer coerces `""` to `None`, so the handler resolves by UUID when present and otherwise by name (`crates/http-server/src/routers/memify.rs:41-84`).
  - Python's `cognee_memify` returns a single `PipelineRunInfo`, not a dict. Rust matches: the handler returns one `PipelineRunInfoDTO`.
  - On `PipelineRunErrored`, Rust returns `500` with `{"error": "Pipeline run errored", "detail": "<message>"}` (`crates/http-server/src/routers/memify.rs:134-141`).

## 3. Cross-cutting behavior

- **Pipeline name**: `"memify_pipeline"` (Python uses the same string in [`memify.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/memify/memify.py)). Used to derive the deterministic `pipeline_id` and `pipeline_run_id` per [pipelines.md §4](../pipelines.md#4-identifiers).

- **`cognee_core::PipelineRunRegistry` methods called**:
  - `register_inline(spec, work)` for blocking.
  - `register_background(spec, work)` for background.
  - `RunSpec { run_id: Some(prid), pipeline_name: "memify_pipeline", user_id: Some(user.id), dataset_id }`.
  - The registry writes the `Initiated → Started → Completed/Errored` rows automatically via its `PipelineWatcher` impl ([pipelines.md §6.3](../pipelines.md#63-the-registry-implements-pipelinewatcher)).

- **`DatasetIdRef` deserializer**: Python types `dataset_id` as `Union[UUID, Literal[""], None]`. Rust uses a dedicated deserializer that accepts:
  - `null` → `None`,
  - `""` (empty string) → `None`,
  - `"<valid-uuid>"` → `Some(uuid)`,
  - any other string → `400 {"detail": "dataset_id must be a UUID or empty string"}`.

  Implementation lives in `crates/http-server/src/dto/util.rs` and is reused by `/improve` and `/remember` (which carry the same Python annotation).

- **No WebSocket**: memify does not have a `/subscribe` endpoint in Python, and we do not add one. Subscribers can still attach to the cognify WS at `/api/v1/cognify/subscribe/{pipeline_run_id}` *if they happen to know the deterministic id* — but that's an unsupported back door (see [websocket.md §1](../websocket.md#1-goals--non-goals) "non-goals"). Document; do not advertise.

- **Auth**: per [auth.md §2](../auth.md#2-three-auth-mechanisms--precedence-and-resolution); cookie / bearer / API key all accepted.

- **Tenant scope**: dataset name resolves only against the user's tenant. `dataset_id` permits cross-tenant access subject to the permission check.

## 4. DTO definitions

```rust
// crates/http-server/src/dto/memify.rs
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// Re-export shared DTO.
pub use super::pipeline_run::PipelineRunInfoDTO;

/// Request body for `POST /api/v1/memify`.
///
/// Mirrors Python's `MemifyPayloadDTO` (an `InDTO`). Wire is camelCase per
/// Decision 10; snake_case is accepted as input via per-field aliases.
#[derive(Debug, Clone, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemifyPayloadDTO {
    /// Dataset name. Either `dataset_id` or `dataset_name` is required.
    #[serde(default, alias = "dataset_name")]
    pub dataset_name: Option<String>,

    /// Dataset UUID. Empty string is treated as absent.
    /// Either `dataset_id` or `dataset_name` is required.
    #[serde(default, alias = "dataset_id")]
    pub dataset_id: super::util::DatasetIdRef,

    /// When `true`, dispatch to the background and return immediately.
    #[serde(default, alias = "run_in_background")]
    pub run_in_background: Option<bool>,
}
```

The DTO has exactly three fields. There is **no** `extraction_tasks`, `enrichment_tasks`, `data`, `node_name`, or `node_type` field. Note that `dataset_id` is **not** wrapped in `Option` — `DatasetIdRef` already encodes "absent" as `DatasetIdRef(None)`.

`DatasetIdRef` (the helper for Python's `Union[UUID, Literal[""], None]`) is defined in `crates/http-server/src/dto/util.rs` (not in this file) and reused by `/improve` / `/remember`. Its deserializer maps `null`, `""`, and whitespace-only strings to `DatasetIdRef(None)`, a valid UUID string to `DatasetIdRef(Some(uuid))`, and any other string to a deserialize error.

The response body is a single `PipelineRunInfoDTO` (the handler constructs it directly; there is no dedicated `MemifyResponseDTO` alias). `PipelineRunInfoDTO` lives in `crates/http-server/src/dto/pipeline_run.rs` — see [cognify.md §4](cognify.md#4-dto-definitions) for its definition.

## 5. Implementation status & remaining tasks

Implemented today (`crates/http-server/src/{dto,routers}/memify.rs`):

- `MemifyPayloadDTO` with the three fields above (`dataset_name`, `dataset_id`, `run_in_background`), camelCase wire + snake_case aliases.
- `DatasetIdRef` in `crates/http-server/src/dto/util.rs`, re-exported and reused by `/improve` / `/remember`.
- Handler `post_memify`:
  - validates at least one of `dataset_id` / `dataset_name` (else `400 {"error": "Either datasetId or datasetName must be provided."}`);
  - resolves the dataset by id (looking up its name) or by name (looking up its id, or `uuid5` when no DB is wired);
  - builds `MemifyConfig::default()` (no task/data plumbing — see `routers/memify.rs:88-93`);
  - dispatches via `crate::pipelines::dispatch::dispatch_pipeline` under the `"memify_pipeline"` name, blocking or background per `run_in_background`;
  - inner work calls `cognee_cognify::run_memify(...)` with the graph/vector/embedding/thread-pool handles from `ComponentHandles`;
  - on `PipelineRunErrored`, returns `500` with `error="Pipeline run errored"` and the run message as `detail`.
- Router wired in `build_router` via `.nest("/memify", memify::router())`.
- Unit tests in `routers/memify.rs`: missing-dataset → 400 (with the `error` key); name-only / id-only dispatch; missing-components → 500; background → `PipelineRunStarted`.

Remaining (gap to Python parity):

1. Plumb Python's richer payload surface (`extraction_tasks`, `enrichment_tasks`, `data`, `node_name`, `node_type`) into `MemifyPayloadDTO` and translate it into a non-default `MemifyConfig`. Until then the handler ignores those dimensions and uses `MemifyConfig::default()`.
2. Add OpenAPI annotations (`#[utoipa::path(post, path = "/api/v1/memify", tag = "memify", ...)]`).
3. Add integration tests in `crates/http-server/tests/test_memify.rs` (end-to-end blocking memify on a real graph gated behind `OPENAI_URL`; 403 path for a user without `write` permission).
4. Add cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_memify.py` (matching response shapes and deterministic `pipeline_run_id`).

## 6. Open questions

1. **Wrap response in a dict?** Python returns the raw `PipelineRunInfo` (single-dataset memify). `/cognify` returns a `Dict[str, PipelineRunInfo]`. The shapes diverge by accident in Python. Should the Rust `/memify` return a dict for consistency with `/cognify`? Decision so far: **no** — Rust keeps Python parity and returns a single `PipelineRunInfoDTO`.

2. **Plumb Python's task/data payload?** Python accepts `extraction_tasks`, `enrichment_tasks`, `data`, `node_name`, `node_type`; Rust's DTO does not, and the handler always uses `MemifyConfig::default()` (`routers/memify.rs:88-93`). Closing this requires both DTO fields and a task registry (proposal sketched in [cognify.md §3](cognify.md#3-cross-cutting-behavior)) plus a `MemifyConfig` translation. Tracked in §5.

3. **Deterministic `pipeline_run_id` collisions across cognify/memify**: `pipeline_id = uuid5(NAMESPACE_OID, "{user}{pipeline_name}{dataset}")` — the `pipeline_name` differentiates cognify and memify, so no collision. Confirmed; flag for tests.

4. **Should memify accept `dataset_ids: List[UUID]` like cognify?** Currently single dataset only — Python is the same. If Rust users want multi-dataset memify, we'd add an array overload. Proposal: defer; track as a Phase 5 feature request.

## 7. References

- Python router: [`cognee/api/v1/memify/routers/get_memify_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/memify/routers/get_memify_router.py).
- Python core function: [`cognee/modules/memify/memify.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/memify/memify.py).
- Default tasks: [`cognee/memify_pipelines/memify_default_tasks.py`](https://github.com/topoteretes/cognee/blob/main/cognee/memify_pipelines/memify_default_tasks.py).
- Pipeline registry & event channel: [pipelines.md](../pipelines.md).
- Auth extractors: [auth.md §5](../auth.md#2-three-auth-mechanisms--precedence-and-resolution).
- Tenant resolution: [tenants.md §5](../tenants.md#5-permission-resolution).
- Rust memify pipeline: `crates/cognify/src/memify/` (see project [CLAUDE.md](../../../.claude/CLAUDE.md) for crate map).
