# Task 05-05 — `PipelineContext`: `provenance_visited` + `user_email` + `user_label()` helper

**Status**: ⬜ not started
**Owner**: _unassigned_
**Depends on**: —
**Blocks**:
- [Task 05-06 — Pipeline executor integration](06-pipeline-executor-integration.md) (reads the visited set + `user_label()`).
- [Task 05-07 — User-label plumbing](07-user-label-plumbing.md) (writes `user_email`).

**Parent doc**: [05 — DataPoint Provenance Stamping](../05-datapoint-provenance.md)
**Locked decisions**: #4 — `user_email: Option<String>` plus a `user_label()` helper.

---

## 1. Goal

Extend
[`cognee_core::PipelineContext`](../../crates/core/src/task_context.rs#L22)
with the per-run state that the new stamping algorithm needs:

```rust
pub struct PipelineContext {
    // … existing fields …

    /// Email of the user running the pipeline, if known. Used by
    /// `stamp_tree` to populate `DataPoint.source_user`. Mirrors
    /// Python's `user.email`.
    pub user_email: Option<String>,

    /// DataPoints already stamped during this pipeline run, keyed on
    /// their UUID. Lives across all tasks so a DP shared between two
    /// tasks is stamped exactly once with the **first** task's name.
    /// Locked decision 2.
    pub provenance_visited: Arc<Mutex<HashSet<Uuid>>>,
}

impl PipelineContext {
    /// Resolved provenance label: prefer `user_email`, fall back to
    /// `user_id.to_string()`. Mirrors Python's
    /// `user.email or str(user.id)` (locked decision 4).
    pub fn user_label(&self) -> Option<String> {
        self.user_email.clone()
            .or_else(|| self.user_id.map(|id| id.to_string()))
    }
}
```

The `TaskContextBuilder` API gains a corresponding setter for
`user_email` (the visited set is auto-initialised — callers should not
have to construct it manually).

## 2. Rationale

- **`user_email` is a separate field from `user_id`** so that the
  field semantics are unambiguous. `user_id` always means "user
  primary key UUID"; `user_email` always means "the email if known".
  The `user_label()` helper is the single place that does the
  fallback resolution, and the executor calls it once per task
  (decision 4).
- **`provenance_visited` is an `Arc<Mutex<HashSet<Uuid>>>`** because
  the executor's `call_with_retry` can run concurrently across items
  inside `execute_items_par`. Mutex (not RwLock) because writes
  vastly outnumber reads.
- **Existing `PipelineContext` callers stay green** — both new fields
  have sensible `Default` implementations (`None` and a fresh empty
  `Arc<Mutex<HashSet<…>>>`); test sites that build `PipelineContext`
  via struct-literal syntax need a one-line update each.

## 3. Pre-conditions

- Clean `cargo check --all-targets` on `main`.
- No outstanding edits to
  [`crates/core/src/task_context.rs`](../../crates/core/src/task_context.rs).

## 4. Step-by-step

### 4.1 Add the fields

Edit [`crates/core/src/task_context.rs`](../../crates/core/src/task_context.rs).

Top of the file, add imports:

```rust
use std::collections::HashSet;
use std::sync::Mutex;
```

(`Arc` and `Uuid` are already imported.)

Inside `pub struct PipelineContext` (line 22 onwards), append the two
new fields after `run_id`:

```rust
    /// Email of the user running the pipeline, if known. Used by the
    /// provenance-stamping algorithm to populate
    /// `DataPoint.source_user`. Mirrors Python's `user.email`.
    /// Resolution priority is captured by [`PipelineContext::user_label`].
    pub user_email: Option<String>,

    /// DataPoints already stamped during this pipeline run, keyed on
    /// their UUID. Shared across all tasks via the per-run
    /// `PipelineContext` so a DataPoint that survives multiple tasks
    /// is stamped exactly once — with the **first** task's name.
    /// Mirrors Python's `PipelineContext._provenance_visited`.
    pub provenance_visited: Arc<Mutex<HashSet<Uuid>>>,
```

### 4.2 Add the `user_label()` helper

Below the struct definition, add an `impl PipelineContext` block:

```rust
impl PipelineContext {
    /// Resolved label used as `DataPoint.source_user` by the
    /// provenance-stamping algorithm.
    ///
    /// Priority order (matches Python's `user.email or str(user.id)`,
    /// locked decision 4):
    ///
    /// 1. `user_email` if set.
    /// 2. Else `user_id.to_string()` if set.
    /// 3. Else `None` (the DP keeps its own value, or stays unstamped).
    pub fn user_label(&self) -> Option<String> {
        self.user_email
            .clone()
            .or_else(|| self.user_id.map(|id| id.to_string()))
    }
}
```

Add a small inline test at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_label_prefers_email() {
        let ctx = PipelineContext {
            pipeline_id: Uuid::new_v4(),
            pipeline_name: "test".into(),
            user_id: Some(Uuid::new_v4()),
            tenant_id: None,
            dataset_id: None,
            current_data: None,
            run_id: None,
            user_email: Some("alice@example.com".into()),
            provenance_visited: Arc::new(Mutex::new(HashSet::new())),
        };
        assert_eq!(ctx.user_label().as_deref(), Some("alice@example.com"));
    }

    #[test]
    fn user_label_falls_back_to_user_id() {
        let uid = Uuid::new_v4();
        let ctx = PipelineContext {
            pipeline_id: Uuid::new_v4(),
            pipeline_name: "test".into(),
            user_id: Some(uid),
            tenant_id: None,
            dataset_id: None,
            current_data: None,
            run_id: None,
            user_email: None,
            provenance_visited: Arc::new(Mutex::new(HashSet::new())),
        };
        assert_eq!(ctx.user_label(), Some(uid.to_string()));
    }

    #[test]
    fn user_label_is_none_when_neither_set() {
        let ctx = PipelineContext {
            pipeline_id: Uuid::new_v4(),
            pipeline_name: "test".into(),
            user_id: None,
            tenant_id: None,
            dataset_id: None,
            current_data: None,
            run_id: None,
            user_email: None,
            provenance_visited: Arc::new(Mutex::new(HashSet::new())),
        };
        assert!(ctx.user_label().is_none());
    }
}
```

### 4.3 Thread `user_email` through `TaskContextBuilder`

The visited set is auto-initialised; callers do not set it. `user_email`
needs an explicit setter because the lib API populates it (task 05-07).

Edit
[`TaskContextBuilder`](../../crates/core/src/task_context.rs#L178) to
take advantage of the existing `pipeline_context()` setter — callers
already set the entire `PipelineContext` at once via that builder method,
so no new `with_user_email` is needed at the `TaskContextBuilder` layer.

If the user_email needs to be updated *after* the context is built (e.g.
late lookup of the User row), add this convenience method on
`TaskContext`:

```rust
impl TaskContext {
    /// Create a new `Arc<TaskContext>` with `user_email` set on the
    /// pipeline context. All `Arc` fields are shallow-cloned.
    ///
    /// Returns the original `Arc` unchanged if no `pipeline_ctx` is
    /// present.
    pub fn with_user_email(self: &Arc<Self>, email: String) -> Arc<Self> {
        let mut pipeline_ctx = match &self.pipeline_ctx {
            Some(ctx) => ctx.clone(),
            None => return Arc::clone(self),
        };
        pipeline_ctx.user_email = Some(email);
        Arc::new(TaskContext {
            thread_pool: Arc::clone(&self.thread_pool),
            database: Arc::clone(&self.database),
            graph_db: Arc::clone(&self.graph_db),
            vector_db: Arc::clone(&self.vector_db),
            cancellation: self.cancellation.clone(),
            progress: self.progress.clone(),
            pipeline_ctx: Some(pipeline_ctx),
            exec_status: Arc::clone(&self.exec_status),
            pipeline_watcher: self.pipeline_watcher.clone(),
        })
    }
}
```

This mirrors the existing `with_run_id` and `with_current_data` shape.

### 4.4 Update test sites that construct `PipelineContext` literally

Grep for struct-literal construction:

```bash
grep -rn "PipelineContext\s*{" crates/ --include="*.rs"
```

Confirmed sites at the time of writing:

- [`crates/core/tests/scoped_watcher_payload_persistence.rs:149`](../../crates/core/tests/scoped_watcher_payload_persistence.rs#L149)
- [`crates/core/tests/pipeline_payload_events.rs:90`](../../crates/core/tests/pipeline_payload_events.rs#L90)
- [`crates/core/tests/pipeline_payload_events.rs:115`](../../crates/core/tests/pipeline_payload_events.rs#L115)
- [`crates/core/tests/pipeline_telemetry_events.rs:132`](../../crates/core/tests/pipeline_telemetry_events.rs#L132)

For each, append the two new fields:

```rust
        pipeline_ctx: Some(PipelineContext {
            // … existing fields …
            user_email: None,
            provenance_visited: Arc::new(Mutex::new(HashSet::new())),
        }),
