"""Stub file for the ``cognee_pipeline._native`` PyO3 extension module.

This file documents the public Python API exposed by the compiled Rust
extension.  Type checkers (mypy, pyright) use it to provide accurate
signatures for all classes and functions in ``_native``.

The stubs mirror the ``#[pyclass]`` / ``#[pymethods]`` / ``#[pyfunction]``
definitions in ``python/src/``.  Keep this file in sync with the Rust source
when adding new methods.
"""

from __future__ import annotations

from typing import Any, Optional, Sequence, Union

# ---------------------------------------------------------------------------
# Exception hierarchy
# ---------------------------------------------------------------------------

class PipelineError(Exception):
    """Base exception for pipeline-engine errors."""
    ...

class TaskFailedError(PipelineError):
    """A task failed (possibly after retries)."""
    ...

class CancelledError(PipelineError):
    """The pipeline was cancelled."""
    ...

class NoTasksError(PipelineError):
    """The pipeline has no tasks."""
    ...

class InvalidConfigError(PipelineError):
    """The pipeline configuration is invalid."""
    ...

class CogneeError(Exception):
    """Base exception for all SDK-tier errors."""
    ...

class CogneeComponentError(CogneeError):
    """A required component could not be initialised."""
    ...

class CogneeServiceBuildError(CogneeError):
    """Service construction failed."""
    ...

class CogneeUserBootstrapError(CogneeError):
    """Default user creation or lookup failed."""
    ...

class CogneeRuntimeError(CogneeError):
    """A runtime error occurred in the Rust core."""
    ...

class CogneeValidationError(CogneeError):
    """Input validation failed (bad arguments, wrong types, unknown keys)."""
    ...

class CogneeUnsupportedError(CogneeError):
    """The requested operation is not supported."""
    ...

class CogneeFeatureNotBuiltError(CogneeError):
    """The requested feature was not compiled in."""
    ...

class CogneeUnknownConfigKeyError(CogneeError):
    """An unknown configuration key was supplied."""
    ...

class CogneeConfigTypeMismatchError(CogneeError):
    """A configuration value has the wrong type."""
    ...

# ---------------------------------------------------------------------------
# CancellationHandle / CancellationToken
# ---------------------------------------------------------------------------

class CancellationHandle:
    """The authority-to-cancel side of a cancellation pair."""

    def cancel(self) -> None:
        """Signal cancellation to all observers."""
        ...

    @property
    def is_cancelled(self) -> bool:
        """``True`` once :meth:`cancel` has been called."""
        ...

class CancellationToken:
    """The observe-only side of a cancellation pair.

    Obtained from :func:`cancellation_pair`.  Can be cloned and shared with
    tasks that need to *observe* cancellation without holding the authority
    to trigger it.
    """

    @property
    def is_cancelled(self) -> bool:
        """``True`` once the paired :class:`CancellationHandle` has been cancelled."""
        ...

    def clone_token(self) -> CancellationToken:
        """Return a copy that shares the same cancellation state."""
        ...

def cancellation_pair() -> tuple[CancellationHandle, CancellationToken]:
    """Create a linked ``(handle, token)`` pair.

    The handle is given to the owner of a task; the token is passed into the
    task itself.  Call ``handle.cancel()`` to signal cancellation; observe it
    via ``token.is_cancelled``.

    Returns a 2-tuple ``(handle, token)``.
    """
    ...

# ---------------------------------------------------------------------------
# ProgressToken
# ---------------------------------------------------------------------------

class ProgressToken:
    """A hierarchical progress tracker.

    Supports splitting into subtokens for weighted sub-operations; each
    subtoken contributes its fraction back to the root.
    """

    def __init__(self) -> None: ...

    def set(self, fraction: float) -> None:
        """Set progress to *fraction*, clamped to ``[0.0, 1.0]``."""
        ...

    @property
    def fraction(self) -> float:
        """This token's own progress in ``[0.0, 1.0]``."""
        ...

    @property
    def root_fraction(self) -> float:
        """Aggregated progress across the entire token tree."""
        ...

    @property
    def is_complete(self) -> bool:
        """``True`` when :attr:`fraction` >= ``1.0``."""
        ...

    @property
    def width(self) -> float:
        """This token's weight as a fraction of the root range."""
        ...

    def subtoken(self, frac_width: float) -> ProgressToken:
        """Create a child token occupying *frac_width* of this token's range.

        *frac_width* must be in ``[0.0, 1.0]``.  Raises :exc:`ValueError`
        for out-of-range values.
        """
        ...

    def split(self, weights: Sequence[int]) -> list[ProgressToken]:
        """Split into subtokens by relative integer *weights*.

        Returns one :class:`ProgressToken` per weight.
        """
        ...

