"""Stub file for the ``cognee_py`` package.

Covers every public symbol re-exported from ``__init__.py``: the
``SearchType`` enum, all ``_native`` re-exports, and the ``Watcher`` class
defined in pure Python.

Type checkers (mypy, pyright) use this file together with
``_native.pyi`` to provide accurate signatures throughout the package.
"""

from __future__ import annotations

from enum import Enum
from typing import Any, Callable, Optional

# Re-export all types from _native so callers can do:
#   from cognee_py import Cognee, CogneeError, ...
from cognee_py._native import (
    # SDK handle
    Cognee as Cognee,
    # SDK config surface
    CogneeConfig as CogneeConfig,
    # SDK datasets sub-object
    CogneeDatasets as CogneeDatasets,
    # SDK sessions / notebooks sub-objects
    CogneeSessions as CogneeSessions,
    CogneeNotebooks as CogneeNotebooks,
    # SDK-tier exceptions
    CogneeError as CogneeError,
    CogneeComponentError as CogneeComponentError,
    CogneeServiceBuildError as CogneeServiceBuildError,
    CogneeUserBootstrapError as CogneeUserBootstrapError,
    CogneeRuntimeError as CogneeRuntimeError,
    CogneeValidationError as CogneeValidationError,
    CogneeUnsupportedError as CogneeUnsupportedError,
    CogneeFeatureNotBuiltError as CogneeFeatureNotBuiltError,
    CogneeUnknownConfigKeyError as CogneeUnknownConfigKeyError,
    CogneeConfigTypeMismatchError as CogneeConfigTypeMismatchError,
    # Pipeline engine
    Pipeline as Pipeline,
    PipelineRunHandle as PipelineRunHandle,
    TaskContext as TaskContext,
    CancellationHandle as CancellationHandle,
    CancellationToken as CancellationToken,
    cancellation_pair as cancellation_pair,
    ProgressToken as ProgressToken,
    # Pipeline exceptions
    PipelineError as PipelineError,
    TaskFailedError as TaskFailedError,
    CancelledError as CancelledError,
    NoTasksError as NoTasksError,
    InvalidConfigError as InvalidConfigError,
    # Setup functions
    setup_logging as setup_logging,
    setup_telemetry as setup_telemetry,
    setup_telemetry_analytics as setup_telemetry_analytics,
)
# Cloud ops (`serve` / `disconnect`) are exposed by the closed Python cdylib
# `cognee-py-cloud` (T15e), not by the OSS `cognee-py` package.

COGNEE_BINDING_SUPPRESS_LOGS: str

class SearchType(str, Enum):
    """Enumeration of the 15 supported search strategy types.

    Inherits from ``str`` so values compare equal to their string forms and
    can be passed wherever a plain string search-type is accepted.  Matches
    the ``SearchType`` enum in the upstream ``cognee`` Python SDK.

    The two upstream types ``AGENTIC_COMPLETION`` and
    ``GRAPH_COMPLETION_DECOMPOSITION`` are **not yet supported** by the Rust
    core.  Passing either raises :exc:`CogneeValidationError` at runtime.
    """

    GRAPH_COMPLETION: str
    GRAPH_COMPLETION_COT: str
    GRAPH_COMPLETION_CONTEXT_EXTENSION: str
    GRAPH_SUMMARY_COMPLETION: str
    TRIPLET_COMPLETION: str
    RAG_COMPLETION: str
    CHUNKS: str
    SUMMARIES: str
    TEMPORAL: str
    CYPHER: str
    NATURAL_LANGUAGE: str
    FEELING_LUCKY: str
    FEEDBACK: str
    CODING_RULES: str
    CHUNKS_LEXICAL: str

    def __str__(self) -> str: ...
    def __format__(self, format_spec: str) -> str: ...

class Watcher:
    """A pipeline watcher that forwards events to Python callbacks.

    Pass keyword arguments corresponding to the event names you want to
    handle.  Any event without a registered callback is silently ignored::

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

    def __init__(
        self,
        *,
        on_pipeline: Optional[Callable[[str, str], None]] = None,
        on_task: Optional[Callable[[str, int, str, int, str], None]] = None,
        on_run_started: Optional[Callable[[str, str], None]] = None,
        on_run_completed: Optional[Callable[[str, int], None]] = None,
        on_run_errored: Optional[Callable[[str, str], None]] = None,
        on_task_started: Optional[Callable[[str, str, int], None]] = None,
        on_task_completed: Optional[Callable[[str, str, int], None]] = None,
        on_task_errored: Optional[Callable[[str, str, str], None]] = None,
        **extra_callbacks: Callable[..., None],
    ) -> None: ...

    @classmethod
    def noop(cls) -> Watcher:
        """Create a watcher that silently ignores all events."""
        ...

    def on_pipeline(self, pipeline_id: str, status: str) -> None: ...
    def on_task(
        self,
        pipeline_id: str,
        task_index: int,
        name: str,
        total: int,
        status: str,
    ) -> None: ...
    def on_run_started(self, run_id: str, pipeline_name: str) -> None: ...
    def on_run_completed(self, run_id: str, output_count: int) -> None: ...
    def on_run_errored(self, run_id: str, error: str) -> None: ...
    def on_task_started(
        self, run_id: str, task_name: str, task_index: int
    ) -> None: ...
    def on_task_completed(
        self, run_id: str, task_name: str, output_count: int
    ) -> None: ...
    def on_task_errored(
        self, run_id: str, task_name: str, error: str
    ) -> None: ...
