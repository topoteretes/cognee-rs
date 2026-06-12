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
    CancellationToken,
    cancellation_pair,
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
    # Cloud ops (process-wide singleton)
    serve,
    disconnect,
)

class Watcher:
    """A pipeline watcher that forwards events to Python callbacks.

    Pass keyword arguments corresponding to the event names you want to
    handle.  Any event without a registered callback is silently ignored.

    .. code-block:: python

        watcher = Watcher(
            on_task_started=lambda run_id, name, idx: print(f"Task {name} started"),
            on_run_completed=lambda run_id, count: print(f"Done: {count} outputs"),
        )
        result = await pipeline.execute(inputs, ctx, watcher=watcher)

    Available event callbacks and their signatures:

    - ``on_pipeline(pipeline_id: str, status: str)``
    - ``on_task(pipeline_id: str, task_index: int, name: str, total: int, status: str)``
    - ``on_run_started(run_id: str, pipeline_name: str)``
    - ``on_run_completed(run_id: str, output_count: int)``
    - ``on_run_errored(run_id: str, error: str)``
    - ``on_task_started(run_id: str, task_name: str, task_index: int)``
    - ``on_task_completed(run_id: str, task_name: str, output_count: int)``
    - ``on_task_errored(run_id: str, task_name: str, error: str)``
    """

    def __init__(self, **callbacks):
        self._callbacks = callbacks

    @classmethod
    def noop(cls) -> "Watcher":
        """Create a watcher that silently ignores all events."""
        return cls()

    # NOTE: method names must match what PyWatcherBridge calls via
    # hasattr/call_method1 in python/src/watcher.rs.

    def on_pipeline(self, pipeline_id: str, status: str) -> None:
        if cb := self._callbacks.get("on_pipeline"):
            cb(pipeline_id, status)

    def on_task(
        self, pipeline_id: str, task_index: int, name: str, total: int, status: str
    ) -> None:
        if cb := self._callbacks.get("on_task"):
            cb(pipeline_id, task_index, name, total, status)

    def on_run_started(self, run_id: str, pipeline_name: str) -> None:
        if cb := self._callbacks.get("on_run_started"):
            cb(run_id, pipeline_name)

    def on_run_completed(self, run_id: str, output_count: int) -> None:
        if cb := self._callbacks.get("on_run_completed"):
            cb(run_id, output_count)

    def on_run_errored(self, run_id: str, error: str) -> None:
        if cb := self._callbacks.get("on_run_errored"):
            cb(run_id, error)

    def on_task_started(self, run_id: str, task_name: str, task_index: int) -> None:
        if cb := self._callbacks.get("on_task_started"):
            cb(run_id, task_name, task_index)

    def on_task_completed(self, run_id: str, task_name: str, output_count: int) -> None:
        if cb := self._callbacks.get("on_task_completed"):
            cb(run_id, task_name, output_count)

    def on_task_errored(self, run_id: str, task_name: str, error: str) -> None:
        if cb := self._callbacks.get("on_task_errored"):
            cb(run_id, task_name, error)


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
    "CancellationToken",
    "cancellation_pair",
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
    # Cloud ops
    "serve",
    "disconnect",
    # Watcher factory
    "Watcher",
]
