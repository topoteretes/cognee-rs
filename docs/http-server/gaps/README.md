# cognee-http-server — Gap Inventory

This inventory was re-checked against the source code on **2026-05-29**.
The status entries below still match the current handlers in
[crates/http-server/src/routers/](../../../crates/http-server/src/routers/)
and the integration tests in
[crates/http-server/tests/](../../../crates/http-server/tests/). It remains the
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
| 2 | `POST /api/v1/improve` | [routers/improve.rs](../../../crates/http-server/src/routers/improve.rs) | **landed** ([plan](impl/02-improve-pipeline-wiring.md)) | Real improve execution is now wired via `run_real_improve`: Stage 1 `apply_feedback_weights_pipeline` (when session handles are present), Stage 2 `persist_sessions_in_knowledge_graph` (when session store + llm are present), Stage 3 `run_memify` (always), Stage 4 `sync_graph_to_session` (when checkpoint store is present). `ComponentHandles` gained `checkpoint_store`. Tests in [tests/test_improve.rs](../../../crates/http-server/tests/test_improve.rs) now cover payload acceptance and blocking 420 behavior when components are unwired; [tests/test_improve_420.rs](../../../crates/http-server/tests/test_improve_420.rs) still guards the raw-DTO 420 parity contract. |
| 3 | `POST /api/v1/remember` (cognify+memify leg) | [routers/remember.rs:313](../../../crates/http-server/src/routers/remember.rs#L313) | **landed** | Merge `51a6b7d`, gap `72850c6`. Both stub futures replaced with inline `run_remember_cognify_memify` (Option A — `cognee-http-server` cannot depend on `cognee-lib` due to the workspace cycle). 409 Python-parity envelope retained as catch-all. Integration test [tests/test_remember.rs](../../../crates/http-server/tests/test_remember.rs) asserts graph edges + non-empty `Triplet` vector collection. |

## Tier 2 — Placeholder content (200 OK, empty/synthetic body)

| # | Endpoint | Source | Status | Notes |
|---|---|---|---|---|
| 4 | `GET /api/v1/datasets/{id}/graph` and `WS /api/v1/cognify/subscribe/{id}` payload | [routers/datasets.rs:298-301](../../../crates/http-server/src/routers/datasets.rs#L298-L301), [routers/cognify.rs:452-465](../../../crates/http-server/src/routers/cognify.rs#L452-L465) | **landed** | Merge `58e1233`, gap `6fab673`. New `cognee_graph::get_formatted_graph_data` (Python parity port). `ComponentHandles::formatted_graph_data` calls the real helper when `graph_db` is wired; empty-shape fallback when `None` is intentional (Python parity). Tests: [tests/test_datasets_graph.rs](../../../crates/http-server/tests/test_datasets_graph.rs), [tests/test_cognify_websocket.rs](../../../crates/http-server/tests/test_cognify_websocket.rs). Deviations: visualize router calls `cognee_visualization::render*` directly (not through the new helper); WS handler passes nil `user_id` because the run row doesn't carry it; cross-SDK WS parity deferred. |
| **5a** | `GET /api/v1/datasets/{dataset_id}/schema` | [routers/datasets.rs:312-333](../../../crates/http-server/src/routers/datasets.rs#L312-L333) | **landed** | Returns the persisted `graph_schema` / `custom_prompt` when a row exists and the Python-parity `{"graph_schema": null, "custom_prompt": null}` body when it does not. Covered by [tests/test_datasets_schema.rs](../../../crates/http-server/tests/test_datasets_schema.rs). |
| **5b** | `PUT /api/v1/datasets/{dataset_id}/schema` | [routers/datasets.rs:386-402](../../../crates/http-server/src/routers/datasets.rs#L386-L402) | **landed** | Upserts the row via `DatasetConfigDb` and still returns the Python-parity envelope `{"status": "ok"}`. Covered by [tests/test_datasets_schema.rs](../../../crates/http-server/tests/test_datasets_schema.rs). |
| 6 | `GET /api/v1/health` (synthetic component entries) | [routers/health.rs:134-150](../../../crates/http-server/src/routers/health.rs#L134-L150) | **landed** | Merge `07ae8c8`, gap `31a48ca`. `RealHealthChecker` with concurrent probes (graph DB / vector DB / SQLite / file storage; opt-in LLM and embedding) matching Python parity status model. [tests/test_health_real.rs](../../../crates/http-server/tests/test_health_real.rs) covers healthy / degraded / unhealthy / cache. The standalone binary now wires default backends in [src/wiring.rs](../../../crates/http-server/src/wiring.rs) and installs the real health checker from [src/main.rs](../../../crates/http-server/src/main.rs). |
| **7** | `POST /api/v1/remember/entry` (`generate_feedback_with_llm` path) | [routers/remember.rs](../../../crates/http-server/src/routers/remember.rs) | **landed** ([plan](impl/11-remember-feedback-llm.md)) | Merge `492df0f`, gap `3479f33`. Generates feedback in the handler via `feedback::generate_session_feedback` using `ComponentHandles.llm`, with `tokio::time::timeout` (8 s default), ANSI/control-char scrubbing, 500-char cap, and Python-parity deterministic fallback on every non-success path. LLM response NEVER logged verbatim. Mirror change applied to `cognee_lib::api::remember::remember_entry`. Parity bump locked: when `generate_feedback_with_llm=false`, deterministic fallback is written (was previously empty string). |
| **8** | `POST /api/v1/forget` (cloud proxy short-circuit) | [routers/forget.rs](../../../crates/http-server/src/routers/forget.rs) | **landed** ([plan](impl/10-forget-cloud-proxy.md)) | Added `CloudDeleteClient` + `CloudClientError` in [src/cloud_client.rs](../../../crates/http-server/src/cloud_client.rs), wired optional `cloud_client` on `ComponentHandles`, and implemented early short-circuit proxying in [routers/forget.rs](../../../crates/http-server/src/routers/forget.rs). When no cloud client is wired, the local `DeleteService` flow remains unchanged. Cloud failures are scrubbed and mapped to 502/503 envelopes without leaking upstream payloads. Coverage in [tests/test_forget.rs](../../../crates/http-server/tests/test_forget.rs) includes proxy success, proxy error mapping, and no-cloud fallback behavior. |

## Tier 3 — Honest `501 Not Implemented` (with regression guards)

All three Tier-3 endpoints have inline router tests that assert `status != 501`,
so a future regression to silent `200 OK` from the 501 branch would fail loudly.

| # | Endpoint | Source | Status | Notes |
|---|---|---|---|---|
| 9 | `PATCH /api/v1/update` | [routers/update.rs:140-157](../../../crates/http-server/src/routers/update.rs#L140-L157) | **landed** | Merge `c67a8d1`, gap `f4ae5a6`. `run_update_pipeline`: resolve target Data → write-ACL gate → soft-delete → re-ingest via AddPipeline → re-run cognify. **Inline regression guard at [update.rs:403-414](../../../crates/http-server/src/routers/update.rs#L403-L414) asserts `status != 501`.** Two non-blocking follow-ups: (1) upgrade the env-gated integration test to assert real downstream side effects (currently a documentation stub); (2) factor `routers/cognify.rs::run_real_cognify` into a shared helper to remove duplication with `run_update_pipeline`. |
| 10 | `POST /api/v1/responses` | [routers/responses.rs:68-95](../../../crates/http-server/src/routers/responses.rs#L68-L95) | **landed** | Merge `3f4c471`, gap `487c595`. New `cognee_llm::ResponsesClient` trait + `OpenAIResponsesClient`. Tool dispatch via `ComponentHandlesDispatcher` for `search` (real `SearchOrchestrator`) and `cognify` (honest error directing to `/api/v1/cognify`). Upstream OpenAI errors **scrubbed** before reaching the client. **Inline regression guard at [responses.rs:427-445](../../../crates/http-server/src/routers/responses.rs#L427-L445).** Important follow-up: Python's `handle_cognify` runs the full add+cognify pipeline inline; the Rust honest-stub is not full parity. |
| 11 | `POST /api/v1/notebooks/{id}/{cell}/run` | [routers/notebooks.rs:303-350](../../../crates/http-server/src/routers/notebooks.rs#L303-L350) | **landed** | Merge `a351ae6`, gap `2c74a89`. `NotebookRunner` trait + `SubprocessRunner` spawning `python3 -I` with `env_clear`, scrubbed PATH, `RLIMIT_AS=512MB` + `RLIMIT_CPU=60s` via `pre_exec` (Unix), `kill_on_drop`, output caps with `[truncated]` marker, errors scrubbed before client. User code fed via **stdin** to a static `-c` wrapper. Embedders opt in via `ComponentHandles::notebook_runner`; legacy 501 envelope retained otherwise. Inline regression guard `run_cell_with_runner_does_not_return_501`. Non-blocking follow-up: replace `wait_with_output()` with a streaming drain (non-Unix has no `RLIMIT_AS` bound). |

---

## Progress as of 2026-05-29

- **Tier 1: 3 of 3 closed.**
- **Tier 2: 6 of 6 closed.**
- **Tier 3: 3 of 3 closed.** ✅ All have inline `status != 501` regression guards.

**Total: 12 of 12 gaps landed.** (The original count was 11; gap 5 was split into 5a/5b by code reading.)

---

## Cross-cutting issues

### C1: Standalone binary wiring — landed

The standalone binary now builds default backend handles in
[crates/http-server/src/wiring.rs](../../../crates/http-server/src/wiring.rs),
uses them from [crates/http-server/src/main.rs](../../../crates/http-server/src/main.rs),
sets `state.lib`, and calls `state.install_real_health_checker()`.

Key outcomes:

| Endpoint | Behavior in tests | Behavior in standalone binary |
|---|---|---|
| `POST /memify` | Runs real memify | Runs real memify when default backends are enabled |
| `POST /remember` | Runs full add → cognify → memify | Runs full pipeline when default backends are enabled |
| `GET /datasets/{id}/graph` | Returns populated snapshot | Uses wired `graph_db` instead of empty fallback |
| `GET/PUT /datasets/{id}/schema` | Returns / saves the dataset schema | Uses wired DB-backed handlers |
| `GET /health` | Probes real backends | Uses `RealHealthChecker` instead of `MockHealthChecker` |
| `PATCH /update` | Runs delete + add + cognify | Uses wired graph/vector/embedding backends |
| `POST /responses` | Routes through `OpenAIResponsesClient` | Can wire a real responses client when enabled and configured |
| `POST /notebooks/{id}/{cell}/run` | Runs `python3` subprocess | Can wire `SubprocessRunner` when enabled |

There is still an explicit escape hatch: `COGNEE_DISABLE_DEFAULT_BACKENDS=1`
preserves the old minimal startup path for test or constrained deployments.

### C2: Live TODO / blocking markers still in source

| File | Line | Marker | Tracks |
|---|---|---|---|
| [routers/remember.rs](../../../crates/http-server/src/routers/remember.rs#L724) | 724 | `// TODO(LIB-01-followup): wire Arc<dyn Llm> through SessionManager` | Gap 7 follow-up |

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

## Recommended next work (in priority order)

1. **Gap 7 follow-up** — thread `Arc<dyn Llm>` through SessionManager to remove the remaining `// TODO(LIB-01-followup)` path in remember-entry internals.
2. **Tier 3 follow-up (update endpoint)** — convert env-gated update integration coverage from contract-only to stronger side-effect assertions.
3. **Responses parity follow-up** — extend `/api/v1/responses` cognify tool toward Python's inline add+cognify behavior.
