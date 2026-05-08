# Task 05-03 — Provenance core module (`HasDataPoint`, `stamp_tree`, extractors)

**Status**: ⬜ not started

> **Post-landing follow-up (locked in 05-04):** the `HasDataPoint`
> trait declared here is moved to `cognee-models` as part of
> [task 05-04](04-has-datapoint-impls.md) §4.1, and re-exported from
> `cognee_core::provenance` so the public paths
> (`cognee_core::provenance::HasDataPoint`, `cognee_core::HasDataPoint`)
> stay unchanged. The algorithm (`stamp_tree`, `ProvenanceContext`,
> the extract helpers) stays in this module. Trait move landed in
> commit 6af9040.

**Owner**: _unassigned_
**Depends on**:
- [Task 05-01 — `source_content_hash` field](01-source-content-hash-field.md) (the trait writes the field).

**Blocks**:
- [Task 05-04 — `HasDataPoint` impls for models](04-has-datapoint-impls.md).
- [Task 05-06 — Pipeline executor integration](06-pipeline-executor-integration.md).
- [Task 05-09 — Cognify pre-stamp](09-cognify-prestamp.md).

**Parent doc**: [05 — DataPoint Provenance Stamping](../05-datapoint-provenance.md)
**Locked decisions**: #1 (trait, not serde-JSON), #2 (UUID identity), #3 (no rename of `ExecStatusManager::stamp_provenance`).

---

## 1. Goal

Create a new module `crates/core/src/provenance.rs` that contains the
trait, context struct, and recursive walker that the pipeline executor
will call after every successful task. The module is the single source
of truth for the stamping algorithm; every other crate consumes it via
`cognee_core::provenance::*`.

Public surface:

```rust
//! Provenance-stamping algorithm shared across the cognify / memify /
//! ingestion pipelines.

use std::collections::HashSet;
use std::sync::Arc;

use cognee_models::DataPoint;
use uuid::Uuid;

use crate::task::Value;

/// Read / write access to the embedded `DataPoint` of a typed container,
/// plus a hook to recurse into nested `DataPoint`-bearing children.
///
/// This trait is the Rust analogue of Python's reflective
/// `model_fields` walk. Implementations are added crate-by-crate
/// (typically in `cognee-models`); types not implementing the trait
/// are silently passed through by `stamp_tree`.
pub trait HasDataPoint {
    fn data_point(&self) -> &DataPoint;
    fn data_point_mut(&mut self) -> &mut DataPoint;

    /// Visit every owned child that itself implements `HasDataPoint`.
    /// Default: no children. Override on container types like
    /// `Entity` (whose `entity_type: Box<EntityType>` is itself a
    /// `HasDataPoint`).
    fn for_each_child_mut(&mut self, _visit: &mut dyn FnMut(&mut dyn HasDataPoint)) {}
}

/// What we know at the call site of `stamp_tree`.
///
/// All fields are borrows so the executor can build a context per task
/// without cloning strings on the hot path.
#[derive(Clone, Copy)]
pub struct ProvenanceContext<'a> {
    pub pipeline_name: &'a str,
    pub task_name: &'a str,
    pub user_label: Option<&'a str>,
    pub node_set: Option<&'a str>,
    pub content_hash: Option<&'a str>,
}

/// Stamp a tree of `HasDataPoint` values in place.
///
/// Mirrors Python's `_stamp_provenance` in `run_tasks_base.py`:
///
/// - **Idempotent**: every assignment is guarded by `if dp.source_X.is_none()`,
///   so a downstream task never overwrites an upstream stamp.
/// - **Visited-set**: keyed on `DataPoint.id: Uuid` (locked decision 2).
///   Re-entering the same DataPoint is a no-op.
/// - **Inheritance**: `node_set` and `content_hash` inherit from the
///   parent context if absent on the DP, but a value already present
///   on the DP overrides for further recursion.
pub fn stamp_tree(
    root: &mut dyn HasDataPoint,
    ctx: &ProvenanceContext<'_>,
    visited: &mut HashSet<Uuid>,
) { /* see §4.3 */ }

/// Walk an `Arc<dyn Value>` looking for the first non-empty
/// `source_node_set` on a `DataPoint`-bearing value.
///
/// Used by the pipeline executor to derive the "default" node-set for
/// a task's outputs from its inputs (mirrors Python's `_extract_node_set`).
pub fn extract_node_set_from_value(value: &dyn Value) -> Option<String> { … }

/// Walk an `Arc<dyn Value>` looking for the first non-empty
/// `Data.content_hash` (raw ingestion artefact) **or**
/// `DataPoint.source_content_hash`. Mirrors Python's
/// `_extract_content_hash`.
pub fn extract_content_hash_from_value(value: &dyn Value) -> Option<String> { … }
```

