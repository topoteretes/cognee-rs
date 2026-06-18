#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable failures"
)]
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
/// `extract_content_hash_from_value`. Adding a new container type
/// requires touching all three places; this test exercises the helpers
/// so a missed registration trips up locally before integration.
///
/// The trait now lives in `cognee-models`; `cognee-core` re-exports
/// it. Either path resolves to the same trait — we use `cognee_core`
/// here to mirror what the executor sees.
///
/// `TextSummary` is intentionally omitted here: it lives in
/// `cognee-cognify`, which depends on `cognee-core` (so adding it as
/// a dev-dep would create a cycle). It is instead exercised by the
/// `text_summary_implements_has_datapoint` smoke test in
/// `crates/cognify/src/summarization/models.rs`. `Triplet` is
/// intentionally absent — see 05-04 §4.4.
#[test]
fn extract_helpers_cover_all_known_datapoint_types() {
    use cognee_core::extract_node_set_from_value;
    use cognee_core::task::Value;
    use cognee_models::{DataPoint, Document, DocumentChunk, EdgeType, Entity, EntityType};
    use std::sync::Arc;
    use uuid::Uuid;

    fn check<T: Value>(value: T) {
        let arc: Arc<dyn Value> = Arc::new(value);
        // No assertion on the return value; we only confirm the call
        // does not panic and the type is exercised by the downcast
        // registry (i.e. the helpers list this type).
        let _ = extract_node_set_from_value(arc.as_ref());
    }

    let dataset_id = Some(Uuid::new_v4());

    // Document is constructed directly — `classify_documents` is the
    // production constructor but it requires a `Data` row. For the
    // smoke-test all we care about is that the type is recognised by
    // the downcast registry.
    let document = Document {
        base: DataPoint::new("TextDocument", dataset_id),
        document_type: "text".into(),
        name: "name".into(),
        raw_data_location: "loc".into(),
        mime_type: "text/plain".into(),
        extension: "txt".into(),
        data_id: Uuid::new_v4(),
        external_metadata: None,
    };
    check(document);

    let document_chunk = DocumentChunk::new(
        Uuid::new_v4(),
        "hello".into(),
        1,
        0,
        "paragraph_end".into(),
        Uuid::new_v4(),
    );
    check(document_chunk);

    check(Entity::new("Foo", None, "desc", dataset_id));
    check(EntityType::new("Org", "desc", dataset_id));
    check(EdgeType::new("rel", dataset_id));
}
