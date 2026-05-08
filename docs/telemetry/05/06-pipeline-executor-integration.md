# Task 05-06 — Pipeline executor integration (`call_with_retry`, `process_iter`, `process_stream`)

**Status**: implemented in commit 7199d43 (introduced stamp_tree_dyn helper for dyn Value cascade; TextSummary deferred to 05-09).
**Owner**: _unassigned_
**Depends on**:
- [Task 05-03 — Provenance core](03-provenance-core.md) (`stamp_tree`, extractors).
- [Task 05-04 — `HasDataPoint` impls](04-has-datapoint-impls.md) (the recursion targets).
- [Task 05-05 — `PipelineContext` fields](05-pipeline-context-fields.md) (`provenance_visited`, `user_label()`).

**Blocks**:
- [Task 05-10 — Tests](10-tests.md) (pipeline integration tests).

**Parent doc**: [05 — DataPoint Provenance Stamping](../05-datapoint-provenance.md)
**Locked decisions**: #2 (UUID-keyed visited set), #3 (do not rename the audit hook), #8 (eager stream/iter stamping).

---

## 1. Goal

Wire `cognee_core::provenance::stamp_tree` into the pipeline executor so
**every successful task** in **every pipeline** stamps its outputs. This
is the integration point where stamping starts being end-to-end visible.

Three call sites in
[`crates/core/src/pipeline.rs`](../../crates/core/src/pipeline.rs):

1. **`call_with_retry`** (line 1125): after `resolve_call` returns `Ok`,
   walk `Resolved::Single(value)` and stamp.
2. **`process_iter`** (line 1068) and **`process_stream`** (line 1096):
   stamp each `Box<dyn Value>` **before** it is converted into
   `Arc<dyn Value>` for `dispatch_batch`.
3. **`execute`** (line 611 / nearby `PipelineRunInfo` initialisation):
   ensure the per-run `PipelineContext` has a non-empty
   `provenance_visited` (today, all callers either rely on the builder
   default or pass an empty Mutex; gap 05-05 makes this implicit).

Behaviour after this task:

- A `DataPoint` emitted by task #1 carries `source_pipeline`,
  `source_task`, `source_user`, `source_node_set` (if the input had
  one), and `source_content_hash` (if the input had one).
- The same DataPoint, if cloned through subsequent tasks, is **not**
  re-stamped (visited-set short-circuits).
- Iterator and stream items are stamped on the consumer side, before
  they hand off to the next task.