# ---------------------------------------------------------------------------
# TaskContext
# ---------------------------------------------------------------------------

class TaskContext:
    """Execution context threaded through a pipeline run.

    Holds references to the thread pool, databases, and the per-run
    :class:`CancellationHandle` and :class:`ProgressToken`.
    """

    @staticmethod
    def mock() -> TaskContext:
        """Create an in-memory mock context suitable for pure-Python pipelines."""
        ...

    @property
    def cancellation_handle(self) -> CancellationHandle:
        """The cancellation handle for this run."""
        ...

    @property
    def progress(self) -> ProgressToken:
        """The root progress token for this run."""
        ...

# ---------------------------------------------------------------------------
# Pipeline / PipelineRunHandle
# ---------------------------------------------------------------------------

class Pipeline:
    """An ordered sequence of tasks that processes a stream of values."""

    def __init__(self, description: str) -> None: ...

    def with_name(self, name: str) -> Pipeline:
        """Set the pipeline name (fluent)."""
        ...

    def add_task(
        self,
        callable: Any,
        *,
        name: Optional[str] = None,
        batch: bool = False,
        batch_size: Optional[int] = None,
        weight: int = 1,
    ) -> None:
        """Append a task.

        The callable type is auto-detected: sync, async, generator, or async
        generator.  Pass ``batch=True`` for batch-mode callables that receive
        a ``list`` instead of a single item.
        """
        ...

    def with_retry(self, max_attempts: int, delay_ms: int) -> Pipeline:
        """Set a constant retry policy (fluent)."""
        ...

    def with_retry_exponential(
        self, max_attempts: int, base_ms: int, factor: int = 2
    ) -> Pipeline:
        """Set an exponential-backoff retry policy (fluent)."""
        ...

    def with_batch_size(self, size: int) -> Pipeline:
        """Set the default batch size for tasks (fluent)."""
        ...

    def with_concurrency(self, n: int) -> Pipeline:
        """Set the number of items processed concurrently (fluent)."""
        ...

    async def execute(
        self,
        inputs: list[Any],
        ctx: TaskContext,
        watcher: Optional[Any] = None,
    ) -> list[Any]:
        """Execute the pipeline asynchronously.

        Returns the list of output values produced by the last task.
        *watcher* is an optional :class:`~cognee_pipeline.Watcher`-like
        object with event callbacks.
        """
        ...

    def execute_sync(
        self,
        inputs: list[Any],
        ctx: TaskContext,
        watcher: Optional[Any] = None,
    ) -> list[Any]:
        """Execute the pipeline synchronously (blocks the calling thread).

        Do **not** call this from within a running asyncio event loop — use
        :meth:`execute` instead.
        """
        ...

    def execute_in_background(
        self,
        inputs: list[Any],
        ctx: TaskContext,
        watcher: Optional[Any] = None,
    ) -> PipelineRunHandle:
        """Spawn a background execution and return a handle.

        Call :meth:`PipelineRunHandle.wait` to retrieve the results.
        """
        ...

class PipelineRunHandle:
    """Handle to a pipeline running in the background."""

    async def wait(self) -> list[Any]:
        """Await the pipeline result.

        Returns the list of output values produced by the last task.
        May be called only once — the handle is consumed.
        """
        ...

# ---------------------------------------------------------------------------
# CogneeConfig
# ---------------------------------------------------------------------------

