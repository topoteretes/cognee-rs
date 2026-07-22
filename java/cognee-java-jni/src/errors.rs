//! JNI error helpers: map an `SdkError`/code+message onto a thrown
//! `ai.cognee.CogneeException(String code, String message)`.

use jni::JNIEnv;
use jni::objects::{JString, JThrowable};

use cognee_bindings_common::SdkError;

/// Throw `ai.cognee.CogneeException(code, message)` on the current thread.
///
/// Best-effort: if constructing/throwing the typed exception itself fails, fall
/// back to a plain `RuntimeException` so an error is always surfaced.
pub(crate) fn throw_cognee_exception(env: &mut JNIEnv, code: &str, message: &str) {
    let built = (|| -> jni::errors::Result<()> {
        let code_j: JString = env.new_string(code)?;
        let msg_j: JString = env.new_string(message)?;
        let exc = env.new_object(
            "ai/cognee/CogneeException",
            "(Ljava/lang/String;Ljava/lang/String;)V",
            &[(&code_j).into(), (&msg_j).into()],
        )?;
        env.throw(JThrowable::from(exc))
    })();
    if built.is_err() {
        let _ = env.throw_new("java/lang/RuntimeException", message);
    }
}

/// Throw a `CogneeException` from an `SdkError` (uses its stable `code()`).
pub(crate) fn throw_sdk_error(env: &mut JNIEnv, err: SdkError) {
    throw_cognee_exception(env, err.code(), &err.to_string());
}
