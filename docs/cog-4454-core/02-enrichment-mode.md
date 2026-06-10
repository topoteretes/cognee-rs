# Gap 2 — Enrichment Mode (`enriches` flag)

**Parent:** [../cog-4454-core-implementation-plan.md](../cog-4454-core-implementation-plan.md)
**Effort:** Small
**Order:** Second (depends on the `sentinels.rs` module created in [Gap 1](./01-drop-sentinel.md))
**Status:** ☑ Implemented (1c87685)

## Goal

Support **optional enrichment** tasks: a task marked `enriches = true` may return
a pass-through sentinel to mean "I had nothing to add — forward my input
unchanged to the next task." This lets enrichment steps no-op on some items
without dropping them or failing the pipeline.

## Python reference

`cognee/modules/pipelines/tasks/task.py:184` — tasks have an `enriches` flag.
When an enriching task returns `None`, the framework substitutes the **original
input** as the task's output and continues. Non-enriching tasks that return
`None` are an error.

## Design rationale

### Why a `PassthroughSentinel` instead of `Option<T>` / returning `None`

Same reasoning as the drop sentinel ([Gap 1](./01-drop-sentinel.md)): tasks
return type-erased `Arc<dyn Value>`, and there is no in-band `None`. A sentinel
value is the minimal, backward-compatible signal. We reuse the `sentinels.rs`
module and the `&dyn Value` downcast helper pattern from Gap 1.

### Why the `enriches` flag gates the sentinel

A bare pass-through sentinel is ambiguous: on a normal task it almost certainly
indicates a bug (the task forgot to produce output). The `enriches` flag makes
intent explicit:

- `enriches = true` + `PassthroughSentinel` → forward input unchanged (success).
- `enriches = false` + `PassthroughSentinel` → `ExecutionError::TaskFailed`
  (programmer error surfaced loudly, not silently swallowed).

This mirrors Python, where `enriches` distinguishes "None means pass-through"
from "None is unexpected."

### Why we clone the input only when `enriches`

To forward the original input we must keep a handle to it, but `input` is moved
into `call_with_retry` at `pipeline.rs:1091`. We take a cheap `Arc::clone` —
**only** when `info.enriches` is set — so the non-enrichment path keeps its
current move semantics and pays nothing extra.

## Step-by-step

### Step 1 — Add `PassthroughSentinel` to `crates/core/src/sentinels.rs`

Append to the module created in Gap 1:

```rust
/// Returned by an *enriching* task to forward its input unchanged.
///
/// Honored only when the task's [`TaskInfo::enriches`](crate::task::TaskInfo)
/// is `true`; on a non-enriching task it is an error. Mirrors Python's
/// `enriches` behavior (`cognee/modules/pipelines/tasks/task.py`): an enriching
/// task that returns `None` passes its input through untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PassthroughSentinel;

/// True if `value` is a [`PassthroughSentinel`]. See [`is_dropped`] for the
/// `&dyn Value` (dereference-the-pointer) contract.
pub fn is_passthrough(value: &dyn Value) -> bool {
    value
        .as_any()
        .downcast_ref::<PassthroughSentinel>()
        .is_some()
}
```

Extend the re-export in `lib.rs`:

```rust
pub use sentinels::{DroppedSentinel, PassthroughSentinel, is_dropped, is_passthrough};
```

### Step 2 — Add the `enriches` field to `TaskInfo` (`crates/core/src/task.rs`)

Add the field to the struct (after `weight`, line 735):

```rust
pub struct TaskInfo {
    pub task: Task,
    pub name: Option<String>,
    pub batch_size: Option<usize>,
    pub summary_template: Option<String>,
    pub weight: u32,
    /// If `true`, a returned `PassthroughSentinel` forwards the task's input
    /// unchanged to the next task instead of being an error. Default `false`.
    pub enriches: bool,
}
```

Set the default in **every** `TaskInfo` constructor/literal — there are two:

1. `TaskInfo::new` (line 739):

```rust
pub fn new(task: Task) -> Self {
    Self {
        task,
        name: None,
        batch_size: None,
        summary_template: None,
        weight: 1,
        enriches: false,
    }
}
```

2. The literal inside `TaskInfo::parallel` (line 787):

