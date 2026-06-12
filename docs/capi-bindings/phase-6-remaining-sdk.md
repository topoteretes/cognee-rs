# Phase 6 — Remaining SDK: memory, data, datasets, sessions, admin

← [Index](README.md) · [Status](STATUS.md)

**Outcome:** full TS parity on the long tail. References:
`js/cognee-neon/src/{sdk_memory,sdk_data,sdk_datasets,sdk_admin}.rs` and the option/result
shapes in `js/src/types.ts`. All functions are async-only per the Phase-2 conventions (D4);
all wire shapes are byte-identical to TS (D3) with strict-JSON scalar results (D9).

## Prerequisites

Phases 1–5 (data ops need add; memory ops need cognify+search for meaningful Tier-B tests;
retrieval smoke established in Phase 5 is the baseline for the Tier-B flagship extension).

## A. Memory ops (`sdk_memory.rs`)

All four are new capi modules; the neon implementations in
`js/cognee-neon/src/sdk_memory.rs` are the single source of truth.

| Function | C parameters | Result |
|---|---|---|
| `cg_sdk_remember` | `sdk`, `inputs_json` (DataInput obj or array), `dataset_name`, `opts_json` `{sessionId?, selfImprovement?, tenant?}` | `CogneeRememberResult` — `RememberResult` derives `Serialize`, pass through `serde_json::to_string` (the `cognify_result`/`memify_result` fields carry `#[serde(skip)]`) |
| `cg_sdk_remember_entry` | `sdk`, `entry_json` (`{type:"qa"\|"trace"\|"feedback", …}`), `dataset_name`, `session_id`, `opts_json` `{tenant?}` | `CogneeRememberResult` — same serialisation as above |
| `cg_sdk_memify` | `sdk`, `opts_json` `{tripletBatchSize?, nodeTypeFilter?, nodeNameFilter?[], nodeNameFilterOperator?}` | `{tripletCount, indexedCount, batchCount, alreadyCompleted, priorPipelineRunId}` — `MemifyResult` does NOT derive `Serialize`; hand-build via `memify_result_json` helper (copy from neon, lives in this module) |
| `cg_sdk_improve` | `sdk`, `opts_json` `{datasetName!, sessionIds?[], nodeName?[], feedbackAlpha?, tenant?}` | `{stagesRun[], memifyResult, feedbackEntriesProcessed, feedbackEntriesApplied, sessionsPersisted, edgesSynced}` — `ImproveResult` does NOT derive `Serialize`; hand-built JSON (copy from neon) |

**Serde notes for implementors** (from `js/cognee-neon/src/sdk_memory.rs` doc comment):
- `RememberResult` derives `Serialize` → direct `serde_json::to_string`.
- `MemifyResult` and `ImproveResult` do NOT derive `Serialize` → hand-built JSON helpers.
  Inline the `memify_result_json` helper in `sdk_memory.rs` (not in `bindings-common`; the
  neon module also keeps it local).
- `ImproveResult.memify_result` is `Option<MemifyResult>` — serialize with `memify_result_json`
  or `serde_json::Value::Null` when absent.
- Lib call for improve uses `ImproveParams` struct: `cognee_lib::api::improve(ImproveParams{…})`.
  Fields `extraction_tasks` and `enrichment_tasks` are always `None` in v1 (R3).
- Lib call for memify goes through `cognee_lib::cognify::run_memify(…)` (not an
  `ImproveParams` path).  Requires constructing `SeaOrmPipelineRunRepository` inline.

`MemifyConfig` extraction/enrichment tasks are **not exposed in v1** — but, correcting the
claim inherited from the TS plan: unlike Neon closures, they ARE expressible in C.
`MemifyTask` is a JSON-array-in → JSON-array-out callback
(`crates/cognify/src/memify/config.rs`), which maps directly onto the established engine
pattern (function pointer + `user_data` + destructor, cf. `cg_task_sync`). Excluded here
because it exceeds TS parity, not because the ABI can't carry it. Reserved post-parity
extension: `CgMemifyTaskFn` + `cg_sdk_memify_with_tasks(...)` — record in the header as
"reserved for a future version" and in the STATUS decision log when Phase 6 lands.

## B. Data ops (`sdk_data.rs`)

Reference implementation: `js/cognee-neon/src/sdk_data.rs`.

