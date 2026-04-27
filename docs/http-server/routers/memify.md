# Router: memify

The memify router runs Cognee's enrichment pipeline on top of an existing knowledge graph. Where `/api/v1/cognify` *creates* a graph from ingested data, `/api/v1/memify` reads the existing graph (or a caller-supplied `data` blob), applies a list of extraction tasks followed by a list of enrichment tasks, and writes the results back. The default Rust pipeline embeds existing triplets into the `Triplet:text` vector collection so that `SearchType::TripletCompletion` works end-to-end.

It distinguishes itself from `/api/v1/cognify` (graph *creation* from data) and `/api/v1/improve` (memify-shaped alias with an additional session-bridging dimension — see [improve.md](improve.md)).

Companion docs: [../plan.md](../plan.md), [../architecture.md](../architecture.md), [../auth.md](../auth.md), [../pipelines.md](../pipelines.md), [../tenants.md](../tenants.md), [../observability.md](../observability.md).

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
- **Request body**: `application/json`, DTO `MemifyPayloadDTO`. Field-by-field mapping:

  | Python field | Python type | Rust field | Rust type | Default | Notes |
  |---|---|---|---|---|---|
  | `extraction_tasks` | `Optional[List[str]]` | `extraction_tasks` | `Option<Vec<String>>` | `None` | Strings naming registered Cognee tasks. `None` falls back to `get_default_memify_extraction_tasks()`. |
  | `enrichment_tasks` | `Optional[List[str]]` | `enrichment_tasks` | `Option<Vec<String>>` | `None` | Same as above for enrichment. |
  | `data` | `Optional[str]` | `data` | `Option<String>` | `Some("")` | Optional caller-supplied input to feed the first extraction task. When empty, the existing graph (filtered by `node_name` if present) is forwarded instead. |
  | `dataset_name` | `Optional[str]` | `dataset_name` | `Option<String>` | `None` | One of `dataset_name` / `dataset_id` is required. |
  | `dataset_id` | `Union[UUID, Literal[""], None]` | `dataset_id` | `Option<DatasetIdRef>` | `None` | Python accepts `""` as "no value" — see §3 cross-cutting behavior for the deserializer. |
  | `node_name` | `Optional[List[str]]` | `node_name` | `Option<Vec<String>>` | `None` | Filter graph to specific named entities; only used when `data` is empty. |
  | `run_in_background` | `Optional[bool]` | `run_in_background` | `Option<bool>` | `Some(false)` | See [pipelines.md §9](../pipelines.md#9-sync-vs-background-dispatch-http-wire-shapes). |

- **Response body**:

  - **Success — blocking (`run_in_background=false`)** — `200 OK`. Body is the raw return value of `cognee_lib::cognify::memify::memify` (Python returns it directly at [line 123](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/memify/routers/get_memify_router.py#L123)):

    ```json
    {
      "<dataset-uuid>": {
        "pipeline_run_id":  "<uuid>",
        "status":           "PipelineRunCompleted",
        "dataset_id":       "<uuid>",
        "dataset_name":     "<str>",
        "payload":          [/* triplets indexed, edge counts, etc. */]
      }
    }
    ```

    Note: unlike `/cognify`, memify operates on a **single** dataset. Python's `cognee_memify` returns a single `PipelineRunInfo` (not a dict). The router serialises the value as-is, so the response body is a flat object, not keyed by dataset id:

    ```json
    {
      "pipeline_run_id":  "<uuid>",
      "status":           "PipelineRunCompleted",
      "dataset_id":       "<uuid>",
      "dataset_name":     "<str>",
      "payload":          [...]
    }
    ```

    Match Python: [`memify.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/memify/memify.py) returns the single run info. The router does **not** wrap it in a dict (open question — see §6).

  - **Success — background (`run_in_background=true`)** — `200 OK`, single `PipelineRunInfo` with `status="PipelineRunStarted"` and `payload=[]`. Per [pipelines.md §9.2](../pipelines.md#92-background-runinbackgroundtrue).

- **Error responses**:

  | Status | Body | Condition | Source |
  |---|---|---|---|
  | `400` | `{"error": "Either datasetId or datasetName must be provided."}` | Both `dataset_id` and `dataset_name` are missing/empty. | [Python lines 92–98](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/memify/routers/get_memify_router.py#L92-L98) |
  | `400` | `{"detail": [{...}]}` | Body fails JSON validation. | Custom `Json` extractor |
  | `401` | `{"detail": "Unauthorized"}` | No JWT/cookie/API key. | `AuthenticatedUser` |
  | `403` | `{"detail": "..."}` | User lacks `write` permission on the target dataset. | `cognee_lib::permissions` |
  | `422` | `{"detail": [...]}` | Pydantic-level type errors (e.g. `dataset_id` neither UUID nor empty string). | |
  | `500` | `{"error": "Pipeline run errored", "detail": "<msg>"}` | `memify_run` is a `PipelineRunErrored`. | [Python lines 113–122](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/memify/routers/get_memify_router.py#L113-L122) |
  | `500` | `{"error": "Internal server error", "detail": "<msg>"}` | Any other exception. | [Python lines 124–132](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/memify/routers/get_memify_router.py#L124-L132) |

  Note: memify follows the **standard 500** Python parity — `PipelineRunErrored` returns `500`, *not* `420`. The 420 quirk is `/improve`-only.

- **Side effects**:
  - Writes `pipeline_runs` rows for `DATASET_PROCESSING_INITIATED → STARTED → COMPLETED|ERRORED`.
  - Emits `RunEvent`s on the in-memory broadcast channel at `pipeline_run_id` (subscribable in principle, though no WS endpoint currently exposes memify — Python is the same; see [websocket.md §1 non-goals](../websocket.md#1-goals--non-goals)).
  - Writes nodes/edges into the graph DB if the supplied tasks add new ones (default tasks only embed existing triplets).
  - Writes into the `Triplet:text` vector collection (default enrichment task).
  - **Idempotent**: re-running memify on an already-enriched graph re-embeds the same triplets without producing duplicates (matches Rust's `cognee-cognify::memify::memify` design).

- **Delegation target**: `cognee_lib::cognify::memify::memify(MemifyConfig { extraction_tasks, enrichment_tasks, data, dataset, node_name, run_in_background, user })`. The handler does *not* construct a custom task list — it forwards the strings to `cognee_lib::cognify::memify::resolve_task_specs` which looks up the registered task names and returns concrete `Task` instances.

- **Validation rules**:
  - At least one of `dataset_id` / `dataset_name` must be set (Python rejects with `400` and the literal message above).
  - If `dataset_id` is the empty string `""` (Python's `Literal[""]`), treat it as `None` and fall back to `dataset_name`. The Rust `DatasetIdRef` deserializer (see §4) accepts `null`, `""`, or a UUID.
  - Unknown task names in `extraction_tasks` / `enrichment_tasks` surface as `500` from `cognee_lib::cognify::memify::resolve_task_specs` (Python matches: the inner function raises and the catch-all converts).
  - `data` is treated as raw text; no length cap at the router level (the global body limit applies — default 100 MiB, see [architecture.md §8](../architecture.md#8-middleware-stack)).

- **Permission gate**: `write` permission on the target dataset via `state.lib.permissions().user_can(user.id, dataset_id, "write")` (see [../tenants.md §9](../tenants.md#9-repository-surface)). Memify mutates the graph and the vector index. The check is enforced inside `cognee_lib::cognify::memify::memify` via the internal `resolve_authorized_user_datasets` helper (which calls the same `PermissionsRepository::user_can` underneath).

- **Rate / size limits**: standard JSON body limit (1 MiB by default for non-multipart; bump to 100 MiB if `data` blobs become large — open question).

- **OpenAPI**:
  - `tags = ["memify"]`.
  - `security = [{BearerAuth: []}, {ApiKeyAuth: []}]`.
  - Documented responses: 200, 400, 403, 422, 500.

- **Telemetry**:
  - Span name: `cognee.api.memify.post`.
  - Attributes: `user.id`, `dataset.ref` (UUID or name), `extraction_tasks.count`, `enrichment_tasks.count`, `data.bytes`, `node_name.count`, `run_in_background`.
  - Sub-spans inherited from `cognee_memify` (per-task `cognee.memify.task.<name>`).
  - Emits the Python-equivalent telemetry event `"Memify API Endpoint Invoked"` via `send_telemetry` once per request ([Python lines 86–90](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/memify/routers/get_memify_router.py#L86-L90)).

- **Python parity notes**:
  - Python passes `data: Optional[str] = ""` and downstream treats `""` as "no data, use the existing graph". The Rust handler must preserve this — turn `Some("")` into `None` before delegating, **or** pass through as-is and rely on `cognee_lib::memify` to apply the same fallback. Pick the latter for parity; do not silently coerce.
  - Python's `dataset` argument is `payload.dataset_id if payload.dataset_id else payload.dataset_name` ([line 107](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/memify/routers/get_memify_router.py#L107)). The empty-string UUID is *not* present here because the validation step above coerces it to `None`. Mirror that.
  - Python's `cognee_memify` returns a single `PipelineRunInfo`, not a dict. Do **not** wrap it (open question §6).
  - The 500 `detail` message uses `getattr(memify_run, "error", None) or str(memify_run)`. Reproduce: prefer `error` field, fall back to `Display`.

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
use uuid::Uuid;

use crate::dto::pipeline_run::PipelineRunInfoDTO;
use crate::dto::util::DatasetIdRef;

/// Mirrors Python's `MemifyPayloadDTO`.
/// Source: cognee/api/v1/memify/routers/get_memify_router.py:22-33
#[derive(Debug, Clone, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct MemifyPayloadDTO {
    /// Names of registered Cognee tasks for the extraction phase.
    /// `None` selects `get_default_memify_extraction_tasks()`.
    #[serde(default)]
    pub extraction_tasks: Option<Vec<String>>,

    /// Names of registered Cognee tasks for the enrichment phase.
    /// `None` selects `get_default_memify_enrichment_tasks()`.
    #[serde(default)]
    pub enrichment_tasks: Option<Vec<String>>,

    /// Optional caller-supplied input to feed the first extraction task.
    /// `Some("")` means "no data — use the existing graph".
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
}

/// Helper for Python's `Union[UUID, Literal[""], None]`.
/// Source: shared deserializer in crates/http-server/src/dto/util.rs.
#[derive(Debug, Clone, ToSchema)]
pub struct DatasetIdRef(pub Option<Uuid>);

impl<'de> Deserialize<'de> for DatasetIdRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let opt: Option<String> = Option::deserialize(deserializer)?;
        match opt.as_deref() {
            None | Some("") => Ok(DatasetIdRef(None)),
            Some(s) => Uuid::parse_str(s)
                .map(|u| DatasetIdRef(Some(u)))
                .map_err(serde::de::Error::custom),
        }
    }
}

impl Serialize for DatasetIdRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            Some(u) => serializer.serialize_str(&u.to_string()),
            None => serializer.serialize_none(),
        }
    }
}

/// Response body — single `PipelineRunInfoDTO`, not a dict.
/// Source: cognee/modules/memify/memify.py returns one info, not a Dict[str, info].
pub type MemifyResponseDTO = PipelineRunInfoDTO;
```

`PipelineRunInfoDTO` lives in `crates/http-server/src/dto/pipeline_run.rs` — see [cognify.md §4](cognify.md#4-dto-definitions) for its definition.

## 5. Implementation tasks

1. Add DTOs in `crates/http-server/src/dto/memify.rs` (`MemifyPayloadDTO`).
2. If not already in place, add `crates/http-server/src/dto/util.rs::DatasetIdRef` and re-export it from `dto::mod`.
3. Add the handler `post_memify` in `crates/http-server/src/routers/memify.rs`:
   - Validate at least one of `dataset_id` / `dataset_name`.
   - Resolve the dataset (via the existing `resolve_authorized_user_datasets` helper used internally by `cognee_lib::cognify::memify::memify` — the public surface for direct dataset resolution is to-be-added in P2 alongside `routers/datasets.md`).
   - Call the HTTP-side dispatcher (`crates/http-server/src/pipelines/dispatch.rs`) which builds a `RunSpec` and invokes `state.pipelines.register_inline` or `register_background` against the `cognee_core::PipelineRunRegistry`. The `work` future is `cognee_lib::cognify::memify::memify(...)` (sync — no `run_in_background` parameter at the library level).
   - On `PipelineRunErrored`, return `500` with `error="Pipeline run errored"` and the run's `error` field as `detail`.
4. Wire the router into `build_router`:

   ```rust
   .nest("/memify", memify::router())
   ```

5. Add OpenAPI annotations: `#[utoipa::path(post, path = "/api/v1/memify", tag = "memify", ...)]`.
6. Add unit tests in the same file:
   - Empty `dataset_id` AND empty `dataset_name` → 400.
   - `dataset_id="" `, `dataset_name="foo"` → resolves by name.
   - Unknown task name in `extraction_tasks` → 500.
   - Mocked `cognee_lib::memify` returning `PipelineRunErrored` → 500 with the inner error in `detail`.
   - Mocked `cognee_lib::memify` returning success → 200 with the body shape from §2.1.
7. Add integration tests in `crates/http-server/tests/test_memify.rs`:
   - End-to-end blocking memify on a tmpfs workspace with a real graph (gated behind `OPENAI_URL`).
   - Background dispatch returns immediately with `status="PipelineRunStarted"`.
   - 403 path: a second user without `write` permission on the dataset gets rejected.
8. Add cross-SDK parity tests in `e2e-cross-sdk/harness/test_http_memify.py`:
   - Same payload, diff response JSON shapes between Python and Rust (excluding free-form error text).
   - Same `pipeline_run_id` for the same `(user, dataset)` pair across both SDKs.

## 6. Open questions

1. **Wrap response in a dict?** Python returns the raw `PipelineRunInfo` (single-dataset memify). `/cognify` returns a `Dict[str, PipelineRunInfo]`. The shapes diverge by accident in Python. Should the Rust `/memify` return a dict for consistency with `/cognify`? Proposal: **no** — keep Python parity; document the divergence.

2. **`data` size cap**: Python applies no per-route cap on `data` other than the global body limit. Rust matches — only the global middleware cap ([../architecture.md §8](../architecture.md#8-middleware-stack)) applies. No `tower_http::limit::RequestBodyLimitLayer` on this route.

3. **Task name resolution**: Python registers tasks by string name lazily. Rust must keep an explicit registry — proposal in [cognify.md §3](cognify.md#3-cross-cutting-behavior). What's the source of truth for the registry, and how do we keep it in sync with Python's? Tracked in [observability.md open questions](../observability.md#11-open-questions).

4. **Deterministic `pipeline_run_id` collisions across cognify/memify**: `pipeline_id = uuid5(NAMESPACE_OID, "{user}{pipeline_name}{dataset}")` — the `pipeline_name` differentiates cognify and memify, so no collision. Confirmed; flag for tests.

5. **Should memify accept `dataset_ids: List[UUID]` like cognify?** Currently single dataset only — Python is the same. If Rust users want multi-dataset memify, we'd add an array overload. Proposal: defer; track as a Phase 5 feature request.

## 7. References

- Python router: [`cognee/api/v1/memify/routers/get_memify_router.py`](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/memify/routers/get_memify_router.py).
- Python core function: [`cognee/modules/memify/memify.py`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/memify/memify.py).
- Default tasks: [`cognee/memify_pipelines/memify_default_tasks.py`](https://github.com/topoteretes/cognee/blob/main/cognee/memify_pipelines/memify_default_tasks.py).
- Pipeline registry & event channel: [pipelines.md](../pipelines.md).
- Auth extractors: [auth.md §5](../auth.md#2-three-auth-mechanisms--precedence-and-resolution).
- Tenant resolution: [tenants.md §5](../tenants.md#5-permission-resolution).
- Rust memify pipeline: `crates/cognify/src/memify/` (see project [CLAUDE.md](../../../.claude/CLAUDE.md) for crate map).
