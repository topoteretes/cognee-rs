# Phase 4 — Core ops: add / cognify / add_and_cognify

← [Index](README.md) · [Status](STATUS.md)

**Outcome:** a C program can build a knowledge graph: `add` (text/file/binary, dedup,
dataset creation) and `cognify`. Reference: `js/cognee-neon/src/sdk_ops.rs` +
`js/src/types.ts` (`CogneeDataInput`, `CogneeAddOptions`, `CogneeCognifyOptions`,
`CogneeAddResult`, `CogneeCognifyResult`).

## Prerequisites

Phases 1–3.

## Exported functions

Async-only (D4), Phase-2 conventions:

| Function | Inputs | Result JSON |
|---|---|---|
| `cg_sdk_add` | `inputs_json` (a `CogneeDataInput` object **or array**), `dataset_name`, `opts_json` (`{"tenant"?}`) | `CogneeAddResult`: `{datasetName, added[], addedCount, deduplicated[], deduplicatedCount}` |
| `cg_sdk_cognify` | `dataset_name`, `opts_json` (`{tenant?, chunkSize?, chunkOverlap?, summarization?, temporalCognify?, triplet?}`) | `CogneeCognifyResult`: `{chunks, entities, edges, summaries, embeddings, alreadyCompleted, priorPipelineRunId}` |
| `cg_sdk_add_and_cognify` | union of the above | `{add: CogneeAddResult, cognify: CogneeCognifyResult}` |

Shape (uniform):

```c
void cg_sdk_add(const CgSdk* sdk,
                const char* inputs_json,
                const char* dataset_name,
                const char* opts_json,        /* NULL ok */
                CgSdkResultCallback cb,
                void* user_data);             /* sync usage: CgSdkWaiter */
```

## Wire shapes (inherit TS decisions verbatim)

- `CogneeDataInput` discriminated union — `type` drives the match:
  `{"type":"text","text":…}` · `{"type":"file","path":…}` · `{"type":"url","url":…}` ·
  `{"type":"binary","bytes":…,"name":…}`. For C, `binary.bytes` is a **base64 string** or a
  JSON number array (the TS Buffer arm doesn't apply). `s3` and recursive `dataItem` →
  `CG_ERR_UNSUPPORTED`.
- `add()` partitions added-vs-deduplicated via the pre-scan/post-scan delta (TS decision,
  2026-06-04). **`add_result_json` / `partition_added` / `existing_data_ids` / `resolve_dataset` /
  `best_effort_user_email` / `opts_tenant` / `cognify_config_with_opts` were NOT hoisted into
  `bindings-common`** — port them verbatim from `js/cognee-neon/src/sdk_ops.rs` into
  `capi/cognee-capi/src/sdk_ops.rs`. Only `cognify_result_json` and `marshal_inputs` /
  `marshal_one` / `marshal_bytes` are shared via `cognee_bindings_common::wire`.
- `CognifyResult` is hand-built JSON from pipeline output fields (not derived `Serialize`).

## Tasks

1. Create `capi/cognee-capi/src/sdk_ops.rs` — three async C-exported functions
   (`cg_sdk_add`, `cg_sdk_cognify`, `cg_sdk_add_and_cognify`), each using `spawn_sdk_op`
   (defined in `capi/cognee-capi/src/sdk.rs`) to funnel through the shared facade:
   `state.services().await?` → `svc.add_pipeline.add(...)` and the 15-arg `cognify(...)`
   free function (`cognee_lib::cognify::cognify`). Copy the exact call sequence — including
   helper functions `add_result_json`, `partition_added`, `existing_data_ids`,
   `resolve_dataset`, `best_effort_user_email`, `opts_tenant`, `cognify_config_with_opts` —
   verbatim from `js/cognee-neon/src/sdk_ops.rs`. Required imports:
   `cognee_lib::cognify::cognify`, `cognee_lib::database::{UserDb, ops}`,
   `cognee_lib::models::{Data, Dataset}`.
   Wire the new module into `capi/cognee-capi/src/lib.rs`.
   Remove the `#[allow(dead_code)]` attribute from `spawn_sdk_op` in `sdk.rs` (no longer
   unused once `sdk_ops.rs` calls it).
2. Input marshalling: reuse `cognee_bindings_common::wire` for `marshal_inputs` / `marshal_one`
   (already hoisted in Phase 1); `cognify_result_json` is also shared there.
   The add-specific helpers (`add_result_json` etc.) are ported in task 1 above, not hoisted.
3. Bump `cg_api_version()` minor from 2 to 3 in `capi/cognee-capi/src/sdk.rs` to reflect the
   new Phase 4 symbols.
4. Deterministic example `capi/examples/example_sdk_add.c` (Tier-A, no LLM): temp dirs
   configured via `cg_sdk_new(settings_json)` with `embedding_provider=mock` and
   `MOCK_EMBEDDING=true` env var, add two texts (one duplicate) via the waiter, assert counts
   via simple JSON substring checks (`strstr`; no JSON lib dependency in examples; keep
   assertions on stable keys like `"addedCount"`, `"deduplicatedCount"`).
   Add the target to `capi/examples/CMakeLists.txt` and run it in `capi/scripts/check.sh`.
5. Gated live example `capi/examples/example_sdk_add_cognify.c` (Tier-B): use `getenv()` to
   check for `OPENAI_URL` and `OPENAI_TOKEN`; if either is absent print `"SKIP: ..."` to
   stdout and `exit(0)`. (The Rust integration tests panic via `require_env()` which causes
   the test harness to skip; C examples use the `getenv + print + exit(0)` pattern instead.)
   Add the target to `CMakeLists.txt` and add a gated section to `check.sh` that runs it
   only when both vars are present.

## Exit criteria

- [ ] `capi/cognee-capi/src/sdk_ops.rs` created and wired into `lib.rs`
- [ ] add with text + file + binary inputs; dedup verified; dataset auto-created
- [ ] cognify + add_and_cognify
- [ ] `cg_api_version()` minor bumped to 3
- [ ] `example_sdk_add` green in check.sh without credentials
- [ ] live `add → cognify` verified (Tier-B, gated in capi-check per D12, skips cleanly)
- [ ] `cognee_sdk.h` regenerated

## Risks

- **Long-running ops**: cognify can run minutes; document that `cg_sdk_waiter_wait` holds the
  calling thread for the duration — UI/host loops (Android) should use the callback directly.
- **Path inputs**: relative paths resolve against the process CWD — document; recommend
  absolute paths from C.
