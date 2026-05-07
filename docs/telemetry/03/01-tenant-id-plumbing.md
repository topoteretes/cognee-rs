# Task 03-01 — Thread `tenant_id` through `PipelineContext` and `PipelineRunInfo`

**Status**: ✅ implemented in commit 70e2d8e (note: a fourth core test file, `crates/core/tests/scoped_run_watcher.rs`, also needed a trivial `tenant_id: None` compile fixup, bringing the modified-files count to 6 instead of the 5 listed in section 6).
**Owner**: _unassigned_
**Depends on**: —
**Blocks**:
- [Task 03-04 — Pipeline lifecycle events](04-pipeline-lifecycle-events.md) (reads `run_info.tenant_id`).
- [Task 03-05 — Task lifecycle events](05-task-lifecycle-events.md) (reads `ctx.pipeline_ctx.tenant_id`).
- [Task 03-06 — `cognee.search EXECUTION STARTED/COMPLETED`](06-search-execution-events.md) (backfills `tenant_id` on the existing `EXECUTION COMPLETED` emitter, which currently sends `Null`).

**Parent doc**: [03 — Pipeline / Task / API Operation Events](../03-pipeline-task-api-events.md)
**Locked decision**: #1 — `tenant_id` is threaded as `Option<Uuid>`; emitters fall back to literal `"Single User Tenant"` when `None`.

---

## 1. Goal

Add `tenant_id: Option<Uuid>` as a first-class field on the two
runtime-context types that downstream lifecycle emitters will read:

| Type | File | Today | After this task |
|---|---|---|---|
| `cognee_core::PipelineContext` (struct) | [`crates/core/src/task_context.rs:22`](../../crates/core/src/task_context.rs#L22) | `pipeline_id`, `pipeline_name`, `user_id`, `dataset_id`, `current_data`, `run_id` | + `tenant_id: Option<Uuid>` |
| `cognee_core::PipelineRunInfo` | [`crates/core/src/pipeline.rs:291`](../../crates/core/src/pipeline.rs#L291) | `run_id`, `pipeline_id`, `pipeline_name`, `user_id`, `dataset_id`, `status`, `started_at`, `completed_at` | + `tenant_id: Option<Uuid>` |

`execute()` populates `run_info.tenant_id` from `ctx.pipeline_ctx`
(line 509 already reads `user_id` and `dataset_id` the same way).
The 3 test files that build `PipelineContext` literally are updated
to pass the new field.

This task does **not** add `tenant_id` to API params for
`recall.rs`/`forget.rs` — that is **out of scope** per locked
decision 1 (existing API event payloads keep their current shape;
backfilling is a separate follow-up gap).

## 2. Rationale — why thread a real value rather than emit a literal

Python's `User.tenant_id` field already exists (`cognee/modules/users/models/User.py`),
and Python's `send_telemetry` reads it as `str(user.tenant_id) if
user.tenant_id else "Single User Tenant"`. The Rust port already
models it on the `User` struct
([`crates/models/src/user.rs:18`](../../crates/models/src/user.rs#L18))
but **does not propagate it through the pipeline runtime**. Threading
it through `PipelineContext` + `PipelineRunInfo` is the smallest
change that:

1. Lets pipeline-lifecycle events emit a real `tenant_id` when one is
   set by the SDK caller.
2. Keeps the `"Single User Tenant"` literal as a fallback so behaviour
   is identical to Python for single-user installs (the common case).
3. Avoids a wider API-signature refactor (the public `recall`,
   `forget`, etc. surfaces are untouched in this gap).

The struct field is `Option<Uuid>` — same convention as the existing
`user_id: Option<Uuid>` and `dataset_id: Option<Uuid>` siblings.

## 3. Pre-conditions

- A clean `cargo check --all-targets` on `main`.
- Gap 02 closed (`cognee_telemetry::send_telemetry` exists; default-on
  for `cognee-lib`/`cognee-cli`).
- No outstanding edits to
  [`crates/core/src/task_context.rs`](../../crates/core/src/task_context.rs)
  or [`crates/core/src/pipeline.rs`](../../crates/core/src/pipeline.rs).

## 4. Step-by-step

### 4.1 Add field to `PipelineContext` struct

Edit [`crates/core/src/task_context.rs`](../../crates/core/src/task_context.rs).
Around line 22-39 the struct currently reads:

```rust
pub struct PipelineContext {
    pub pipeline_id: Uuid,
    pub pipeline_name: String,
    pub user_id: Option<Uuid>,
    pub dataset_id: Option<Uuid>,
    pub current_data: Option<Arc<dyn Value>>,
    pub run_id: Option<Uuid>,
}
```

Add `tenant_id` next to `user_id`:

```rust
pub struct PipelineContext {
    pub pipeline_id: Uuid,
    pub pipeline_name: String,
    pub user_id: Option<Uuid>,
    /// Tenant the pipeline run belongs to. `None` for single-user
    /// deployments — telemetry emitters substitute the literal
    /// `"Single User Tenant"` to match Python's behaviour.
    pub tenant_id: Option<Uuid>,
    pub dataset_id: Option<Uuid>,
    pub current_data: Option<Arc<dyn Value>>,
    pub run_id: Option<Uuid>,
}
```

No `Default` impl exists for `PipelineContext` — every construction
site must be updated.

### 4.2 Add field to `PipelineRunInfo`

Edit [`crates/core/src/pipeline.rs`](../../crates/core/src/pipeline.rs)
around line 291:

```rust
pub struct PipelineRunInfo {
    pub run_id: Uuid,
    pub pipeline_id: Uuid,
    pub pipeline_name: String,
    pub user_id: Option<Uuid>,
    /// Tenant the pipeline run belongs to. `None` for single-user
    /// deployments. Emitted as `"Single User Tenant"` on the wire
    /// when `None` (Python parity).
    pub tenant_id: Option<Uuid>,
    pub dataset_id: Option<Uuid>,
    pub status: PipelineRunStatus,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}
```

### 4.3 Populate `run_info.tenant_id` in `execute()`

Edit `execute()` in [`crates/core/src/pipeline.rs`](../../crates/core/src/pipeline.rs#L486).
Around lines 509-523, the existing code reads:

```rust
let user_id = ctx.pipeline_ctx.as_ref().and_then(|p| p.user_id);
let dataset_id = ctx.pipeline_ctx.as_ref().and_then(|p| p.dataset_id);
let pipeline_id = deterministic_pipeline_id(...);

let mut run_info = PipelineRunInfo {
    run_id,
    pipeline_id,
    pipeline_name: pipeline.name.clone().unwrap_or_default(),
    user_id,
    dataset_id,
    status: PipelineRunStatus::Started,
    started_at: chrono::Utc::now(),
    completed_at: None,
};
```

Add a `tenant_id` extraction next to `user_id` / `dataset_id` and
include it in the struct literal:

```rust
let user_id = ctx.pipeline_ctx.as_ref().and_then(|p| p.user_id);
let tenant_id = ctx.pipeline_ctx.as_ref().and_then(|p| p.tenant_id);
let dataset_id = ctx.pipeline_ctx.as_ref().and_then(|p| p.dataset_id);

// ...

let mut run_info = PipelineRunInfo {
    run_id,
    pipeline_id,
    pipeline_name: pipeline.name.clone().unwrap_or_default(),
    user_id,
    tenant_id,
    dataset_id,
    status: PipelineRunStatus::Started,
    started_at: chrono::Utc::now(),
    completed_at: None,
};
```

### 4.4 Update test sites

Three test files construct `PipelineContext` literally and will fail
to compile until they pass `tenant_id`:

- [`crates/core/tests/pipeline_payload_events.rs:90`](../../crates/core/tests/pipeline_payload_events.rs#L90)
- [`crates/core/tests/pipeline_payload_events.rs:114`](../../crates/core/tests/pipeline_payload_events.rs#L114)
- [`crates/core/tests/scoped_watcher_payload_persistence.rs:149`](../../crates/core/tests/scoped_watcher_payload_persistence.rs#L149)

In each, add `tenant_id: None,` next to the existing `user_id: None,`
or whatever value is already there. The tests do not exercise tenant
behaviour, so `None` is the appropriate value.

### 4.5 Helper for the wire-format string

Lifecycle emitters in tasks 03-04 / 03-05 / 03-06 all need to convert
`Option<Uuid>` to the wire-format string with the `"Single User
Tenant"` fallback. Place the helper in
[`crates/telemetry/src/lib.rs`](../../crates/telemetry/src/lib.rs)
as a `pub` free function:

```rust
// crates/telemetry/src/lib.rs

/// Format a `tenant_id` for the telemetry wire payload, mirroring
/// Python `str(user.tenant_id) if user.tenant_id else "Single User Tenant"`.
#[inline]
pub fn tenant_id_for_telemetry(tenant_id: Option<uuid::Uuid>) -> String {
    match tenant_id {
        Some(id) => id.to_string(),
        None => "Single User Tenant".to_string(),
    }
}
```

> **Why `cognee-telemetry` and not `cognee-core`:** verified on
> 2026-05-07 that `crates/search/Cargo.toml` depends on
> `cognee-telemetry` (via `[dependencies] cognee-telemetry = { path =
> "../telemetry" }`) but **not** on `cognee-core`. Placing the helper
> in `cognee-telemetry` lets all three emitter sites
> (`crates/core/src/pipeline.rs`, `crates/search/src/...`, and
> `crates/lib/src/api/...`) reach it without adding a new crate
> dependency. The crate already has a `uuid` direct dep, so no new
> deps are introduced.

If `cognee-telemetry` does not yet have a public `uuid` re-export or
direct `uuid` use in its `lib.rs`, just `use uuid::Uuid;` at the top
of the function — the dep is already in `crates/telemetry/Cargo.toml`.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. Compile with telemetry feature explicitly off (the field must
#    exist regardless of feature state — only the emitters consuming
#    it are gated).
cargo check --all-targets --no-default-features

# 3. Run the affected core tests.
cargo test -p cognee-core --test pipeline_payload_events
cargo test -p cognee-core --test scoped_watcher_payload_persistence

# 4. Clippy.
cargo clippy --all-targets -- -D warnings

# 5. Full check.
scripts/check_all.sh
```

A targeted smoke test is sufficient — no behaviour changes yet, only
a new field. Real end-to-end coverage lands in [task 03-08](08-tests.md).

## 6. Files modified

- [`crates/core/src/task_context.rs`](../../crates/core/src/task_context.rs)
  — add `tenant_id` field to `PipelineContext` struct.
- [`crates/core/src/pipeline.rs`](../../crates/core/src/pipeline.rs)
  — add `tenant_id` field to `PipelineRunInfo`; populate it in
  `execute()` (around line 509–523).
- [`crates/core/tests/pipeline_payload_events.rs`](../../crates/core/tests/pipeline_payload_events.rs)
  — pass `tenant_id: None` in the two literal constructions
  (lines 90 and 114).
- [`crates/core/tests/scoped_watcher_payload_persistence.rs`](../../crates/core/tests/scoped_watcher_payload_persistence.rs)
  — pass `tenant_id: None` at line 149.
- [`crates/telemetry/src/lib.rs`](../../crates/telemetry/src/lib.rs)
  — add `pub fn tenant_id_for_telemetry(tenant_id: Option<Uuid>) -> String`
  (see step 4.5; placed here because `cognee-search` already depends on
  `cognee-telemetry` but not on `cognee-core`).

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Other call sites construct `PipelineContext` literally outside the 3 test files. | Low — `grep -rn "PipelineContext\s*{" crates/` returned only the 3 sites + the struct definition itself at the time of writing. | Sub-agent A re-runs the grep before claiming the file list is exhaustive. |
| Cognify / ingestion pipelines bypass `cognee_core::execute()` (see source comments at `crates/cognify/src/tasks.rs:1719`), so the new field flows through but lifecycle events still don't fire from the SDK. | Documented limitation, not a regression. | Sub-doc explicitly notes this; the larger refactor is out of scope. |
| `Option<Uuid>::to_string()` formats `None` as `"None"` if accidentally used directly. | Trivial bug. | The helper exists precisely to prevent this; pin tests to the literal `"Single User Tenant"` in [task 03-08](08-tests.md). |

## 8. Out of scope

- Adding `tenant_id` to `recall()`, `forget()`, or any other public
  API signature (locked decision 1 — existing API events keep their
  current payload shape).
- Backfilling `cognee.recall` / `cognee.forget` payloads with
  `tenant_id` (same — separate follow-up gap).
- Routing the SDK production paths (cognify, ingestion, memify)
  through `execute()` so lifecycle events actually fire from those
  paths.
