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
  2026-06-04) — reuse the shared helper if `wire` was hoisted in Phase 1, else port it.
- `CognifyResult` is hand-built JSON from pipeline output fields (not derived `Serialize`).

## Tasks

1. `capi/cognee-capi/src/sdk_ops.rs` — three async ops, all funneling through the shared
   facade (`state.services()` → `svc.add_pipeline` / cognify entry points — copy the exact
   call sequence from `sdk_ops.rs` in neon).
2. Input marshalling: reuse `cognee_bindings_common::wire` (`marshal_inputs`/`marshal_one`,
   hoisted in Phase 1).
3. Deterministic example `capi/examples/example_sdk_add.c` (Tier-A, no LLM): temp dirs +
   `MOCK_EMBEDDING`, add two texts (one duplicate) via the waiter, assert counts via simple
   JSON substring checks (no JSON lib dependency in examples; keep assertions on stable
   keys).
4. Gated live example `capi/examples/example_sdk_add_cognify.c` (Tier-B): runs only when
   `OPENAI_URL`/`OPENAI_TOKEN` are set, otherwise prints SKIP and exits 0 — same conditional
   skip pattern the Rust integration tests use.

## Exit criteria

- [ ] add with text + file + binary inputs; dedup verified; dataset auto-created
- [ ] cognify + add_and_cognify
- [ ] `example_sdk_add` green in check.sh without credentials
- [ ] live `add → cognify` verified (Tier-B, gated in capi-check per D12, skips cleanly)
- [ ] `cognee_sdk.h` regenerated

## Risks

- **Long-running ops**: cognify can run minutes; document that `cg_sdk_waiter_wait` holds the
  calling thread for the duration — UI/host loops (Android) should use the callback directly.
- **Path inputs**: relative paths resolve against the process CWD — document; recommend
  absolute paths from C.
