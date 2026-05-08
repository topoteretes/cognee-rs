# Task 05-04 — `HasDataPoint` impls for model containers

**Status**: ⬜ not started — placement decision locked: `HasDataPoint` trait moves into `cognee-models` (option 1); `cognee-core::provenance` re-exports it so existing public paths keep working.
**Owner**: _unassigned_
**Depends on**:
- [Task 05-03 — Provenance core](03-provenance-core.md) (defines the `HasDataPoint` trait and the algorithm).

**Blocks**:
- [Task 05-06 — Pipeline executor integration](06-pipeline-executor-integration.md).
- [Task 05-09 — Cognify pre-stamp](09-cognify-prestamp.md).

**Parent doc**: [05 — DataPoint Provenance Stamping](../05-datapoint-provenance.md)
**Locked decisions**: #1 (trait, not serde-JSON).

---

## 1. Goal

Implement `HasDataPoint` for every type in the workspace that wraps a
`DataPoint` via `#[serde(flatten)] base: DataPoint`. After this task, the
`stamp_tree` algorithm from 05-03 can mutate any of these types.

Concrete impl set (verified against the current source):

| Type | Crate / file | Has nested `HasDataPoint` children? |
|---|---|---|
| `Document` | [`crates/models/src/document.rs`](../../crates/models/src/document.rs) | No |
| `DocumentChunk` | [`crates/models/src/document_chunk.rs`](../../crates/models/src/document_chunk.rs) | No (links to `Document` via `document_id: Uuid`) |
| `Entity` | [`crates/models/src/entity.rs`](../../crates/models/src/entity.rs) | No (links to `EntityType` via `is_a: Option<Uuid>`) |
| `EntityType` | [`crates/models/src/entity_type.rs`](../../crates/models/src/entity_type.rs) | No |
| `EdgeType` | [`crates/models/src/edge_type.rs`](../../crates/models/src/edge_type.rs) | No |
| `TextSummary` | [`crates/cognify/src/summarization/models.rs`](../../crates/cognify/src/summarization/models.rs) | No |
| `Triplet` | [`crates/models/src/triplet.rs`](../../crates/models/src/triplet.rs) | **Skip — does not embed `DataPoint`** (has its own `id: Uuid` plus `source_entity_id` / `target_entity_id` references). Document why in §4.7. |

Each impl is ~6 lines. The `for_each_child_mut` body is the default
no-op for every type listed: Rust's containers reference siblings by
`Uuid`, not by owned `Box<HasDataPoint>`, so there is nothing to recurse
into.

If a future container type owns a nested DP (e.g. a hypothetical
`EntityWithType { base: DataPoint, entity_type: Box<EntityType> }`), its
impl overrides `for_each_child_mut` to walk the child.

## 2. Rationale

- Rust's model layer is intentionally flat: nested DataPoints are
  referenced by `Uuid`, not owned. This makes `HasDataPoint` impls
  trivial (default `for_each_child_mut`) but also means most of the
  recursive machinery in `stamp_tree` is over-engineered for the
  current schema. We keep the recursion in place because it's
  required for parity with Python's reflective walk and harmless for
  flat types.
- `Triplet` is the one container that *looks* like it should be a
  `HasDataPoint` but isn't — it has its own UUID and never went
  through the `DataPoint` base. Skipping it preserves correctness.
  It still gets its provenance via the vector-store payload helper
  in 05-08, which serialises it on a different path.

## 3. Pre-conditions

- [Task 05-03](03-provenance-core.md) is committed.
- Clean `cargo check --all-targets` on `main`.
- `cognee-cognify` already depends on `cognee-models` (verify with
  `cargo tree -p cognee-cognify | grep cognee-models`); the
  `TextSummary` impl in §4.3b uses the trait via `cognee_models::HasDataPoint`.