The existing
[`ExecStatusManager::stamp_provenance` audit-log call at lines 1180-1205](../../crates/core/src/pipeline.rs#L1180-L1205)
**stays intact**. Per locked decision 3, the two functions coexist.

## 2. Rationale

- **`call_with_retry` is the universal post-task hook.** Every task,
  regardless of variant, funnels through here exactly once on the
  success path. Hooking here mirrors Python's per-yield call site in
  `run_tasks_base.py` line 142-191.
- **Iter / stream items need their own pass.** The current code
  resolves a task to `Resolved::Iter(_) | Resolved::Stream(_)` and
  treats each yielded item as opaque. Stamping has to happen at the
  consumer side because `Box<dyn Value>` is uniquely owned at that
  moment (`Arc::get_mut` would not work after the conversion).
- **Eager stamping (decision 8)** keeps the algorithm simple. Lazy
  stamping via a stream wrapper would require a `Stream` adapter that
  also holds the visited-set lock — fiddly, and the per-item cost is
  trivial (a UUID hash + 5 `Option::is_none` checks).
- **Visited-set ownership.** Lives on `PipelineContext`, not in
  `ExecEnv`, because tasks that fan out across `execute_items_par` must
  share it (a DataPoint shared between two parallel items still
  stamps once). The existing `Arc<TaskContext>` cloning path through
  `with_progress` / `with_current_data` keeps the same `Arc<Mutex<…>>`,
  so concurrent items see the same set.

## 3. Pre-conditions

- Tasks [05-03](03-provenance-core.md), [05-04](04-has-datapoint-impls.md),
  [05-05](05-pipeline-context-fields.md) are committed.
- Clean `cargo check --all-targets` on `main`.

## 4. Step-by-step

### 4.1 Add a downcast registry helper inside `pipeline.rs`

The `Resolved::Single(Arc<dyn Value>)` case needs to know which
concrete types to try mutating in place. Create a private helper:

```rust
// crates/core/src/pipeline.rs (near the top of the impl section,
// after the `Resolved` enum definition)

/// Try to mutate the contents of `value` as a `HasDataPoint` and run
/// `stamp_tree` against it. Returns `true` if the call recognised the
/// concrete type; `false` is "passed through unchanged" (matches
/// Python's no-op branch for non-DataPoint values).
fn try_stamp_value(
    value: &mut Arc<dyn Value>,
    ctx: &ProvenanceContext<'_>,
    visited: &mut HashSet<Uuid>,
) -> bool {
    use cognee_models::{Document, DocumentChunk, Entity, EntityType, EdgeType};
    use cognee_cognify::TextSummary; // may need a feature gate; see §4.5

    // `Arc::get_mut` returns Some only when this is the unique owner.
    // Tasks emit fresh values so this is true at this call site;
    // we're tolerant of failure (warn-and-skip per design discussion).
    let Some(inner): Option<&mut dyn Value> = Arc::get_mut(value).map(|v| v as &mut dyn Value) else {
        tracing::warn!(
            "skipping provenance stamping: shared Arc<dyn Value> for task '{}'",
            ctx.task_name
        );
        return false;
    };

    if let Some(d) = inner.as_any_mut().downcast_mut::<Document>() {
        cognee_core::stamp_tree(d, ctx, visited);
        return true;
    }
    if let Some(d) = inner.as_any_mut().downcast_mut::<DocumentChunk>() {
        cognee_core::stamp_tree(d, ctx, visited);
        return true;
    }
    if let Some(d) = inner.as_any_mut().downcast_mut::<Entity>() {
        cognee_core::stamp_tree(d, ctx, visited);
        return true;
    }
    if let Some(d) = inner.as_any_mut().downcast_mut::<EntityType>() {
        cognee_core::stamp_tree(d, ctx, visited);
        return true;
    }
    if let Some(d) = inner.as_any_mut().downcast_mut::<EdgeType>() {
        cognee_core::stamp_tree(d, ctx, visited);
        return true;
    }
    if let Some(d) = inner.as_any_mut().downcast_mut::<TextSummary>() {
        cognee_core::stamp_tree(d, ctx, visited);
        return true;
    }

    // Vec<T> containers: walk and recurse so a task that emits
    // `Vec<DocumentChunk>` (wrapped in Arc<dyn Value>) gets every chunk
    // stamped. Add the Vec variants we actually emit:
    if let Some(items) = inner.as_any_mut().downcast_mut::<Vec<DocumentChunk>>() {
        for item in items.iter_mut() {
            cognee_core::stamp_tree(item, ctx, visited);
        }
        return true;
    }
    if let Some(items) = inner.as_any_mut().downcast_mut::<Vec<Entity>>() {
        for item in items.iter_mut() {
            cognee_core::stamp_tree(item, ctx, visited);
        }
        return true;
    }
    // … add other Vec<T> shapes as needed; the cognify pipeline emits
    // `ClassifiedDocuments`, `ExtractedChunks`, etc. — those are
    // single-DP-bearing **container** structs, not bare DataPoints.
    // Decide per-case: either implement `HasDataPoint` on the
    // container with a `for_each_child_mut` walk, or downcast and
    // walk here. Prefer the former for cleanliness.

    false
}

/// Same as `try_stamp_value` but for the `Box<dyn Value>` items
/// yielded by `process_iter` / `process_stream`. Box ownership means
/// `Arc::get_mut` is not needed.
fn try_stamp_boxed(
    value: &mut Box<dyn Value>,
    ctx: &ProvenanceContext<'_>,
    visited: &mut HashSet<Uuid>,
) -> bool {
    let inner: &mut dyn Value = value.as_mut();
    // Same downcast cascade as `try_stamp_value`. Refactor into a
    // shared free function taking `&mut dyn Value` to avoid duplication.
    cognee_core::stamp_tree_dyn(inner, ctx, visited)
    //                  ^^^^^^^^^^^^^^^^^ helper introduced below
}
```

A cleaner approach is to add the dispatch helper to the `provenance`
module from 05-03:

```rust
// in crates/core/src/provenance.rs
pub fn stamp_tree_dyn(
    value: &mut dyn Value,
    ctx: &ProvenanceContext<'_>,
    visited: &mut HashSet<Uuid>,
) -> bool { /* same downcast cascade, returning bool */ }
```

This keeps the type-list maintenance in one file (next to the trait
itself) instead of spilling into `pipeline.rs`. Sub-agent B chooses
whichever placement compiles cleanly; the §6 list expects
`stamp_tree_dyn` to live in `provenance.rs`.

### 4.2 Build the `ProvenanceContext` once per task

In `call_with_retry` (around line 1156, after the existing `user_id` /
`tenant_id` resolution):

```rust
let user_label = env.ctx.pipeline_ctx
    .as_ref()
    .and_then(|p| p.user_label());
let user_label_str = user_label.as_deref();

// Walk the input once to extract node_set / content_hash defaults.
let input_node_set = cognee_core::extract_node_set_from_value(input.as_ref());
let input_content_hash = cognee_core::extract_content_hash_from_value(input.as_ref());
```

Bind these as locals so the post-`resolve_call` block can reuse them
without repeating the extraction.

### 4.3 Stamp `Resolved::Single` inside `call_with_retry`

After `resolve_call` returns `Ok(resolved)` (currently lines 1164-1208),
**before** the existing `exec_status.stamp_provenance(...)` audit call,
insert:

```rust
let pipeline_name = env.pipeline_name.unwrap_or("");
let task_label = task_name.unwrap_or("");

if let Some(pctx) = env.ctx.pipeline_ctx.as_ref() {
    let prov_ctx = ProvenanceContext {
        pipeline_name,
        task_name: task_label,
        user_label: user_label_str,
        node_set: input_node_set.as_deref(),
        content_hash: input_content_hash.as_deref(),
    };

    // Lock the visited set for the duration of the recursion. Held
    // briefly: walking a few hundred DataPoints is microseconds.
    // lock poison is unrecoverable
    let mut visited = pctx.provenance_visited.lock().unwrap();

    if let Resolved::Single(ref mut v) = resolved {
        let _ = try_stamp_value(v, &prov_ctx, &mut visited);
    }
    // Iter / Stream are handled in `process_iter` / `process_stream`
    // (decision 8: eager at consumption), not here.
}
```

`resolved` must be made `mut` — change the `let resolved = ` binding
to `let mut resolved = `.

The existing audit-log call (`env.ctx.exec_status.stamp_provenance(...)`)
remains immediately after this block, untouched. Decision 3.

### 4.4 Stamp items inside `process_iter`

[`process_iter`](../../crates/core/src/pipeline.rs#L1068) iterates a
`ValueIter` and pushes `Box<dyn Value>` into `batch`. Stamp each item
before the push:

```rust
async fn process_iter(
    iter: ValueIter,
    tail: &[TaskInfo],
    batch_size: usize,
    first_index: usize,
    env: &ExecEnv<'_>,
) -> Result<Vec<Arc<dyn Value>>, ExecutionError> {
    let mut outputs = Vec::new();
    let mut batch: Vec<Box<dyn Value>> = Vec::with_capacity(batch_size);

    // Build the provenance context once for this task's iter pass.
    let prov_input = /* same shape as call_with_retry §4.2 — see note */;

    for mut item in iter {
        if let Some(pctx) = env.ctx.pipeline_ctx.as_ref() {
            // lock poison is unrecoverable
            let mut visited = pctx.provenance_visited.lock().unwrap();
            let _ = cognee_core::stamp_tree_dyn(&mut *item, &prov_input, &mut visited);
        }
        batch.push(item);
        if batch.len() >= batch_size {
            outputs
                .append(&mut dispatch_batch(mem::take(&mut batch), tail, first_index, env).await?);
        }
    }
    // … existing tail …
}
```

The "build the provenance context" step needs to know **which task**
emitted the item. `process_iter` runs after the parent `execute_from`
that called `call_with_retry`, so the task name and input have been
consumed. Two options:

- **(a)** Thread `ProvenanceContext` (or its inputs: task name +
  pre-extracted node_set / content_hash) through the call chain.
  `execute_from` → `process_iter` adds two parameters.
- **(b)** Stamp inside `call_with_retry` for stream / iter too by
  intercepting at the boundary: wrap the iter / stream into a stamping
  adapter that consumes the visited-set lock per pull.

**Choose (a).** It's two extra parameters but keeps semantics local.
Specifically, `execute_from` (line 887) already builds the per-task
state at lines 929-947; thread `task_name`, the pre-extracted
`input_node_set`, and `input_content_hash` into `process_iter` /
`process_stream` / `dispatch_batch` as a small struct:

```rust
struct ProvenanceInputs<'a> {
    task_name: &'a str,
    pipeline_name: &'a str,
    user_label: Option<&'a str>,
    input_node_set: Option<String>,
    input_content_hash: Option<String>,
}
```

Pass `&ProvenanceInputs` everywhere. The struct is built once in
`execute_from` (using `extract_*_from_value` against the input), reused
for `call_with_retry`, `process_iter`, and `process_stream`.

### 4.5 Stamp items inside `process_stream`

Symmetric to §4.4. The only difference is `while let Some(item) =
stream.next().await` instead of `for item in iter`.

```rust
while let Some(mut item) = stream.next().await {
    if let Some(pctx) = env.ctx.pipeline_ctx.as_ref() {
        // lock poison is unrecoverable
        let mut visited = pctx.provenance_visited.lock().unwrap();
        let _ = cognee_core::stamp_tree_dyn(&mut *item, &prov_input, &mut visited);
    }
    batch.push(item);
    if batch.len() >= batch_size {
        outputs
            .append(&mut dispatch_batch(mem::take(&mut batch), tail, first_index, env).await?);
    }
}
```

### 4.6 Initialise the visited set when `execute()` builds the run

Find the `PipelineContext` construction inside
[`pipeline.rs::execute`](../../crates/core/src/pipeline.rs#L611)
(around the per-run `PipelineContext` setup; if `execute()` does not
construct one and instead reuses a caller-supplied one, the
initialisation already happens at the lib API layer — verify and
either initialise here or note in §7).

**Confirmation (post-05-05):** `execute()` does **not** construct
`PipelineContext`. The caller supplies `Arc<TaskContext>` and `execute`
only adjusts the `run_id` via `ctx.with_run_id(run_id)`. So:

- The lib API layer is the right place to ensure `provenance_visited`
  is populated (the `Default` impl from 05-05 already does this — every
  freshly-built `PipelineContext` has an empty `Arc<Mutex<HashSet>>`).
- For cross-run isolation, prefer the **clear** form below at the top
  of `execute()` (right after `let ctx = ctx.with_run_id(run_id)`):

If a per-run construction exists, ensure it sets:

```rust
provenance_visited: Arc::new(Mutex::new(HashSet::new())),
```

If `execute()` mutates a caller-supplied `PipelineContext`, just
**clear** the visited set at the start of `execute()` so each run is
isolated:

```rust
if let Some(pctx) = ctx.pipeline_ctx.as_ref() {
    // lock poison is unrecoverable
    pctx.provenance_visited.lock().unwrap().clear();
}
```

The clear is cheap and avoids cross-run contamination if the caller
recycles a `TaskContext`.

### 4.7 Decide on the `cognee-cognify` dep

Listing `TextSummary` in the downcast cascade brings `cognee-cognify`
into `cognee-core`'s dep set. **Do not do this.** Two alternatives:

- **(a)** Move `TextSummary` from `cognee-cognify` to `cognee-models`.
  This is a refactor; not in scope here.
- **(b)** Have the `stamp_tree_dyn` cascade in
  `crates/core/src/provenance.rs` cover **only** types in
  `cognee-models`. Cognify's `TextSummary` gets stamped via the
  existing local helper in `crates/cognify/src/tasks.rs` (decision 6
  keeps that helper alive); the executor walk passes `TextSummary`
  through unchanged.

**Choose (b).** The locked decisions table accepts this gap (decision
6 keeps cognify's local stamping). Cognify's pre-stamp [task 05-09](09-cognify-prestamp.md)
covers `TextSummary` explicitly through the local helper.

If a future cleanup moves `TextSummary` to `cognee-models`, the
cascade in `provenance.rs` adds one more `if let Some(_) = downcast`
arm and the local helper can be retired.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. The provenance-specific integration test passes (will be added
#    in 05-10, but if 05-10 already landed, this catches regressions).
cargo test -p cognee-core --test provenance_pipeline_integration

# 3. Existing pipeline / payload / telemetry tests still pass — no
#    behavioural change to non-DataPoint flows.
cargo test -p cognee-core --test pipeline_payload_events
cargo test -p cognee-core --test pipeline_telemetry_events
cargo test -p cognee-core --test scoped_watcher_payload_persistence

# 4. Concurrency: parallel-items test still passes (visited set is
#    shared via Arc<Mutex<…>>; no deadlock).
cargo test -p cognee-core --test execute_items_par

# 5. Clippy.
cargo clippy --all-targets -- -D warnings

# 6. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/core/src/pipeline.rs`](../../crates/core/src/pipeline.rs) —
  three call-site insertions, one `ProvenanceInputs` struct, the
  `let mut resolved` change, the visited-set initialisation in
  `execute()`.
- [`crates/core/src/provenance.rs`](../../crates/core/src/provenance.rs) —
  new public `stamp_tree_dyn(value: &mut dyn Value, …) -> bool`
  function with the type-cascade.
- (No changes outside `cognee-core`.)

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `Arc::get_mut` returns `None` for a value pre-shared by a custom user task | Medium for the long tail; low for the cognify / memify / ingestion path which always emit fresh values | The helper logs `tracing::warn!` and skips. The cognify / memify paths get exercised by 05-10 tests. |
| Visited-set lock contention under `execute_items_par` | Low — the lock is held for microseconds per stamp call | Mutex is the right tool; `RwLock` would not help (writes dominate). |
| `extract_*_from_value` is called on every iter / stream pass — re-walks the input value each time | Low — we extract **once** per task (in `execute_from`) and reuse via `ProvenanceInputs` | The §4.4 plan threads this through; verify in code review that no extraction happens inside the loops. |
| Threading `ProvenanceInputs` through `dispatch_batch` increases the public-ish struct surface | Low — `dispatch_batch` is private | Acceptable; the alternative (inlining the extraction at every call site) is worse. |
| Recursion through `for_each_child_mut` deadlocks if a child impl re-locks `visited` | None — the lock is released before recursion entry | The §4.3 / §4.4 / §4.5 code holds the lock across the entire `stamp_tree_dyn` call, which mutates `visited` in place. The trait does not re-lock anything. |

## 8. Out of scope

- Stamping batch tasks. They run via `dispatch_batch` and bypass
  `call_with_retry` (see the doc comment at line 1003-1007). Adding
  stamping for batch tasks is a separate consideration: it'd require
  walking the slice. Defer to a follow-up if the cognify pipeline grows
  to use batch tasks for DataPoint emission (it currently does not).
- Removing the `ExecStatusManager::stamp_provenance` audit hook
  (decision 3 prohibits).
- Per-attempt stamping. Stamping happens on the **success** branch
  only. A retried task that eventually fails leaves no DataPoints to
  stamp.
- Performance optimisation past "lock briefly, walk once". If the
  visited-set ever grows to dominate, switch to a sharded
  `dashmap::DashSet` — but only with profiling evidence.
