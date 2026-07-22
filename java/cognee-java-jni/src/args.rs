//! Shared JNI argument helpers used by every op wrapper.

use jni::JNIEnv;
use jni::objects::JString;

use cognee_bindings_common::SdkError;

/// Read a required string argument.
pub(crate) fn arg_string(env: &mut JNIEnv, s: &JString) -> Result<String, SdkError> {
    if s.is_null() {
        return Err(SdkError::Validation(
            "required string argument was null".into(),
        ));
    }
    env.get_string(s)
        .map(|v| v.into())
        .map_err(|_| SdkError::Validation("invalid string argument".into()))
}

/// Read an optional JSON-string argument into a `Value`; null/empty/"null" → `Null`.
pub(crate) fn arg_json(env: &mut JNIEnv, s: &JString) -> Result<serde_json::Value, SdkError> {
    if s.is_null() {
        return Ok(serde_json::Value::Null);
    }
    let raw: String = env
        .get_string(s)
        .map(|v| v.into())
        .map_err(|_| SdkError::Validation("invalid JSON string argument".into()))?;
    let t = raw.trim();
    if t.is_empty() || t == "null" {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(t).map_err(|e| SdkError::Validation(format!("invalid JSON argument: {e}")))
}
