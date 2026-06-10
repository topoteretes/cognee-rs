//! Control-flow sentinel values that pipeline tasks return to steer the
//! executor. Sentinels are ordinary [`Value`]s (via the blanket
//! `impl<T> Value for T` in `task.rs`), so no manual trait impl is needed.

use crate::task::Value;

/// Returned by a task to discard the current item: it is not forwarded to
/// downstream tasks and does not appear in the pipeline output.
///
/// Mirrors Python's `_Drop` sentinel (`cognee/pipelines/types.py`).
///
/// # Usage
///
/// Return `Ok(Box::new(DroppedSentinel))` from any task to silently discard
/// the current item without raising an error. The executor filters it out
/// before forwarding to downstream tasks or including it in the final output.
///
/// # Batch tasks
///
/// A batch task that wants to drop *individual* items of its slice should
/// emit an iterator or stream that omits them, or yield `DroppedSentinel`
/// per item — the executor's iterator/stream path filters those out too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DroppedSentinel;

/// Returns `true` if `value` is a [`DroppedSentinel`].
///
/// `value` must be the **dereferenced** trait object (`&dyn Value`), **not**
/// an `Arc<dyn Value>` or `Box<dyn Value>` directly: the blanket
/// `impl<T: Any + Send + Sync + 'static> Value for T` means `.as_any()` on a
/// smart pointer downcasts to the pointer type, never the inner value. Pass
/// `arc.as_ref()` / `boxed.as_ref()`.
pub fn is_dropped(value: &dyn Value) -> bool {
    value.as_any().downcast_ref::<DroppedSentinel>().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn detects_dropped_sentinel() {
        let v: Arc<dyn Value> = Arc::new(DroppedSentinel);
        assert!(is_dropped(v.as_ref()));
    }

    #[test]
    fn ignores_regular_value() {
        let v: Arc<dyn Value> = Arc::new(42_usize);
        assert!(!is_dropped(v.as_ref()));
    }

    #[test]
    fn ignores_boxed_regular_value() {
        let v: Box<dyn Value> = Box::new(99_i32);
        assert!(!is_dropped(v.as_ref()));
    }

    #[test]
    fn detects_boxed_dropped_sentinel() {
        let v: Box<dyn Value> = Box::new(DroppedSentinel);
        assert!(is_dropped(v.as_ref()));
    }
}