class CogneeConfig:
    """Configuration surface for a :class:`Cognee` handle.

    Obtained via the ``config`` property on a :class:`Cognee` instance::

        cognee.config.set_str("llm_api_key", "sk-...")
        cfg = cognee.config.get()
    """

    def set(self, key: str, value: Any) -> None:
        """Set a single configuration key.

        *key* is a ``snake_case`` ``Settings`` field name (e.g. ``"llm_model"``).
        *value* can be ``str``, ``int``, ``float``, ``bool``, ``list``, or ``dict``.

        Raises :exc:`CogneeUnknownConfigKeyError` for unrecognised keys.
        Raises :exc:`CogneeConfigTypeMismatchError` on type mismatch.
        """
        ...

    def set_str(self, key: str, value: str) -> None:
        """Convenience wrapper: set a string-typed configuration key.

        Raises :exc:`CogneeUnknownConfigKeyError` for unrecognised keys.
        Raises :exc:`CogneeConfigTypeMismatchError` when the field is not a string.
        """
        ...

    def get(self) -> dict[str, Any]:
        """Return the current configuration as a ``dict``.

        Secret fields (``llm_api_key``, ``embedding_api_key``, etc.) are
        replaced with ``"***REDACTED***"`` before being returned.
        """
        ...

    def set_llm_config(self, values: dict[str, Any]) -> None:
        """Bulk-update LLM configuration fields.

        Accepted keys: ``llm_provider``, ``llm_model``, ``llm_api_key``,
        ``llm_endpoint``, ``llm_api_version``, ``llm_temperature``,
        ``llm_streaming``, ``llm_max_completion_tokens``, ``llm_max_retries``,
        ``llm_max_parallel_requests``.
        """
        ...

    def set_embedding_config(self, values: dict[str, Any]) -> None:
        """Bulk-update embedding configuration fields.

        Accepted keys: ``embedding_provider``, ``embedding_model``,
        ``embedding_dimensions``, ``embedding_endpoint``, ``embedding_api_key``,
        ``embedding_model_path``, ``embedding_tokenizer_path``.
        """
        ...

    def set_vector_db_config(self, values: dict[str, Any]) -> None:
        """Bulk-update vector DB configuration fields.

        Accepted keys: ``vector_db_provider``, ``vector_db_url``, ``vector_db_key``,
        ``vector_db_host``, ``vector_db_port``, ``vector_db_name``.
        """
        ...

    def set_graph_db_config(self, values: dict[str, Any]) -> None:
        """Bulk-update graph DB configuration fields.

        Accepted keys: ``graph_database_provider``, ``graph_model``,
        ``graph_file_path``.
        """
        ...

# ---------------------------------------------------------------------------
# CogneeDatasets
# ---------------------------------------------------------------------------

class CogneeDatasets:
    """Dataset management sub-object.

    Accessible as ``cognee.datasets`` on every :class:`Cognee` instance::

        datasets = await cognee.datasets.list()
        has = await cognee.datasets.has(dataset_id)
        await cognee.datasets.empty(dataset_id)
    """

    async def list(self) -> list[dict[str, Any]]:
        """List all datasets for the current owner.

        Returns a list of dataset dicts (each has at least ``"id"`` and
        ``"name"`` keys).
        """
        ...

    async def list_data(self, dataset_id: str) -> list[dict[str, Any]]:
        """List all data items in *dataset_id*.

        *dataset_id* must be a valid UUID string.
        Raises :exc:`CogneeValidationError` for an invalid UUID.
        """
        ...

    async def has(self, dataset_id: str) -> bool:
        """Return ``True`` if *dataset_id* contains at least one data item.

        Returns ``False`` for a non-existent dataset UUID.
        Raises :exc:`CogneeValidationError` for an invalid UUID.
        """
        ...

    async def status(self, dataset_ids: list[str]) -> dict[str, str]:
        """Return pipeline run statuses for *dataset_ids*.

        Returns a dict mapping each UUID to a status string
        (``"INITIATED"``, ``"STARTED"``, ``"COMPLETED"``, ``"ERRORED"``).
        Raises :exc:`CogneeValidationError` for invalid UUID strings.
        """
        ...

    async def empty(self, dataset_id: str) -> dict[str, Any]:
        """Remove all data from *dataset_id* and delete the dataset record.

        Raises :exc:`CogneeValidationError` for an invalid UUID.
        Returns a delete-result dict.
        """
        ...

    async def delete_data(
        self,
        dataset_id: str,
        data_id: str,
        opts: Optional[dict[str, Any]] = None,
    ) -> dict[str, Any]:
        """Delete a single data item from a dataset.

        Both IDs must be valid UUID strings.  Accepted *opts* keys
        (``snake_case`` and ``camelCase`` both accepted):

        - ``soft_delete`` / ``softDelete`` — bool (default ``False``)
        - ``delete_dataset_if_empty`` / ``deleteDatasetIfEmpty`` — bool (default ``False``)

        Raises :exc:`CogneeValidationError` for invalid UUIDs or bad opts.
        """
        ...

    async def delete_all(self) -> list[dict[str, Any]]:
        """Delete all datasets for the current owner.

        Returns a list of delete-result dicts (one per deleted dataset).
        """
        ...

