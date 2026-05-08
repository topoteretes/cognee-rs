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
//!
//! See `docs/telemetry/05-datapoint-provenance.md` for the locked
//! design decisions backing this module — in particular:
//!
//! - Decision 1: walk via the [`HasDataPoint`] trait, not serde-JSON
//!   reflection.
//! - Decision 2: visited-set keyed on `DataPoint.id: Uuid`, not
//!   pointer identity.
//! - Decision 3: the name collision with
//!   `ExecStatusManager::stamp_provenance` is intentional and locked
//!   — neither symbol is renamed.

use std::collections::HashSet;

use cognee_models::DataPoint;
use uuid::Uuid;

use crate::task::Value;

/// Read / write access to the embedded `DataPoint` of a typed container,
/// plus a hook to recurse into nested `DataPoint`-bearing children.
///
/// This trait is the Rust analogue of Python's reflective
/// `model_fields` walk. Implementations are added crate-by-crate
/// (typically in `cognee-models`); types not implementing the trait
/// are silently passed through by [`stamp_tree`].
pub trait HasDataPoint {
    /// Borrow the embedded `DataPoint` of this container.
    fn data_point(&self) -> &DataPoint;

    /// Mutably borrow the embedded `DataPoint` of this container.
    fn data_point_mut(&mut self) -> &mut DataPoint;

    /// Visit every owned child that itself implements `HasDataPoint`.
    /// Default: no children. Override on container types like
    /// `Entity` (whose `entity_type: Box<EntityType>` is itself a
    /// `HasDataPoint`).
    fn for_each_child_mut(&mut self, _visit: &mut dyn FnMut(&mut dyn HasDataPoint)) {}
}

/// What we know at the call site of [`stamp_tree`].
///
/// All fields are borrows so the executor can build a context per task
/// without cloning strings on the hot path.
#[derive(Clone, Copy)]
pub struct ProvenanceContext<'a> {
    /// Pipeline name, e.g. `"cognify_pipeline"`.
    pub pipeline_name: &'a str,
    /// Task name, e.g. `"extract_graph_from_data"`.
    pub task_name: &'a str,
    /// Resolved user label (`email` or `id` fallback). `None` if the
    /// pipeline has no user attached.
    pub user_label: Option<&'a str>,
    /// Default `node_set` inherited by stamped DPs whose own
    /// `source_node_set` is `None`.
    pub node_set: Option<&'a str>,
    /// Default `content_hash` inherited by stamped DPs whose own
    /// `source_content_hash` is `None`.
    pub content_hash: Option<&'a str>,
}

/// Stamp a tree of [`HasDataPoint`] values in place.
///
/// Mirrors Python's `_stamp_provenance` in
/// `cognee/modules/pipelines/operations/run_tasks_base.py`:
///
/// - **Idempotent**: every assignment is guarded by
///   `if dp.source_X.is_none()`, so a downstream task never overwrites
///   an upstream stamp.
/// - **Visited-set**: keyed on `DataPoint.id: Uuid` (locked decision
///   2). Re-entering the same DataPoint (by UUID) is a no-op.
/// - **Inheritance**: `node_set` and `content_hash` inherit from the
///   parent context if absent on the DP, but a value already present
///   on the DP overrides for further recursion.
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
        if dp.source_user.is_none()
            && let Some(u) = ctx.user_label
        {
            dp.source_user = Some(u.to_string());
        }
    }

    // Compute the inherited values once before recursing. A DP that
    // already carries node_set / content_hash exposes its own value to
    // children; otherwise the parent context's value flows down. The
    // temporary String allocations are only consumed when there are
    // children to recurse into; for leaf DPs `for_each_child_mut`'s
    // default impl is a no-op and the strings are dropped immediately.
    let current_node_set = match root.data_point().source_node_set.as_deref() {
        Some(v) => Some(v.to_string()),
        None => ctx.node_set.map(|s| s.to_string()),
    };
    if root.data_point().source_node_set.is_none()
        && let Some(v) = ctx.node_set
    {
        root.data_point_mut().source_node_set = Some(v.to_string());
    }

    let current_hash = match root.data_point().source_content_hash.as_deref() {
        Some(v) => Some(v.to_string()),
        None => ctx.content_hash.map(|s| s.to_string()),
    };
    if root.data_point().source_content_hash.is_none()
        && let Some(v) = ctx.content_hash
    {
        root.data_point_mut().source_content_hash = Some(v.to_string());
    }

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

/// Walk a type-erased [`Value`] looking for the first non-empty
/// `source_node_set` on an embedded `DataPoint`. Mirrors Python's
/// `_extract_node_set`.
///
/// The downcast list below is the canonical set of `HasDataPoint`
/// container types that the executor recognises. Types not in the
/// list are passed through (return `None`); add them here in lockstep
/// with new `HasDataPoint` impls in `cognee-models` (gap 05-04).
pub fn extract_node_set_from_value(value: &dyn Value) -> Option<String> {
    downcast_to_datapoint(value).and_then(|dp| dp.source_node_set.clone())
}

/// Walk a type-erased [`Value`] looking for the first non-empty
/// `Data.content_hash` (raw ingestion artefact) **or**
/// `DataPoint.source_content_hash`. Mirrors Python's
/// `_extract_content_hash`.
///
/// The raw `Data` artefact takes priority over a stamped DataPoint —
/// it is the lineage anchor closest to the ingestion source.
pub fn extract_content_hash_from_value(value: &dyn Value) -> Option<String> {
    use cognee_models::Data;

    if let Some(d) = value.as_any().downcast_ref::<Data>()
        && !d.content_hash.is_empty()
    {
        return Some(d.content_hash.clone());
    }

    downcast_to_datapoint(value).and_then(|dp| dp.source_content_hash.clone())
}

/// Internal helper: `value` → optional borrow of its embedded
/// `DataPoint`. Keeps a single registered list of
/// `HasDataPoint`-bearing container types, used by both
/// [`extract_node_set_from_value`] and
/// [`extract_content_hash_from_value`].
fn downcast_to_datapoint(value: &dyn Value) -> Option<&DataPoint> {
    use cognee_models::{Document, DocumentChunk, EdgeType, Entity, EntityType};

    if let Some(d) = value.as_any().downcast_ref::<Document>() {
        return Some(&d.base);
    }
    if let Some(d) = value.as_any().downcast_ref::<DocumentChunk>() {
        return Some(&d.base);
    }
    if let Some(d) = value.as_any().downcast_ref::<Entity>() {
        return Some(&d.base);
    }
    if let Some(d) = value.as_any().downcast_ref::<EntityType>() {
        return Some(&d.base);
    }
    if let Some(d) = value.as_any().downcast_ref::<EdgeType>() {
        return Some(&d.base);
    }
    // `cognee_models::Triplet` is intentionally absent: it is a flat
    // struct without an embedded `DataPoint`. `cognify::TextSummary`
    // and any other future container types should be added here in
    // lockstep with their `HasDataPoint` impls (gap 05-04).
    None
}