- `cognee-core` already depends on `cognee-models` (it is part of the
  algorithm crate's input set); verify with
  `cargo tree -p cognee-core | grep cognee-models` so the re-export in §4.2
  compiles.

## 4. Step-by-step

### 4.1 Move the trait into `cognee-models` (placement decision — option 1)

The `HasDataPoint` trait declared by 05-03 in
`crates/core/src/provenance.rs` moves to a new module
`crates/models/src/has_datapoint.rs`, alongside the six model impls
landing in this task. The algorithm (`stamp_tree`,
`ProvenanceContext`, `extract_node_set_from_value`,
`extract_content_hash_from_value`) stays in `crates/core/src/provenance.rs`
and imports the trait from `cognee_models`.

Concretely:

1. Cut the `pub trait HasDataPoint { … }` block from
   `crates/core/src/provenance.rs` and paste it into a new file
   `crates/models/src/has_datapoint.rs`. Keep the doc-comment exactly
   as it was in 05-03.
2. In `crates/models/src/lib.rs`, add `pub mod has_datapoint;` and
   `pub use has_datapoint::HasDataPoint;` so the trait is re-exported
   from the crate root.
3. In `crates/core/src/provenance.rs`, replace the trait declaration
   with `pub use cognee_models::HasDataPoint;`. This preserves the
   existing public API: `cognee_core::provenance::HasDataPoint` and
   `cognee_core::HasDataPoint` (via the re-export in
   `crates/core/src/lib.rs` from 05-03 §4.5) still resolve to the
   same trait.
4. Update the crate root re-exports in
   `crates/core/src/lib.rs` if needed: the `pub use provenance::{ … HasDataPoint … }`
   line from 05-03 stays as written — the `provenance` module now
   re-exports the trait from `cognee_models`, and the lib.rs re-export
   forwards it transparently. No downstream caller needs an import
   change.

**Why option 1 is correct here.** The six target containers all live
in `cognee-models` (and one in `cognee-cognify` which already depends
on `cognee-models`). Putting the trait in `cognee-models` keeps the
trait next to its primary impls and avoids forcing `cognee-models` to
depend on `cognee-core` just to see a trait it owns morally. The
algorithm (`stamp_tree`) still lives in `cognee-core` because that is
where the executor calls it.

### 4.2 Add the import to each model file

Top of each target file in `crates/models/src/`, add:

```rust
use crate::has_datapoint::HasDataPoint;
use crate::DataPoint; // already in scope in most of these files
```

In `crates/cognify/src/summarization/models.rs`, add:

```rust
use cognee_models::HasDataPoint;
```

(This file already imports `cognee_models::DataPoint`.)

### 4.3a Implement `HasDataPoint` for the five `cognee-models` containers

Pattern, repeated for `Document`, `DocumentChunk`, `Entity`,
`EntityType`, `EdgeType` — each lives in its own `crates/models/src/*.rs`
file alongside the struct declaration:

```rust
impl HasDataPoint for Entity {
    fn data_point(&self) -> &DataPoint {
        &self.base
    }
    fn data_point_mut(&mut self) -> &mut DataPoint {
        &mut self.base
    }
    // for_each_child_mut: default no-op — Entity references EntityType
    // by UUID (`is_a`), not by ownership.
}
```

All six target structs (the five above plus `TextSummary`) carry their
DataPoint via `#[serde(flatten)] pub base: DataPoint`, so `&self.base`
/ `&mut self.base` is the correct accessor in every case (verified in
the source: `crates/models/src/{document,document_chunk,entity,entity_type,edge_type}.rs`
and `crates/cognify/src/summarization/models.rs`).

For each impl, do **not** add a docstring beyond a single comment
explaining why `for_each_child_mut` is the default (Rust readers will
otherwise wonder if the recursion was forgotten).

### 4.3b Implement `HasDataPoint` for `TextSummary` in `cognee-cognify`

Same shape, in `crates/cognify/src/summarization/models.rs`:

```rust
impl HasDataPoint for TextSummary {
    fn data_point(&self) -> &DataPoint {
        &self.base
    }
    fn data_point_mut(&mut self) -> &mut DataPoint {
        &mut self.base
    }
    // for_each_child_mut: default no-op — TextSummary's `made_from`
    // reference is a UUID, not an owned child.
}
```

`cognee-cognify` already depends on `cognee-models`, so the trait is
visible via `use cognee_models::HasDataPoint;` (added in §4.2) without
any new Cargo.toml change.

### 4.4 (No impl) `Triplet` — document the skip

Add a short comment in `crates/models/src/triplet.rs` near the struct
declaration:

```rust
// `Triplet` intentionally does NOT implement `HasDataPoint`: it does
// not embed a `DataPoint` (it has its own `id: Uuid` field and is
// constructed deterministically via UUID v5 from the edge key). Its
// provenance lands via the vector-store payload helper in
// `cognee_core::provenance` indirectly when the originating edge is
// stamped, not via `stamp_tree` recursion. See gap-05 task 05-04 §4.4.
```

### 4.5 Update the drift-guard test from 05-03

`crates/core/tests/provenance.rs::extract_helpers_cover_all_known_datapoint_types`
(see [05-03 §4.6](03-provenance-core.md#46-unit-tests-port-from-python-parity-suite))
flesh-outs:

```rust
#[test]
fn extract_helpers_cover_all_known_datapoint_types() {
    use cognee_models::{Document, DocumentChunk, Entity, EntityType, EdgeType, DataPoint};
    use cognee_cognify::TextSummary;
    // The trait now lives in cognee-models; cognee-core re-exports it.
    // Either path resolves to the same trait — use cognee-core here to
    // mirror what the executor sees.
    use cognee_core::{HasDataPoint, extract_node_set_from_value};
    use std::sync::Arc;
    use uuid::Uuid;

    fn check<T>(value: T)
    where
        T: cognee_core::Value + 'static,
    {
        let arc: Arc<dyn cognee_core::Value> = Arc::new(value);
        // No assertion on the return value; we only confirm the call
        // does not panic and the type is recognised by the downcast
        // (i.e. the helpers list this type).
        let _ = extract_node_set_from_value(arc.as_ref());
    }

    let dataset_id = Some(Uuid::new_v4());

    check(Document { base: DataPoint::new("TextDocument", dataset_id), document_type: "text".into() });
    check(DocumentChunk { /* … minimal fields … */ });
    check(Entity::new("Foo", None, "desc".into(), dataset_id));
    check(EntityType::new("Org", "desc".into(), dataset_id));
    check(EdgeType::new("rel", dataset_id));
    check(TextSummary { base: DataPoint::new("TextSummary", dataset_id), made_from: None, text: "".into(), description: None, model: "".into() });

    // Triplet is intentionally absent — see 05-04 §4.4.
}
```

The test "passes" by not panicking. If a new container type is added
later but forgotten in `extract_node_set_from_value`, the new caller
will not be exercised here, but a follow-up grep in CI is the actual
defence; this test is the smoke alarm.

### 4.6 Add a `HasDataPoint` smoke test per crate

Two test locations (one per crate that hosts impls):

- **`cognee-models`** — append five smoke tests to the existing inline
  test modules in `crates/models/src/{document,document_chunk,entity,entity_type,edge_type}.rs`
  (or one consolidated module under `crates/models/tests/has_datapoint.rs`,
  whichever matches the existing convention in that file — most of
  these files already have inline `#[cfg(test)] mod tests` blocks, so
  prefer inline). The trait is in scope via `use crate::HasDataPoint;`
  (re-exported from the crate root in §4.1 step 2).
- **`cognee-cognify`** — append one smoke test for `TextSummary` to the
  existing test module in `crates/cognify/src/summarization/models.rs`.
  Trait via `use cognee_models::HasDataPoint;`.

Pattern (one per type, ~6 lines each):

```rust
#[test]
fn entity_implements_has_datapoint() {
    use crate::HasDataPoint; // in cognee-models; or `use cognee_models::HasDataPoint;` in cognify
    let e = Entity::new("Foo", None, "desc".into(), None);
    let dp_id = e.base.id;
    assert_eq!(e.data_point().id, dp_id);
    let mut e2 = e;
    assert_eq!(e2.data_point_mut().id, dp_id);
}
```

These are mechanical and ensure that a renamed `base` field or a
forgotten import is caught at unit-test time, not at integration time.

### 4.7 No impl for non-DataPoint types

Confirm by greppping that the workspace does **not** introduce
`HasDataPoint` impls on:

- `CognifyInput` / `ClassifiedDocuments` / `ExtractedChunks` (pipeline-stage carriers — they hold `Vec<HasDataPoint>` but are not themselves DataPoints)
- `KnowledgeGraph` (graph snapshot, not a node)
- `Triplet` (per §4.4)
- `Tagged<T>` / `TaggedMeta` (executor metadata, not a DataPoint)
- `Data` / `Dataset` (raw ingestion artefacts; their `content_hash` is read by the extractor in 05-03 but they are not stamped as DataPoints)

If the implementor is unsure about a type, default to **no impl**: the
algorithm passes through unrecognised types. False negatives (a missed
container) are detected by the cross-SDK parity test in 05-10.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. The drift-guard test from 05-03 is no longer a stub and now
#    exercises every implemented type.
cargo test -p cognee-core extract_helpers_cover_all_known_datapoint_types

# 3. Per-crate smoke tests pass.
cargo test -p cognee-models has_datapoint
cargo test -p cognee-cognify text_summary_implements_has_datapoint

# 4. Clippy.
cargo clippy --all-targets -- -D warnings

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/models/src/has_datapoint.rs`](../../crates/models/src/has_datapoint.rs)
  — NEW. Holds the `HasDataPoint` trait moved from
  `crates/core/src/provenance.rs` (placement decision, §4.1).
- [`crates/models/src/lib.rs`](../../crates/models/src/lib.rs)
  — add `pub mod has_datapoint;` and `pub use has_datapoint::HasDataPoint;`.
- [`crates/core/src/provenance.rs`](../../crates/core/src/provenance.rs)
  — remove the trait declaration; replace with
  `pub use cognee_models::HasDataPoint;` so existing
  `cognee_core::provenance::HasDataPoint` and `cognee_core::HasDataPoint`
  paths keep resolving.
- [`crates/models/src/document.rs`](../../crates/models/src/document.rs)
- [`crates/models/src/document_chunk.rs`](../../crates/models/src/document_chunk.rs)
- [`crates/models/src/entity.rs`](../../crates/models/src/entity.rs)
- [`crates/models/src/entity_type.rs`](../../crates/models/src/entity_type.rs)
- [`crates/models/src/edge_type.rs`](../../crates/models/src/edge_type.rs)
- [`crates/models/src/triplet.rs`](../../crates/models/src/triplet.rs) — comment only.
- [`crates/cognify/src/summarization/models.rs`](../../crates/cognify/src/summarization/models.rs)
- [`crates/core/tests/provenance.rs`](../../crates/core/tests/provenance.rs)
  — replace the drift-guard stub from 05-03 with the real assertions.
- No Cargo.toml changes: `cognee-cognify` already depends on
  `cognee-models`; `cognee-core` already depends on `cognee-models`.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| Hidden DataPoint container we missed (e.g. a memify-specific type) | Medium | Run `grep -rn "#\[serde(flatten)\]" crates/ | grep -B1 -A1 "DataPoint"` at the start of the task; flag every hit not in the target list. |
| The trait move breaks any out-of-tree consumer that pinned `cognee_core::provenance::HasDataPoint` to a specific item path | Low — the re-export in §4.1 step 3 keeps the public path stable | Verified at integration time by `cargo check --all-targets`; no further mitigation needed. |
| The smoke tests churn `Entity::new` / `DocumentChunk::new` signatures | Low — the test bodies are tiny and constructors are stable | Trivial fix at sub-agent C time. |

## 8. Out of scope

- A `derive` macro for `HasDataPoint`. Six manual impls fit on one
  screen each.
- Adding `HasDataPoint` impls for `Triplet` / `KnowledgeGraph` /
  pipeline-stage carriers — they don't carry a `DataPoint` base.
- Renaming the `base: DataPoint` field on any container (would touch
  every serde consumer).
