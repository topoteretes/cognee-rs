# Task 03-05 — Task lifecycle events (`${task_type} Task Started/Completed/Errored`)

**Status**: ⬜ unimplemented
**Status**: implemented in commit 365e6ad
**Owner**: _unassigned_
**Depends on**:
- [Task 03-01 — `tenant_id` plumbing](01-tenant-id-plumbing.md) (reads `ctx.pipeline_ctx.tenant_id`).
- [Task 03-02 — `Task::python_task_type()`](02-task-type-mapping.md) (renders the event-name template).

**Blocks**:
- [Task 03-08 — Tests](08-tests.md).

**Parent doc**: [03 — Pipeline / Task / API Operation Events](../03-pipeline-task-api-events.md)
**Locked decisions**: #1 (tenant_id), #6 (omit task index / pipeline_run_id), #7 (once-per-task, not per-attempt).

---

## 1. Goal

Emit three new analytics events at the `cognee_core::pipeline::call_with_retry()`
boundary — one each per task lifecycle stage:

| Event | Trigger | Payload |
|---|---|---|
| `${task_type} Task Started` | Just before the retry loop ([line 1107](../../crates/core/src/pipeline.rs#L1107) — first attempt only, **once per task**) | `task_name`, `cognee_version`, `tenant_id` |
| `${task_type} Task Completed` | In the `Ok(resolved)` arm ([line 1152](../../crates/core/src/pipeline.rs#L1152) — first successful attempt only) | same |
| `${task_type} Task Errored` | After retries are exhausted ([line 1184](../../crates/core/src/pipeline.rs#L1184)) | same — **no error string** (Python parity) |

`${task_type}` is rendered via `Task::python_task_type()` from
[task 03-02](02-task-type-mapping.md) and resolves to one of:
`Function`, `Coroutine`, `Generator`, `Async Generator`.

`task_name` falls back to `"unknown"` when the optional
`task_name: Option<&str>` parameter at
[line 1083](../../crates/core/src/pipeline.rs#L1083) is `None` —
matching the existing OTEL-span fallback at
[line 1092](../../crates/core/src/pipeline.rs#L1092).

## 2. Rationale

### 2.1 Once per task, not per attempt

Python has no retry layer at this point in the codebase, so the
question of "per attempt" never arises there. Locked decision 7
mirrors that: emit `Started` exactly once before the first attempt,
`Completed` exactly once on the first success, `Errored` exactly
once after all retries are exhausted. Internal retry attempts do
not surface to the analytics layer.

This matches the user's mental model of a "task" (the executable
they registered) rather than the wire-level retry attempts.

### 2.2 `${task_type}` is a runtime template

Because the event name varies per task variant, we cannot ship
fixed string literals. The emit helper takes `&Task` and renders
the name lazily:

```rust
let event_name = format!("{} Task Started", task.python_task_type());
```

This is one allocation per task lifecycle event — comparable to the
existing OTEL span overhead. Optimising it (e.g. with a small lookup
table of pre-formatted strings) is premature.

### 2.3 No error string property — same reasoning as task 03-04

Python's `${task_type} Task Errored` likewise omits the error
string from the analytics payload (only logs it locally). Mirror
that. Operators read errors from logs / OTEL spans
([pipeline.rs:1188](../../crates/core/src/pipeline.rs#L1188)
records `task.error` on the OTEL span in the terminal-failure
block, with a transient per-attempt record at
[line 1159](../../crates/core/src/pipeline.rs#L1159)), which
have their own redaction layer.

### 2.4 Where to read `tenant_id` from

`call_with_retry()` already accepts `env: &ExecEnv<'_>` which
exposes `env.ctx.pipeline_ctx`. Read `tenant_id` from there:

```rust
let tenant_id = env.ctx.pipeline_ctx.as_ref().and_then(|p| p.tenant_id);
```

Same pattern as how `user_id` and `dataset_id` are already extracted
in `execute()` (e.g. line 1128 inside `call_with_retry`'s provenance block).

## 3. Pre-conditions

- [Task 03-01](01-tenant-id-plumbing.md) merged — `PipelineContext.tenant_id`
  exists and is propagated.
- [Task 03-02](02-task-type-mapping.md) merged — `Task::python_task_type`
  and `cognee_telemetry::cognee_version` exist.
- A clean `cargo check --all-targets` on the post-tasks-01-and-02 tree.

## 4. Step-by-step

### 4.1 Add the emit helper

In `crates/core/src/pipeline.rs`, near the existing `emit_pipeline_event`
helper from [task 03-04](04-pipeline-lifecycle-events.md) (or below
`call_with_retry` if 03-04 is not yet merged into the working tree):

```rust
#[cfg(feature = "telemetry")]
fn emit_task_event(
    stage: &'static str,        // "Started" | "Completed" | "Errored"
    task: &Task,
    task_name: Option<&str>,
    user_id: Option<Uuid>,
    tenant_id: Option<Uuid>,
) {
    let event_name = format!("{} Task {}", task.python_task_type(), stage);
    let props = serde_json::json!({
        "task_name": task_name.unwrap_or("unknown"),
        "cognee_version": cognee_telemetry::cognee_version(),
        "tenant_id": tenant_id_for_telemetry(tenant_id),
    });
    cognee_telemetry::send_telemetry(&event_name, user_id, Some(props));
}

#[cfg(not(feature = "telemetry"))]
#[inline]
fn emit_task_event(
    _stage: &'static str,
    _task: &Task,
    _task_name: Option<&str>,
    _user_id: Option<Uuid>,
    _tenant_id: Option<Uuid>,
) {
}
```

### 4.2 Wire `Started` before the retry loop

In `call_with_retry()` ([line 1079](../../crates/core/src/pipeline.rs#L1079)),
the existing structure is:

```rust
async fn call_with_retry(
    task: &Task,
    input: Arc<dyn Value>,
    task_index: usize,
    task_name: Option<&str>,
    data_id: Option<&str>,
    summary_template: Option<&str>,
    env: &ExecEnv<'_>,
) -> Result<Resolved, ExecutionError> {
    #[cfg(feature = "telemetry")]
    let span = tracing::info_span!(...);

    let max_attempts = env.policy.max_attempts();
    let mut last_error: Option<TaskError> = None;
    let subtoken = env.task_subtokens[task_index].clone();
    let scoped_ctx = env.ctx.with_progress(subtoken);
    let task_ctx = scoped_ctx.with_current_data(input.clone());

    for attempt in 1..=max_attempts {
        // ... call & match ...
    }
    // ... terminal failure ...
}
```

Insert the `Started` emit between the `let task_ctx = …` line and
the `for attempt in 1..=max_attempts` loop:

```rust
let user_id = env.ctx.pipeline_ctx.as_ref().and_then(|p| p.user_id);
let tenant_id = env.ctx.pipeline_ctx.as_ref().and_then(|p| p.tenant_id);

emit_task_event("Started", task, task_name, user_id, tenant_id);

for attempt in 1..=max_attempts {
    // …
}
```

### 4.3 Wire `Completed` in the success arm

The success arm starts at [line 1110](../../crates/core/src/pipeline.rs#L1110).
Just before `return Ok(resolved);` at
[line 1152](../../crates/core/src/pipeline.rs#L1152), add:

```rust
emit_task_event("Completed", task, task_name, user_id, tenant_id);
return Ok(resolved);
```

### 4.4 Wire `Errored` in the terminal-failure block

After the retry loop exits (line 1184-1188), the existing code reads:

```rust
let source = last_error.expect("loop ran at least once");
let error_str = source.to_string();

#[cfg(feature = "telemetry")]
span.record("task.error", error_str.as_str());

env.watcher
    .on_task(...)
    .await;
```

Insert the `Errored` emit after the `span.record` but before the
`env.watcher.on_task` call (so the order matches Started/Completed
relative to the watcher):

```rust
let source = last_error.expect("loop ran at least once");
let error_str = source.to_string();

#[cfg(feature = "telemetry")]
span.record("task.error", error_str.as_str());

emit_task_event("Errored", task, task_name, user_id, tenant_id);

env.watcher
    .on_task(...)
    .await;
```

### 4.5 Confirm scoping

The `user_id` / `tenant_id` reads must be **outside** the retry loop
so they happen exactly once per task. Sub-agent C must verify, when
reading the diff, that no `emit_task_event` call is inside the
`for attempt` loop.

## 5. Verification

```bash
# 1. Compile both feature states.
cargo check --all-targets
cargo check --all-targets --no-default-features

# 2. Run the existing core tests — Started/Completed/Errored
#    should not change the test outcomes (events are fire-and-forget).
cargo test -p cognee-core --tests

# 3. Clippy.
cargo clippy --all-targets -- -D warnings

# 4. Full check.
scripts/check_all.sh
```

End-to-end assertion (4-event sequence: `Pipeline Run Started`,
`<task_type> Task Started`, `<task_type> Task Completed`, `Pipeline
Run Completed` — and the error variant) lives in
[task 03-08](08-tests.md). This task ships only the wiring.

## 6. Files modified

- [`crates/core/src/pipeline.rs`](../../crates/core/src/pipeline.rs)
  — add `emit_task_event` helper and three call sites in
  `call_with_retry`.

Task 03-04 has been merged (commit 694dd5a) so the
`cognee-telemetry` crate-dep wiring in `crates/core/Cargo.toml`
and the sibling `emit_pipeline_event` helper are already in
place. This task only needs to add the new `emit_task_event`
helper and the three call sites described above.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Per-attempt emission accidentally introduced (e.g. emitter inside the `for attempt` loop) | Possible during refactor. | Sub-agent C reviews diff and asserts emission is outside the loop. Integration test in [03-08](08-tests.md) counts events — `Started: 1, Completed: 1` for a 1-task pipeline regardless of `max_attempts`. |
| `Task::python_task_type` doesn't exist when this task runs first | Hard dep — sub-agent A enforces. | Pre-condition gate. |
| Emission order vs `env.watcher.on_task(...)` calls confuses tests | Pin order: telemetry first, then watcher (matches `execute()` ordering — locked by precedent). | Document the ordering in a comment at the call site. |

## 8. Out of scope

- Adding `task_index`, `attempt_count`, or `error_message` to the
  payload (locked — Python omits all three).
- Per-attempt analytics (locked decision 7).
- Adding a per-task duration field — wall-clock duration belongs on
  the OTEL span, not on a fire-and-forget analytics event.
