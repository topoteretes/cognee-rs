use std::ffi::c_void;
use std::sync::Arc;

use cognee_core::Value;

use crate::value::CgValue;

/// C-side iterator vtable.
#[repr(C)]
pub struct CgValueIterVtable {
    /// Return the next value, or null when exhausted.
    pub next: unsafe extern "C" fn(state: *mut c_void) -> *mut CgValue,
    /// Destroy the iterator state.
    pub destroy: unsafe extern "C" fn(state: *mut c_void),
}

/// Opaque handle for a C-backed value iterator.
pub struct CgValueIter {
    state: *mut c_void,
    vtable: CgValueIterVtable,
    exhausted: bool,
}

// C caller guarantees thread-safety of state.
unsafe impl Send for CgValueIter {}

impl Iterator for CgValueIter {
    type Item = Box<dyn Value>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.exhausted {
            return None;
        }
        let ptr = unsafe { (self.vtable.next)(self.state) };
        if ptr.is_null() {
            self.exhausted = true;
            return None;
        }
        let cg_val = unsafe { Box::from_raw(ptr) };
        // The CgValue.inner is Arc<dyn Value> where the concrete type is e.g. i64.
        // We wrap it in ArcValueHolder so the pipeline can carry it forward.
        // When the result is accessed via cg_value_as_*, we need to look through
        // the ArcValueHolder wrapper — this is handled in value.rs accessors.
        Some(Box::new(ArcValueHolder(cg_val.inner)) as Box<dyn Value>)
    }
}

impl Drop for CgValueIter {
    fn drop(&mut self) {
        unsafe { (self.vtable.destroy)(self.state) };
    }
}

/// Wrapper to hold an `Arc<dyn Value>` as a `Value` itself.
/// The inner value is accessible via `as_any().downcast_ref::<ArcValueHolder>()`.
pub(crate) struct ArcValueHolder(pub(crate) Arc<dyn Value>);

/// Create a new iterator from a C vtable and state pointer.
///
/// # Safety
/// - `state` must be valid until `vtable.destroy` is called.
/// - `vtable.next` and `vtable.destroy` must be valid function pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_value_iter_new(
    state: *mut c_void,
    vtable: CgValueIterVtable,
) -> *mut CgValueIter {
    Box::into_raw(Box::new(CgValueIter {
        state,
        vtable,
        exhausted: false,
    }))
}

/// # Safety
/// `iter` must have been created by this library, or be null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cg_value_iter_destroy(iter: *mut CgValueIter) {
    if !iter.is_null() {
        unsafe { drop(Box::from_raw(iter)) };
    }
}

/// Convert a `CgValueIter` into a Rust `ValueIter` (consuming the pointer).
///
/// # Safety
/// `iter` must be a valid pointer created by `cg_value_iter_new`.
pub(crate) unsafe fn into_value_iter(iter: *mut CgValueIter) -> cognee_core::ValueIter {
    let iter = unsafe { Box::from_raw(iter) };
    Box::new(*iter)
}
