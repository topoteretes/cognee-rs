# Task 03-04 — Pipeline lifecycle events (`Pipeline Run Started/Completed/Errored`)

**Status**: ⬜ unimplemented
**Owner**: _unassigned_
**Depends on**:
- [Task 03-01 — `tenant_id` plumbing](01-tenant-id-plumbing.md) (reads `run_info.tenant_id` and the helper that formats it for the wire).
- [Task 03-03 — Settings snapshot](03-settings-snapshot.md) (`Config::telemetry_snapshot()` is the `\| config` merge).

**Blocks**:
- [Task 03-08 — Tests](08-tests.md) (asserts the 4-event sequence end-to-end).

**Parent doc**: [03 — Pipeline / Task / API Operation Events](../03-pipeline-task-api-events.md)
**Locked decisions**: #1 (tenant_id), #5 (settings allowlist), #6 (omit dataset_id / pipeline_run_id).

---

## 1. Goal

Emit three new analytics events at the `cognee_core::pipeline::execute()`
boundary — one each for the pipeline-run lifecycle:

| Event | Trigger | Payload (besides identity from [`cognee-telemetry`]) |
|---|---|---|
| `Pipeline Run Started` | After `watcher.on_pipeline_run_started(&run_info).await` ([line 538](../../crates/core/src/pipeline.rs#L538)) | `pipeline_name`, `cognee_version`, `tenant_id`, plus the merged `Settings::telemetry_snapshot()` |
| `Pipeline Run Completed` | In the `Ok` arm after `watcher.on_pipeline_run_completed(...)` ([line 580](../../crates/core/src/pipeline.rs#L580)) | same as Started |
| `Pipeline Run Errored` | In both error arms (`Cancelled` at [583](../../crates/core/src/pipeline.rs#L583) and generic `Err` at [593](../../crates/core/src/pipeline.rs#L593)) | same as Started — **no error string** on the wire (Python parity, see [§2.2](#22-no-error-property-on-the-wire)) |

Two key shape decisions enforced by locked decision 6:

- **No `dataset_id`** on the analytics payload (Python omits it).
- **No `pipeline_run_id`** on the analytics payload (Python omits it).

Both remain on the existing OTEL span attributes — the analytics
payload is intentionally narrower than the span.

## 2. Rationale

### 2.1 Why emit from `execute()`, not from `PipelineWatcher`

The Rust pipeline runner has a richer `PipelineWatcher` trait
(`on_pipeline_run_started/completed/errored`) which is **observability**,
not analytics. Hooking emission into the watcher is tempting, but
mixing wire-format analytics into a public extension trait conflates
concerns: a downstream consumer who implements `PipelineWatcher`
should not unwittingly cause analytics POSTs.

**Decision:** emit directly from `execute()` in
[`crates/core/src/pipeline.rs`](../../crates/core/src/pipeline.rs),
*next to* the watcher calls, gated on the `telemetry` feature. The
watcher remains a structural callback.

### 2.2 No error property on the wire

Python's `Pipeline Run Errored` does **not** include the exception
string in the analytics payload (only logs it locally). We mirror
that. Reasoning: error strings can carry user data (filenames,
queries, uploaded content paths) that the proxy redactor cannot be
trusted to scrub for arbitrary callers. Keep them out of analytics
entirely; operators read errors from logs / OTEL spans, which are
gated by their own redaction.

The OTEL span at the watcher level still carries the error string —
that path is for operator-facing tracing, not for analytics dashboards.

### 2.3 Source of `pipeline_name`

`run_info.pipeline_name` is already populated from
[`pipeline.name.clone().unwrap_or_default()`](../../crates/core/src/pipeline.rs#L522).
When the caller did not name the pipeline, Python emits an empty
string (matching `str(None)` → `""`). The Rust port already
produces the empty string in this case, so reuse it as-is. Do **not**
inject a `"unnamed_pipeline"` placeholder — that would break Python
parity for unnamed pipelines.

### 2.4 Why a small helper, not three inline closures

Three terminal arms emit either Started, Completed, or Errored with
the same property set. Extract a single helper to construct the
property dict so adding a new field touches one place:

```rust
fn pipeline_event_properties(
    pipeline_name: &str,
    tenant_id: Option<Uuid>,
    settings: serde_json::Map<String, serde_json::Value>,
    cognify_token_counter: Option<&str>,  // see task 03-03 §4.2
) -> serde_json::Value { /* … */ }
```

## 3. Pre-conditions

- [Task 03-01](01-tenant-id-plumbing.md) merged — `run_info.tenant_id`
  and `tenant_id_for_telemetry` helper exist.
- [Task 03-03](03-settings-snapshot.md) merged — `Config::telemetry_snapshot()`
  exists.
- A clean `cargo check --all-targets` on the post-tasks-01-and-03 tree.

## 4. Step-by-step

### 4.1 Resolve the `Config` access path

`cognee_core::execute()` does **not** import `cognee_lib::Config`
today. Two options:

- **Option A — caller passes the snapshot in.** Extend
  `cognee_core::Pipeline` with an optional `telemetry_settings:
  Option<serde_json::Map<String, serde_json::Value>>` field that the
  caller (cognee-lib, tests) populates from `Config::telemetry_snapshot()`.
  `execute()` passes it into the emit helper.
- **Option B — read from a process-level `OnceLock<Config>`** stored
  by `cognee-lib` initialisation. Avoids the field on `Pipeline` but
  introduces a hidden global that is awkward to override in tests.

**Recommend Option A.** Implementation sketch:

```rust
// crates/core/src/pipeline.rs (struct already exists, around line ~250)
pub struct Pipeline {
    // existing fields…

    /// Optional pre-built telemetry settings snapshot. When `None`,
    /// `Pipeline Run *` events emit with an empty `\| config` merge.
    /// Populated by `cognee-lib` from `Config::telemetry_snapshot()`.
    pub telemetry_settings: Option<serde_json::Map<String, serde_json::Value>>,
}
```

Sub-agent A should re-confirm this design choice before sub-agent B
edits — if there is already a "config-snapshot pass-through" mechanism
the doc was unaware of, prefer that.

### 4.2 Add the emit helper

In `crates/core/src/pipeline.rs`, near the top of the file or as a
free function below `execute()`:

```rust
#[cfg(feature = "telemetry")]
fn emit_pipeline_event(
    event_name: &str,
    user_id: Option<Uuid>,
    pipeline_name: &str,
    tenant_id: Option<Uuid>,
    settings: Option<&serde_json::Map<String, serde_json::Value>>,
) {
    use serde_json::{Map, Value};

    let mut props: Map<String, Value> = settings.cloned().unwrap_or_default();
    props.insert(
        "pipeline_name".into(),
        Value::String(pipeline_name.to_string()),
    );
    props.insert(
        "cognee_version".into(),
        Value::String(cognee_telemetry::cognee_version().to_string()),
    );
    props.insert(
        "tenant_id".into(),
        Value::String(tenant_id_for_telemetry(tenant_id)),
    );

    cognee_telemetry::send_telemetry(event_name, user_id, Some(Value::Object(props)));
}

#[cfg(not(feature = "telemetry"))]
#[inline]
fn emit_pipeline_event(
    _event_name: &str,
    _user_id: Option<Uuid>,
    _pipeline_name: &str,
    _tenant_id: Option<Uuid>,
    _settings: Option<&serde_json::Map<String, serde_json::Value>>,
) {
}
```

Notes:

- The `tenant_id` is always present on the wire (Python parity).
- `settings` is the merged-in `\| config` allowlist; the helper takes
  it by reference and clones into a fresh `Map` so the same snapshot
  can be reused across the three terminal arms without re-allocation
  in the no-op cases.
- `cognee_telemetry::send_telemetry` is the gap-02 entry point —
  fire-and-forget. The helper itself is also `#[cfg(feature =
  "telemetry")]` to keep `serde_json::Map` references out of the
  no-feature build.

### 4.3 Wire the three call sites

In `execute()` ([line 490](../../crates/core/src/pipeline.rs#L490)),
read `tenant_id` from `ctx.pipeline_ctx` and the settings from
`pipeline.telemetry_settings`. The current code at lines 513-538
becomes:

```rust
let user_id = ctx.pipeline_ctx.as_ref().and_then(|p| p.user_id);
let tenant_id = ctx.pipeline_ctx.as_ref().and_then(|p| p.tenant_id);
let dataset_id = ctx.pipeline_ctx.as_ref().and_then(|p| p.dataset_id);
let pipeline_id = deterministic_pipeline_id(...);

let mut run_info = PipelineRunInfo {
    run_id, pipeline_id,
    pipeline_name: pipeline.name.clone().unwrap_or_default(),
    user_id, tenant_id, dataset_id,
    status: PipelineRunStatus::Started,
    started_at: chrono::Utc::now(),
    completed_at: None,
};

let ctx = ctx.with_run_id(run_id);
watcher
    .on_pipeline(pipeline_id, PipelineStatus::Started { task_count })
    .await;
watcher.on_pipeline_run_started(&run_info).await;

// ── Analytics: Pipeline Run Started ─────────────────────────────────
emit_pipeline_event(
    "Pipeline Run Started",
    user_id,
    &run_info.pipeline_name,
    tenant_id,
    pipeline.telemetry_settings.as_ref(),
);
```

Add `Pipeline Run Completed` after the `on_pipeline_run_completed`
call ([line 580](../../crates/core/src/pipeline.rs#L580)):

```rust
Ok(outputs) => {
    run_info.status = PipelineRunStatus::Completed;
    run_info.completed_at = Some(chrono::Utc::now());
    watcher.on_pipeline(...).await;
    watcher.on_pipeline_run_completed(&run_info, outputs.len()).await;

    emit_pipeline_event(
        "Pipeline Run Completed",
        user_id,
        &run_info.pipeline_name,
        tenant_id,
        pipeline.telemetry_settings.as_ref(),
    );
}
```

Add `Pipeline Run Errored` to **both** error arms ([line 583](../../crates/core/src/pipeline.rs#L583)
and [593](../../crates/core/src/pipeline.rs#L593)) — same emit call,
no error string property:

```rust
Err(ExecutionError::Cancelled) => {
    // existing watcher calls...
    emit_pipeline_event(
        "Pipeline Run Errored",
        user_id, &run_info.pipeline_name, tenant_id,
        pipeline.telemetry_settings.as_ref(),
    );
}
Err(e) => {
    // existing watcher calls...
    emit_pipeline_event(
        "Pipeline Run Errored",
        user_id, &run_info.pipeline_name, tenant_id,
        pipeline.telemetry_settings.as_ref(),
    );
}
```

### 4.4 Caller-side wiring (cognee-lib)

In whichever `cognee-lib` code paths build `cognee_core::Pipeline`
literals (typically inside `cognify_pipeline()`, `add_pipeline()`,
`memify_pipeline()`), populate `pipeline.telemetry_settings`:

```rust
let settings = config.telemetry_snapshot();
// optional: merge token_counter from cognify_config when relevant
if let Some(tc) = cognify_config.token_counter_kind.as_python_label() {
    settings.insert("token_counter".into(), serde_json::Value::String(tc.into()));
}
let pipeline = cognee_core::Pipeline {
    // existing fields…
    telemetry_settings: Some(settings),
};
```

Sub-agent A should locate every `Pipeline { ... }` literal in
`cognee-lib` / `cognee-cognify` / `cognee-ingestion` and update them.
A `grep -rn "Pipeline\s*{" crates/lib crates/cognify crates/ingestion`
walk is the right starting point.

> **Reminder on production-path coverage.** Per the orchestrator
> scope guard (runbook §"Scope guard"), most production SDK paths
> bypass `cognee_core::execute()` today (see source comment at
> [`crates/cognify/src/tasks.rs:1719`](../../crates/cognify/src/tasks.rs#L1719)),
> so the events will fire only from paths that *do* go through
> `execute()`. That is the existing limitation — don't try to fix
> it in this gap.

## 5. Verification

```bash
# 1. Compile both feature states.
cargo check --all-targets
cargo check --all-targets --no-default-features

# 2. The existing pipeline tests still pass — they use literal
#    `Pipeline { ... }` constructions in `crates/core/tests/`,
#    so the new field must default to None or be passed explicitly.
cargo test -p cognee-core --tests

# 3. Clippy.
cargo clippy --all-targets -- -D warnings

# 4. Full check.
scripts/check_all.sh

# 5. (Manual smoke) Run an example that goes through execute() with
#    `RUST_LOG=cognee.telemetry=debug` and confirm three log lines
#    appear without an actual proxy round-trip
#    (TELEMETRY_DISABLED=1 will *not* show them — set ENV=test
#    instead… actually no, ENV=test also drops them).
#    Easier: rely on the integration test in task 03-08.
```

The full assertion (`Pipeline Run Started → Coroutine Task Started
→ Coroutine Task Completed → Pipeline Run Completed` in order)
lives in [task 03-08](08-tests.md). This task only ships the wiring.

## 6. Files modified

- [`crates/core/src/pipeline.rs`](../../crates/core/src/pipeline.rs)
  — add `telemetry_settings` field on `Pipeline` (or use Option B if
  agreed by sub-agent A); add `emit_pipeline_event` helper; wire
  three call sites in `execute()`.
- [`crates/core/Cargo.toml`](../../crates/core/Cargo.toml) — add
  `cognee-telemetry` as a dependency under `[dependencies]` (workspace
  reference; **always** required, not feature-gated, because the
  helper functions reference `cognee_telemetry::cognee_version()`
  which exists unconditionally).
- (Caller side) `cognee-lib` / `cognee-cognify` / `cognee-ingestion`
  pipeline-builder sites — populate `telemetry_settings` from
  `Config::telemetry_snapshot()`.
- Test fixtures in `crates/core/tests/*.rs` and
  `crates/lib/tests/*.rs` that build `Pipeline` literals — pass
  `telemetry_settings: None` (the events are silently dropped in tests
  unless the test explicitly opts in).

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `cognee-core` does not currently depend on `cognee-telemetry` → cycle risk | Verify with `cargo tree -p cognee-telemetry` — `cognee-telemetry` does not depend on `cognee-core`, so the edge `cognee-core → cognee-telemetry` is safe. | Sub-agent A re-runs the check before sub-agent B edits the manifest. |
| Adding `telemetry_settings` field to `Pipeline` breaks every literal construction | Compile errors will catch this. | Pass `telemetry_settings: None` in all test sites; add `..Default::default()` if a `Default` impl exists. |
| Hot-loop emission overhead | Negligible — `cognee_telemetry::send_telemetry` is fire-and-forget; the gap-02 client measured < 100 µs per call when `TELEMETRY_DISABLED=1`. | Stress-tested in gap-02-09 already. |
| Production paths bypass `execute()` so events never fire from the SDK | Documented limitation, not a regression. | Note in commit body; a separate gap covers re-routing. |

## 8. Out of scope

- Re-routing `cognify_pipeline()` / `add_pipeline()` / `memify_pipeline()`
  through `cognee_core::execute()` (separate gap).
- Adding `error` string property to `Pipeline Run Errored` (locked
  decision — Python omits it).
- Adding `pipeline_run_id` / `dataset_id` (locked decision 6).
- HTTP-router-level `... API Endpoint Invoked` events (out of scope).

**Status**: implemented in commit 694dd5a (note: caller-side wiring deferred — execute() is currently bypassed by production SDK paths)