| Function | C parameters | Result |
|---|---|---|
| `cg_sdk_forget` | `sdk`, `target_json` (`{kind:"item",dataId,dataset:{name\|id}}\|{kind:"dataset",dataset:{…}}\|{kind:"all"}`), `opts_json` `{tenant?}` | `{target, deleteResult}` — `ForgetResult` not `Serialize`; hand-built; `deleteResult` uses `serde_json::to_value(&result.delete_result)` |
| `cg_sdk_update` | `sdk`, `data_id` (C string, not JSON), `new_data_json` (DataInput or array), `dataset_name`, `opts_json` `{tenant?}` | `{deletedDataId, deleteResult, newData[], cognifyResult}` — `UpdateResult` not `Serialize`; hand-built; `cognifyResult` via `cognify_result_json` from `bindings_common::wire` |
| `cg_sdk_prune_data` | `sdk` | `"null"` (strict JSON, D9) — neon returns `undefined`; the C tier returns the JSON null string per D9 |
| `cg_sdk_prune_system` | `sdk`, `opts_json` `{pruneGraph?, pruneVector?, pruneMetadata?, pruneCache?}` | `{dataPruned, graphPruned, vectorPruned, metadataPruned, cachePruned}` — `PruneResult` not `Serialize`; hand-built JSON |

**Implementation notes:**
- `cg_sdk_forget`: `opts_json` is accepted but ignored (same as neon: `let _ = opts`; reserved
  for future `tenant` support).
- `cg_sdk_update`: `data_id` arrives as a C string (not a JSON string), parsed with
  `parse_c_str_or_fire`.  `new_data_json` is parsed from C string → `serde_json::Value` →
  `marshal_inputs` (same path as `cg_sdk_add`).
- `cg_sdk_prune_data`: returns `Ok(serde_json::Value::Null)` to `spawn_sdk_op`, which
  serialises it to the `"null"` string (D9).  No opts parameter.
- `cg_sdk_prune_system`: uses `PruneTarget::default_system()` for defaults, then overrides
  from opts (matches neon exactly).  Lib call: `prune_system(&target, Some(graph), Some(vector),
  Some(session_store))`.
- `opts_tenant` helper: copy from `sdk_ops.rs` — it is NOT in `bindings-common` (same pattern
  as neon, where each `sdk_data.rs`/`sdk_memory.rs` defines its own local copy).

## C. Datasets (`sdk_datasets.rs`)

Reference implementation: `js/cognee-neon/src/sdk_datasets.rs`.

| Function | C parameters | Result |
|---|---|---|
| `cg_sdk_list_datasets` | `sdk` | `Dataset[]` — `Dataset` IS `Serialize`; direct `serde_json::to_string` |
| `cg_sdk_list_data` | `sdk`, `dataset_id` (C string UUID) | `Data[]` — `Data` IS `Serialize`; direct |
| `cg_sdk_has_data` | `sdk`, `dataset_id` (C string UUID) | `true`/`false` (strict JSON bool, D9) — `serde_json::Value::Bool(b)` |
| `cg_sdk_dataset_status` | `sdk`, `dataset_ids_json` (JSON array of UUID strings) | `{<uuid-str>: <status-str>}` — `HashMap<Uuid, PipelineRunStatus>` has non-string keys; convert to `HashMap<String, _>` before serialising (same as neon) |
| `cg_sdk_empty_dataset` | `sdk`, `dataset_id` (C string UUID) | `DeleteResult` — IS `Serialize`; direct |
| `cg_sdk_delete_data` | `sdk`, `dataset_id` (C string UUID), `data_id` (C string UUID), `opts_json` `{softDelete?, deleteDatasetIfEmpty?}` | `DeleteResult` — IS `Serialize`; direct |
| `cg_sdk_delete_all_datasets` | `sdk` | `DeleteResult[]` — IS `Serialize`; direct |

**Implementation notes:**
- All `DatasetManager` ops require `Arc<dyn DatasetDb>`: cast with
  `Arc::clone(&svc.database) as Arc<dyn DatasetDb>`.
- `cg_sdk_has_data` returns `serde_json::Value::Bool(b)` into `spawn_sdk_op`; `spawn_sdk_op`
  serialises it to `"true"` or `"false"` (D9).
- `cg_sdk_dataset_status`: `dataset_ids_json` comes as a C string → parse with
  `parse_c_str_or_fire` then `serde_json::from_str` → validate UUID array.
- `cg_sdk_delete_data`: `softDelete` maps to `DeleteMode::Soft`, otherwise `DeleteMode::Hard`.

## D. Sessions + admin (`sdk_admin.rs`)

Reference implementation: `js/cognee-neon/src/sdk_admin.rs`.

