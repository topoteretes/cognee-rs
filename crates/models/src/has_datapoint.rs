//! The `HasDataPoint` trait used by the provenance-stamping algorithm
//! in `cognee-core::provenance`.
//!
//! This trait lives in `cognee-models` (next to its primary impls) and
//! is re-exported from `cognee_core::provenance` so the public path
//! `cognee_core::provenance::HasDataPoint` keeps resolving for
//! downstream consumers. Placement decision recorded in
//! `docs/telemetry/05/04-has-datapoint-impls.md` §4.1.

use crate::DataPoint;

/// Read / write access to the embedded `DataPoint` of a typed container,
/// plus a hook to recurse into nested `DataPoint`-bearing children.
///
/// This trait is the Rust analogue of Python's reflective
/// `model_fields` walk. Implementations are added crate-by-crate
/// (typically here in `cognee-models`); types not implementing the
/// trait are silently passed through by `cognee_core::provenance::stamp_tree`.
pub trait HasDataPoint {
    /// Borrow the embedded `DataPoint` of this container.
    fn data_point(&self) -> &DataPoint;

    /// Mutably borrow the embedded `DataPoint` of this container.
    fn data_point_mut(&mut self) -> &mut DataPoint;

    /// Visit every owned child that itself implements `HasDataPoint`.
    /// Default: no children. Override on container types whose fields
    /// own (rather than reference by `Uuid`) another `HasDataPoint`.
    fn for_each_child_mut(&mut self, _visit: &mut dyn FnMut(&mut dyn HasDataPoint)) {}
}
