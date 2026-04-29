# E-05 — `POST /api/v1/improve`

| | |
|---|---|
| Wire path | `POST /api/v1/improve` |
| Status | **Partial** — DTO missing `extraction_tasks`, `enrichment_tasks`, `data`, `node_name`, `session_ids`. |
| Depends on | LIB-04 (`ImproveParams` struct refactor — Decision 8). |
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

- DTO at [`crates/http-server/src/dto/improve.rs:14`](../../../crates/http-server/src/dto/improve.rs#L14) has only `dataset_name`, `dataset_id`, `run_in_background`.
- Handler at `crates/http-server/src/routers/improve.rs:159` calls `cognee_lib::api::improve::improve(...)` but does NOT pass `session_ids`.
- `cognee_lib::api::improve::improve` accepts an `ImproveParams<'_>` struct after **LIB-04** lands (B-3 in the §0 phase order). This task assumes that struct exists; if LIB-04 hasn't run yet the investigation agent must report BLOCKED.

## 4. Implementation steps

> **Decision (2026-04-29) — Decision 8**: this task does NOT refactor the `improve()` signature. That work is owned by **LIB-04** (B-3 in the phase order) and lands before this task. E-05 just adds three new fields (`extraction_tasks`, `enrichment_tasks`, `data`) to the `ImproveParams` struct LIB-04 introduces, plus the HTTP DTO and handler wiring. Investigation agent: do not re-litigate.

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

3. **Update the handler** at `crates/http-server/src/routers/improve.rs:82`:
   ```rust
   let result = cognee_lib::api::improve::improve(
       cognee_lib::api::improve::ImproveParams {
           dataset_name: payload.dataset_name.unwrap_or_default(),
           // ...existing infra fields populated from `components` / `user` exactly as before...
           extraction_tasks: payload.extraction_tasks,
           enrichment_tasks: payload.enrichment_tasks,
           data: payload.data,
           node_name: payload.node_name,
           session_ids: payload.session_ids,
           ..ImproveParams::default()         // only valid if LIB-04 chose Default-derive (option (a)); otherwise spell out every field
       },
   ).await?;
   ```
   The exact constructor shape depends on LIB-04's choice between Default-derive vs spell-out-every-field (LIB-04 §3 step 2 picks (b) — spell out — by default).

4. **OpenAPI** — re-derive `ToSchema`; the new fields with `serde(default)` produce `nullable: true` schemas matching Python's `Optional[...]` shape.

5. **Telemetry** — add `cognee.improve.session_ids_count = payload.session_ids.as_ref().map_or(0, |s| s.len())` to the `tracing::instrument` field list, mirroring Python's `send_telemetry({"session_ids_count": ...})`.

## 5. Tests

- Update `crates/http-server/src/dto/improve.rs` tests with deserialization round-trips for every new field.
- `crates/http-server/tests/test_improve.rs`:
  - `session_ids_triggers_session_bridge_path` — mock `improve()` and assert it received the session_ids list.
  - `empty_session_ids_runs_memify_only` — same, but session_ids is `None` or `[]`.
  - `extraction_tasks_and_enrichment_tasks_passed_through`.
  - `node_name_filters_subset_of_graph` (regression for memify path).
- Cross-SDK in `e2e-cross-sdk/harness/test_http_v2_improve.py`: drive both backends with `{"session_ids": ["s1"], ...}` and confirm both write the same node/edge counts post-flow.

## 6. Acceptance criteria

- [ ] `ImprovePayloadDTO` matches Python's `ImprovePayloadDTO` field-for-field.
- [ ] `session_ids` reaches `cognee_lib::api::improve::improve` without dropping.
- [ ] When `session_ids` is set, the 4-stage session bridge runs (verifiable via mocked `MemifyResult.stages_executed`).
- [ ] Cross-SDK structural parity test passes.

## 7. References

- [Python improve router](https://github.com/topoteretes/cognee/blob/main/cognee/api/v1/improve/routers/get_improve_router.py)
- [Rust improve API](../../../crates/lib/src/api/improve.rs)
- [api-v2 improve doc](../../api-v2/improve.md)