| Function | C parameters | Result |
|---|---|---|
| `cg_sdk_get_session` | `sdk`, `session_id` (C string), `opts_json` `{lastN?}` | `SessionQAEntry[]` — derives `Serialize`; direct |
| `cg_sdk_add_feedback` | `sdk`, `session_id` (C string), `qa_id` (C string), `opts_json` `{feedbackText?, feedbackScore?}` | `true`/`false` (strict JSON bool, D9) |
| `cg_sdk_delete_feedback` | `sdk`, `session_id` (C string), `qa_id` (C string) | `true`/`false` (strict JSON bool, D9) — no opts in neon; omit `opts_json` param |
| `cg_sdk_get_graph_context` | `sdk`, `session_id` (C string) | quoted JSON string or `"null"` (D9) — neon returns `string\|null`; C equivalent: `"\"<ctx>\""` or `"null"` |
| `cg_sdk_set_graph_context` | `sdk`, `session_id` (C string), `context` (C string) | `"null"` (D9) — no opts in neon |
| `cg_sdk_reset_pipeline_run_status` | `sdk`, `dataset_id` (C string UUID), `pipeline_name` (C string) | `"null"` (D9) |
| `cg_sdk_reset_dataset_pipeline_run_status` | `sdk`, `dataset_id` (C string UUID) | `"null"` (D9) |
| `cg_sdk_get_or_create_default_user` | `sdk` | `User` JSON — `User` derives `Serialize`; direct |
| `cg_sdk_list_notebooks` | `sdk` | `Notebook[]` — derives `Serialize`; direct |
| `cg_sdk_create_notebook` | `sdk`, `name` (C string), `cells_json` (may be NULL → empty array), `deletable` (int, 0=false) | `Notebook` — derives `Serialize`; direct |
| `cg_sdk_update_notebook` | `sdk`, `id` (C string UUID), `patch_json` `{name?, cells?}` | `Notebook` JSON or `"null"` (D9) when not found |
| `cg_sdk_delete_notebook` | `sdk`, `id` (C string UUID) | `true`/`false` (strict JSON bool, D9) |

**Implementation notes (extracted from neon — do not re-derive at impl time):**

- `cg_sdk_add_feedback`: `feedbackText` and `feedbackScore` come from `opts_json` (not separate
  C args). In neon these are positional JS args; in C, fold them into `opts_json` for uniformity
  with other ops. Lib call: `cognee_lib::session::add_feedback(svc.session_manager.as_ref(),
  session_id, qa_id, Some(&owner_str), feedback_text.as_deref(), feedback_score)`.

- `cg_sdk_delete_feedback`: neon has no `opts` arg; C also omits it (no `opts_json` param at
  all). Lib call: `cognee_lib::session::delete_feedback(svc.session_manager.as_ref(), session_id,
  qa_id, Some(&owner_str))`.

- `cg_sdk_get_graph_context`: neon does NOT have an `opts_json` arg; C also omits it. Result is
  `Option<String>` — serialize as `"\"<ctx>\""` (D9 quoted string) or `"null"` (D9 null). Do
  NOT return a bare unquoted string.

- `cg_sdk_set_graph_context`: neon does NOT have an `opts_json` arg; C also omits it. Returns
  `Ok(serde_json::Value::Null)` → `"null"`.

- `cg_sdk_reset_pipeline_run_status` / `cg_sdk_reset_dataset_pipeline_run_status`: both return
  `()` in neon (JS `undefined`); C returns `"null"`. Lib: `reset_pipeline_run_status(
  Arc::clone(&svc.pipeline_run_repo), owner_id, dataset_id, pipeline_name)` and
  `reset_dataset_pipeline_run_status(Arc::clone(&svc.pipeline_run_repo), owner_id, dataset_id)`.

- `cg_sdk_get_or_create_default_user`: reads `state.cm.settings().default_user_email` for the
  email arg (same as neon). Cast: `Arc::clone(&svc.database).as_ref() as &dyn UserDb`.

- `cg_sdk_create_notebook`: `cells_json` NULL → parse as empty JSON array `[]`. `deletable`
  arrives as a C `int` (non-zero = true). Lib: `create_notebook(&nb_db, owner_id, name, cells,
  deletable)`.

- `cg_sdk_update_notebook`: result `None` → `"null"`, `Some(nb)` → serialised notebook.

- `cg_sdk_delete_notebook`: returns `bool` → `serde_json::Value::Bool(b)` → `"true"`/`"false"`.

## Tasks

### Task 1 — Four capi modules

Create four new files in `capi/cognee-capi/src/`:
- `sdk_memory.rs` — `cg_sdk_remember`, `cg_sdk_remember_entry`, `cg_sdk_memify`, `cg_sdk_improve`
- `sdk_data.rs` — `cg_sdk_forget`, `cg_sdk_update`, `cg_sdk_prune_data`, `cg_sdk_prune_system`
- `sdk_datasets.rs` — 7 dataset ops
- `sdk_admin.rs` — 12 session/pipeline/user/notebook ops