# ---------------------------------------------------------------------------
# CogneeSessions
# ---------------------------------------------------------------------------

class CogneeSessions:
    """Session management sub-object.

    Accessible as ``cognee.sessions`` on every :class:`Cognee` instance::

        entries = await cognee.sessions.get("session-id")
        await cognee.sessions.set_graph_context("session-id", "ctx")
    """

    async def get(
        self,
        session_id: str,
        opts: Optional[dict[str, Any]] = None,
    ) -> list[dict[str, Any]]:
        """Retrieve QA history entries for *session_id*.

        *opts* accepts ``"lastN"`` (or ``"last_n"``) to limit results.
        Returns a list of ``SessionQAEntry`` dicts (may be empty).
        """
        ...

    async def add_feedback(
        self,
        session_id: str,
        qa_id: str,
        opts: Optional[dict[str, Any]] = None,
    ) -> bool:
        """Add feedback to a QA entry.

        *opts* accepts ``"feedbackText"`` (str) and/or ``"feedbackScore"``
        (int); ``snake_case`` spellings also accepted.
        Returns ``True`` on success.
        """
        ...

    async def delete_feedback(self, session_id: str, qa_id: str) -> bool:
        """Remove feedback from a QA entry.

        Returns ``True`` if feedback was removed, ``False`` if not found.
        """
        ...

    async def get_graph_context(self, session_id: str) -> Optional[str]:
        """Retrieve the stored graph context for *session_id*.

        Returns ``None`` when no context has been stored, or a ``str``.
        """
        ...

    async def set_graph_context(self, session_id: str, context: str) -> None:
        """Store a graph context snapshot for *session_id*.

        Returns ``None``.
        """
        ...

# ---------------------------------------------------------------------------
# CogneeNotebooks
# ---------------------------------------------------------------------------

class CogneeNotebooks:
    """Notebook management sub-object.

    Accessible as ``cognee.notebooks`` on every :class:`Cognee` instance::

        nbs = await cognee.notebooks.list()
        nb  = await cognee.notebooks.create("My Notebook")
        await cognee.notebooks.delete(nb["id"])
    """

    async def list(self) -> list[dict[str, Any]]:
        """List all notebooks for the current owner.

        Returns a list of ``Notebook`` dicts (each has ``"id"`` and ``"name"``).
        """
        ...

    async def create(
        self,
        name: str,
        cells: Optional[list[Any]] = None,
        deletable: bool = True,
    ) -> dict[str, Any]:
        """Create a new notebook.

        *cells* defaults to an empty list.  ``deletable`` is forced to
        ``True`` (Python-library parity).
        Returns the created ``Notebook`` dict.
        """
        ...

    async def update(
        self, id: str, patch: dict[str, Any]
    ) -> Optional[dict[str, Any]]:
        """Update a notebook's ``name`` and/or ``cells``.

        Returns the updated ``Notebook`` dict, or ``None`` if not found.
        """
        ...

    async def delete(self, id: str) -> bool:
        """Delete a notebook by UUID.

        Returns ``True`` if deleted, ``False`` if not found.
        """
        ...

# ---------------------------------------------------------------------------
# Cognee — the main SDK handle
# ---------------------------------------------------------------------------

