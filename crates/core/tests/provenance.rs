//! Unit tests for `cognee_core::provenance::stamp_tree` ported from the
//! Python parity suite at
//! `cognee/tests/unit/modules/pipelines/test_provenance_stamping.py`.
//!
//! Eight cases mirror the locked semantics:
//!
//! 1. bare DataPoint stamping
//! 2. existing values are not overwritten
//! 3. nested DataPoint recursion via `for_each_child_mut`
//! 4. visited-set short-circuits cycles (and re-entries)
//! 5. node_set inherits from context
//! 6. node_set on DP overrides context for further recursion
//! 7. content_hash inherits from context
//! 8. content_hash on DP overrides context for further recursion
//!
//! A ninth drift-guard test (`extract_helpers_cover_all_known_datapoint_types`)
//! is a stub; it will be fleshed out by gap 05-04 when `HasDataPoint`
//! impls land on the `cognee-models` container types.

use std::collections::HashSet;

use cognee_core::provenance::{HasDataPoint, ProvenanceContext, stamp_tree};
use cognee_models::DataPoint;

/// Minimal test container that wraps a `DataPoint` with no children.
/// Mirrors the shape of `cognee_models::Entity` etc. that gap 05-04
/// will eventually wire up.
struct LeafContainer {
    base: DataPoint,
}

impl LeafContainer {
    fn new() -> Self {
        Self {
            base: DataPoint::new("LeafTest", None),
        }
    }
}

impl HasDataPoint for LeafContainer {
    fn data_point(&self) -> &DataPoint {
        &self.base
    }
    fn data_point_mut(&mut self) -> &mut DataPoint {
        &mut self.base
    }
}

/// Test container with one nested `HasDataPoint` child, mirroring how
/// e.g. `Entity { entity_type: Box<EntityType> }` will recurse.
struct ParentContainer {
    base: DataPoint,
    child: LeafContainer,
}

impl ParentContainer {
    fn new() -> Self {
        Self {
            base: DataPoint::new("ParentTest", None),
            child: LeafContainer::new(),
        }
    }
}

impl HasDataPoint for ParentContainer {
    fn data_point(&self) -> &DataPoint {
        &self.base
    }
    fn data_point_mut(&mut self) -> &mut DataPoint {
        &mut self.base
    }
    fn for_each_child_mut(&mut self, visit: &mut dyn FnMut(&mut dyn HasDataPoint)) {
        visit(&mut self.child);
    }
}

fn ctx<'a>(
    pipeline_name: &'a str,
    task_name: &'a str,
    user_label: Option<&'a str>,
    node_set: Option<&'a str>,
    content_hash: Option<&'a str>,
) -> ProvenanceContext<'a> {
    ProvenanceContext {
        pipeline_name,
        task_name,
        user_label,
        node_set,
        content_hash,
    }
}

#[test]
fn bare_datapoint_gets_stamped() {
    let mut leaf = LeafContainer::new();
    let mut visited = HashSet::new();
    let c = ctx(
        "cognify_pipeline",
        "extract_chunks",
        Some("alice@x"),
        None,
        None,
    );

    stamp_tree(&mut leaf, &c, &mut visited);

    assert_eq!(
        leaf.base.source_pipeline.as_deref(),
        Some("cognify_pipeline")
    );
    assert_eq!(leaf.base.source_task.as_deref(), Some("extract_chunks"));
    assert_eq!(leaf.base.source_user.as_deref(), Some("alice@x"));
    assert!(leaf.base.source_node_set.is_none());
    assert!(leaf.base.source_content_hash.is_none());
}

#[test]
fn existing_values_not_overwritten() {
    let mut leaf = LeafContainer::new();
    leaf.base.source_pipeline = Some("OldPipeline".to_string());
    leaf.base.source_task = Some("old_task".to_string());
    leaf.base.source_user = Some("old@x".to_string());

    let mut visited = HashSet::new();
    let c = ctx("cognify_pipeline", "new_task", Some("new@x"), None, None);

    stamp_tree(&mut leaf, &c, &mut visited);

    // Nothing previously set should have been overwritten.
    assert_eq!(leaf.base.source_pipeline.as_deref(), Some("OldPipeline"));
    assert_eq!(leaf.base.source_task.as_deref(), Some("old_task"));
    assert_eq!(leaf.base.source_user.as_deref(), Some("old@x"));
}

