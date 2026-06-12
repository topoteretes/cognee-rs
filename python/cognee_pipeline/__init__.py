"""Python bindings for the cognee-core pipeline engine."""

COGNEE_BINDING_SUPPRESS_LOGS = "COGNEE_BINDING_SUPPRESS_LOGS"
"""Env-var name that suppresses the auto-installed tracing bridge.

Set this to any non-empty value *before* importing ``cognee_pipeline``
if the host application already owns its ``logging``/``tracing``
configuration. When unset, importing ``cognee_pipeline`` installs a
minimal ``tracing_subscriber::Registry`` that forwards every Rust
``tracing::*`` event into Python's standard ``logging`` module via
``pyo3-log`` (gap 07 decisions 1 and 5)."""


class SearchType:
    """Constants for the 15 supported search strategy types.

    Pass these as the ``search_type`` option to :meth:`Cognee.search` or
    :meth:`Cognee.recall`:

    .. code-block:: python

        result = await cognee.search("query", {"search_type": SearchType.CHUNKS})
    """

    GRAPH_COMPLETION = "GRAPH_COMPLETION"
    GRAPH_COMPLETION_COT = "GRAPH_COMPLETION_COT"
    GRAPH_COMPLETION_CONTEXT_EXTENSION = "GRAPH_COMPLETION_CONTEXT_EXTENSION"
    GRAPH_SUMMARY_COMPLETION = "GRAPH_SUMMARY_COMPLETION"
    TRIPLET_COMPLETION = "TRIPLET_COMPLETION"
    RAG_COMPLETION = "RAG_COMPLETION"
    CHUNKS = "CHUNKS"
    SUMMARIES = "SUMMARIES"
    TEMPORAL = "TEMPORAL"
    CYPHER = "CYPHER"
    NATURAL_LANGUAGE = "NATURAL_LANGUAGE"
    FEELING_LUCKY = "FEELING_LUCKY"
    FEEDBACK = "FEEDBACK"
    CODING_RULES = "CODING_RULES"
    CHUNKS_LEXICAL = "CHUNKS_LEXICAL"


from cognee_pipeline._native import (
    # SDK handle
    Cognee,
    # SDK config surface
    CogneeConfig,
    # SDK datasets sub-object
    CogneeDatasets,
    # SDK sessions / notebooks sub-objects
    CogneeSessions,
    CogneeNotebooks,
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
    # Search type constants
    "SearchType",
    # SDK handle
    "Cognee",
    # SDK config surface
    "CogneeConfig",
    # SDK datasets sub-object
    "CogneeDatasets",
    # SDK sessions / notebooks sub-objects
    "CogneeSessions",
    "CogneeNotebooks",
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
