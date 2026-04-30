# E-05 — `POST /api/v1/improve`

| | |
|---|---|
| Wire path | `POST /api/v1/improve` |
| Status | **Done (commit 43e2a72)** — DTO extended from 3 to 8 fields (added `extractionTasks`, `enrichmentTasks`, `data`, `nodeName`, `sessionIds` with camelCase wire + snake_case alias per Decision 10). `ImproveParams<'_>` extended with 3 new fields (`extraction_tasks`, `enrichment_tasks`, `data`); all 5 LIB-04 call sites updated. Handler tracing scope adds `session_ids_count`/`extraction_tasks_count`/`enrichment_tasks_count`/`node_name_count`/`has_data` per Python parity at `get_improve_router.py:67-74`. **The no-op handler stub stays in place** — wiring the real `cognee_lib::api::improve::improve(...)` call is the deferred P5 follow-up (cycle constraint plus missing `ComponentHandles` slots). 5 new integration + 4 DTO unit + 4 lib regression tests + cross-SDK harness. **No new wire divergence.** |
| Depends on | LIB-04 (`ImproveParams` struct refactor — Decision 8) — **Done (commit 9f1879e)**. |
| Effort | ~1 day (down from 1.5 — the refactor that would have been bundled in here is now its own task LIB-04). |
| Owner crate | `cognee-http-server` |

## 1. Goal

Bring the Rust `/improve` request DTO to parity with Python's `ImprovePayloadDTO`. The most important addition is **`session_ids`** — without it the HTTP endpoint cannot trigger the v2 session-bridge path (Stages 1, 2, 4 in [`crates/cognify/src/memify/`](../../../crates/cognify/src/memify/)) even though the library code is already in place from commit `646ebbc`.

## 2. Python source-of-truth

| Symbol | File | Lines |
|---|---|---|
| `ImprovePayloadDTO` | `cognee/api/v1/improve/routers/get_improve_router.py` | 21–37 |
| `POST ""` handler | same | 39–~110 |
| `cognee.improve(...)` | `cognee/api/v1/improve/improve.py` | full file |

Full request body:

```json
{
  "extraction_tasks": ["..."],   // optional, currently informational
  "enrichment_tasks": ["..."],   // optional
  "data":             "",        // optional inline text payload
  "dataset_name":     "...",     // optional
  "dataset_id":       "<uuid>" | "",
  "node_name":        ["..."],   // optional graph node filter
  "run_in_background": false,
  "session_ids":      ["s1","s2"] // ←── triggers the v2 session-bridge path
}
```

Behavior reminder (from [`api-v2/improve.md`](../../api-v2/improve.md)): when `session_ids` is non-empty, `improve()` runs feedback weights → session-cognify → memify → graph→session sync. Without it, only the memify enrichment stage runs.

## 3. Current Rust state