The `extract_*_from_value` helpers do dynamic downcasting against a
small static list of types (`Data`, `Entity`, `EntityType`, …). The
list lives in this module; new types added in the future register here.

## 2. Rationale

- **Single algorithm in one place.** Python's `_stamp_provenance` and
  `_stamp_provenance_deep` live in two files and have subtly diverged
  over time. Putting one canonical impl in `cognee-core` prevents the
  same drift in Rust.
- **`HasDataPoint` is the right boundary.** The pipeline executor (in
  `cognee-core`) cannot depend on `cognee-models` (would create a cycle:
  `models` is a leaf crate). Instead, models implement the trait
  defined in `cognee-core`. Reverse direction is fine because
  `cognee-models` already depends on `cognee-core` indirectly via
  application crates that re-export both.

  *Wait — verify this dep direction at the start of the task.* If
  `cognee-models` does not currently depend on `cognee-core`, declare
  the trait in `cognee-models` instead and have `cognee-core` import
  it. The trait must live wherever both the models and the executor
  can see it.

- **Visited-set is a `HashSet<Uuid>`, not `HashSet<*const _>`.** Pointer
  identity in Rust is unstable across `Arc::clone` and move-into-`Arc`
  (the pointee may relocate at any `Box::new`). UUID is stable, matches
  Python's de-facto behaviour for cloned objects, and is locked by
  decision 2.

## 3. Pre-conditions

- [Task 05-01](01-source-content-hash-field.md) is committed
  (`source_content_hash` exists on `DataPoint`).
- Clean `cargo check --all-targets` on `main`.
- Confirm `cognee-models` is in the dep graph reachable from
  `cognee-core` for the new trait (`cargo tree -p cognee-core | head`).
  If not, the trait lives in `cognee-models` (see §2) — adjust §4.1.

## 4. Step-by-step

### 4.1 Decide the trait's home crate

Run:

```bash
cargo tree -p cognee-core --no-default-features 2>/dev/null \
  | grep cognee-models
```

- **If `cognee-core` already depends on `cognee-models`** — declare the
  trait in `crates/core/src/provenance.rs` (the obvious choice).
- **If not** — declare the trait in
  `crates/models/src/provenance_trait.rs` and put the algorithm
  (`stamp_tree`, extract helpers) in `crates/core/src/provenance.rs`,
  importing the trait from `cognee_models`. This keeps the algorithm
  with the executor that calls it.

The rest of this doc assumes the first case; if you take the second
path, update §4.2 / §4.3 to split files accordingly.

### 4.2 Create `crates/core/src/provenance.rs`

Add the file with the public surface from §1. The full algorithm body
for `stamp_tree`:

