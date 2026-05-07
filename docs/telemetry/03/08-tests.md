# Task 03-08 — Unit + integration tests

**Status**: ⬜ unimplemented
**Owner**: _unassigned_
**Depends on**:
- [Task 03-04 — Pipeline lifecycle events](04-pipeline-lifecycle-events.md)
- [Task 03-05 — Task lifecycle events](05-task-lifecycle-events.md)
- [Task 03-06 — `cognee.search EXECUTION STARTED`](06-search-execution-events.md)

**Blocks**:
- [Task 03-09 — User docs + CI](09-docs-and-ci.md).

**Parent doc**: [03 — Pipeline / Task / API Operation Events](../03-pipeline-task-api-events.md)

---

## 1. Goal

A combined unit + integration test suite that locks the wire format
and emission ordering of every event introduced by gap 03. Five
test groups:

1. **`Task::python_task_type` mapping** — 8 variants → 4 strings.
   Already partly covered by the inline test added in
   [task 03-02](02-task-type-mapping.md); extend if needed.
2. **`Config::telemetry_snapshot` allowlist & redaction** — already
   covered by inline tests added in [task 03-03](03-settings-snapshot.md).
3. **Full pipeline lifecycle integration test** — drive a tiny
   pipeline through `cognee_core::execute()` against a mockito proxy.
   Assert the 4-event sequence in order:

   ```
   Pipeline Run Started
   Coroutine Task Started
   Coroutine Task Completed
   Pipeline Run Completed
   ```

4. **Error-path test** — inject a failing task, assert
   `Coroutine Task Errored` and `Pipeline Run Errored` fire (in
   order, after retries are exhausted).
5. **Opt-out + fire-and-forget** — `TELEMETRY_DISABLED=1` produces
   zero POSTs; a stalled proxy (5-second sleep) does not block the
   pipeline (wall-clock < 100 ms for a no-op pipeline).

## 2. Rationale

The wire format is the contract with the analytics dashboards. A
snapshot-quality test on the property dict is the cheapest insurance
against silent regressions.

The order assertion is the load-bearing one for telemetry consumers:
the analytics dashboard shows funnel views built on event ordering.
A swap of `Started` and `Completed` (e.g. after a refactor) would
silently break those funnels — the test catches it.

The fire-and-forget timing check is what separates this from a
plain logging library. Pipeline hot loops emit task events on every
iteration; if the dispatch ever blocked, the pipeline runtime would
collapse.

## 3. Pre-conditions

