# Gap 02 — Wire POST /api/v1/improve to real pipeline

## Source / current state

The handler at [crates/http-server/src/routers/improve.rs:32-181](../../../../crates/http-server/src/routers/improve.rs#L32-L181) already does dataset resolution, payload telemetry, dispatch plumbing, the 420 Python-quirk error path, and full outcome→response mapping. **The only thing it does not do is actual work**: at [crates/http-server/src/routers/improve.rs:114](../../../../crates/http-server/src/routers/improve.rs#L114) the dispatched future is the no-op

```rust
let work = box_pipeline_future(async move { Ok::<(), std::io::Error>(()) });
```

with live markers at [crates/http-server/src/routers/improve.rs:106-113](../../../../crates/http-server/src/routers/improve.rs#L106-L113):

- `// Blocking gap stub — improve requires the same components as memify.` (L106)
- `// TODO(P5): wire real improve() call once ComponentHandles gains graph/vector handles.` (L107)
- A `// E-05 scope:` block (L108-113) listing the slots needed: `vector_db`, `embedding_engine`, `add_pipeline`, `checkpoint_store`, `cognify_config`, `ontology_resolver`.

The integration test [crates/http-server/tests/test_improve.rs:75-88](../../../../crates/http-server/tests/test_improve.rs#L75-L88) is a documented skip-stub:

```rust
eprintln!(
    "test_improve: skipping end-to-end — real improve() is not wired through \
     ComponentHandles yet"
);
```

The DTO ([crates/http-server/src/dto/improve.rs:22-59](../../../../crates/http-server/src/dto/improve.rs#L22-L59)) is **already** at full Python parity for the eight payload fields: `extraction_tasks`, `enrichment_tasks`, `data`, `dataset_name`, `dataset_id`, `node_name`, `run_in_background`, `session_ids`. **No DTO field work is required.** (Gap 01's deferred memify fields do not apply here.)

The canonical four-stage composition lives in [crates/lib/src/api/improve.rs:127-424](../../../../crates/lib/src/api/improve.rs#L127-L424), with the `ImproveParams` struct at L67-119 listing every dependency.

## Why it's blocked

`cognee-http-server` cannot depend on `cognee-lib` due to the workspace cycle constraint ([crates/http-server/Cargo.toml:44-46](../../../../crates/http-server/Cargo.toml#L44-L46)):

```toml
# NOTE: cognee-lib is intentionally NOT a direct dep here — adding it would
# create a cycle when cognee-lib's `server` feature pulls in cognee-http-server.
# Concrete cognee-lib types are wired via AppState in P1+.
```

This is the same constraint that blocked gap 03 (remember pipeline wiring) and was resolved with Option A: inline composition in the handler directly against `cognee-cognify`, `cognee-ingestion`, `cognee-database`, `cognee-graph`, `cognee-vector`, etc. — never touching `cognee-lib`.

## Strategy / approach

**Option A — inline composition in the handler.** This is the same pattern that landed in gap 03 ([crates/http-server/src/routers/remember.rs:430-553](../../../../crates/http-server/src/routers/remember.rs#L430-L553), `run_remember_cognify_memify`). Reasons:

- It is the **only** option compatible with the workspace cycle. Option B (moving improve composition into a shared crate, then having `cognee-lib::api::improve` and the HTTP handler both consume it) is a strictly larger refactor that would also force gap 03 to be re-done in lockstep. Not worth it for a single endpoint.
- It mirrors the precedent set by gap 01 (memify, `run_real_memify` at [crates/http-server/src/routers/memify.rs:187-234](../../../../crates/http-server/src/routers/memify.rs#L187-L234)) and gap 03 — readers/maintainers already understand the pattern.
- `cognee-cognify` already re-exports every building block we need ([crates/cognify/src/lib.rs:26-32](../../../../crates/cognify/src/lib.rs#L26-L32)): `apply_feedback_weights_pipeline`, `persist_sessions_in_knowledge_graph`, `sync_graph_to_session`, `run_memify`, `MemifyConfig`, `CognifyConfig`. No new crate boundaries.

Rejected alternatives:

- **Option B (shared crate).** Larger blast radius. Defer to a future cleanup pass that also re-homes `run_real_memify` and `run_remember_cognify_memify`.
- **Option C (lift `cognee-lib::api::improve::improve` into the handler via type erasure).** The function signature takes `ImproveParams<'a>` with a borrowed `add_pipeline` and `cognify_config`; moving it requires touching `cognee-lib` itself (which the HTTP crate cannot depend on). Pointless.

The new helper will mirror `cognee_lib::api::improve::improve` ([crates/lib/src/api/improve.rs:127-424](../../../../crates/lib/src/api/improve.rs#L127-L424)) **stage by stage**, with the same warning-only error containment so a failure in one stage does not abort subsequent stages (matches Python `improve.py:155-232`).

## Implementation steps

### Step 1 — Add `checkpoint_store` slot to `ComponentHandles`

[crates/http-server/src/components.rs](../../../../crates/http-server/src/components.rs) lacks a `checkpoint_store` slot. Stage 4 (`sync_graph_to_session`) requires `&dyn CheckpointStore` ([crates/cognify/src/memify/sync_graph_session.rs:145-152](../../../../crates/cognify/src/memify/sync_graph_session.rs#L145-L152)).

Add to `ComponentHandles` (after `permissions` at [crates/http-server/src/components.rs:88](../../../../crates/http-server/src/components.rs#L88)):

```rust
/// Checkpoint store for graph→session deltas. Required by improve
/// stage 4 (`sync_graph_to_session`). `None` means improve stage 4
/// is skipped (matches the `cognee_lib::api::improve` fallback).
pub checkpoint_store: Option<Arc<dyn cognee_database::CheckpointStore>>,
```

Update every `ComponentHandles { ... }` literal in the crate to default the new slot to `None`. Use `cargo check --all-features` and `rg "ComponentHandles \{"` to find them all. Known sites at the time of writing:

- [crates/http-server/src/state.rs](../../../../crates/http-server/src/state.rs) (the default-test builder).
- [crates/http-server/tests/test_remember.rs:185-203](../../../../crates/http-server/tests/test_remember.rs#L185-L203).
- Any other test fixtures under `crates/http-server/tests/`.

### Step 2 — Add the boxed-future error wrapper

In [crates/http-server/src/routers/improve.rs](../../../../crates/http-server/src/routers/improve.rs), after the existing module imports, add a private error type that mirrors `RememberDispatchError` ([crates/http-server/src/routers/remember.rs:407-416](../../../../crates/http-server/src/routers/remember.rs#L407-L416)) and `MemifyDispatchError` ([crates/http-server/src/routers/memify.rs:169-178](../../../../crates/http-server/src/routers/memify.rs#L169-L178)):

```rust
#[derive(Debug)]
struct ImproveDispatchError(String);

impl std::fmt::Display for ImproveDispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ImproveDispatchError {}
```

### Step 3 — Add the `run_real_improve` helper

After the new error type, add a helper with the exact signature:

```rust
async fn run_real_improve(
    components: &ComponentHandles,
    user: &AuthenticatedUser,
    dataset_id: Uuid,
    dataset_name: &str,
    session_ids: Option<&[String]>,
    node_name: Option<&[String]>,
) -> Result<(), ImproveDispatchError>
```

This is the inline equivalent of `cognee_lib::api::improve::improve` ([crates/lib/src/api/improve.rs:127-424](../../../../crates/lib/src/api/improve.rs#L127-L424)). The body must mirror the four stages, with each stage wrapped in a warning-only handler so failure in one stage does not abort later stages. Use `NoopPipelineRunRepository::arc()` for **every** inner pipeline (gap 08-07 rule — the outer `dispatch_pipeline` already wires the four-state `pipeline_runs` trail).

**Stage 0 — Resolve required handles** (mirroring the gap 03 pattern at [crates/http-server/src/routers/remember.rs:438-466](../../../../crates/http-server/src/routers/remember.rs#L438-L466)):

```rust
let graph_db = components.graph_db.clone()
    .ok_or_else(|| ImproveDispatchError("graph_db not wired in ComponentHandles".into()))?;
let vector_db = components.vector_db.clone()
    .ok_or_else(|| ImproveDispatchError("vector_db not wired in ComponentHandles".into()))?;
let embedding_engine = components.embedding_engine.clone()
    .ok_or_else(|| ImproveDispatchError("embedding_engine not wired in ComponentHandles".into()))?;
let thread_pool = components.thread_pool.clone()
    .ok_or_else(|| ImproveDispatchError("thread_pool not wired in ComponentHandles".into()))?;
let llm = components.llm.clone()
    .ok_or_else(|| ImproveDispatchError("llm not wired in ComponentHandles".into()))?;
let storage = components.storage.clone();
let database = components.database.clone();
let ontology_resolver = components.ontology_resolver.clone()
    .unwrap_or_else(|| Arc::new(NoOpOntologyResolver::new()));
```

The session-driven stages are gated on the optional slots (Python `improve.py:155-182` semantics): when `session_store` / `session_manager` / `checkpoint_store` are `None`, log a warning and skip — **do not error**.

**Stage 1 — Apply feedback weights** ([crates/lib/src/api/improve.rs:188-227](../../../../crates/lib/src/api/improve.rs#L188-L227)):

If `session_ids` is `Some(&non-empty)` and both `session_store` + `session_manager` are wired, call `cognee_cognify::apply_feedback_weights_pipeline(session_ids, user.id, 0.1, graph_db.as_ref(), session_store, session_manager).await`. On error, `tracing::warn!` and continue. The 0.1 `feedback_alpha` mirrors the Python default at `improve.py:130`.

**Stage 2 — Persist session Q&A to graph** ([crates/lib/src/api/improve.rs:229-292](../../../../crates/lib/src/api/improve.rs#L229-L292)):

If `session_ids` is `Some(&non-empty)` and `session_store` is wired, construct a fresh `AddPipeline` exactly like [crates/http-server/src/routers/remember.rs:266-275](../../../../crates/http-server/src/routers/remember.rs#L266-L275):

```rust
let add_pipeline = AddPipeline::new(
    Arc::clone(&storage),
    database.clone() as Arc<dyn cognee_database::IngestDb>,
)
.with_acl_db(database.clone() as Arc<dyn cognee_database::AclDb>)
.with_thread_pool(Arc::clone(&thread_pool))
.with_graph_db(Arc::clone(&graph_db))
.with_vector_db(Arc::clone(&vector_db))
.with_database(Arc::clone(&database))
.with_pipeline_run_repo(NoopPipelineRunRepository::arc());
```

Call `cognee_cognify::persist_sessions_in_knowledge_graph(...)` with a `CognifyConfig::default()` and an inner `NoopPipelineRunRepository::arc()`. On error, warn and continue.

**Stage 3 — Default memify enrichment** ([crates/lib/src/api/improve.rs:294-343](../../../../crates/lib/src/api/improve.rs#L294-L343), always runs):

Build `MemifyConfig::default()` (apply `with_node_name_filter(node_name)` when `node_name` is `Some` — see [crates/lib/src/api/improve.rs:295-299](../../../../crates/lib/src/api/improve.rs#L295-L299)). Call `cognee_cognify::run_memify(graph_db, vector_db, embedding_engine, thread_pool, database, NoopPipelineRunRepository::arc(), Some(dataset_id), Some(user.id), user.tenant_id, &memify_config).await`. On error, warn and continue.

**Stage 4 — Sync graph to session cache** ([crates/lib/src/api/improve.rs:345-421](../../../../crates/lib/src/api/improve.rs#L345-L421)):

If `session_ids` is `Some(&non-empty)` and both `session_manager` + `checkpoint_store` are wired, iterate over the sessions and call `cognee_cognify::sync_graph_to_session(&user.id.to_string(), sid, dataset_id, database.as_ref(), session_manager.as_ref(), checkpoint_store.as_ref(), cognee_cognify::DEFAULT_MAX_LINES).await`. Per-session failures are logged and skipped.

Return `Ok(())` after the loop. **No `.unwrap()` anywhere** — coding convention is strict.

### Step 4 — Replace the no-op stub in `post_improve`

In [crates/http-server/src/routers/improve.rs:105-114](../../../../crates/http-server/src/routers/improve.rs#L105-L114), replace the entire `// ── Dispatch ──` no-op block with the same pattern as memify ([crates/http-server/src/routers/memify.rs:89-102](../../../../crates/http-server/src/routers/memify.rs#L89-L102)):

```rust
let components_owned = state.components().cloned();
let user_for_run = user.clone();
let dataset_name_for_run = dataset_name.clone();
let session_ids_for_run = payload.session_ids.clone();
let node_name_for_run = payload.node_name.clone();

let work = box_pipeline_future(async move {
    let Some(components) = components_owned else {
        return Err(ImproveDispatchError(
            "Component handles not initialized; cannot run improve pipeline".to_string(),
        ));
    };
    run_real_improve(
        &components,
        &user_for_run,
        dataset_id,
        &dataset_name_for_run,
        session_ids_for_run.as_deref(),
        node_name_for_run.as_deref(),
    )
    .await
});
```

Leave the existing `dispatch_pipeline` call (L116-124) and the 420 outcome-mapping block (L126-181) untouched — they already handle the `RunPhase::Errored { message }` branch correctly via `ApiError::PipelineErrored { pipeline_source: Improve, .. }`.

Remove the obsolete `// E-05 scope: DTO + ImproveParams + telemetry only.` comment and the `// TODO(P5)` marker (L106-113). Replace with a short docstring on `run_real_improve` linking back to `crates/lib/src/api/improve.rs` as the canonical reference.

### Step 5 — Imports

Add to the top of [crates/http-server/src/routers/improve.rs](../../../../crates/http-server/src/routers/improve.rs):

```rust
use std::sync::Arc;
use cognee_cognify::{
    CognifyConfig, MemifyConfig, apply_feedback_weights_pipeline,
    persist_sessions_in_knowledge_graph, run_memify, sync_graph_to_session,
    memify::sync_graph_session::DEFAULT_MAX_LINES,
};
use cognee_database::{AclDb, NoopPipelineRunRepository};
use cognee_ingestion::AddPipeline;
use cognee_ontology::{NoOpOntologyResolver, OntologyResolver};

use crate::components::ComponentHandles;
```

(`IngestDb` is already imported at L13.)

Verify `cognee-ingestion`, `cognee-cognify`, `cognee-ontology` are all in [crates/http-server/Cargo.toml](../../../../crates/http-server/Cargo.toml)'s `[dependencies]`. They are — see the gap 03 wiring at [crates/http-server/src/routers/remember.rs:16-24](../../../../crates/http-server/src/routers/remember.rs#L16-L24). **No new Cargo.toml work needed.**

## Tests

### A — Rewrite `test_improve.rs` end-to-end test (mirrors `test_remember.rs` template)

Rewrite the gated skip-stub at [crates/http-server/tests/test_improve.rs:74-88](../../../../crates/http-server/tests/test_improve.rs#L74-L88) into a real integration test, modeled byte-for-byte on `post_remember_blocking_runs_full_pipeline` ([crates/http-server/tests/test_remember.rs:115-311](../../../../crates/http-server/tests/test_remember.rs#L115-L311)). Required structure:

1. **Env gate**: read `OPENAI_URL`, `OPENAI_TOKEN`, `COGNEE_E2E_EMBED_MODEL_PATH` via the existing `maybe_env` helper (mirror by copy from `test_remember.rs:77-93`). Skip with an `eprintln!` when any is missing.
2. **Build a real backend stack**: `LocalStorage`, sqlite via `connect`+`initialize`, `LadybugAdapter`, `QdrantAdapter` (384 dims), `OnnxEmbeddingEngine`, `OpenAIAdapter`, `RayonThreadPool`. Same boilerplate as `test_remember.rs:127-172`.
3. **Pre-seed the dataset**: drive `POST /api/v1/remember` once (small text payload like `"Alice met Bob in Paris."`) so the graph has real edges and vector triplets for improve to enrich. Capture the resulting `dataset_id` from the response.
4. **Build the `ComponentHandles`** with the new `checkpoint_store: None` field (improve stages 1, 2, 4 will be skipped — that's OK; the test asserts Stage 3 ran and the response shape is correct). For a richer test, optionally wire `Some(Arc::new(SeaOrmCheckpointStore::new(Arc::clone(&database))))` and a `SessionStore` so stages 1+4 also run.
5. **Drive `POST /api/v1/improve`** with `{"datasetName": "improve_e2e_blocking", "runInBackground": false}` and a JSON body (improve is `application/json`, not multipart — see [crates/http-server/src/routers/improve.rs:35](../../../../crates/http-server/src/routers/improve.rs#L35)).
6. **Assertions** — must include **at least one downstream side effect**, not just a status code:
   - `status == 200`.
   - Response body: `status == "PipelineRunCompleted"`, `pipeline_run_id` is a valid UUID string, `dataset_id` matches, `dataset_name == "improve_e2e_blocking"`.
   - **Downstream**: `vector_db.collection_size("Triplet", "text").await > 0` — Stage 3 (memify) re-ran and re-populated the triplet collection.
   - Compare the graph edge count before vs. after the improve call — must be `>=` (memify enrichment may add triplet edges but is allowed to be idempotent).
7. **420-on-error variant**: add a second test `post_improve_inner_failure_returns_420` that wires an intentionally broken `ComponentHandles` (e.g., `graph_db: None`) and asserts the response is HTTP 420 with the raw `PipelineRunInfoDTO` body shape (`status == "PipelineRunErrored"`, `error` field populated with the missing-component message). This locks in the Python 420 quirk.

### B — Inline router test asserting missing-component behavior

Add to the `mod tests` block in [crates/http-server/src/routers/improve.rs](../../../../crates/http-server/src/routers/improve.rs) (mirroring `post_memify_with_dataset_id_surfaces_missing_components` at [crates/http-server/src/routers/memify.rs:349-382](../../../../crates/http-server/src/routers/memify.rs#L349-L382)):

```rust
#[tokio::test]
async fn post_improve_with_dataset_id_surfaces_missing_components() {
    // The default test state has no ComponentHandles wired. The blocking
    // dispatch now reaches `run_real_improve`, which surfaces the missing
    // graph_db / vector_db / etc. as a `RunPhase::Errored { message }` →
    // the 420 quirk envelope.
    let state = test_state().await;
    let app = Router::new()
        .route("/", post(post_improve_no_auth))
        .with_state(state);

    let dataset_id = Uuid::new_v4();
    let body = json!({ "datasetId": dataset_id.to_string(), "run_in_background": false });
    let req = Request::builder()
        .method("POST").uri("/")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap();

    let resp = app.oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status().as_u16(), 420,
        "missing components must surface through the improve 420 quirk");

    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(v["status"], "PipelineRunErrored");
    let err = v["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("Component handles not initialized")
            || err.contains("not wired in ComponentHandles"),
        "error must point at the missing-component path, got: {err}"
    );
}
```

This is the regression guard that would flip if the no-op stub were ever reintroduced (200 vs. 420).

### C — Regression guards already passing

The five existing E-05 v2 wire-shape tests at [crates/http-server/tests/test_improve.rs:99-249](../../../../crates/http-server/tests/test_improve.rs#L99-L249) (`session_ids_accepted_*`, `extraction_tasks_and_enrichment_tasks_passed_through`, `node_name_camelcase_and_alias`, `data_field_round_trip`) currently assert `status == 200` because the no-op stub always succeeds. After this gap lands they must be updated to **expect 420** when no backends are wired (matching test B), or pre-seed `ComponentHandles` to drive a real `200`.

The preferred fix: change them to assert `status == 200 || status == 420` with a body-shape check on the 420 branch, and document this in a `// Wire-shape only — body content is gap-02-conditional` comment. This preserves their original intent (DTO field plumbing) without coupling them to the new pipeline path.

The existing 420 unit test at [crates/http-server/src/routers/improve.rs:349-389](../../../../crates/http-server/src/routers/improve.rs#L349-L389) (`post_improve_420_via_error_response`) continues to pass unchanged — it builds the error response directly.

## Acceptance criteria

- [ ] [crates/http-server/src/routers/improve.rs:114](../../../../crates/http-server/src/routers/improve.rs#L114) no longer ships `box_pipeline_future(async move { Ok::<(), std::io::Error>(()) })`; the future calls `run_real_improve`.
- [ ] `// Blocking gap stub` and `// TODO(P5)` markers at [crates/http-server/src/routers/improve.rs:106-113](../../../../crates/http-server/src/routers/improve.rs#L106-L113) are removed.
- [ ] `ComponentHandles` has a new `checkpoint_store: Option<Arc<dyn CheckpointStore>>` slot and every existing literal initializer defaults it to `None`.
- [ ] `run_real_improve` mirrors `cognee_lib::api::improve::improve` stage-by-stage, each stage wrapped in a warning-only handler.
- [ ] Every inner pipeline (memify, persist_sessions_in_knowledge_graph) uses `NoopPipelineRunRepository::arc()` per gap 08-07.
- [ ] No `.unwrap()` in non-test code in `routers/improve.rs`.
- [ ] `test_improve.rs::post_improve_blocking_runs_full_pipeline` (the rewritten integration test) asserts at least one downstream side effect — `vector_db.collection_size("Triplet", "text") > 0` after the run.
- [ ] `routers::improve::tests::post_improve_with_dataset_id_surfaces_missing_components` is added and asserts a 420 envelope when `ComponentHandles` is absent.
- [ ] The 420 unit test `post_improve_420_via_error_response` still passes unchanged.
- [ ] [crates/http-server/Cargo.toml](../../../../crates/http-server/Cargo.toml) is **not** edited (no new dependencies are needed; cycle constraint preserved).
- [ ] [docs/http-server/gaps/README.md](../README.md) Tier-1 row for gap 2 is updated from `**not-started**` to `**landed**` once verified.

## Status

**not-started**