```rust
TaskInfo {
    task: Task::parallel(tasks),
    name: Some(format!("parallel([{}])", names.join(", "))),
    batch_size: None,
    summary_template: None,
    weight: 1,
    enriches: false,
}
```

> The `From<Task>` (line 797) and `From<TypedTask>` (line 1008) impls delegate to
> `TaskInfo::new`, so they need no change once `new` is updated. Confirm this when
> editing — if either uses a struct literal, add `enriches: false` there too.

Add the builder method (alongside `with_weight`):

```rust
/// Mark this task as an enrichment step: returning `PassthroughSentinel`
/// forwards the input unchanged rather than failing.
pub fn with_enriches(mut self) -> Self {
    self.enriches = true;
    self
}
```

### Step 3 — Handle pass-through in `execute_from` (`crates/core/src/pipeline.rs`)

**3a.** Capture the input for pass-through *before* it is moved into
`call_with_retry`. Insert immediately before the `let resolved = call_with_retry(`
call at **line 1089**:

```rust
// Keep a handle to the original input only for enrichment tasks, so a
// PassthroughSentinel can forward it unchanged. Cheap Arc clone; skipped
// entirely for non-enriching tasks.
let input_passthrough = info.enriches.then(|| Arc::clone(&input));
```

**3b.** Extend the `Resolved::Single(v)` arm (the same arm edited in Gap 1 Step 3).
The pass-through check goes **before** the drop check:

```rust
Resolved::Single(v) => {
    // Enrichment: a PassthroughSentinel forwards the original input.
    if crate::sentinels::is_passthrough(v.as_ref()) {
        match input_passthrough {
            Some(orig) => return execute_from(rest, orig, first_index + 1, env).await,
            None => {
                return Err(ExecutionError::TaskFailed {
                    task_index: first_index,
                    attempts: 1,
                    source: "task returned PassthroughSentinel but enriches=false".into(),
                });
            }
        }
    }
    // Drop sentinel (Gap 1).
    if crate::sentinels::is_dropped(v.as_ref()) {
        return Ok(vec![]);
    }
    execute_from(rest, v, first_index + 1, env).await
}
```

Notes:
- `ExecutionError::TaskFailed.source` is `TaskError = Box<dyn Error + Send + Sync>`
  (`task.rs:142`), so `"…".into()` constructs it directly.
- `attempts: 1` because the failure is a logic error in the returned value, not a
  transient fault — no retry would help.

### Scope note: iterator/stream and batch tasks

Pass-through is defined for **single-value** outputs only — it forwards *the one
input* of a 1-in/1-out task. It is intentionally **not** wired into
`process_iter` / `process_stream`, where there is no single "original input" to
forward (the task produced N items from a batch). This matches Python, where
`enriches` applies to the task's return, and an iterator/generator task that
wants to skip an item uses the drop sentinel instead. If an enriching iterator
task yields a `PassthroughSentinel`, it is simply treated as a regular value
flowing downstream (or, more usefully, the task should yield `DroppedSentinel`).
Document this in `with_enriches`'s doc-comment.

## Test plan

New integration test file `crates/core/tests/enrichment.rs`:

1. **Pass-through forwards input** — pipeline `[enrich, collect]` where `enrich`
   is `.with_enriches()` and returns `PassthroughSentinel` for odd inputs but
   wraps even inputs. Assert odd inputs reach `collect` unchanged and even inputs
   arrive wrapped.
2. **Non-enriching pass-through errors** — a task *without* `.with_enriches()`
   returns `PassthroughSentinel`. Assert the run fails with
   `ExecutionError::TaskFailed` whose message mentions `enriches=false`.
3. **Enrichment + real output mix** — confirm output count equals input count
   when an enrichment stage no-ops on a subset.

## Acceptance criteria

- [ ] `PassthroughSentinel` + `is_passthrough` added to `sentinels.rs` and exported
- [ ] `TaskInfo.enriches` field + `with_enriches()` builder; all constructors updated
- [ ] `execute_from` forwards input on pass-through when `enriches`, errors otherwise
- [ ] New integration tests pass
- [ ] `cargo check --all-targets` + `cargo test -p cognee-core` green