- Tasks 03-04, 03-05, 03-06 merged. ✅ verified — `emit_pipeline_event`
  at [`crates/core/src/pipeline.rs:514`](../../crates/core/src/pipeline.rs#L514)
  with call sites at [:662](../../crates/core/src/pipeline.rs#L662) (Started),
  [:714](../../crates/core/src/pipeline.rs#L714)/[:734](../../crates/core/src/pipeline.rs#L734)/[:764](../../crates/core/src/pipeline.rs#L764)
  (Completed/Errored arms); `emit_task_event` at
  [`pipeline.rs:571`](../../crates/core/src/pipeline.rs#L571) called from
  [:1160](../../crates/core/src/pipeline.rs#L1160),
  [:1207](../../crates/core/src/pipeline.rs#L1207),
  [:1247](../../crates/core/src/pipeline.rs#L1247) inside `call_with_retry`;
  `emit_search_started`/`emit_search_completed` at
  [`crates/search/src/orchestration/search_orchestrator.rs:19`](../../crates/search/src/orchestration/search_orchestrator.rs#L19)/[:41](../../crates/search/src/orchestration/search_orchestrator.rs#L41)
  with call sites at [:163](../../crates/search/src/orchestration/search_orchestrator.rs#L163),
  [:341](../../crates/search/src/orchestration/search_orchestrator.rs#L341),
  [:385](../../crates/search/src/orchestration/search_orchestrator.rs#L385),
  [:434](../../crates/search/src/orchestration/search_orchestrator.rs#L434).
- `cargo check --all-targets` passes (verified 2026-05-07).
- `mockito = "1"`, `insta = "1"`, `serial_test` are workspace
  dev-deps (added by gap 02-09); they are **not** yet listed in
  [`crates/core/Cargo.toml`'s `[dev-dependencies]`](../../crates/core/Cargo.toml#L1)
  — task 03-08 must add them. (`crates/search/Cargo.toml` already has
  `serial_test.workspace = true`.)

## 4. Step-by-step

### 4.1 Where the integration tests live

Two valid choices:

- **`crates/core/tests/pipeline_telemetry_events.rs`** — closest
  to the wired call sites, can directly invoke `execute()` with a
  hand-built `Pipeline` and `Task::async_typed`. **Preferred**:
  this is where the existing pipeline tests live (e.g.
  `pipeline_payload_events.rs`).
- **`crates/telemetry/tests/pipeline_telemetry_events.rs`** —
  symmetric with the gap-02 integration tests but would require
  pulling `cognee-core` as a dev-dep, which it doesn't today.

**Recommend `crates/core/tests/pipeline_telemetry_events.rs`.**
Sub-agent A should add `mockito = "1"` to `crates/core/Cargo.toml`'s
`[dev-dependencies]` if not already present.

### 4.2 Test scaffolding

Reuse the gap-02 integration test pattern (sketched in
[`crates/telemetry/tests/`](../../crates/telemetry/tests/)). Each
test follows this shape:

```rust
use mockito::Server;

#[tokio::test]
async fn pipeline_lifecycle_emits_4_events_in_order() {
    let mut server = Server::new_async().await;
    let m = server
        .mock("POST", "/")
        .with_status(200)
        .expect_at_least(4)
        .create_async()
        .await;

    // Override the proxy URL via the gap-02-09 test override hook.
    // The env var is `COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS` and is
    // honored only when `COGNEE_TELEMETRY_INTEGRATION_TEST=1` is also
    // set (see `crates/telemetry/src/env.rs:48-65`). Mirror the
    // pattern used by `crates/telemetry/tests/dispatch_with_mockito.rs`.
    std::env::set_var("COGNEE_TELEMETRY_INTEGRATION_TEST", "1");
    std::env::set_var("COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS", server.url());

    // Build a 1-task pipeline.
    let task = Task::async_typed(|_input: Arc<dyn Value>, _ctx| async move {
        Ok(Box::new(()) as Box<dyn Value>)
    });
    let pipeline = Pipeline {
        id: Uuid::new_v4(),
        name: Some("test_pipeline".into()),
        tasks: vec![task],
        // ... other fields ...
        telemetry_settings: Some(serde_json::Map::new()),
    };

    let ctx = build_test_task_context_with_tenant(Some(Uuid::new_v4()));
    let outputs = execute(&pipeline, vec![Arc::new(())], ctx, &NoopWatcher).await.unwrap();

    // Wait briefly for the fire-and-forget POSTs to land.
    tokio::time::sleep(Duration::from_millis(200)).await;

    m.assert_async().await;

    // The mockito mock records request bodies; pull them and assert the
    // event_name field is in the expected order.
    // (mockito's `received_requests()` API or a custom recording handler.)
}
```

> **Note on the proxy URL override:** gap-02-09 already provides the
> hook. `crates/telemetry/src/env.rs::proxy_url()` checks for
> `COGNEE_TELEMETRY_INTEGRATION_TEST=1` + `COGNEE_TELEMETRY_PROXY_URL_FOR_TESTS=<url>`
> and falls back to `https://test.prometh.ai` otherwise. The wire
> never leaves `127.0.0.1` in tests as long as **both** env vars are
> set. Use `serial_test::serial` on every test that mutates these
> env vars (the override is process-global).

### 4.3 Test cases

#### 4.3.1 Happy-path 4-event sequence

```
test_name: pipeline_lifecycle_emits_4_events_in_order
```

- 1-task `Async` pipeline with `pipeline.name = "test"`.
- After `execute()` returns, sleep 200 ms, then assert the proxy
  saw exactly 4 POSTs.
- Assert event names in order: `["Pipeline Run Started",
  "Coroutine Task Started", "Coroutine Task Completed",
  "Pipeline Run Completed"]`.
- Assert `tenant_id` is `"Single User Tenant"` when the test passes
  `None`, and the formatted UUID when the test passes `Some(uuid)`.

#### 4.3.2 Variant matrix

```
test_name: task_type_strings_match_python_for_each_variant
```

Drive a single-task pipeline four times, once per variant family:

| Pipeline contains | Expected event names |
|---|---|
| `Task::sync_typed` | `Function Task Started`, `Function Task Completed` |
| `Task::async_typed` | `Coroutine Task Started`, `Coroutine Task Completed` |
| `Task::sync_iter_typed` | `Generator Task Started`, `Generator Task Completed` |
| `Task::async_stream_typed` | `Async Generator Task Started`, `Async Generator Task Completed` |

Each cycle resets the mockito server.

#### 4.3.3 Error path

```
test_name: pipeline_run_errored_fires_after_retries_exhausted
```

- Build a `Task::async_typed` whose body always returns
  `Err(TaskError::...)`.
- Pipeline `retry_policy` set to `max_attempts = 2`.
- Run `execute()`, expect `Err(ExecutionError::TaskFailed)`.
- Assert exactly 4 events, in order: `Pipeline Run Started`,
  `Coroutine Task Started`, `Coroutine Task Errored`, `Pipeline
  Run Errored`. (Note: only **one** `Task Started` despite 2
  attempts — this validates locked decision 7.)
- No `error` property on either Errored event (Python parity).

#### 4.3.4 Opt-out

```
test_name: telemetry_disabled_emits_zero_events
```

- Same as 4.3.1 but with `TELEMETRY_DISABLED=1`.
- Assert the mockito mock saw **zero** POSTs.
- Use `serial_test::serial` because the test sets a process-wide
  env var.

#### 4.3.5 Fire-and-forget timing

```
test_name: stalled_proxy_does_not_block_pipeline
```

- Mockito mock with `with_chunked_body` + a 5-second delay
  (or `tokio::time::sleep` inside a custom handler).
- 1-task no-op pipeline.
- Wall-clock from `execute()` start to return must be < 100 ms.
- Use `tokio::time::Instant::now()` to measure.

#### 4.3.6 Search lifecycle

```
test_name: search_emits_started_then_completed
```

- Build a minimal `SearchOrchestrator` + `SearchRequest` (the
  existing search test fixtures cover this).
- Drive one `search()` call.
- Assert the proxy saw exactly 2 POSTs in order:
  `cognee.search EXECUTION STARTED`, `cognee.search EXECUTION COMPLETED`.

### 4.4 Property-shape snapshot

For one happy-path case, capture the full payload body and lock it
with `insta::assert_json_snapshot!`:

```rust
insta::assert_json_snapshot!("pipeline_run_started_payload",
    captured_payloads[0]);
```

Re-snap on intentional schema changes. Keep the snapshot under
[`crates/core/tests/snapshots/`](../../crates/core/tests/).

The snapshot must use a JSON-stable shape — e.g. mask the volatile
identity fields:

```yaml
{
    "anonymous_id": "[uuid]",
    "event_name": "Pipeline Run Started",
    "user_properties": { ... },
    "properties": {
        "pipeline_name": "test",
        "cognee_version": "[version]",
        "tenant_id": "Single User Tenant",
        "sdk_runtime": "rust",
        "vector_db_provider": "lancedb",
        // ... full allowlist ...
    }
}
```

Use `insta`'s `redactions` config to wildcard the volatile fields.

## 5. Verification

```bash
# 1. Run the new tests.
cargo test -p cognee-core --test pipeline_telemetry_events

# 2. Run them with telemetry feature explicitly off — they must
#    still compile (the test file is feature-gated where needed).
cargo test -p cognee-core --test pipeline_telemetry_events --no-default-features

# 3. Clippy (catches any unused-import drift).
cargo clippy --all-targets -- -D warnings

# 4. Full check.
scripts/check_all.sh

# 5. (Manual) confirm `insta review` shows the snapshot once before
#    landing.
cargo install cargo-insta  # if not already
cargo insta review
```

## 6. Files modified

- `crates/core/tests/pipeline_telemetry_events.rs` — **new**.
- `crates/core/tests/snapshots/` — **new** snapshot dir.
- [`crates/core/Cargo.toml`](../../crates/core/Cargo.toml) — add
  `mockito.workspace = true`, `insta.workspace = true`, and
  `serial_test.workspace = true` to `[dev-dependencies]`. Confirmed
  not yet present as of 2026-05-07; the workspace itself already
  declares them via gap 02.
- (Possibly) [`crates/search/tests/`](../../crates/search/tests/) —
  add `search_telemetry_events.rs` if the search test goes there
  rather than `cognee-core`.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Tests are flaky due to fire-and-forget timing | Real risk — needs the `tokio::time::sleep(200ms)` after `execute()` returns. | The 200 ms drain window is generous; if it remains flaky in CI, lift to 500 ms or use a custom mockito handler with an explicit completion signal. |
| Multiple tests sharing process env vars (`TELEMETRY_DISABLED`) collide | Real. | `#[serial_test::serial]` on every test that sets envs. |
| `mockito::Server::new_async()` clashes with the gap-02-09 patterns | Should match. | Sub-agent A reuses the existing pattern from `crates/telemetry/tests/`. |
| Snapshot churn on every cognee version bump | Yes — `cognee_version` field changes every release. | `insta`'s `redactions` config: replace `cognee_version` with `"[version]"` before comparing. |
| Production paths bypass `execute()` so the integration test is unrepresentative | The test exercises `execute()` directly, which is what the events fire from. | Document in the test file's module-level doc comment that the test asserts emission semantics, not production-path coverage. |

## 8. Out of scope

- A cross-SDK parity test for the lifecycle events. Python's payload
  schema for these events is byte-equal to gap-02's `cognee.recall`
  payload's identity fields, which the gap-02-10 cross-SDK test
  already covers. A separate parity test for the lifecycle payload
  is **future work** if dashboards report drift.
- Performance benchmarks. The fire-and-forget timing test is a
  smoke check; full benchmarking belongs in the perf-test track.

**Status**: implemented in commit 07356c7 (note: search-orchestrator test from §4.3.6 skipped — would create backwards crate dep cognee-core → cognee-search → cognee-core; existing search byte-parity coverage from gap-02 is retained)
