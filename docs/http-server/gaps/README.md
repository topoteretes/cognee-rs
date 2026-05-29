# cognee-http-server — Gap Inventory

This inventory was re-derived from the source code on **2026-05-28** by reading
every handler in [crates/http-server/src/routers/](../../../crates/http-server/src/routers/)
and cross-referencing against the integration tests in
[crates/http-server/tests/](../../../crates/http-server/tests/). It is the
authoritative list — the previous tree was untracked and lost, and the
git-log-derived reconstruction missed several gaps that still exist as TODO/
"Blocking gap" markers in the code.

A gap qualifies when at least one of these is true:

- the handler **claims success** but performs no real work (no-op pipeline future);
- the handler returns `501 Not Implemented` and the surrounding tests cannot
  detect a future silent regression to `200 OK`;
- the handler returns a structurally valid response with placeholder content
  (`{"nodes": [], "edges": []}`, `{"graph_schema": null}`, hard-coded `"ok"`)
  that a Python-parity client cannot distinguish from a real reply;
- the source carries a live `// TODO`, `// FIXME`, or `// Blocking gap` marker.

---

## Tier 1 — Handler claims success, does no real work

| # | Endpoint | Source | Status | Notes |
|---|---|---|---|---|
| 1 | `POST /api/v1/memify` | [routers/memify.rs:95](../../../crates/http-server/src/routers/memify.rs#L95) | **landed** | Merge `3b4ac97`, gap `5f9e181`. `run_real_memify` helper wires the real pipeline. Integration test `post_memify_blocking_indexes_triplets` in [tests/test_memify.rs](../../../crates/http-server/tests/test_memify.rs) asserts the `("Triplet","text")` vector collection is non-empty after the call. Deviation: `MemifyConfig::default()` used; `MemifyPayloadDTO` extension to carry `extraction_tasks`/`enrichment_tasks`/node filters deferred. |
| 2 | `POST /api/v1/improve` | [routers/improve.rs:106-114](../../../crates/http-server/src/routers/improve.rs#L106-L114) | **not-started** ([plan](impl/02-improve-pipeline-wiring.md)) | Still ships `box_pipeline_future(async move { Ok::<(), std::io::Error>(()) })` at line 114. Live markers in source: `// Blocking gap stub — improve requires the same components as memify.` (L106) and `// TODO(P5): wire real improve() call once ComponentHandles gains graph/vector handles.` (L107). [tests/test_improve.rs](../../../crates/http-server/tests/test_improve.rs) is a skip-stub. Plan: inline Option-A composition (`run_real_improve` helper) mirroring gap 03's pattern; adds `checkpoint_store` slot to `ComponentHandles`. |
| 3 | `POST /api/v1/remember` (cognify+memify leg) | [routers/remember.rs:313](../../../crates/http-server/src/routers/remember.rs#L313) | **landed** | Merge `51a6b7d`, gap `72850c6`. Both stub futures replaced with inline `run_remember_cognify_memify` (Option A — `cognee-http-server` cannot depend on `cognee-lib` due to the workspace cycle). 409 Python-parity envelope retained as catch-all. Integration test [tests/test_remember.rs](../../../crates/http-server/tests/test_remember.rs) asserts graph edges + non-empty `Triplet` vector collection. |

## Tier 2 — Placeholder content (200 OK, empty/synthetic body)

| # | Endpoint | Source | Status | Notes |
|---|---|---|---|---|
| 4 | `GET /api/v1/datasets/{id}/graph` and `WS /api/v1/cognify/subscribe/{id}` payload | [routers/datasets.rs:298-301](../../../crates/http-server/src/routers/datasets.rs#L298-L301), [routers/cognify.rs:452-465](../../../crates/http-server/src/routers/cognify.rs#L452-L465) | **landed** | Merge `58e1233`, gap `6fab673`. New `cognee_graph::get_formatted_graph_data` (Python parity port). `ComponentHandles::formatted_graph_data` calls the real helper when `graph_db` is wired; empty-shape fallback when `None` is intentional (Python parity). Tests: [tests/test_datasets_graph.rs](../../../crates/http-server/tests/test_datasets_graph.rs), [tests/test_cognify_websocket.rs](../../../crates/http-server/tests/test_cognify_websocket.rs). Deviations: visualize router calls `cognee_visualization::render*` directly (not through the new helper); WS handler passes nil `user_id` because the run row doesn't carry it; cross-SDK WS parity deferred. |
| **5a** | `GET /api/v1/datasets/{dataset_id}/schema` | [routers/datasets.rs:312-333](../../../crates/http-server/src/routers/datasets.rs#L312-L333) | **not-started** ([plan](impl/05-dataset-configuration.md)) | Returns `{"graph_schema": null, "custom_prompt": null}` unconditionally. Live markers in source: `/// **BLOCKING GAP**: get_dataset_configuration does not exist.` (L310) and `// TODO(blocking): implement dataset_configurations table in cognee-models/cognee-database` (L328). [tests/test_datasets_schema.rs](../../../crates/http-server/tests/test_datasets_schema.rs) is a skip-stub. Plan covers both 5a and 5b: new `dataset_configurations` table + entity + `DatasetConfigDb` trait, then wire both handlers. Three Python-parity decisions locked (per-dataset scoping, 200+nulls when no row, `{"status":"ok"}` PUT body). |
| **5b** | `PUT /api/v1/datasets/{dataset_id}/schema` | [routers/datasets.rs:386-402](../../../crates/http-server/src/routers/datasets.rs#L386-L402) | **not-started** ([plan](impl/05-dataset-configuration.md)) | Returns `{"status": "ok"}` after the permission check; the schema payload is bound but ignored (`Json(_payload)`). Live markers: `/// **BLOCKING GAP**: dataset_configurations table does not exist.` (L383) and `// TODO(blocking): implement dataset_configurations upsert in cognee-database` (L400). Covered by the same plan as 5a. |
| 6 | `GET /api/v1/health` (synthetic component entries) | [routers/health.rs:134-150](../../../crates/http-server/src/routers/health.rs#L134-L150) | **landed (binary-degraded)** | Merge `07ae8c8`, gap `31a48ca`. `RealHealthChecker` with concurrent probes (graph DB / vector DB / SQLite / file storage; opt-in LLM and embedding) matching Python parity status model. [tests/test_health_real.rs](../../../crates/http-server/tests/test_health_real.rs) covers healthy / degraded / unhealthy / cache. **Important systemic limitation:** `AppState::install_real_health_checker()` must be called by embedders after wiring `state.lib`. The standalone binary ([src/main.rs](../../../crates/http-server/src/main.rs)) does not yet wire `state.lib`, so the binary still serves the `MockHealthChecker`. See [C1 below](#c1-standalone-binary-wiring-pre-existing-systemic). |
| **7** | `POST /api/v1/remember/entry` (`generate_feedback_with_llm` path) | [routers/remember.rs](../../../crates/http-server/src/routers/remember.rs) | **landed** ([plan](impl/11-remember-feedback-llm.md)) | Merge `492df0f`, gap `3479f33`. Generates feedback in the handler via `feedback::generate_session_feedback` using `ComponentHandles.llm`, with `tokio::time::timeout` (8 s default), ANSI/control-char scrubbing, 500-char cap, and Python-parity deterministic fallback on every non-success path. LLM response NEVER logged verbatim. Mirror change applied to `cognee_lib::api::remember::remember_entry`. Parity bump locked: when `generate_feedback_with_llm=false`, deterministic fallback is written (was previously empty string). |
| **8** | `POST /api/v1/forget` (cloud proxy short-circuit) | [routers/forget.rs:57](../../../crates/http-server/src/routers/forget.rs#L57) | **not-started (low priority)** ([plan](impl/10-forget-cloud-proxy.md)) | Local delete works correctly through `DeleteService`. The cloud-proxy branch is a TODO comment only. There is no `cloud_client` field on `state.lib` yet. Not a regression — multi-tenant cloud routing is a future feature. Plan adds a `CloudDeleteClient` trait + `ComponentHandles` slot; Stage A is the trait+slot+handler branch, Stage B (real HTTP impl) is optional and explicitly deferred. |

## Tier 3 — Honest `501 Not Implemented` (with regression guards)

All three Tier-3 endpoints have inline router tests that assert `status != 501`,
so a future regression to silent `200 OK` from the 501 branch would fail loudly.

| # | Endpoint | Source | Status | Notes |
|---|---|---|---|---|
| 9 | `PATCH /api/v1/update` | [routers/update.rs:140-157](../../../crates/http-server/src/routers/update.rs#L140-L157) | **landed** | Merge `c67a8d1`, gap `f4ae5a6`. `run_update_pipeline`: resolve target Data → write-ACL gate → soft-delete → re-ingest via AddPipeline → re-run cognify. **Inline regression guard at [update.rs:403-414](../../../crates/http-server/src/routers/update.rs#L403-L414) asserts `status != 501`.** Two non-blocking follow-ups: (1) upgrade the env-gated integration test to assert real downstream side effects (currently a documentation stub); (2) factor `routers/cognify.rs::run_real_cognify` into a shared helper to remove duplication with `run_update_pipeline`. |
| 10 | `POST /api/v1/responses` | [routers/responses.rs:68-95](../../../crates/http-server/src/routers/responses.rs#L68-L95) | **landed** | Merge `3f4c471`, gap `487c595`. New `cognee_llm::ResponsesClient` trait + `OpenAIResponsesClient`. Tool dispatch via `ComponentHandlesDispatcher` for `search` (real `SearchOrchestrator`) and `cognify` (honest error directing to `/api/v1/cognify`). Upstream OpenAI errors **scrubbed** before reaching the client. **Inline regression guard at [responses.rs:427-445](../../../crates/http-server/src/routers/responses.rs#L427-L445).** Important follow-up: Python's `handle_cognify` runs the full add+cognify pipeline inline; the Rust honest-stub is not full parity. |
| 11 | `POST /api/v1/notebooks/{id}/{cell}/run` | [routers/notebooks.rs:303-350](../../../crates/http-server/src/routers/notebooks.rs#L303-L350) | **landed** | Merge `a351ae6`, gap `2c74a89`. `NotebookRunner` trait + `SubprocessRunner` spawning `python3 -I` with `env_clear`, scrubbed PATH, `RLIMIT_AS=512MB` + `RLIMIT_CPU=60s` via `pre_exec` (Unix), `kill_on_drop`, output caps with `[truncated]` marker, errors scrubbed before client. User code fed via **stdin** to a static `-c` wrapper. Embedders opt in via `ComponentHandles::notebook_runner`; legacy 501 envelope retained otherwise. Inline regression guard `run_cell_with_runner_does_not_return_501`. Non-blocking follow-up: replace `wait_with_output()` with a streaming drain (non-Unix has no `RLIMIT_AS` bound). |

---

## Progress as of 2026-05-28

- **Tier 1: 2 of 3 closed.** Gap 02 (improve) still ships the no-op `box_pipeline_future(Ok(()))`.
- **Tier 2: 2 of 6 closed.** Gaps 04 (graph data) and 06 (health, binary-degraded) landed; gaps 5a (schema GET), 5b (schema PUT), 7 (feedback LLM), and 8 (forget cloud proxy) all still open with explicit `// TODO(blocking)` or `// TODO(LIB-01-followup)` markers in source.
- **Tier 3: 3 of 3 closed.** ✅ All have inline `status != 501` regression guards.

**Total: 7 of 12 gaps landed.** (The original count was 11; gap 5 was split into 5a/5b by code reading.)

---

## Cross-cutting issues

### C1: Standalone binary wiring (pre-existing systemic) — [plan](impl/C1-standalone-binary-wiring.md)

[crates/http-server/src/main.rs](../../../crates/http-server/src/main.rs) calls
`AppState::build(cfg)` but **never** populates `state.lib`, `state.health`, or
the `ComponentHandles`. Consequence: every landed gap that depends on
`ComponentHandles` works in tests (which wire their own state) but the
standalone binary still serves the placeholder/501 envelopes.

| Endpoint | Behavior in tests | Behavior in standalone binary |
|---|---|---|
| `POST /memify` | Runs real memify | Returns `PipelineRunCompleted` with no actual work |
| `POST /remember` | Runs full add → cognify → memify | Returns `PipelineRunCompleted` with no actual work |
| `GET /datasets/{id}/graph` | Returns populated snapshot | Returns `{"nodes": [], "edges": []}` |
| `GET /health` | Probes real backends | Returns `MockHealthChecker` synthetic entries |
| `PATCH /update` | Runs delete + add + cognify | Returns 500 (vector_db / embedding_engine not wired) |
| `POST /responses` | Routes through `OpenAIResponsesClient` | Returns 500 "responses client is not wired" |
| `POST /notebooks/{id}/{cell}/run` | Runs `python3` subprocess | Returns 501 (notebook_runner not wired) |

**Fix:** see [impl/C1-standalone-binary-wiring.md](impl/C1-standalone-binary-wiring.md) — adds a `wire_default_backends(cfg: &HttpServerConfig) -> ComponentHandles` constructor in a new `wiring.rs` module, calls it from `main.rs`, then calls `state.install_real_health_checker()`. The plan also defines ~22 new config fields (`DATA_ROOT_DIRECTORY`, `RELATIONAL_DB_URL`, embedding/LLM/session/notebook settings) with sensible defaults, and a smoke test that boots the binary against in-memory backends to confirm `/health/detailed` returns real probes. This is the largest single piece of outstanding work and unblocks the binary for all landed gaps simultaneously.

### C2: Live TODO / blocking markers still in source

| File | Line | Marker | Tracks |
|---|---|---|---|
| [routers/improve.rs](../../../crates/http-server/src/routers/improve.rs#L106) | 106-107 | `// Blocking gap stub`, `// TODO(P5)` | Gap 2 |
| [routers/datasets.rs](../../../crates/http-server/src/routers/datasets.rs#L328) | 328 | `// TODO(blocking): implement dataset_configurations table` | Gap 5a |
| [routers/datasets.rs](../../../crates/http-server/src/routers/datasets.rs#L400) | 400 | `// TODO(blocking): implement dataset_configurations upsert` | Gap 5b |
| [routers/remember.rs](../../../crates/http-server/src/routers/remember.rs#L724) | 724 | `// TODO(LIB-01-followup): wire Arc<dyn Llm> through SessionManager` | Gap 7 |
| [routers/forget.rs](../../../crates/http-server/src/routers/forget.rs#L57) | 57 | `// TODO(cloud): proxy via cloud client` | Gap 8 (future feature) |

The stale `// TODO(P1): wire Arc<dyn cognee_lib::health::HealthChecker>` marker
in `state.rs` was cleaned up on **2026-05-28** as part of this audit — health
is now wired (gap 6).

### C3: `ComponentHandles` slots with silent vs. loud degradation

| Slot | Used by | When `None` |
|---|---|---|
| `graph_db` | `GET /datasets/{id}/graph`, WS `/cognify/subscribe/{id}` | Empty arrays — **silent** (intentional Python parity) |
| `vector_db` | `POST /cognify`, `/memify`, `/update` | 500 — loud |
| `embedding_engine` | `POST /cognify`, `/memify`, `/update` | 500 — loud |
| `responses_client` | `POST /responses` | 500 "responses client is not wired" — loud |
| `notebook_runner` | `POST /notebooks/{id}/{cell}/run` | 501 — loud (opt-in feature) |
| `llm` | `POST /responses` cognify tool | Honest error directing to `/api/v1/cognify` — loud |
| `session_store`, `session_manager` | `POST /recall` session+trace sources | Empty arrays — silent (intentional fallback) |

Only `graph_db` and the session slots fall back silently; both are intentional
Python-parity behaviors.

---

## Recommended next gaps (in priority order)

1. **Gap 5a + 5b — dataset schema GET/PUT** (Tier 2, blocked on a new `dataset_configurations` table in `cognee-database`). Tackle together. Includes a migration.
2. **Gap 7 — `POST /remember/entry` LLM feedback path** (Tier 2). Wire `Arc<dyn Llm>` + prompt template through `SessionManager`. Tightly scoped.
3. **C1 — standalone binary wiring** (cross-cutting, biggest lever). Closes the "binary serves real responses" gap for every already-landed feature at once.
4. **Gap 2 — `POST /improve`** (Tier 1). Requires the library composition or inline Option-A equivalent. Same crate-cycle constraint as gap 3.
5. **Gap 8 — forget cloud proxy** (future feature, not a regression). Only worth doing once `state.lib.cloud_client` is added.
