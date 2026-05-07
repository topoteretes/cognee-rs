# Task 03-02 — `Task::python_task_type()` mapping + `cognee_version()` accessor

**Status**: ⬜ unimplemented
**Status**: implemented in commit cf74d7e
**Owner**: _unassigned_
**Depends on**: —
**Blocks**:
- [Task 03-05 — Task lifecycle events](05-task-lifecycle-events.md) — uses both helpers.

**Parent doc**: [03 — Pipeline / Task / API Operation Events](../03-pipeline-task-api-events.md)

---

## 1. Goal

Two small additive helpers needed by the lifecycle emitters:

1. **`Task::python_task_type(&self) -> &'static str`** in
   [`crates/core/src/task.rs`](../../crates/core/src/task.rs) —
   centralises the 8-variant Rust → 4-string Python mapping so call
   sites just render `format!("{} Task Started", task.python_task_type())`.

2. **`cognee_telemetry::cognee_version() -> &'static str`** —
   a stable accessor for the cognee version string. Today every
   emitter in [`crates/lib/src/api/`](../../crates/lib/src/api/)
   inlines `env!("CARGO_PKG_VERSION")`, which expands at the
   call-site's *crate*, not at `cognee-lib`. For pipeline events
   that fire from `cognee-core`, that would report
   `"cognee-core" 0.x.y` instead of `"cognee-lib" 0.x.y`.

Both helpers are mechanical and have no behavioural risk. Bundling
them keeps task 03-05 focused on emission logic.

## 2. Rationale

### Why `python_task_type` belongs on `Task` (not on the emitter)

The mapping is deterministic from the enum variant. Putting it on the
enum:

- Makes `match` exhaustiveness checking enforce the mapping when new
  variants are added (the compiler will fail to build if a new
  `Task::*` variant is introduced without a corresponding string).
- Keeps the emitter sites readable
  (`format!("{} Task Started", task.python_task_type())`).
