# Task 05-09 — Pre-stamp inside `extract_graph_from_data` / `expand_with_nodes_and_edges`

**Status**: implemented in commit 9777f49 (pre_stamp_extraction helper; expand_with_nodes_and_edges and process_ontology_nodes gain user_label + visited-set parameters; add_data_points pre-stamps edge_types Vec; cognify's local stamp_provenance preserved per decision 6).
**Owner**: _unassigned_
**Depends on**:
- [Task 05-03 — Provenance core](03-provenance-core.md) (`ProvenanceContext`, `stamp_tree`).
- [Task 05-04 — `HasDataPoint` impls](04-has-datapoint-impls.md) (the recursion targets).

**Blocks**:
- [Task 05-10 — Tests](10-tests.md) (the cognify E2E and cross-SDK tests assert pre-stamp shape).

**Parent doc**: [05 — DataPoint Provenance Stamping](../05-datapoint-provenance.md)
**Locked decisions**: #6 (keep cognify's local `stamp_provenance` helper alongside executor stamping), #9 (pre-stamp in scope).

---

## 1. Goal

Mirror Python's
[`_stamp_provenance_deep`](https://github.com/topoteretes/cognee/blob/main/cognee/tasks/graph/extract_graph_from_data.py#L31)
in the Rust cognify pipeline: stamp freshly-LLM-constructed `Entity`,
`EntityType`, and `EdgeType` DataPoints with
`source_pipeline = "cognify_pipeline"` and
`source_task = "extract_graph_from_data"` **at the moment they're
emitted**, so the executor's recursive walk in
[task 05-06](06-pipeline-executor-integration.md) is a no-op for those
nodes.

After this task, two things are true:

1. Every `Entity` / `EntityType` / `EdgeType` produced by
   [`expand_with_nodes_and_edges`](../../crates/cognify/src/graph_integration/expansion.rs#L45)
   already carries `source_pipeline` and `source_task`. Subsequent
   tasks (summarize, add_data_points) only fill in `source_user`,
   `source_node_set`, `source_content_hash` (the input-derived fields).
2. Cognify's local `stamp_provenance` helper at
   [`tasks.rs:1694`](../../crates/cognify/src/tasks.rs#L1694) **stays**
   per locked decision 6, but its docstring gets a cross-reference to
   `cognee_core::provenance::stamp_tree` so future readers see the
   relationship.

The pre-stamp uses `cognee_core::stamp_tree` (the canonical algorithm
from 05-03) — not a third copy. Centralisation around that function is
the whole point of the gap.

## 2. Rationale

- **Parity** — Python pre-stamps so the recursion in `_stamp_provenance`
  finds `source_pipeline` already set and skips the assignment. Rust
  matches.
- **Deterministic ordering** — entities are constructed inside an LLM
  fan-out loop. Pre-stamping before the fan-out reaches the visited
  set guarantees the "first-seen" task name is the LLM-extraction
  task, regardless of which task the executor visits first.
- **Cross-SDK byte parity** — without pre-stamping, Rust nodes end up
  with the same content but the `source_task` may differ when the
  pipeline-driven path's "first-seen" task is `summarize_text` instead
  of `extract_graph_from_data`. The cross-SDK parity test in 05-10
  catches this difference.

## 3. Pre-conditions

- [Task 05-03](03-provenance-core.md) and
  [Task 05-04](04-has-datapoint-impls.md) committed.
- Clean `cargo check --all-targets` on `main`.

## 4. Step-by-step

### 4.1 Add a dedicated pre-stamp helper

Inside
[`crates/cognify/src/graph_integration/expansion.rs`](../../crates/cognify/src/graph_integration/expansion.rs)
(or a new sibling file), add:

```rust
use std::collections::HashSet;

use cognee_core::{HasDataPoint, ProvenanceContext, stamp_tree};
use cognee_models::DataPoint;
use uuid::Uuid;

/// Stamp a freshly-constructed Entity / EntityType / EdgeType at
/// emission time so the pipeline executor's recursion finds
/// `source_pipeline` and `source_task` already set.
///
/// Mirrors Python's `_stamp_provenance_deep` in
/// [`tasks/graph/extract_graph_from_data.py`](https://github.com/topoteretes/cognee/blob/main/cognee/tasks/graph/extract_graph_from_data.py#L31).
///
/// `user_label` is the resolved provenance label
/// ([`PipelineContext::user_label`](../../core/src/task_context.rs)).
/// Pass `None` if the user is not known at construction time — the
/// executor walk fills in the field later.
fn pre_stamp_extraction(
    target: &mut dyn HasDataPoint,
    user_label: Option<&str>,
    visited: &mut HashSet<Uuid>,
) {
    let ctx = ProvenanceContext {
        pipeline_name: "cognify_pipeline",
        task_name: "extract_graph_from_data",
        user_label,
        node_set: None,
        content_hash: None,
    };
    stamp_tree(target, &ctx, visited);
}
```

### 4.2 Plumb a visited-set local to `expand_with_nodes_and_edges`

`expand_with_nodes_and_edges` does not have access to the per-run
`PipelineContext::provenance_visited` (it's a free function with no
`TaskContext`). Use a function-local `HashSet<Uuid>` so the pre-stamp's
"stamp once" invariant holds within this single function call:

```rust
let mut local_visited: HashSet<Uuid> = HashSet::new();
```

The executor's per-run set will see the same DataPoints during its
own walk and short-circuit (the `source_pipeline` is already `Some`,
so the algorithm does not overwrite — locked decision 2). The two
visited sets do **not** need to share state.

If a future cleanup threads the per-run set down to this function, the
local set can go away. Out of scope for now.

### 4.3 Pre-stamp at every Entity / EntityType / EdgeType construction

In [`expansion.rs`](../../crates/cognify/src/graph_integration/expansion.rs),
locate the construction sites:

- Line 79 — `let mut et = EntityType::from_node_type(...)` — pre-stamp
  before insertion into `type_map`.
- `create_entity_node(...)` (called at line 156, defined at line 294) —
  pre-stamp the constructed entity *after* `create_entity_node` returns
  but *before* `e.insert(entity_pair)` at line 195.
- Ontology-derived entities and entity types constructed inside
  [`process_ontology_nodes`](../../crates/cognify/src/graph_integration/expansion.rs#L345)
  (line 372 `EntityType::new` for `NodeCategory::Classes`; line 386
  `Entity::new` for `NodeCategory::Individuals`). These still come from
  the same task, so pre-stamp them too — pass a mutable reference to
  `local_visited` and the `user_label` into `process_ontology_nodes`.

`EdgeType::new_deterministic(...)` is **not** called inside
`expand_with_nodes_and_edges`; it's called at
[`tasks.rs:764`](../../crates/cognify/src/tasks.rs#L764) inside
`add_data_points`. Pre-stamping `EdgeType` is handled separately in §4.5
below (or as a sibling change inside `add_data_points`), not as part of
the expansion-layer signature change. The expansion change only covers
`Entity` and `EntityType`.

Pattern:

```rust
// Existing line:
let mut et = EntityType::from_node_type(&node.node_type, Some(dataset_id));

// Insert immediately after construction:
pre_stamp_extraction(&mut et, user_label, &mut local_visited);
```

`user_label` is threaded down from the caller. Update
`expand_with_nodes_and_edges`'s signature:

```rust
pub async fn expand_with_nodes_and_edges(
    graphs: Vec<(Uuid, KnowledgeGraph)>,
    dataset_id: Uuid,
    existing_edges_set: &HashSet<String>,
    ontology_resolver: &dyn OntologyResolver,
    user_label: Option<&str>,    // NEW
) -> (Vec<GraphNodePair>, Vec<GraphEdgePair>) {
    // …
    let mut local_visited: HashSet<Uuid> = HashSet::new();
    // …
}
```

Update the single call site of `expand_with_nodes_and_edges` (in
[`tasks.rs::extract_graph_from_data`](../../crates/cognify/src/tasks.rs#L308))
to pass the resolved label:

```rust
let user_label = input.user_id.as_ref().map(|id| id.to_string());
let (nodes, edges) = expand_with_nodes_and_edges(
    all_graphs,
    input.dataset_id,
    &existing_edges_set,
    ontology_resolver.as_ref(),
    user_label.as_deref(),
)
.await;
```

The `user_id`-to-string fallback here is correct because at this
point in the pipeline-driven path we don't yet have `user_email`
plumbed through `ExtractedChunks`. The executor's downstream walk
fills in the email-form label if the run has it
(`PipelineContext::user_label()` — task 05-07). Idempotent.

### 4.4 Update the local `stamp_provenance` helper docstring

Edit
[`crates/cognify/src/tasks.rs:1689-1693`](../../crates/cognify/src/tasks.rs#L1689-L1693)
— the existing docstring:

```rust
/// Stamp pipeline provenance fields on a [`DataPoint`].
///
/// Used by the **convenience [`cognify`] entry point** which bypasses
/// `cognee_core::execute()` and therefore does not benefit from the
/// executor-driven walk in
/// [`cognee_core::provenance::stamp_tree`]. Per locked decision 6 of
/// [`docs/telemetry/05-datapoint-provenance.md`](../../../docs/telemetry/05-datapoint-provenance.md),
/// both code paths land stamping; the `if dp.source_X.is_none()`
/// guards make double-stamping a no-op.
///
/// Pipeline-driven cognify uses the executor walk via
/// [`cognee_core::provenance::stamp_tree_dyn`] — see
/// [`crates/core/src/provenance.rs`](../../../core/src/provenance.rs).
///
/// Only sets each field if it is currently `None`, so earlier (more
/// specific) stamps are never overwritten.  Mirrors the Python
/// `run_tasks_base.py` post-task provenance stamping.
fn stamp_provenance(dp: &mut DataPoint, pipeline: &str, task: &str, user: Option<&str>) {
    /* unchanged body */
}
```

This is the durable cross-reference between the two stamping paths.
Without it, future maintainers will see two helpers with the same
purpose and try to "consolidate" — accidentally regressing one path
or the other.

### 4.5 Pre-stamp `EdgeType` inside `add_data_points`

`EdgeType` DataPoints are constructed at
[`tasks.rs:761-768`](../../crates/cognify/src/tasks.rs#L761) inside
`add_data_points`, not inside `expand_with_nodes_and_edges`. After the
construction loop, walk the freshly-built `edge_types: Vec<EdgeType>`
and pre-stamp each one with the same `("cognify_pipeline",
"extract_graph_from_data")` context (the LLM-derived edge-type names
trace back to the entity extraction task) using a function-local
`HashSet<Uuid>`:

```rust
let mut local_visited: HashSet<Uuid> = HashSet::new();
let user_label = /* derive as in §4.3 */;
for et in &mut edge_types {
    pre_stamp_extraction(et, user_label, &mut local_visited);
}
```

Place the helper call inline; do not change `add_data_points`'s
signature for this. The user label here can come from `input.user_id`
(string-form) — the executor walk fills in the email-form later.

DLT-derived edges (`extract_dlt_fk_edges`, line 1302) construct
`GraphEdgePair` instances rather than DataPoints; they carry no
DataPoint to stamp, so no pre-stamp call is needed in that path.

### 4.6 Add a unit test

Append to
[`crates/cognify/src/graph_integration/expansion.rs`](../../crates/cognify/src/graph_integration/expansion.rs)'s
existing test module:

```rust
#[tokio::test]
async fn pre_stamp_sets_pipeline_and_task_on_entity_types() {
    use cognee_models::KnowledgeGraph;
    use cognee_ontology::NoOpOntologyResolver;

    let dataset_id = Uuid::new_v4();
    let knowledge_graph = KnowledgeGraph {
        nodes: vec![/* one minimal node */],
        edges: vec![],
    };
    let chunk_id = Uuid::new_v4();
    let resolver = NoOpOntologyResolver::new();
    let existing = HashSet::new();

    let (nodes, _) = expand_with_nodes_and_edges(
        vec![(chunk_id, knowledge_graph)],
        dataset_id,
        &existing,
        &resolver,
        Some("alice@example.com"),
    )
    .await;

    assert!(!nodes.is_empty());
    for pair in &nodes {
        assert_eq!(
            pair.entity_type.base.source_pipeline.as_deref(),
            Some("cognify_pipeline")
        );
        assert_eq!(
            pair.entity_type.base.source_task.as_deref(),
            Some("extract_graph_from_data")
        );
        assert_eq!(
            pair.entity_type.base.source_user.as_deref(),
            Some("alice@example.com")
        );
    }
}
```

The minimal `KnowledgeGraph` shape is a trivial fixture; copy from
sibling tests in the same module.

## 5. Verification

```bash
# 1. Compile.
cargo check --all-targets

# 2. The new pre-stamp test passes.
cargo test -p cognee-cognify pre_stamp_sets_pipeline_and_task

# 3. Existing cognify tests still pass.
cargo test -p cognee-cognify

# 4. Clippy.
cargo clippy --all-targets -- -D warnings

# 5. Full check.
scripts/check_all.sh
```

## 6. Files modified

- [`crates/cognify/src/graph_integration/expansion.rs`](../../crates/cognify/src/graph_integration/expansion.rs)
  — `pre_stamp_extraction` helper, new `user_label` parameter, pre-stamp
  calls at every Entity / EntityType / EdgeType construction site, one
  unit test.
- [`crates/cognify/src/tasks.rs`](../../crates/cognify/src/tasks.rs)
  — pass `user_label` to `expand_with_nodes_and_edges`; expand the
  docstring on the local `stamp_provenance` helper.
- §4.5: pre-stamp loop over freshly-built `edge_types` inside
  `add_data_points` at
  [`tasks.rs:761-768`](../../crates/cognify/src/tasks.rs#L761). DLT FK
  extraction (`extract_dlt_fk_edges`) does not need a stamp because it
  produces edges, not DataPoints.

## 7. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| The local visited set in §4.2 lets a DP be pre-stamped *and* re-visited by the executor walk, double-paying the algorithm cost | Low — the executor walk's `if dp.source_pipeline.is_none()` guard short-circuits immediately | Acceptable; the cost is one HashSet hit per pre-stamped DP, well below other costs in the pipeline. |
| `expand_with_nodes_and_edges` signature change touches binding crates (capi/python/js) | None — this is an internal cognify crate function | n/a |
| Ontology-derived entities (line 97-104) skip the pre-stamp because they go through a different code path | Medium — easy to forget in §4.3 | Sub-agent A's review checklist explicitly mentions this site. The unit test in §4.6 can be extended with an ontology fixture if needed. |
| Adding a `cognee-core` dep to `cognee-cognify` if not already present | None — `cognee-cognify` already depends on `cognee-core` | n/a |

## 8. Out of scope

- Removing the local `stamp_provenance` helper (decision 6 prohibits).
- Switching `cognify::cognify()` to route through `cognee_core::execute`.
  Tracked as a follow-up under decision 6.
- Pre-stamping in memify (`crates/cognify/src/memify.rs`). Memify
  emits triplets which do not embed DataPoints — pre-stamping is
  a no-op there. If memify ever produces fresh DataPoints, repeat the
  pattern.
- Pre-stamping in the temporal cognify pipeline. Sub-agent A flags it
  if the temporal path emits fresh DataPoints worth pre-stamping.
