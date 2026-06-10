# Gap 1 — Drop / Filter Sentinel

**Parent:** [../cog-4454-core-implementation-plan.md](../cog-4454-core-implementation-plan.md)
**Effort:** Small
**Order:** First (creates the shared `sentinels.rs` module reused by Gap 2)
**Status:** ☑ Implemented (d1a7967)

## Goal

Let any pipeline task signal "discard this item" so that it is **not** forwarded
to downstream tasks and does **not** appear in the pipeline's final output —
without raising an error.

## Python reference

`cognee/pipelines/types.py:8–32` defines a `_Drop` sentinel. When a task returns
`_Drop`, the framework silently filters that item out of the stream. Rust
currently has no item-level filter: every task output is either forwarded or, on
`Err`, aborts the pipeline.

## Design rationale

### Why a sentinel value rather than a new return type

Tasks return type-erased `Arc<dyn Value>` / `Box<dyn Value>`. Changing every
task signature to `Option<…>` or a new `TaskOutput` enum would ripple across all
8 task variants in `task.rs`, every `TypedTask` constructor, and every existing
task in the workspace. A **sentinel value** — a zero-size struct that implements
`Value` — is fully backward compatible: existing tasks are untouched, and only
tasks that want to drop items opt in by returning the sentinel.

### Why no manual `Value` impl is needed

`task.rs:22` has a blanket impl:

```rust
impl<T: Any + Send + Sync + 'static> Value for T { … }
```

So a plain unit struct `DroppedSentinel` **automatically** implements `Value`. Do
not hand-write an `impl Value for DroppedSentinel` — it would conflict with the
blanket impl and fail to compile.

### The `Arc<dyn Value>` downcast trap

Because of that same blanket impl, `Arc<dyn Value>` *itself* implements `Value`.
Calling `.as_any()` on an `Arc<dyn Value>` returns the `Any` view of the **Arc**,
not the inner value, so `downcast_ref::<DroppedSentinel>()` on it always returns
`None`. The existing code already works around this at `pipeline.rs:1395` with
`(**v).as_any()`. Our helper therefore takes `&dyn Value` and callers pass the
**dereferenced** trait object (`arc.as_ref()` for `Arc<dyn Value>`, `item.as_ref()`
for `Box<dyn Value>`).

## Step-by-step

### Step 1 — Create `crates/core/src/sentinels.rs`

```rust
//! Control-flow sentinel values that pipeline tasks return to steer the
//! executor. Sentinels are ordinary [`Value`]s (via the blanket
//! `impl<T> Value for T` in `task.rs`), so no manual trait impl is needed.

use crate::task::Value;

/// Returned by a task to discard the current item: it is not forwarded to
/// downstream tasks and does not appear in the pipeline output.
///
/// Mirrors Python's `_Drop` sentinel (`cognee/pipelines/types.py`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DroppedSentinel;

/// True if `value` is a [`DroppedSentinel`].
///
/// `value` must be the dereferenced trait object (`&dyn Value`), **not** an
/// `Arc`/`Box<dyn Value>`: the blanket `impl<T> Value for T` means `.as_any()`
/// on a smart pointer downcasts to the pointer, never the inner value. Pass
/// `arc.as_ref()` / `boxed.as_ref()`.
pub fn is_dropped(value: &dyn Value) -> bool {
    value.as_any().downcast_ref::<DroppedSentinel>().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn detects_dropped_sentinel() {
        let v: Arc<dyn Value> = Arc::new(DroppedSentinel);
        assert!(is_dropped(v.as_ref()));
    }

    #[test]
    fn ignores_regular_value() {
        let v: Arc<dyn Value> = Arc::new(42_usize);
        assert!(!is_dropped(v.as_ref()));
    }
}
```

### Step 2 — Register & export the module in `crates/core/src/lib.rs`

Add the module declaration alongside the other `pub mod` lines (after
`pub mod provenance;`):

```rust
pub mod sentinels;
```

