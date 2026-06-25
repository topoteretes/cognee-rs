//! Neon-specific SDK error helpers.
//!
//! The portable [`SdkError`] enum now lives in `cognee-bindings-common` and is
//! re-exported here for backwards compatibility with existing call-sites in this
//! crate. Only the neon-specific helper (`throw_sdk_error`) stays here because
//! it requires `neon::prelude::*`.

use neon::prelude::*;

// Re-export so existing `use crate::errors::SdkError` call-sites keep working.
pub use cognee_bindings_common::SdkError;

/// Throw a JS `Error` carrying the message, a `code` property, and a `kind`
/// property from an [`SdkError`].
///
/// Both `code` and `kind` carry the same string value. `kind` is the stable
/// API identifier; `code` is kept as a backwards-compatible alias so existing
/// call-sites that check `e.code` continue to work.
pub fn throw_sdk_error<'cx, T>(cx: &mut impl Context<'cx>, err: SdkError) -> NeonResult<T> {
    let code = err.code();
    let msg = err.to_string();
    let js_err = cx.error(msg)?;
    let obj = js_err.downcast_or_throw::<JsObject, _>(cx)?;
    let code_val = cx.string(code);
    let kind_val = cx.string(code);
    obj.set(cx, "code", code_val)?;
    obj.set(cx, "kind", kind_val)?;
    cx.throw(js_err)
}