```rust
pub fn stamp_tree(
    root: &mut dyn HasDataPoint,
    ctx: &ProvenanceContext<'_>,
    visited: &mut HashSet<Uuid>,
) {
    let dp_id = root.data_point().id;
    if !visited.insert(dp_id) {
        return;
    }

    {
        let dp = root.data_point_mut();
        if dp.source_pipeline.is_none() {
            dp.source_pipeline = Some(ctx.pipeline_name.to_string());
        }
        if dp.source_task.is_none() {
            dp.source_task = Some(ctx.task_name.to_string());
        }
        if dp.source_user.is_none() {
            if let Some(u) = ctx.user_label {
                dp.source_user = Some(u.to_string());
            }
        }
    }

    // Compute the inherited values once before recursing. A DP that
    // already carries node_set / content_hash exposes its own value to
    // children; otherwise the parent context's value flows down.
    let current_node_set = {
        let dp = root.data_point();
        match dp.source_node_set.as_deref() {
            Some(v) => Some(v.to_string()),
            None => ctx.node_set.map(|s| s.to_string()),
        }
    };
    if root.data_point().source_node_set.is_none() {
        if let Some(v) = ctx.node_set {
            root.data_point_mut().source_node_set = Some(v.to_string());
        }
    }

    let current_hash = {
        let dp = root.data_point();
        match dp.source_content_hash.as_deref() {
            Some(v) => Some(v.to_string()),
            None => ctx.content_hash.map(|s| s.to_string()),
        }
    };
    if root.data_point().source_content_hash.is_none() {
        if let Some(v) = ctx.content_hash {
            root.data_point_mut().source_content_hash = Some(v.to_string());
        }
    }

    // Recurse into children with the updated context.
    let child_ctx = ProvenanceContext {
        pipeline_name: ctx.pipeline_name,
        task_name: ctx.task_name,
        user_label: ctx.user_label,
        node_set: current_node_set.as_deref(),
        content_hash: current_hash.as_deref(),
    };

    root.for_each_child_mut(&mut |child| {
        stamp_tree(child, &child_ctx, visited);
    });
}
```

Two subtle points worth a comment:

- The temporary `current_node_set` / `current_hash` `String`
  allocations are only made when recursion will actually use them
  (i.e. the DP has children). For leaf DPs, `for_each_child_mut`'s
  default impl is a no-op, so the strings are dropped immediately.
- The order of "set on DP" vs "build child ctx" matches Python: if the
  DP had no value but the parent ctx did, both the DP and the children
  see the inherited value (consistent attribution).

### 4.3 Implement `extract_node_set_from_value`

```rust
pub fn extract_node_set_from_value(value: &dyn Value) -> Option<String> {
    use cognee_models::{Document, DocumentChunk, Entity, EntityType, EdgeType, Triplet};

    if let Some(d) = value.as_any().downcast_ref::<Document>() {
        return d.base.source_node_set.clone();
    }
    if let Some(d) = value.as_any().downcast_ref::<DocumentChunk>() {
        return d.base.source_node_set.clone();
    }
    if let Some(d) = value.as_any().downcast_ref::<Entity>() {
        return d.base.source_node_set.clone();
    }
    // … same shape for EntityType, EdgeType, Triplet, TextSummary, …

    // Containers (Vec<T>, CognifyInput, ExtractedChunks, etc.) are
    // intentionally not walked: the executor sees them as opaque
    // `Arc<dyn Value>` and the Python equivalent only walks the
    // first-level `args` list. If callers want richer extraction,
    // they should pass a flat input.

    None
}
```

The downcast list is the same set as `for_each_child_mut` impls in
[task 05-04](04-has-datapoint-impls.md). Keep the two lists in sync;
add a unit test in this task that fails if they drift (see §4.6).

### 4.4 Implement `extract_content_hash_from_value`