Register all four as `pub mod sdk_memory; pub mod sdk_data; pub mod sdk_datasets; pub mod sdk_admin;`
in `capi/cognee-capi/src/lib.rs`.

Every function follows the established pattern from `sdk_ops.rs` / `sdk_retrieval.rs`:
1. Null-check `sdk`; call `set_last_error` + return if null.
2. `Arc::clone(unsafe { &(*sdk).state })`.
3. `parse_c_str_or_fire` for each required C string; check optional strings without firing callback.
4. `spawn_sdk_op(callback, SendUserData(user_data), async move { … })`.
5. Inner async: `state.services().await?`, `state.owner_id().await?`, call lib API, serialise result.

Helpers to copy locally (NOT into `bindings-common` — same decision as neon):
- `opts_tenant` (same as in `sdk_ops.rs`) — copy to each module that needs it.
- `memify_result_json` — local to `sdk_memory.rs`.

Helpers already in `cognee_bindings_common::wire` to reuse:
- `marshal_inputs` (for `cg_sdk_remember` inputs and `cg_sdk_update` new_data)
- `cognify_result_json` (for `cg_sdk_update` cognify result field)

After phase 6, bump `cg_api_version()` minor in `sdk.rs` to 5.

### Task 2 — Tier-A deterministic smoke test

New file `capi/examples/sdk_data_smoke.c`.

Exercises without LLM/live credentials (MOCK_EMBEDDING=true, waiter pattern):
1. `cg_sdk_new` → `cg_sdk_warm`
2. `cg_sdk_add` (text input)
3. `cg_sdk_list_datasets` — assert JSON array contains `"name"` key
4. `cg_sdk_has_data` — assert result is `"true"`
5. `cg_sdk_dataset_status` — assert JSON object with UUID key
6. `cg_sdk_delete_data` — assert JSON with `"deletedCount"` or similar
7. `cg_sdk_forget` with `{kind:"all"}` — assert JSON with `"target"` key
8. `cg_sdk_prune_data` — assert result is `"null"`
9. `cg_sdk_prune_system` — assert JSON with `"graphPruned"` key

Add to `check.sh` under a new section header `=== Phase 6 data-ops smoke test (Tier-A) ===`
with `MOCK_EMBEDDING=true`.

Also add `sdk_data_smoke` to `CMakeLists.txt` (follow the pattern of `sdk_retrieval_smoke`).

### Task 3 — Tier-B flagship extension

Extend `capi/examples/example_sdk_add_cognify_search.c` with two extra steps after the
existing cognify+search:
4. `cg_sdk_memify` — assert result JSON has `"tripletCount"` key.
5. `cg_sdk_remember` (with same text input, dataset name) — assert result JSON has expected keys.

The existing SKIP guard (`OPENAI_URL` / `OPENAI_TOKEN` absent) covers these additions
automatically (they are inside the same conditionally-gated block).

## Exit criteria

- [ ] all functions in tables A–D exported (async, Phase-2 conventions)
- [ ] `capi/cognee-capi/src/lib.rs` has `pub mod` entries for all four new modules
- [ ] `cargo check --all-targets` clean in the capi workspace (default + slim)
- [ ] Tier-A data-ops smoke (`sdk_data_smoke.c`) green in `check.sh` with `MOCK_EMBEDDING=true`
- [ ] Tier-B memory-ops additions (`memify` + `remember`) in the flagship example, gated per D12
- [ ] `cognee_sdk.h` regenerated (cbindgen) — new symbols appear; `cg_api_version` returns 1.5
- [ ] `scripts/check_all.sh` passes

## Risks

- **Sheer surface** (~25 ops, 4 modules): keep each wrapper mechanical; any logic beyond
  parse/call/serialize belongs in `cognee-bindings-common`, not in capi modules.
- **`opts_json` parity divergences**: several neon ops have no `opts_json` arg
  (`cg_sdk_delete_feedback`, `cg_sdk_get_graph_context`, `cg_sdk_set_graph_context`) — the
  corresponding C functions also omit it; do not add a spurious `opts_json` parameter.
- **`cg_sdk_add_feedback` opts consolidation**: neon takes `feedbackText`/`feedbackScore` as
  positional JS args (positions 3 and 4); in C they move into `opts_json` to keep the calling
  convention uniform (single `opts_json` object). This is a deliberate C-ABI convenience
  decision, not a bug.
- **`prune_data` void vs null**: neon resolves to JS `undefined`; C resolves to `"null"` per D9.
  `spawn_sdk_op` handles this automatically when the future returns `Ok(serde_json::Value::Null)`.
- **`cg_sdk_update` `new_data_json` shape**: same discriminated-union as `cg_sdk_add` inputs;
  reuse `marshal_inputs` from `bindings_common::wire`.
