//! SDK-tier error types for the Python binding.
//!
//! Converts [`SdkError`] (from `cognee-bindings-common`) and
//! [`ConfigError`] (from `cognee`) into the appropriate Python exceptions.
//!
//! All SDK exceptions extend [`CogneeError`] so callers can catch broadly.
//! This hierarchy is separate from the engine-tier [`PipelineError`] hierarchy
//! (same split as in the C API: codes 0–10 engine, 11–18 SDK).

use cognee::config::ConfigError;
use cognee_bindings_common::SdkError;
use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

// ── Base exception ────────────────────────────────────────────────────────────

create_exception!(cognee_py, CogneeError, PyException);

// ── SdkError variants ─────────────────────────────────────────────────────────

create_exception!(cognee_py, CogneeComponentError, CogneeError);
create_exception!(cognee_py, CogneeServiceBuildError, CogneeError);
create_exception!(cognee_py, CogneeUserBootstrapError, CogneeError);
create_exception!(cognee_py, CogneeRuntimeError, CogneeError);
create_exception!(cognee_py, CogneeImproveAbortedError, CogneeRuntimeError);
create_exception!(cognee_py, CogneeValidationError, CogneeError);
create_exception!(cognee_py, CogneeUnsupportedError, CogneeError);
create_exception!(cognee_py, CogneeFeatureNotBuiltError, CogneeError);

// ── ConfigError variants ──────────────────────────────────────────────────────

create_exception!(cognee_py, CogneeUnknownConfigKeyError, CogneeError);
create_exception!(cognee_py, CogneeConfigTypeMismatchError, CogneeError);

// ── Conversion helpers ────────────────────────────────────────────────────────

/// Convert an [`SdkError`] to a Python exception.
pub fn sdk_error_to_py(e: SdkError) -> PyErr {
    let msg = e.to_string();
    match e {
        SdkError::Component(_) => CogneeComponentError::new_err(msg),
        SdkError::ServiceBuild(_) => CogneeServiceBuildError::new_err(msg),
        SdkError::UserBootstrap(_) => CogneeUserBootstrapError::new_err(msg),
        SdkError::Runtime(_) => CogneeRuntimeError::new_err(msg),
        SdkError::Validation(_) => CogneeValidationError::new_err(msg),
        SdkError::Unsupported(_) => CogneeUnsupportedError::new_err(msg),
        SdkError::FeatureNotBuilt(_) => CogneeFeatureNotBuiltError::new_err(msg),
        SdkError::ImproveAborted { improve_result, .. } => {
            improve_aborted_to_py(msg, &improve_result)
        }
    }
}

/// A [`CogneeImproveAbortedError`] whose `improve_result` attribute holds the
/// partial result, as Python cognee's raised error does. Without the
/// interpreter (finalizing) the attribute is left off; the message stands.
fn improve_aborted_to_py(msg: String, improve_result: &serde_json::Value) -> PyErr {
    let err = CogneeImproveAbortedError::new_err(msg);
    crate::gil::attach_best_effort("improve_result attribute", |py| {
        let attached = crate::json::serde_to_py(py, improve_result)
            .and_then(|value| err.value(py).setattr("improve_result", value));
        if let Err(e) = attached {
            tracing::debug!("could not attach improve_result to the error: {e}");
        }
    });
    err
}

/// Wrap a plain validation message string as a [`CogneeValidationError`].
///
/// Used by the settings-patch path when `apply_settings_json_patch` returns a
/// `String` error.
pub fn validation_err(msg: String) -> PyErr {
    CogneeValidationError::new_err(msg)
}

/// Convert a [`ConfigError`] to a Python exception.
pub fn config_error_to_py(e: ConfigError) -> PyErr {
    let msg = e.to_string();
    match e {
        ConfigError::UnknownKey(_) => CogneeUnknownConfigKeyError::new_err(msg),
        ConfigError::TypeMismatch { .. } => CogneeConfigTypeMismatchError::new_err(msg),
        ConfigError::Unsupported { .. } => CogneeUnsupportedError::new_err(msg),
    }
}

/// Register all SDK exception classes on the module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("CogneeError", m.py().get_type::<CogneeError>())?;
    m.add(
        "CogneeComponentError",
        m.py().get_type::<CogneeComponentError>(),
    )?;
    m.add(
        "CogneeServiceBuildError",
        m.py().get_type::<CogneeServiceBuildError>(),
    )?;
    m.add(
        "CogneeUserBootstrapError",
        m.py().get_type::<CogneeUserBootstrapError>(),
    )?;
    m.add(
        "CogneeRuntimeError",
        m.py().get_type::<CogneeRuntimeError>(),
    )?;
    m.add(
        "CogneeImproveAbortedError",
        m.py().get_type::<CogneeImproveAbortedError>(),
    )?;
    m.add(
        "CogneeValidationError",
        m.py().get_type::<CogneeValidationError>(),
    )?;
    m.add(
        "CogneeUnsupportedError",
        m.py().get_type::<CogneeUnsupportedError>(),
    )?;
    m.add(
        "CogneeFeatureNotBuiltError",
        m.py().get_type::<CogneeFeatureNotBuiltError>(),
    )?;
    m.add(
        "CogneeUnknownConfigKeyError",
        m.py().get_type::<CogneeUnknownConfigKeyError>(),
    )?;
    m.add(
        "CogneeConfigTypeMismatchError",
        m.py().get_type::<CogneeConfigTypeMismatchError>(),
    )?;
    Ok(())
}