```rust
pub fn extract_content_hash_from_value(value: &dyn Value) -> Option<String> {
    use cognee_models::Data;

    // Raw ingestion artefact takes priority: this is the lineage anchor.
    if let Some(d) = value.as_any().downcast_ref::<Data>() {
        if !d.content_hash.is_empty() {
            return Some(d.content_hash.clone());
        }
    }

    // Fall through: any DataPoint that already carries a hash.
    if let Some(dp) = downcast_to_datapoint(value) {
        return dp.source_content_hash.clone();
    }

    None
}

/// Internal helper: `value` → optional borrow of its embedded DataPoint.
/// One-liner matches against the same registered types as
/// `extract_node_set_from_value`.
fn downcast_to_datapoint(value: &dyn Value) -> Option<&DataPoint> {
    use cognee_models::{Document, DocumentChunk, Entity, EntityType, EdgeType, Triplet};

    if let Some(d) = value.as_any().downcast_ref::<Document>() {
        return Some(&d.base);
    }
    if let Some(d) = value.as_any().downcast_ref::<DocumentChunk>() {
        return Some(&d.base);
    }
    // … keep aligned with §4.3's list.
    None
}
```

Per locked decision 7 / [task 05-02](02-data-content-hash-audit.md), we
treat empty `content_hash` as "no hash" — the audit confirmed this is
not a real production case but it is cheap defensiveness.

### 4.5 Re-export from `cognee-core`

Edit
[`crates/core/src/lib.rs`](../../crates/core/src/lib.rs):

```rust
pub mod provenance;

pub use provenance::{
    HasDataPoint, ProvenanceContext, extract_content_hash_from_value,
    extract_node_set_from_value, stamp_tree,
};
```

If you took the split-crates path from §4.1, also add
`pub use cognee_models::HasDataPoint;` so consumers see one canonical
trait path.

### 4.6 Unit tests (port from Python parity suite)

