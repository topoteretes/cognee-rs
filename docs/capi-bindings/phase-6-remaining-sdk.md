# Phase 6 — Remaining SDK: memory, data, datasets, sessions, admin

← [Index](README.md) · [Status](STATUS.md)

**Outcome:** full TS parity on the long tail. References:
`js/cognee-neon/src/{sdk_memory,sdk_data,sdk_datasets,sdk_admin}.rs` and the option/result
shapes in `js/src/types.ts`. All functions are async-only per the Phase-2 conventions (D4);
all wire shapes are byte-identical to TS (D3) with strict-JSON scalar results (D9).

## Prerequisites

Phases 1–4 (data ops need add; memory ops need cognify for meaningful Tier-B tests).

## A. Memory ops (`sdk_memory.rs`)

| Function | Inputs | Result |
|---|---|---|
| `cg_sdk_remember` | `inputs_json` (DataInput or array), `dataset_name`, `opts_json` `{sessionId?, selfImprovement?, tenant?}` | `CogneeRememberResult` (pass-through) |
| `cg_sdk_remember_entry` | `entry_json` (`{type:"qa"|"trace"|"feedback", …}`), `dataset_name`, `session_id`, `opts_json` `{tenant?}` | `CogneeRememberResult` |
| `cg_sdk_memify` | `opts_json` `{tripletBatchSize?, nodeTypeFilter?, nodeNameFilter?[], nodeNameFilterOperator?}` | `{tripletCount, indexedCount, batchCount, alreadyCompleted, priorPipelineRunId}` (hand-built, TS decision) |
| `cg_sdk_improve` | `opts_json` `{datasetName!, sessionIds?[], nodeName?[], feedbackAlpha?, tenant?}` | `{stagesRun[], memifyResult, feedbackEntriesProcessed, feedbackEntriesApplied, sessionsPersisted, edgesSynced}` |

`MemifyConfig` extraction/enrichment tasks are **not exposed in v1** — but, correcting the
claim inherited from the TS plan: unlike Neon closures, they ARE expressible in C.
`MemifyTask` is a JSON-array-in → JSON-array-out callback
(`crates/cognify/src/memify/config.rs`), which maps directly onto the established engine
pattern (function pointer + `user_data` + destructor, cf. `cg_task_sync`). Excluded here
because it exceeds TS parity, not because the ABI can't carry it. Reserved post-parity
extension: `CgMemifyTaskFn` + `cg_sdk_memify_with_tasks(...)` — record in the header as
"reserved for a future version" and in the STATUS decision log when Phase 6 lands.

## B. Data ops (`sdk_data.rs`)

| Function | Inputs | Result |
|---|---|---|
| `cg_sdk_forget` | `target_json` (`{kind:"item",dataId,dataset:{name|id}} \| {kind:"dataset",dataset:{…}} \| {kind:"all"}`), `opts_json` `{tenant?}` | `{target, deleteResult}` |
| `cg_sdk_update` | `data_id`, `new_data_json`, `dataset_name`, `opts_json` | `{deletedDataId, deleteResult, newData[], cognifyResult}` |
| `cg_sdk_prune_data` | — | `"null"` |
| `cg_sdk_prune_system` | `opts_json` `{pruneGraph?, pruneVector?, pruneMetadata?, pruneCache?}` | `{dataPruned, graphPruned, vectorPruned, metadataPruned, cachePruned}` |

## C. Datasets (`sdk_datasets.rs`)

| Function | Inputs | Result |
|---|---|---|
| `cg_sdk_list_datasets` | — | `CogneeDataset[]` |
| `cg_sdk_list_data` | `dataset_id` | `CogneeData[]` |
| `cg_sdk_has_data` | `dataset_id` | `true`/`false` (strict JSON, D9) |
| `cg_sdk_dataset_status` | `dataset_ids_json` (string array) | `{<id>: <status>}` |
| `cg_sdk_empty_dataset` | `dataset_id` | `CogneeDeleteResult` |
| `cg_sdk_delete_data` | `dataset_id`, `data_id`, `opts_json` `{softDelete?, deleteDatasetIfEmpty?}` | `CogneeDeleteResult` |
| `cg_sdk_delete_all_datasets` | — | `CogneeDeleteResult[]` |

## D. Sessions + admin (`sdk_admin.rs`)

| Function | Inputs | Result |
|---|---|---|
| `cg_sdk_get_session` | `session_id`, `opts_json` `{lastN?}` | `CogneeSessionQAEntry[]` |
| `cg_sdk_add_feedback` | `session_id`, `qa_id`, `opts_json` `{feedbackText?, feedbackScore?}` | `true`/`false` (strict JSON) |
| `cg_sdk_delete_feedback` | `session_id`, `qa_id`, `opts_json` | `true`/`false` (strict JSON) |
| `cg_sdk_get_graph_context` | `session_id`, `opts_json` | string or `null` |
| `cg_sdk_set_graph_context` | `session_id`, `context`, `opts_json` | `"null"` |
| `cg_sdk_reset_pipeline_run_status` | (match neon signature) | per neon |
| `cg_sdk_reset_dataset_pipeline_run_status` | (match neon signature) | per neon |
| `cg_sdk_get_or_create_default_user` | — | `CogneeUser` |
| `cg_sdk_list_notebooks` | — | `CogneeNotebook[]` |
| `cg_sdk_create_notebook` | (match neon signature) | `CogneeNotebook` |
| `cg_sdk_update_notebook` | (match neon signature) | `CogneeNotebook` |
| `cg_sdk_delete_notebook` | (match neon signature) | per neon |

For rows marked "match neon signature": read the authoritative parameter lists from
`js/cognee-neon/src/sdk_admin.rs` at implementation time (they were Phase-5 additions on the
TS side and are the single source of truth).

## Tasks

1. Four capi modules mirroring the four neon modules; every function = parse JSON → facade →
   shared result-JSON builder from `cognee_bindings_common::wire`.
2. Tier-A smoke `capi/examples/sdk_data_smoke.c`: add (text) → list_datasets → has_data →
   dataset_status → delete_data → forget(all) → prune, all against mock embedding via the
   waiter; assert on stable JSON keys.
3. Tier-B additions to the flagship example: remember + memify after cognify.

## Exit criteria

- [ ] all functions in tables A–D exported (async, Phase-2 conventions)
- [ ] Tier-A data-ops smoke green in check.sh
- [ ] Tier-B memory-ops path verified (gated in capi-check per D12)
- [ ] `cognee_sdk.h` regenerated

## Risks

- **Sheer surface** (~25 ops): keep each wrapper mechanical; any logic beyond
  parse/call/serialize belongs in `cognee-bindings-common`, not in capi.
