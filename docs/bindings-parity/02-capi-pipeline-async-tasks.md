# CR-2 — C API: fix empty-task pipeline in `execute_async` / `execute_in_background`

- **Binding:** C API (`capi/cognee-capi/`)
- **Dimension:** Correctness
- **Priority:** P0
- **Status:** Not started

## Problem

The engine-tier asynchronous and background pipeline execution paths run a
pipeline with **zero tasks**, so they silently produce empty/no-op results while
the public header advertises them as functional.

The root cause is `clone_pipeline` in
[capi/cognee-capi/src/pipeline_exec.rs:220-233](../../capi/cognee-capi/src/pipeline_exec.rs#L220),
which reconstructs a `cognee_core::Pipeline` for the spawned future but
deliberately leaves the task list empty:

```rust
fn clone_pipeline(p: &cognee_core::Pipeline) -> cognee_core::Pipeline {
    let mut new_p = Pipeline::new(p.description.clone());
    new_p.id = p.id;
    new_p.name = p.name.clone();
    new_p.retry_policy = p.retry_policy.clone();
    new_p.batch_size = p.batch_size;
    new_p.data_id_fn = p.data_id_fn.clone();
    new_p.concurrency = p.concurrency;
    // Note: tasks are left empty — this is a known limitation for
    // execute_in_background/execute_async. The blocking path works fine.
    new_p
}
```

`cg_pipeline_execute_in_background` (line 209) and the async variant both call
`clone_pipeline`, so both execute nothing. Only `cg_pipeline_execute_blocking`
works. The header [capi/include/cognee.h](../../capi/include/cognee.h) lists all three
without flagging the limitation, so a C caller using the async/background engine
path gets silently wrong results.

> Scope: this is an **engine-tier** concern. The high-level `cg_sdk_*` ops do not
> use `clone_pipeline` (they dispatch through `spawn_sdk_op`), so the SDK surface
> is unaffected. It is still a public, documented entry point that returns wrong
> results.

## Goal / definition of done

`cg_pipeline_execute_async` and `cg_pipeline_execute_in_background` run the same
tasks as `cg_pipeline_execute_blocking`, verified by an example/test that adds a
task and observes its effect through the async path. No public entry point
returns silently-empty results.

## Implementation plan

The fix is to share the task list with the spawned pipeline instead of dropping
it. The header comment in `pipeline_exec.rs` already notes "task closures are
Arc-wrapped, so we can reconstruct a Pipeline that shares the same task
closures" — the cloning just needs to actually copy the `tasks` field.

### Step 1 — Inspect the `Pipeline` task representation

Read `cognee_core::pipeline::Pipeline` (in `crates/core/src/`) to confirm the
`tasks` field type. Determine whether tasks are stored as `Arc<dyn …>` /
`Vec<Arc<…>>` (cheaply shareable) or as a non-`Clone` owning type.

### Step 2a — Preferred: share the tasks

If tasks are `Arc`-wrapped (the comment claims they are), add:

```rust
new_p.tasks = p.tasks.clone(); // Arc-wrapped closures are cheap to share
```

to `clone_pipeline`, and delete the "tasks are left empty" comment. This makes
all three execution paths equivalent.

### Step 2b — If tasks are not cheaply clonable

If `Pipeline` cannot share its task list (e.g. `FnMut` or non-`Clone` closures),
restructure so the spawned future **takes ownership of the original
`CgPipeline`** rather than reconstructing one:

- Change `cg_pipeline_execute_in_background` / `_async` to consume the
  `*mut CgPipeline` (matching the ownership-transfer convention used elsewhere,
  e.g. `cg_pipeline_add_task` consuming `CgTaskInfo`), move its `inner` into the
  spawned task, and document the consume semantics in the header.
- Alternatively, wrap the pipeline's `inner` in `Arc<Pipeline>` at construction
  so both the blocking and async paths clone the `Arc`.

Prefer 2a if the field is already shareable; fall back to 2b only if the core
type forbids it. If 2b changes ownership semantics, that is an API change to call
out in the header and the changelog.

### Step 3 — Update the header

In [capi/include/cognee.h](../../capi/include/cognee.h), remove any "blocking only"
wording and, if Step 2b changed ownership, document that
`cg_pipeline_execute_in_background`/`_async` consume the pipeline handle.

### Step 4 — Add a regression example/test

Add (or extend) a C example under [capi/examples/](../../capi/examples/) — model it on
`example_pipeline.c` — that:

1. builds a pipeline with one sync task that has an observable side effect
   (e.g. appends to a result value),
2. runs it via `cg_pipeline_execute_in_background` + `cg_pipeline_run_handle_wait`,
3. asserts the task actually ran (non-empty result).

Wire it into [capi/scripts/check.sh](../../capi/scripts/check.sh) so it builds and runs
in CI. A failing version of this test should reproduce the current empty-result
bug before the fix.

## Verification

```bash
# from capi/
bash scripts/check.sh   # new background-execution example must run a real task
# from repo root
scripts/check_all.sh
```

## Risks / notes

- Touching `clone_pipeline` may surface lifetime/`Send` constraints on the task
  closures when moved into `runtime.spawn`; the existing `SendPtr`/`SendCallback`
  newtypes in `pipeline_exec.rs` show the pattern for asserting `Send` where the
  C contract guarantees it.
- If the engine async path turns out to be genuinely unused by any consumer and
  hard to fix, the *minimum* acceptable outcome is to make it return an explicit
  "unsupported" error code and document it — never silently empty. The preferred
  outcome is a real fix per Step 2.