```

If new test sites have appeared since this doc was written, sub-agent A
flags them via `STATUS: needs-update`.

### 4.5 Document the helper at the public re-export

Edit [`crates/core/src/lib.rs`](../../crates/core/src/lib.rs) — the
existing `pub use task_context::{PipelineContext, …}` re-export covers
the helper automatically. No new exports needed; the helper is a method
on the existing public type.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. The new helper tests pass.
cargo test -p cognee-core user_label

# 3. The existing pipeline / payload / telemetry tests still pass.
cargo test -p cognee-core --test scoped_watcher_payload_persistence
cargo test -p cognee-core --test pipeline_payload_events
cargo test -p cognee-core --test pipeline_telemetry_events

# 4. Clippy.
cargo clippy --all-targets -- -D warnings

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/core/src/task_context.rs`](../../crates/core/src/task_context.rs)
  — two new fields on `PipelineContext`, the `user_label()` helper, the
  `with_user_email` convenience method on `TaskContext`, and three
  inline tests.
- [`crates/core/tests/scoped_watcher_payload_persistence.rs`](../../crates/core/tests/scoped_watcher_payload_persistence.rs)
- [`crates/core/tests/pipeline_payload_events.rs`](../../crates/core/tests/pipeline_payload_events.rs)
- [`crates/core/tests/pipeline_telemetry_events.rs`](../../crates/core/tests/pipeline_telemetry_events.rs)
  — one-line addition each for the new fields.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| New test site introduced after the doc was written, missed in §4.4 | Medium — the workspace is moving | Sub-agent A's pre-check runs `grep -rn "PipelineContext\s*{" crates/` and updates this list before B starts. |
| `Mutex::lock().unwrap()` lint trips clippy (per CLAUDE.md the pattern is allowed but needs a comment) | Low | Add `// lock poison is unrecoverable` next to any `.lock().unwrap()` introduced in this file. |
| The added `user_label()` is shadowed by Python-style helper expectations elsewhere | None | The method name is unique in the codebase (verified via grep). |

## 8. Out of scope

- Wiring `user_email` to actual `User` rows. That is
  [task 05-07](07-user-label-plumbing.md).
- Wiring `provenance_visited` into the executor. That is
  [task 05-06](06-pipeline-executor-integration.md).
- Adding more telemetry-flavoured fields to `PipelineContext`. Other
  signals (api_key_tracking_id, anonymous_id) live elsewhere.

**Status**: implemented in commit d1b4c96 (note: TaskContextBuilder unchanged; new fields land via existing pipeline_context() setter; with_user_email() convenience added on TaskContext for ad-hoc updates).