class Cognee:
    """SDK handle. Entry point for all SDK-tier operations.

    Create with an optional JSON settings object that overrides env-derived
    defaults::

        cognee = Cognee()
        cognee = Cognee('{"llm_model": "gpt-4o", "embedding_provider": "openai"}')
        await cognee.warm()
    """

    def __init__(self, settings: Optional[str] = None) -> None: ...

    @property
    def config(self) -> CogneeConfig:
        """Configuration surface for this handle."""
        ...

    @property
    def datasets(self) -> CogneeDatasets:
        """Dataset management surface for this handle."""
        ...

    @property
    def sessions(self) -> CogneeSessions:
        """Session management surface for this handle."""
        ...

    @property
    def notebooks(self) -> CogneeNotebooks:
        """Notebook management surface for this handle."""
        ...

    async def warm(self) -> None:
        """Build engines and resolve the default user.

        Call before the first ``add()`` / ``cognify()`` / ``search()`` to
        avoid a cold-start latency spike on the first operation.
        """
        ...

    async def owner_id(self) -> str:
        """Return the owner UUID string (warms the handle lazily)."""
        ...

    # -- Core pipeline --------------------------------------------------------

    async def add(
        self,
        inputs: Union[dict[str, Any], list[dict[str, Any]]],
        dataset_name: str,
        opts: Optional[dict[str, Any]] = None,
    ) -> dict[str, Any]:
        """Ingest one or more inputs into *dataset_name*.

        *inputs* is a single typed-descriptor dict or a list of them; each
        must have a ``"type"`` key (``"text"``, ``"file"``, ``"url"``,
        ``"binary"``).

        Returns a dict with camelCase keys: ``datasetName``, ``added``,
        ``addedCount``, ``deduplicated``, ``deduplicatedCount``.

        Accepted *opts* keys (``snake_case`` and ``camelCase`` both accepted):
        ``tenant``.
        """
        ...

    async def cognify(
        self,
        dataset_name: str,
        opts: Optional[dict[str, Any]] = None,
    ) -> dict[str, Any]:
        """Extract a knowledge graph from *dataset_name*.

        Returns a dict with camelCase keys: ``chunks``, ``entities``,
        ``edges``, ``summaries``, ``embeddings``, ``alreadyCompleted``,
        ``priorPipelineRunId``.

        Accepted *opts* keys (``snake_case`` and ``camelCase`` both accepted):
        ``tenant``, ``chunkSize``, ``chunkOverlap``, ``summarization``,
        ``temporalCognify``, ``triplet``.
        """
        ...

    async def add_and_cognify(
        self,
        inputs: Union[dict[str, Any], list[dict[str, Any]]],
        dataset_name: str,
        opts: Optional[dict[str, Any]] = None,
    ) -> dict[str, Any]:
        """Ingest inputs and immediately extract the knowledge graph.

        Returns a dict with two top-level keys: ``add`` and ``cognify``
        (same shapes as the individual operations).
        """
        ...

    # -- Retrieval ------------------------------------------------------------

    async def search(
        self,
        query: str,
        opts: Optional[dict[str, Any]] = None,
    ) -> Any:
        """Query the knowledge graph.

        Returns a list or dict matching the ``SearchResponse`` shape
        (depends on the ``search_type`` requested).

        Accepted *opts* keys (``snake_case`` and ``camelCase`` both accepted):
        ``search_type`` / ``searchType``, ``datasets``, ``dataset_ids`` /
        ``datasetIds``, ``top_k`` / ``topK``, ``system_prompt`` /
        ``systemPrompt``, ``session_id`` / ``sessionId``, ``node_type`` /
        ``nodeType``, ``node_name`` / ``nodeName``, ``only_context`` /
        ``onlyContext``, ``use_combined_context`` / ``useCombinedContext``,
        ``verbose``, ``save_interaction`` / ``saveInteraction``,
        ``auto_feedback_detection`` / ``autoFeedbackDetection``.
        """
        ...

    async def recall(
        self,
        query: str,
        opts: Optional[dict[str, Any]] = None,
    ) -> dict[str, Any]:
        """Recall from memory using the session-aware routing pipeline.

        Returns a dict with camelCase keys: ``items``, ``searchTypeUsed``,
        ``autoRouted``, ``searchResponse``.

        Accepted *opts* keys (``snake_case`` and ``camelCase`` both accepted):
        ``search_type`` / ``searchType``, ``datasets``, ``top_k`` / ``topK``,
        ``auto_route`` / ``autoRoute``, ``session_id`` / ``sessionId``,
        ``scope``.
        """
        ...

    # -- Data management ------------------------------------------------------

    async def forget(
        self,
        target: dict[str, Any],
        opts: Optional[dict[str, Any]] = None,
    ) -> dict[str, Any]:
        """Delete data from the knowledge graph.

        *target* is a discriminated union on ``kind``:

        - ``{"kind": "all"}`` — delete everything for this owner
        - ``{"kind": "dataset", "dataset": {"name": str} | {"id": str}}``
        - ``{"kind": "item", "dataId": str, "dataset": ...}``

        Both ``snake_case`` and ``camelCase`` top-level keys are accepted
        (e.g. both ``data_id`` and ``dataId`` work).

        Returns a dict with keys ``target`` (string) and ``deleteResult`` (dict).
        Raises :exc:`CogneeValidationError` for an unknown ``kind`` value.
        """
        ...

    async def update(
        self,
        data_id: str,
        new_data: Union[dict[str, Any], list[dict[str, Any]]],
        dataset_name: str,
        opts: Optional[dict[str, Any]] = None,
    ) -> dict[str, Any]:
        """Replace a data item with new content and re-cognify.

        Returns a dict with camelCase keys: ``deletedDataId``, ``deleteResult``,
        ``newData``, ``cognifyResult``.
        """
        ...

    async def prune_data(self) -> None:
        """Remove all files from data storage.

        Returns ``None``.
        """
        ...

    async def prune_system(
        self,
        opts: Optional[dict[str, Any]] = None,
    ) -> dict[str, Any]:
        """Selective backend cleanup.

        Accepted *opts* keys (``snake_case`` and ``camelCase`` both accepted):
        ``prune_graph`` / ``pruneGraph``, ``prune_vector`` / ``pruneVector``,
        ``prune_metadata`` / ``pruneMetadata``, ``prune_cache`` / ``pruneCache``.

        Returns a dict with camelCase keys: ``dataPruned``, ``graphPruned``,
        ``vectorPruned``, ``metadataPruned``, ``cachePruned``.
        """
        ...

    # -- Memory ---------------------------------------------------------------

    async def remember(
        self,
        inputs: Union[dict[str, Any], list[dict[str, Any]]],
        dataset_name: str,
        opts: Optional[dict[str, Any]] = None,
    ) -> Any:
        """One-call add + cognify + optional self-improvement.

        Accepted *opts* keys: ``sessionId``, ``selfImprovement``, ``tenant``
        (``snake_case`` spellings accepted too).
        Returns the ``RememberResult`` as a dict.
        """
        ...

    async def remember_entry(
        self,
        entry: dict[str, Any],
        dataset_name: str,
        session_id: str,
        opts: Optional[dict[str, Any]] = None,
    ) -> Any:
        """Store a single typed memory entry (QA, trace, or feedback).

        *entry* is a discriminated-union dict with a ``"type"`` key:
        ``"qa"``, ``"trace"``, or ``"feedback"``.
        """
        ...

    async def memify(
        self,
        opts: Optional[dict[str, Any]] = None,
    ) -> dict[str, Any]:
        """Build triplet embeddings over the entire knowledge graph.

        Idempotent.  Returns a dict with camelCase keys: ``tripletCount``,
        ``indexedCount``, ``batchCount``, ``alreadyCompleted``,
        ``priorPipelineRunId``.
        """
        ...

    async def improve(self, opts: dict[str, Any]) -> dict[str, Any]:
        """Apply graph improvement based on session feedback.

        *opts* must contain ``"datasetName"`` (camelCase) or
        ``"dataset_name"`` (normalised).

        Returns a dict with camelCase keys: ``stagesRun``, ``memifyResult``,
        ``feedbackEntriesProcessed``, ``feedbackEntriesApplied``,
        ``sessionsPersisted``, ``edgesSynced``.
        """
        ...

    # -- Visualisation --------------------------------------------------------

    async def visualize(
        self,
        opts: Optional[dict[str, Any]] = None,
    ) -> str:
        """Render the knowledge graph as a self-contained d3.js HTML document.

        Returns the full HTML as a ``str``.
        Raises :exc:`CogneeFeatureNotBuiltError` when the ``visualization``
        Cargo feature was not compiled in.
        """
        ...

    async def visualize_to_file(
        self,
        opts: Optional[dict[str, Any]] = None,
    ) -> str:
        """Render the knowledge graph to a file and return the written path.

        *opts* accepts ``destination_path`` / ``destinationPath`` to override
        the default ``~/graph_visualization.html`` output path.
        Returns the absolute path of the written file as a ``str``.
        Raises :exc:`CogneeFeatureNotBuiltError` when the ``visualization``
        Cargo feature was not compiled in.
        """
        ...

    # -- Admin ----------------------------------------------------------------

    async def reset_pipeline_run_status(
        self,
        dataset_id: str,
        pipeline_name: str,
    ) -> None:
        """Reset the run status for a specific pipeline within a dataset.

        Raises :exc:`CogneeValidationError` if *dataset_id* is not a valid UUID.
        """
        ...

    async def reset_dataset_pipeline_run_status(self, dataset_id: str) -> None:
        """Reset all pipeline run statuses for a dataset.

        Raises :exc:`CogneeValidationError` if *dataset_id* is not a valid UUID.
        """
        ...

    async def get_or_create_default_user(self) -> dict[str, Any]:
        """Get or create the default user account.

        Returns a ``User`` dict with at least ``"id"`` and ``"email"`` fields.
        """
        ...