#[test]
fn nested_datapoint_recursion() {
    let mut parent = ParentContainer::new();
    let mut visited = HashSet::new();
    let c = ctx("cognify_pipeline", "task_x", Some("u@x"), None, None);

    stamp_tree(&mut parent, &c, &mut visited);

    // Both parent and the nested child should be stamped.
    assert_eq!(
        parent.base.source_pipeline.as_deref(),
        Some("cognify_pipeline")
    );
    assert_eq!(parent.base.source_task.as_deref(), Some("task_x"));
    assert_eq!(
        parent.child.base.source_pipeline.as_deref(),
        Some("cognify_pipeline")
    );
    assert_eq!(parent.child.base.source_task.as_deref(), Some("task_x"));
    assert_eq!(parent.child.base.source_user.as_deref(), Some("u@x"));
}

#[test]
fn visited_set_short_circuits_cycles() {
    let mut leaf = LeafContainer::new();
    let mut visited = HashSet::new();

    let first = ctx("first_pipeline", "first_task", Some("u1"), None, None);
    stamp_tree(&mut leaf, &first, &mut visited);
    assert_eq!(leaf.base.source_pipeline.as_deref(), Some("first_pipeline"));

    // Mutate to a sentinel, then re-enter with the same visited set.
    // The second call must be a no-op so the sentinel survives — proves
    // the UUID-keyed visited-set short-circuited recursion.
    leaf.base.source_pipeline = Some("SENTINEL".to_string());

    let second = ctx("second_pipeline", "second_task", Some("u2"), None, None);
    stamp_tree(&mut leaf, &second, &mut visited);
    assert_eq!(leaf.base.source_pipeline.as_deref(), Some("SENTINEL"));
}

#[test]
fn node_set_inherits_from_context() {
    let mut leaf = LeafContainer::new();
    let mut visited = HashSet::new();
    let c = ctx("p", "t", None, Some("custom_set"), None);

    stamp_tree(&mut leaf, &c, &mut visited);

    assert_eq!(leaf.base.source_node_set.as_deref(), Some("custom_set"));
}

#[test]
fn node_set_on_dp_overrides_context() {
    let mut parent = ParentContainer::new();
    parent.base.source_node_set = Some("dp_set".to_string());

    let mut visited = HashSet::new();
    let c = ctx("p", "t", None, Some("ctx_set"), None);

    stamp_tree(&mut parent, &c, &mut visited);

    // The parent's pre-set node_set is preserved.
    assert_eq!(parent.base.source_node_set.as_deref(), Some("dp_set"));
    // Children downstream see the parent's value, not the ctx default.
    assert_eq!(parent.child.base.source_node_set.as_deref(), Some("dp_set"));
}

#[test]
fn content_hash_inherits_from_context() {
    let mut leaf = LeafContainer::new();
    let mut visited = HashSet::new();
    let c = ctx("p", "t", None, None, Some("md5:abc"));

    stamp_tree(&mut leaf, &c, &mut visited);

    assert_eq!(leaf.base.source_content_hash.as_deref(), Some("md5:abc"));
}

#[test]
fn content_hash_on_dp_overrides_context() {
    let mut parent = ParentContainer::new();
    parent.base.source_content_hash = Some("md5:dp".to_string());

    let mut visited = HashSet::new();
    let c = ctx("p", "t", None, None, Some("md5:ctx"));

    stamp_tree(&mut parent, &c, &mut visited);

    // Parent keeps its own hash; children inherit the parent's hash, not the ctx default.
    assert_eq!(parent.base.source_content_hash.as_deref(), Some("md5:dp"));
    assert_eq!(
        parent.child.base.source_content_hash.as_deref(),
        Some("md5:dp")
    );
}

/// Drift guard: every type listed below must have a `HasDataPoint`
/// impl AND be recognised by `extract_node_set_from_value` /
/// `extract_content_hash_from_value`. The full body lands with gap
/// 05-04, when the `HasDataPoint` impls do; until then this is a
/// passing stub so the test name is reserved and CI surfaces drift
/// reviews against this exact location.
#[test]
fn extract_helpers_cover_all_known_datapoint_types() {
    let known_types: &[&str] = &[
        "cognee_models::document::Document",
        "cognee_models::document_chunk::DocumentChunk",
        "cognee_models::entity::Entity",
        "cognee_models::entity_type::EntityType",
        "cognee_models::edge_type::EdgeType",
        // Add TextSummary, etc., as 05-04 expands the list.
    ];
    let _ = known_types; // body filled in once 05-04 lands.
}