- DTO at [`crates/http-server/src/dto/improve.rs:17`](../../../crates/http-server/src/dto/improve.rs#L17) has only `dataset_name`, `dataset_id`, `run_in_background`. `rename_all = "camelCase"` and per-field `serde(alias)` for snake_case input are already in place (CLEAN-01, Decision 10).
- Handler at [`crates/http-server/src/routers/improve.rs:32`](../../../crates/http-server/src/routers/improve.rs#L32) does **NOT** call `cognee_lib::api::improve::improve(...)`. The dispatched work at line 87 is a no-op stub: `box_pipeline_future(async move { Ok::<(), std::io::Error>(()) })`. The line 86 TODO marks this as "wire real improve() call once ComponentHandles gains graph/vector handles".
- **Cycle constraint** (same as E-04): `crates/http-server/Cargo.toml:36-38` documents that `cognee-lib` is intentionally NOT a direct dep, because cognee-lib's `server` feature pulls cognee-http-server. The handler cannot `use cognee_lib::api::improve::*` directly.
- `cognee_lib::api::improve::ImproveParams<'a>` exists ([`crates/lib/src/api/improve.rs:61-101`](../../../crates/lib/src/api/improve.rs#L61-L101)) with the 18 fields LIB-04 specified, **no `Default` derive** (option (b) per LIB-04 §3 step 2). Constructor must spell out every field.
- `ComponentHandles` ([`crates/http-server/src/components.rs:26-82`](../../../crates/http-server/src/components.rs#L26-L82)) currently exposes: `database`, `storage`, `delete_service`, `ontology_manager`, `search_orchestrator`, `llm`, `graph_db`, `permissions`, `sync_ops`, `session_store`, `session_manager`. **Missing for improve**: `vector_db`, `embedding_engine`, `add_pipeline`, `checkpoint_store`, `cognify_config`, `ontology_resolver` (only `ontology_manager` is present). Calling `improve()` from the handler requires extending ComponentHandles or replicating logic inline at the http-server layer.
- Existing tests: [`crates/http-server/tests/test_improve.rs`](../../../crates/http-server/tests/test_improve.rs) (88 LOC, no v2-field coverage), [`crates/http-server/tests/test_improve_420.rs`](../../../crates/http-server/tests/test_improve_420.rs) (110 LOC, 420 quirk only), [`e2e-cross-sdk/harness/test_http_improve.py`](../../../e2e-cross-sdk/harness/test_http_improve.py) (69 LOC, no v2 fields).

## 4. Implementation steps

> **Decision (2026-04-29) — Decision 8**: this task does NOT refactor the `improve()` signature. That work is owned by **LIB-04** (B-4 in the phase order) and lands before this task. E-05 just adds three new fields (`extraction_tasks`, `enrichment_tasks`, `data`) to the `ImproveParams` struct LIB-04 introduces, plus the HTTP DTO and handler wiring. Investigation agent: do not re-litigate.

1. **Extend the DTO** to match Python field-for-field. Wire format is camelCase per Decision 10; snake_case input forms accepted via `serde(alias)`:
   ```rust
   #[derive(Debug, Clone, Default, Deserialize, Serialize, ToSchema)]
   #[serde(rename_all = "camelCase")]
   pub struct ImprovePayloadDTO {
       #[serde(default, alias = "extraction_tasks")]
       pub extraction_tasks: Option<Vec<String>>,           // wire: "extractionTasks"
       #[serde(default, alias = "enrichment_tasks")]
       pub enrichment_tasks: Option<Vec<String>>,           // wire: "enrichmentTasks"
       #[serde(default)]
       pub data: Option<String>,                            // single-word, unaffected
       #[serde(default, alias = "dataset_name")]
       pub dataset_name: Option<String>,                    // wire: "datasetName"
       #[serde(default, alias = "dataset_id")]
       pub dataset_id: super::util::DatasetIdRef,           // wire: "datasetId"
       #[serde(default, alias = "node_name")]
       pub node_name: Option<Vec<String>>,                  // wire: "nodeName"
       #[serde(default, alias = "run_in_background")]
       pub run_in_background: Option<bool>,                 // wire: "runInBackground"
       #[serde(default, alias = "session_ids")]
       pub session_ids: Option<Vec<String>>,                // wire: "sessionIds"
   }
   ```
   The previous bespoke `rename = "datasetId"` is now redundant — `rename_all = "camelCase"` covers it. Drop any per-field rename that's already produced by the top-level rule.

2. **Extend `ImproveParams`** (defined in LIB-04) with the three v2 fields:
   ```rust
   pub struct ImproveParams<'a> {
       ...existing fields from LIB-04...
       pub extraction_tasks: Option<Vec<String>>,
       pub enrichment_tasks: Option<Vec<String>>,
       pub data: Option<String>,
   }
   ```
   These are pure-data fields with `Default::default() == None`; no other call site needs to change.

3. **Update the handler** at [`crates/http-server/src/routers/improve.rs:32`](../../../crates/http-server/src/routers/improve.rs#L32). The handler today dispatches a no-op stub (`box_pipeline_future(async move { Ok::<(), std::io::Error>(()) })` at line 87). E-05's scope **is not** wiring the real `improve()` library call (that's the deferred P5 work flagged by the line 86 TODO and gated on `ComponentHandles` gaining `vector_db` / `embedding_engine` / `add_pipeline` / `checkpoint_store` / `cognify_config`, plus the **`cognee-lib` cycle constraint** at `crates/http-server/Cargo.toml:36-38`). E-05's scope is:
   - Plumb the five new payload fields through `dispatch_pipeline` so they're observable in tracing / telemetry.
   - Confirm the wire DTO matches Python field-for-field.
   - Where possible, exercise stage selection logic via the existing stub path (e.g. log when `session_ids` is non-empty so cross-SDK harness can observe the difference even with the stub).

   ```rust
   let session_ids_count = payload.session_ids.as_ref().map_or(0, |s| s.len());
   tracing::info!(
       session_ids_count,
       extraction_tasks = ?payload.extraction_tasks.as_ref().map(|v| v.len()),
       enrichment_tasks = ?payload.enrichment_tasks.as_ref().map(|v| v.len()),
       node_name_count = ?payload.node_name.as_ref().map(|v| v.len()),
       has_data = payload.data.as_deref().is_some_and(|d| !d.is_empty()),
       "improve payload received"
   );
   // dispatch stub remains; real improve() call is the P5 follow-up
   ```

   When the cycle constraint is resolved (P5: extend `ComponentHandles` with the missing handles, OR follow the E-04 pattern of replicating the orchestration inline at http-server using public exports from `cognee-cognify`), the constructor shape will be:
   ```rust
   let result = some_callable_improve(ImproveParams {
       dataset_name: payload.dataset_name.unwrap_or_default(),
       session_ids: payload.session_ids,
       node_name: payload.node_name,
       owner_id: user.id,
       tenant_id: user.tenant_id,
       feedback_alpha: 0.3,
       llm: components.llm.clone().expect("llm wired"),
       storage: components.storage.clone(),
       graph_db: components.graph_db.clone().expect("graph_db wired"),
       vector_db: /* TODO P5 */,
       embedding_engine: /* TODO P5 */,
       ontology_resolver: /* TODO P5 */,
       db: Some(components.database.clone()),
       session_store: components.session_store.clone(),
       session_manager: components.session_manager.clone(),
       add_pipeline: /* TODO P5 */,
       checkpoint_store: /* TODO P5 */,
       cognify_config: /* TODO P5 */,
       // E-05 NEW v2 fields:
       extraction_tasks: payload.extraction_tasks,
       enrichment_tasks: payload.enrichment_tasks,
       data: payload.data,
   }).await?;
   ```
   LIB-04 chose option (b) (no `Default` derive) — every field must be named explicitly; `..ImproveParams::default()` will not compile.

4. **OpenAPI** — re-derive `ToSchema`; the new fields with `serde(default)` produce `nullable: true` schemas matching Python's `Optional[...]` shape.

5. **Telemetry** — add `cognee.improve.session_ids_count = payload.session_ids.as_ref().map_or(0, |s| s.len())` to the `tracing::instrument` field list, mirroring Python's `send_telemetry({"session_ids_count": ...})`.

## 5. Tests

- Update [`crates/http-server/src/dto/improve.rs`](../../../crates/http-server/src/dto/improve.rs) inline tests with deserialization round-trips for every new field (camelCase + snake_case alias paths; serialization-only-emits-camelCase assertion mirroring the existing pattern at lines 36-86).
- [`crates/http-server/tests/test_improve.rs`](../../../crates/http-server/tests/test_improve.rs):
  - `session_ids_accepted_camelcase` — `{"sessionIds": ["s1","s2"], "datasetName": "ds"}` returns 200; payload reaches the handler.
  - `session_ids_accepted_snake_case_alias` — same with `session_ids`.
  - `extraction_tasks_and_enrichment_tasks_passed_through` — both wire keys + their snake_case aliases.
  - `node_name_camelcase_and_alias` — `nodeName` + `node_name`.
  - `data_field_round_trip` — single-word, no rename.
  - When the real library call lands (post-P5), additional integration tests gate on `MemifyResult.stages_executed` containing the four stage names. Until then the stub-mode tests just verify the DTO plumbing.
- Cross-SDK in `e2e-cross-sdk/harness/test_http_v2_improve.py` (NEW — distinct from existing `test_http_improve.py` which covers v1-era fields only): drive both backends with `{"sessionIds": ["s1"], "extractionTasks": [], "enrichmentTasks": [], "data": "", "nodeName": [], "datasetName": "ds"}` and confirm:
  - Both return the same wire shape (PipelineRunInfoDTO).
  - Cross-byte equality of every camelCase key emitted.
  - When the stub is replaced (post-P5), node/edge counts converge.

## 6. Acceptance criteria

- [x] `ImprovePayloadDTO` matches Python's `ImprovePayloadDTO` field-for-field (8 fields total: `extractionTasks`, `enrichmentTasks`, `data`, `datasetName`, `datasetId`, `nodeName`, `runInBackground`, `sessionIds`).
- [x] All 5 new fields accept both camelCase (wire) and snake_case (input alias).
- [x] `ImproveParams<'_>` extended with `extraction_tasks: Option<Vec<String>>`, `enrichment_tasks: Option<Vec<String>>`, `data: Option<String>` — workspace `cargo check --all-targets` passes (all 5 LIB-04 call sites updated, option (b) preserved — no `Default` derive).
- [x] `session_ids` is observable in handler tracing (`session_ids_count` field in the `tracing::info!`).
- [ ] When the real library call lands (P5 follow-up), the 4-stage session bridge runs (verifiable via `ImproveResult.stages_run`). **Deferred to P5.**
- [x] Cross-SDK structural parity test passes for the wire DTO; stage-level parity gated on P5.

## 7. References

- [Python improve router](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/improve/routers/get_improve_router.py)
- [Rust improve API](../../../crates/lib/src/api/improve.rs)
- [api-v2 improve doc](../../api-v2/improve.md)