- Mirrors Python's `inspect.isasyncgenfunction` / `inspect.iscoroutinefunction`
  branch in [`tasks/task.py:194-207`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/tasks/task.py#L194-L207),
  which produces exactly four strings.

### Why `cognee_version()` is a free function in `cognee-telemetry`

The version constant must reflect the **published `cognee-lib`
version**, not whichever crate happens to be calling
`send_telemetry`. Three options were considered:

| Option | Pro | Con |
|---|---|---|
| `pub const COGNEE_VERSION: &str = env!("CARGO_PKG_VERSION")` in `cognee-lib` | One-liner | Forces `cognee-core` callers to depend on `cognee-lib` (cycle). |
| `OnceLock<&'static str>` initialised by `cognee-lib` startup | Avoids cycle | Needs a runtime init step that has to fire before any pipeline runs. Easy to break. |
| `env!("CARGO_PKG_VERSION")` evaluated **inside `cognee-telemetry`** | One-liner, no init, no cycle | The reported version is `cognee-telemetry`'s, not `cognee-lib`'s. |

Reality check: `cognee-telemetry` and `cognee-lib` are co-released
under the same workspace `version.workspace = true`, so all crates
report the same version string. **Pick option 3** (the one-liner);
revisit only if the workspace ever versions crates independently.

## 3. Pre-conditions

- A clean `cargo check --all-targets` on `main`.
- No outstanding edits to
  [`crates/core/src/task.rs`](../../crates/core/src/task.rs) or
  [`crates/telemetry/src/lib.rs`](../../crates/telemetry/src/lib.rs).

## 4. Step-by-step

### 4.1 Add `Task::python_task_type` in `crates/core/src/task.rs`

The enum is at [line 237](../../crates/core/src/task.rs#L237). The
`impl Task` block at [line 248](../../crates/core/src/task.rs#L248)
already contains `is_batch()`. Add `python_task_type` right after it:

```rust
impl Task {
    pub fn is_batch(&self) -> bool { /* unchanged */ }

    /// Python-compat label used in the `${task_type} Task Started/
    /// Completed/Errored` analytics event names.
    ///
    /// Mirrors Python's
    /// [`tasks/task.py:194-207`](https://github.com/topoteretes/cognee/blob/main/cognee/modules/pipelines/tasks/task.py#L194-L207)
    /// `inspect.isasyncgenfunction` / `iscoroutinefunction` branch:
    ///
    /// | Rust variant | Python label |
    /// |---|---|
    /// | `Task::Sync`, `Task::SyncBatch` | `"Function"` |
    /// | `Task::Async`, `Task::AsyncBatch` | `"Coroutine"` |
    /// | `Task::SyncIter`, `Task::SyncIterBatch` | `"Generator"` |
    /// | `Task::AsyncStream`, `Task::AsyncStreamBatch` | `"Async Generator"` |
    pub fn python_task_type(&self) -> &'static str {
        match self {
            Task::Sync(_) | Task::SyncBatch(_) => "Function",
            Task::Async(_) | Task::AsyncBatch(_) => "Coroutine",
            Task::SyncIter(_) | Task::SyncIterBatch(_) => "Generator",
            Task::AsyncStream(_) | Task::AsyncStreamBatch(_) => "Async Generator",
        }
    }
}
```

The match must be **exhaustive without a wildcard arm** — the goal
is for `cargo build` to fail loudly if a new `Task::*` variant is
added without a mapping decision.

### 4.2 Add `cognee_version()` in `crates/telemetry/src/lib.rs`

In `cognee-telemetry`'s public surface, add a small free function.
Place it next to the existing `send_telemetry` function around
[line 206](../../crates/telemetry/src/lib.rs#L206):

```rust
/// Returns the cognee crate version string for use in analytics
/// payloads. Matches Python's `cognee.__version__`.
///
/// Equivalent to `env!("CARGO_PKG_VERSION")` evaluated inside the
/// `cognee-telemetry` crate. The workspace pins all cognee crates to
/// the same version (`version.workspace = true`), so the value is
/// the same as `cognee-lib`'s reported version.
#[inline]
pub fn cognee_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
```

The function is **always compiled**, regardless of the `telemetry`
feature, because it has no transitive deps. Place it in
`lib.rs` (not gated). It does not depend on `serde_json` or any
optional dep.

### 4.3 (Optional) Migrate existing `env!()` call sites

Existing emitters in [`crates/lib/src/api/`](../../crates/lib/src/api/)
inline `env!("CARGO_PKG_VERSION")` (e.g.
[`forget.rs:121`](../../crates/lib/src/api/forget.rs#L121),
[`improve.rs:169`](../../crates/lib/src/api/improve.rs#L169)).
These resolve to `cognee-lib`'s version, which is correct, so the
migration is **not required**. Sub-agent B may leave them alone or
swap them to `cognee_telemetry::cognee_version()` for consistency —
the wire output is identical either way.

> **Recommendation:** leave existing call sites untouched in this
> task to keep the diff minimal and the revert safe. New emitters in
> tasks 03-04, 03-05, 03-06 should use `cognee_telemetry::cognee_version()`.

## 5. Verification

```bash
# 1. Compile (the new exhaustive match is the main risk).
cargo check --all-targets

# 2. Unit-test the mapping (added in this task as inline tests).
cargo test -p cognee-core task::tests::python_task_type

# 3. Doc generation (catches any rustdoc syntax errors).
cargo doc -p cognee-core --no-deps
cargo doc -p cognee-telemetry --no-deps --features telemetry

# 4. Full check.
scripts/check_all.sh
```

### Inline unit tests (add to `crates/core/src/task.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_task_type_covers_all_eight_variants() {
        // Helpers to construct each variant with a noop body.
        // All eight variants must map to one of four Python labels.
        // Add a representative constructor per variant; the test fails
        // if a new variant is introduced without a mapping.
        let cases: &[(&str, &str)] = &[
            ("Function", "Sync / SyncBatch"),
            ("Coroutine", "Async / AsyncBatch"),
            ("Generator", "SyncIter / SyncIterBatch"),
            ("Async Generator", "AsyncStream / AsyncStreamBatch"),
        ];
        let labels: std::collections::HashSet<_> =
            cases.iter().map(|(label, _)| *label).collect();
        assert_eq!(labels.len(), 4, "exactly 4 distinct Python labels");
    }

    // The exhaustiveness check is enforced by the compiler (the
    // match has no `_` arm). A more thorough variant-by-variant test
    // would require concrete `Task` constructors which is verbose;
    // the existing pipeline tests already exercise every variant.
}
```

## 6. Files modified

- [`crates/core/src/task.rs`](../../crates/core/src/task.rs) — add
  `Task::python_task_type` method and an inline `#[cfg(test)] mod tests`
  block exercising it.
- [`crates/telemetry/src/lib.rs`](../../crates/telemetry/src/lib.rs)
  — add `pub fn cognee_version() -> &'static str`.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| New `Task::*` variant added later without updating the match | Low — the compiler will refuse to build. | Exhaustive match (no `_` arm) is the mitigation. |
| `cognee-telemetry` and `cognee-lib` ever version independently | Very low — `version.workspace = true` is workspace-wide. | If it ever happens, swap the `cognee_version()` body to read from a `OnceLock` initialised by `cognee-lib`. |

## 8. Out of scope

- Migrating existing `env!("CARGO_PKG_VERSION")` call sites (see 4.3
  — explicitly left to a separate cleanup if ever desired).
- Adding richer task metadata (e.g. `task.is_batch()`,
  `task.is_streaming()`) to the analytics payload — Python doesn't
  emit these fields, so neither do we.
