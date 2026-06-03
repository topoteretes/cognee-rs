# Phase 5 — Remaining core SDK (full parity)

← [Index](../typescript-bindings-plan.md)

**Goal:** complete parity for every non-feature-gated `cognee-lib` API function. Surfaces **#5–#16**.
Every function follows the Phase 1 canonical pattern (`svc = handle.services()` → call lib →
`serde_to_js`) — uniform, no ad-hoc wiring.

## Scope & grouping

Group native functions by concern (one Rust file each) for maintainability:

### `sdk_memory.rs` — remember / improve / memify
- `cogneeRemember(handle, data, datasetName, opts?) -> Promise<RememberResult>` → `api::remember`
  (one-call add + cognify + optional improve). `opts`: `sessionId?`, `selfImprovement`,
  `owner`/`tenant`. Passes the full `svc` handle set incl. `session_store`, `session_manager`,
  `checkpoint_store`, `ontology_resolver`, `cognify_config`.
- `cogneeRememberEntry(handle, entry, datasetName, sessionId, opts?)` → `api::remember_entry`.
  `entry` is a JSON union `MemoryEntry`: `Qa` | `Trace` | `Feedback`.
- `cogneeMemify(handle, opts?) -> Promise<MemifyResult>` → `cognify::run_memify` with a
  `MemifyConfig` built from `opts` (optional node-type / node-name filters).
- `cogneeImprove(handle, opts) -> Promise<ImproveResult>` → `api::improve` with `ImproveParams`
  from `opts` (`datasetName`, `sessionIds?`, `nodeName?`, `feedbackAlpha`, …).

### `sdk_data.rs` — forget / delete / update / prune
- `cogneeForget(handle, target) -> Promise<ForgetResult>` → `api::forget`. `target` is the JSON
  union `ForgetTarget`: `{ kind: "item", dataId, dataset }` | `{ kind: "dataset", dataset }` |
  `{ kind: "all" }`; `dataset` is a `DatasetRef` (`{ name }` | `{ id }`).
- `cogneeUpdate(handle, dataId, newData, datasetName, opts?) -> Promise<UpdateResult>` →
  `api::update` (delete → re-add → re-cognify).
- `cogneePrune(handle, opts) -> Promise<PruneResult>` → `api::prune_data` (storage) and/or
  `api::prune_system` (`PruneTarget` flags: graph / vector / cache).

### `sdk_datasets.rs` — DatasetManager (#12)
- `cogneeListDatasets(handle) -> Dataset[]`, `cogneeListData(handle, datasetId) -> Data[]`,
  `cogneeHasData(handle, datasetId) -> bool`, `cogneeDatasetStatus(handle, datasetIds) -> map`,
  `cogneeEmptyDataset(...)`, `cogneeDeleteData(...)`, `cogneeDeleteAllDatasets(...)`.
  Built on a `DatasetManager` instantiated from `svc.database`, plus `svc.delete_service` for
  the destructive ones.

### `sdk_sessions.rs` — sessions (#13)
- `getSession`, `addFeedback`, `deleteFeedback`, `getGraphContext`, `setGraphContext` →
  `lib::session::*`, using `svc.session_store` / `svc.session_manager`.

### `sdk_admin.rs` — pipeline-runs (#14), user (#15), notebooks (#16)
- `resetPipelineRunStatus`, `resetDatasetPipelineRunStatus` → `api::pipeline_runs::*`.
- `getOrCreateDefaultUser` → `api::user::get_or_create_default_user` (also used at Phase 1
  bootstrap; expose for explicit owner management).
- Notebooks `list/create/update/delete` → `api::notebooks::*` — **confirm SDK-vs-HTTP scope
  first** (open question in the index). Include only if SDK-facing.

## Data shapes

Each result type is serde-serializable: `RememberResult`, `MemifyResult`, `ForgetResult`,
`UpdateResult`, `PruneResult`, `ImproveResult`, `Dataset[]`, `Data[]`, the status map,
`SessionQAEntry[]`. Marshal via the Phase 8 `json.rs` helpers. Input unions (`MemoryEntry`,
`ForgetTarget`, `DatasetRef`, `PruneTarget`) get explicit TS types in Phase 7.

## Dependencies & ordering

Needs Phases 1–2. Management ops (datasets, forget, prune, sessions, user) are **Tier-A
testable** (no LLM). Memory ops (remember, memify, improve) need cognified data + LLM → **Tier-B**.

## Risks

- `remember`'s large handle set is exactly why the Phase 1 facade exists — confirm every field is
  populated before this phase.
- Destructive ops (`forget`, `prune`, `delete*`) must scope by `owner_id`; test isolation.

## Done when

- Every `cognee-lib` `api/` function is reachable from Node (checklist rows #5–#16 ticked).
- Tier-A tests cover datasets / forget / prune / sessions; Tier-B covers remember / memify /
  improve.
