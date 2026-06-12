"""Python bindings for the cognee-core pipeline engine."""

COGNEE_BINDING_SUPPRESS_LOGS = "COGNEE_BINDING_SUPPRESS_LOGS"
"""Env-var name that suppresses the auto-installed tracing bridge.

Set this to any non-empty value *before* importing ``cognee_pipeline``
if the host application already owns its ``logging``/``tracing``
configuration. When unset, importing ``cognee_pipeline`` installs a
minimal ``tracing_subscriber::Registry`` that forwards every Rust
``tracing::*`` event into Python's standard ``logging`` module via
``pyo3-log`` (gap 07 decisions 1 and 5)."""

from cognee_pipeline._native import (
    # SDK handle
    Cognee,
    # SDK-tier exceptions (CogneeError hierarchy)
    CogneeError,
    CogneeComponentError,
    CogneeServiceBuildError,
    CogneeUserBootstrapError,
    CogneeRuntimeError,
    CogneeValidationError,
    CogneeUnsupportedError,
    CogneeFeatureNotBuiltError,
    CogneeUnknownConfigKeyError,
    CogneeConfigTypeMismatchError,
    # Pipeline engine
    CancellationHandle,
    CancelledError,
    InvalidConfigError,
    NoTasksError,
    Pipeline,
    PipelineError,
    PipelineRunHandle,
    ProgressToken,
    TaskContext,
    TaskFailedError,
    setup_logging,
    setup_telemetry,
    setup_telemetry_analytics,
)

__all__ = [
    # SDK handle
    "Cognee",
    # SDK-tier exceptions
    "CogneeError",
    "CogneeComponentError",
    "CogneeServiceBuildError",
    "CogneeUserBootstrapError",
    "CogneeRuntimeError",
    "CogneeValidationError",
    "CogneeUnsupportedError",
    "CogneeFeatureNotBuiltError",
    "CogneeUnknownConfigKeyError",
    "CogneeConfigTypeMismatchError",
    # Pipeline engine
    "Pipeline",
    "TaskContext",
    "CancellationHandle",
    "ProgressToken",
    "PipelineRunHandle",
    "PipelineError",
    "TaskFailedError",
    "CancelledError",
    "NoTasksError",
    "InvalidConfigError",
    "setup_logging",
    "setup_telemetry",
    "setup_telemetry_analytics",
]