Add `crates/core/tests/provenance.rs`. Eight cases ported from
[`/tmp/cognee-python/cognee/tests/unit/modules/pipelines/test_provenance_stamping.py`](https://github.com/topoteretes/cognee/blob/main/cognee/tests/unit/modules/pipelines/test_provenance_stamping.py)
(clone the Python repo per the project guide if not already present):

1. **`bare_datapoint_gets_stamped`** — a `DataPoint` with all
   `source_*` fields `None` ends up with `source_pipeline`,
   `source_task`, `source_user` populated after `stamp_tree`.
2. **`existing_values_not_overwritten`** — pre-set
   `source_pipeline = Some("OldPipeline")` survives the call; only
   the `None` fields are filled.
3. **`nested_datapoint_recursion`** — stub a parent type with one
   child DP via `for_each_child_mut`; both get stamped.
4. **`visited_set_short_circuits_cycles`** — manually re-enter the
   same DP (call `stamp_tree` twice on the same root with one shared
   `visited` set); the second call is a no-op verified by mutating
   `source_pipeline` between the two calls and asserting the second
   call did not overwrite.
5. **`node_set_inherits_from_context`** — DP with `source_node_set =
   None` and `ctx.node_set = Some("custom_set")` ends up with
   `Some("custom_set")`.
6. **`node_set_on_dp_overrides_context`** — DP with `source_node_set
   = Some("dp_set")` and `ctx.node_set = Some("ctx_set")` keeps
   `"dp_set"`. Verify children downstream see `"dp_set"` (not
   `"ctx_set"`).
7. **`content_hash_inherits_from_context`** — symmetric to #5.
8. **`content_hash_on_dp_overrides_context`** — symmetric to #6.

Each test is ~15 lines. They form the canonical Rust parity suite for
the algorithm.

Add a **ninth** test that fails if the downcast list in §4.3 / §4.4
drifts from the `HasDataPoint` impl list once 05-04 lands:

```rust
#[test]
fn extract_helpers_cover_all_known_datapoint_types() {
    // Every type listed below must have a `HasDataPoint` impl AND be
    // recognised by extract_node_set_from_value /
    // extract_content_hash_from_value. Adding a new container type
    // requires touching all three places — this test enforces it.
    let known_types: &[&str] = &[
        "cognee_models::document::Document",
        "cognee_models::document_chunk::DocumentChunk",
        "cognee_models::entity::Entity",
        "cognee_models::entity_type::EntityType",
        "cognee_models::edge_type::EdgeType",
        "cognee_models::triplet::Triplet",
        // Add TextSummary, etc., as 05-04 expands the list.
    ];
    // Construct one of each; build a Vec<Arc<dyn Value>>;
    // assert that each yields a non-None DP via the helper used
    // through downcast_to_datapoint OR through stamp_tree on a
    // wrapper container.
    // (Compile-time enforcement is impossible without a procedural
    // macro; this runtime smoke test is the next-best guard.)
    let _ = known_types; // body filled in once 05-04 lands.
}
```

This stays as a stub returning `Ok(())` until 05-04 implements the
trait everywhere — at which point sub-agent A for 05-04 will flesh it
out.

### 4.7 Module-level docstring on the name collision (decision 3)

Inside `crates/core/src/provenance.rs`, lead with:

```rust
//! Provenance stamping for DataPoints emitted by pipeline tasks.
//!
//! This module is **not** the same thing as
//! [`crate::exec_status::ExecStatusManager::stamp_provenance`]. That
//! trait method is an audit-log hook keyed on `data_id` (the input
//! item's content hash) and never mutates DataPoint fields. The
//! function in *this* module mutates DataPoint fields (the five
//! `source_*` columns) and is called from the pipeline executor after
//! every successful task. Both run during a normal pipeline run; they
//! address different concerns (one writes a per-data-id row in the
//! relational DB, the other writes onto the DataPoints flowing through
//! the executor).
```

This is the durable home for the explanation; downstream readers do
not have to understand the history to make sense of the two functions.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. New unit tests pass.
cargo test -p cognee-core provenance

# 3. The cognee-core crate still builds without default features.
cargo check -p cognee-core --no-default-features

# 4. Clippy.
cargo clippy --all-targets -- -D warnings

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/core/src/provenance.rs`](../../crates/core/src/provenance.rs)
  — NEW. Trait, context, algorithm, extractors, module docstring.
- [`crates/core/src/lib.rs`](../../crates/core/src/lib.rs) — add
  `pub mod provenance;` and the four public re-exports.
- [`crates/core/tests/provenance.rs`](../../crates/core/tests/provenance.rs)
  — NEW. Eight Python-parity tests + the drift-guard stub.
- (Conditional, see §4.1) [`crates/core/Cargo.toml`](../../crates/core/Cargo.toml)
  if a `cognee-models` direct dep needs to be added.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `cognee-models` is a leaf and cannot be a dep of `cognee-core` | Medium — the dep graph today is application-crate-driven. | §4.1 has a fallback: declare the trait in `cognee-models`, keep the algorithm in `cognee-core`. |
| `for_each_child_mut` borrow checker issues with `&mut self` recursion through `Box<dyn FnMut>` | Medium | The proposed signature uses `&mut dyn FnMut(&mut dyn HasDataPoint)`, which is the standard pattern for visitor recursion in Rust and is known to compile (see e.g. `serde::Serialize::serialize`). The unit tests in §4.6 catch any signature mismatches. |
| Visited-set grows unboundedly within one pipeline run | Low — bounded by the number of unique DataPoints actually emitted per run, typically O(thousands). | Acceptable: same magnitude as the graph DB it's about to be written to. |
| Adding a new container type later forgets to update the extractors | Medium-high — there is no compile-time enforcement | Drift-guard test in §4.6 (`extract_helpers_cover_all_known_datapoint_types`). Sub-agent A for new container types must update both lists. |

## 8. Out of scope

- The actual `HasDataPoint` impls — those are [task 05-04](04-has-datapoint-impls.md).
- Pipeline executor wiring — that is [task 05-06](06-pipeline-executor-integration.md).
- Renaming `ExecStatusManager::stamp_provenance` (decision 3 prohibits).
- A procedural macro to derive `HasDataPoint` automatically — premature;
  the manual impls fit on one screen each.