# ---------------------------------------------------------------------------
# Module-level functions
# ---------------------------------------------------------------------------

def setup_logging() -> None:
    """Initialize cognee's file-based logging subsystem from environment variables.

    All configuration is via env vars set *before* calling this function:
    ``COGNEE_LOG_FILE``, ``COGNEE_LOGS_DIR``, ``LOG_FILE_NAME``,
    ``COGNEE_LOG_ROTATION``, ``COGNEE_LOG_FORMAT``,
    ``COGNEE_LOG_BACKUP_COUNT``, ``LOG_LEVEL``, ``RUST_LOG``.

    Idempotent: calling more than once is a no-op (first call wins).
    """
    ...

def setup_telemetry() -> None:
    """Initialize the OpenTelemetry tracing pipeline.

    Reads ``OTEL_EXPORTER_OTLP_ENDPOINT``, ``COGNEE_TRACING_ENABLED``, and
    related env vars.  Idempotent.
    """
    ...

def setup_telemetry_analytics() -> None:
    """Arm the product-analytics client (opt-out via ``TELEMETRY_DISABLED``).

    Idempotent.
    """
    ...

async def serve(opts: Optional[dict[str, Any]] = None) -> dict[str, Any]:
    """Connect to a Cognee Cloud instance (process-wide singleton).

    When ``opts["url"]`` is set, **direct mode** is used — no Auth0 flow.
    Otherwise the Auth0 device-code flow runs (requires a TTY).

    Accepted *opts* keys (``snake_case`` and ``camelCase`` both accepted):
    ``url``, ``api_key`` / ``apiKey``, ``cloud_url`` / ``cloudUrl``,
    ``auth0_domain`` / ``auth0Domain``, ``auth0_client_id`` /
    ``auth0ClientId``, ``auth0_audience`` / ``auth0Audience``.

    Returns ``{"connected": True, "serviceUrl": "…"}`` on success.
    Raises :exc:`CogneeFeatureNotBuiltError` when the ``cloud`` Cargo feature
    was not compiled in.
    """
    ...

async def disconnect(opts: Optional[dict[str, Any]] = None) -> None:
    """Disconnect from Cognee Cloud and revert to local-execution mode.

    Accepted *opts* keys: ``wipe_credentials`` / ``wipeCredentials`` — when
    ``True``, the on-disk credential cache is deleted (default ``False``).

    Raises :exc:`CogneeFeatureNotBuiltError` when the ``cloud`` Cargo feature
    was not compiled in.
    """
    ...