Add the re-export after the `provenance` re-export block:

```rust
pub use sentinels::{DroppedSentinel, is_dropped};
```

### Step 3 — Filter in the single-value path (`execute_from`)

In `crates/core/src/pipeline.rs`, the `match resolved` block at **line 1127**.
Change the `Resolved::Single(v)` arm:

```rust
match resolved {
    Resolved::Single(v) => {
        // Drop sentinel: discard this item; nothing flows downstream.
        if crate::sentinels::is_dropped(v.as_ref()) {
            return Ok(vec![]);
        }
        execute_from(rest, v, first_index + 1, env).await
    }
    Resolved::Iter(iter) => {
        process_iter(iter, rest, batch_size, first_index + 1, &prov_inputs, env).await
    }
    Resolved::Stream(stream) => {
        process_stream(stream, rest, batch_size, first_index + 1, &prov_inputs, env).await
    }
}
```

Returning `Ok(vec![])` is correct whether the dropping task is the last task
(item is filtered from output) or a middle task (item never reaches the tail).
The `Started`/`Succeeded` watcher events already fired before the call — that is
intended: the task *succeeded*, it simply produced no output.

### Step 4 — Filter items emitted by iterators (`process_iter`)

In `process_iter` at **line 1241**, guard the loop body so dropped items are
skipped before stamping/accumulation:

```rust
for mut item in iter {
    if crate::sentinels::is_dropped(item.as_ref()) {
        continue;
    }
    stamp_boxed_item(&mut item, prov_inputs, env);
    batch.push(item);
    if batch.len() >= batch_size {
        outputs.append(
            &mut dispatch_batch(mem::take(&mut batch), tail, first_index, prov_inputs, env)
                .await?,
        );
    }
}
```

### Step 5 — Filter items emitted by streams (`process_stream`)

Identical guard in `process_stream` at **line 1281**:

```rust
while let Some(mut item) = stream.next().await {
    if crate::sentinels::is_dropped(item.as_ref()) {
        continue;
    }
    stamp_boxed_item(&mut item, prov_inputs, env);
    batch.push(item);
    // … unchanged …
}
```

### Coverage note: batch path

The batch dispatch path (`dispatch_batch`, line 1149) does **not** need a
separate guard:

- The **non-batch** branch (line 1204) calls `execute_from` per item → Step 3
  covers it.
- The **batch-task** branch feeds the batch task's `Resolved` output back through
  `execute_from`/`process_iter`/`process_stream` (lines 1187–1201) → Steps 3–5
  cover it.
- Items are already filtered *before* entering a batch because `process_iter` /
  `process_stream` (Steps 4–5) run upstream of `dispatch_batch`.

A batch task that wants to drop *individual* items of its slice should instead
emit an iterator/stream that omits them (or yields `DroppedSentinel` per item,
which Steps 4–5 filter). Document this in the `DroppedSentinel` doc-comment if
desired; no code change required.

## Test plan

New integration test file `crates/core/tests/sentinels_drop.rs`:

1. **Filters every other item** — a `SyncIter` task yielding `0..10`, followed by
   a `Sync` task that returns `DroppedSentinel` for odd `n` and the value for even
   `n`. Assert the final output contains exactly `[0, 2, 4, 6, 8]`.
2. **Drop in the last task** — single task returns `DroppedSentinel` for one input
   among several. Assert that input is absent from the output and no error is
   raised.
3. **Iterator yields sentinels directly** — a `SyncIter` task that yields a mix of
   real values and `DroppedSentinel`; assert sentinels are filtered and the
   downstream task never receives them.

## Acceptance criteria

- [x] `crates/core/src/sentinels.rs` exists with `DroppedSentinel` + `is_dropped` and passing unit tests
- [x] Exported from `lib.rs`
- [x] `execute_from`, `process_iter`, `process_stream` all filter `DroppedSentinel`
- [x] New integration tests pass
- [x] `cargo check --all-targets` + `cargo test -p cognee-core` green
