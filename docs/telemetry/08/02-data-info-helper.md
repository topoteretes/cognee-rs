# Task 08-02 — `data_info` helper + `RunSpec.data_ids` carrier

**Status**: implemented in commit f05c04e (executor uses data_id_fn + Uuid::parse_str filter_map; non-UUID extractor outputs silently dropped, matching Python list[Data] semantics)
**Owner**: _unassigned_
**Depends on**: 08-01.
**Blocks**:
- [Task 08-03 — `run_info` shape alignment](03-run-info-shape-alignment.md) (the watcher needs `data_info` to produce Python-shaped `{"data": …}` JSON).
- [Task 08-04 — INITIATED from executor](04-initiated-from-executor.md) (the executor passes `data_ids` through `PipelineRunInfo` to the watcher).

**Parent doc**: [08 — Pipeline Run Status Persistence](../08-pipeline-run-status.md)
**Locked decisions**: 5 (`data_info` helper lives in `crates/core/src/pipeline_run_registry/data_info.rs` and matches Python byte-for-byte), 6 (items are `Value::String`, not `Value::String` of a UUID's "compact" form — explicit `id.to_string()` which yields the hyphenated UUID form).

---

## 1. Goal

Add the plumbing that lets later tasks write the `"data"` key into `run_info` exactly the way Python does:

1. Create `crates/core/src/pipeline_run_registry/data_info.rs` with a `data_info` helper that takes a `&[Uuid]` (the list of `Data.id`s) and returns a `serde_json::Value`.
2. Extend `RunSpec` ([`types.rs:57-63`](../../crates/core/src/pipeline_run_registry/types.rs#L57-L63)) and `PipelineRunInfo` ([`pipeline.rs:310-336`](../../crates/core/src/pipeline.rs#L310-L336)) with a `data_ids: Vec<Uuid>` field.
3. Wire `data_ids` through every `RunSpec` construction site (HTTP dispatch, plus the new library wiring from task 08-07 will populate it from the cognify/memify/ingestion input list).
4. **No watcher behaviour change in this task.** Task 08-03 lights up the JSON write.

## 2. Rationale

Python computes `data_info` inside each of the three runtime helpers (`log_pipeline_run_start`, `_complete`, `_error`):

```python
# Excerpted from log_pipeline_run_start.py
if not data:
    data_info = "None"
elif isinstance(data, list) and all(isinstance(d, Data) for d in data):
    data_info = [str(item.id) for item in data]
else:
    data_info = str(data)
```

Three branches:
- **Empty / falsy** → literal string `"None"` (NOT JSON `null`).
- **`list[Data]`** → list of stringified UUIDs.
- **Anything else** → `repr(data)` via `str()`.

In Rust, the executor only ever sees `Vec<Data>` (Rust is strongly typed; the "anything else" branch is unreachable from typed call sites). The helper therefore only needs the first two branches. The third is preserved as `format!("{:?}", input)` for the edge case where a binding constructs a `RunSpec` with a non-`Data` payload — but in practice this is the empty list ⇒ `"None"` path.

Landing the helper + carrier in a standalone commit keeps task 08-03's diff focused on watcher behaviour.

## 3. Pre-conditions

- Task 08-01 committed — `dataset_id: Option<Uuid>` is in place across the database crate so the watcher contract can pass through `Option<Uuid>` without diverging.
- `serde_json::Value` is already available in `cognee-core` (used by `RunEvent.payload`).
- `Data.id: Uuid` confirmed at [`crates/models/src/data.rs:10`](../../crates/models/src/data.rs#L10).

## 4. Step-by-step

### 4.1 Add the helper module

Create `crates/core/src/pipeline_run_registry/data_info.rs`:

```rust
//! Python-parity `data_info` helper for `pipeline_runs.run_info`.
//!
//! Python writes `run_info["data"]` as one of three values:
//!   - `"None"` (literal string) when the input is empty / falsy
//!   - `[str(item.id) for item in data]` for `list[Data]`
//!   - `str(data)` (i.e. `repr()`) for anything else
//!
//! Rust only sees typed `Vec<Uuid>` inputs from typed call sites, so the
//! third branch is reduced to "the carrier is empty" — same wire shape as
//! Python's `if not data: data_info = "None"`.

use serde_json::Value;
use uuid::Uuid;

/// Build the value Python writes under `run_info["data"]`.
///
/// - Empty slice → `Value::String("None".into())` to match Python's
///   `data_info = "None"` branch.
/// - Non-empty slice → JSON array of hyphenated UUID strings
///   (`Uuid::to_string()`, which matches Python's `str(uuid.UUID(...))`).
///
/// The returned value is intended to be inserted as `run_info["data"]`,
/// not as the entire `run_info` document.
pub fn data_info(data_ids: &[Uuid]) -> Value {
    if data_ids.is_empty() {
        Value::String("None".into())
    } else {
        Value::Array(
            data_ids
                .iter()
                .map(|id| Value::String(id.to_string()))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_emits_string_none() {
        assert_eq!(data_info(&[]), Value::String("None".into()));
    }

    #[test]
    fn nonempty_emits_string_array() {
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let v = data_info(&[id1, id2]);
        let arr = v.as_array().expect("array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0], Value::String(id1.to_string()));
        assert_eq!(arr[1], Value::String(id2.to_string()));
    }

    #[test]
    fn output_serialises_to_python_shape() {
        // The hyphenated-UUID form matches Python's `str(uuid.UUID(hex))`.
        let id = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let v = data_info(&[id]);
        assert_eq!(
            v.to_string(),
            "[\"00000000-0000-0000-0000-000000000001\"]"
        );
    }
}
```

Expose from [`crates/core/src/pipeline_run_registry/mod.rs`](../../crates/core/src/pipeline_run_registry/mod.rs):

```rust
mod data_info;
pub use data_info::data_info;
```

### 4.2 Extend `RunSpec`

Edit [`crates/core/src/pipeline_run_registry/types.rs`](../../crates/core/src/pipeline_run_registry/types.rs):

```rust
pub struct RunSpec {
    pub run_id: Option<Uuid>,
    pub pipeline_name: String,
    pub user_id: Option<Uuid>,
    pub dataset_id: Option<Uuid>,
    /// IDs of the `Data` rows the run is processing. Surfaced into
    /// `pipeline_runs.run_info["data"]` by the watcher (see task 08-03).
    /// Empty when the run has no `Data` input (rare; ad-hoc paths).
    pub data_ids: Vec<Uuid>,
}
```

### 4.3 Extend `PipelineRunInfo`

Edit [`crates/core/src/pipeline.rs`](../../crates/core/src/pipeline.rs) `PipelineRunInfo` struct around line 313:

```rust
pub struct PipelineRunInfo {
    pub run_id: Uuid,
    pub pipeline_id: Uuid,
    pub pipeline_name: String,
    pub user_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub dataset_id: Option<Uuid>,
    /// `Data.id`s for the inputs of the run. Surfaced into `run_info["data"]`
    /// by the watcher. Empty when the run has no `Data` input.
    pub data_ids: Vec<Uuid>,
    pub status: PipelineRunStatus,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}
```

### 4.4 Wire `data_ids` through executor

In `pipeline::execute` ([line 611-770 area](../../crates/core/src/pipeline.rs#L611)), the function receives `inputs: Vec<T>` where `T: Pipelinable`. Pipelines that operate over `Data` set `pipeline.data_id_fn` (see `PipelineBuilder.data_id_fn` field). The executor uses this fn to map inputs → `Uuid` for tagging spans.

Extract a `data_ids: Vec<Uuid>` at the top of `execute` (before the `run_info` construction at line 647-ish) using `pipeline.data_id_fn`:

```rust
let data_ids: Vec<Uuid> = if let Some(id_fn) = pipeline.data_id_fn.as_ref() {
    inputs.iter().filter_map(|x| id_fn(x)).collect()
} else {
    Vec::new()
};
```

Populate `PipelineRunInfo.data_ids` from this vector when constructing `run_info` at line 647.

### 4.5 Wire `data_ids` through HTTP dispatch

Edit [`crates/http-server/src/pipelines/dispatch.rs`](../../crates/http-server/src/pipelines/dispatch.rs) `RunSpec` construction (line 98-103):

```rust
let spec = RunSpec {
    run_id: prid,
    pipeline_name: pipeline_name.to_owned(),
    user_id: Some(user.id),
    dataset_id,
    data_ids: Vec::new(), // HTTP dispatch does not have the input list at this layer
};
```

> **Note for task 08-07:** the library wiring will populate `RunSpec.data_ids` from the cognify/memify/ingestion input list before invoking the registry. The HTTP path keeps `Vec::new()` because the router doesn't see the resolved `Data` rows — those are resolved inside the boxed pipeline future.

### 4.6 All other `RunSpec` / `PipelineRunInfo` construction sites

Run:

```bash
rg "RunSpec\s*\{|PipelineRunInfo\s*\{" crates/
```

Add `data_ids: Vec::new()` to every construction site (test fixtures included — `cognee-test-utils` may have a mock). Compile errors will enumerate them.

### 4.7 Build

```bash
cargo check --all-targets
cargo test -p cognee-core -- data_info
```

## 5. Verification

```bash
# 1. Workspace compiles.
cargo check --all-targets

# 2. Helper unit tests pass.
cargo test -p cognee-core --lib -- data_info::tests

# 3. No behaviour regression in existing pipeline tests.
cargo test -p cognee-core
cargo test -p cognee-http-server

# 4. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/core/src/pipeline_run_registry/data_info.rs`](../../crates/core/src/pipeline_run_registry/data_info.rs) — **NEW**.
- [`crates/core/src/pipeline_run_registry/mod.rs`](../../crates/core/src/pipeline_run_registry/mod.rs) — `mod data_info; pub use data_info::data_info;`.
- [`crates/core/src/pipeline_run_registry/types.rs`](../../crates/core/src/pipeline_run_registry/types.rs) — `RunSpec.data_ids`.
- [`crates/core/src/pipeline.rs`](../../crates/core/src/pipeline.rs) — `PipelineRunInfo.data_ids`; populate from `pipeline.data_id_fn` in `execute`.
- [`crates/http-server/src/pipelines/dispatch.rs`](../../crates/http-server/src/pipelines/dispatch.rs) — `RunSpec { data_ids: Vec::new(), .. }`.
- Every other `RunSpec` / `PipelineRunInfo` construction site discovered by `rg`.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `Pipelinable` inputs are not `Data`; `pipeline.data_id_fn` returns `None` for all items → empty `data_ids` even when there are inputs | Acknowledged — Python's `data_info` only special-cases `list[Data]`; for non-`Data` inputs it falls back to `str(data)`. Rust skips that fallback and emits `"None"`. | Document as a known minor divergence in the closure summary; cross-SDK test (task 09) asserts the `list[Data]` shape, which is the common path. |
| Adding a required field to `RunSpec` / `PipelineRunInfo` breaks every test | Medium — desired outcome of the change. | The compiler enumerates them. Use `data_ids: Vec::new()` as the safe default. |
| `pipeline.data_id_fn` is `Option<Arc<dyn Fn(...)>>` — calling it on every input adds overhead | Low — call sites are already O(N) per input for span tagging. | Reuse the existing call result rather than calling twice; cache in a local `data_ids` vector. |
| HTTP dispatch can't populate `data_ids` (the router doesn't see the input list); the watcher's `"data"` will be `"None"` for HTTP runs until task 08-07 wires it through the boxed future | Acknowledged — HTTP runs land `"data": "None"` until library wiring is in place. | Note in the commit body; task 08-07 fills the gap. |
| `serde_json::Value::String(id.to_string())` allocates per id | Low | UUID stringification is one allocation; runs are bounded in size. |

## 8. Out of scope

- Writing `run_info["data"]` into actual rows (lands in task 08-03).
- Honoring a custom `data_info` formatter passed by the caller (Python doesn't expose one; out of scope).
- Surfacing `data_ids` through the `RunEvent` channel — `RunEvent.payload` is a separate carrier (per-task progress) and is not modified.
- Adding `data_ids` to the `RunHandle` returned by `register_*` (no consumer needs it; can be added later if a consumer materialises).
