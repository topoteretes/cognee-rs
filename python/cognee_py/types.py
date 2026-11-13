"""Public TypedDict types for the cognee-py Python binding.

These types document the shapes of inputs, option dicts, and result dicts
used throughout the :class:`~cognee_py.Cognee` API.  They are importable
at runtime and referenced from the companion ``.pyi`` stubs.

Input types
-----------
:data:`DataInput` is a discriminated union of :class:`TextInput`,
:class:`FileInput`, :class:`UrlInput`, and :class:`BinaryInput`.  The ``type``
field is a literal string that acts as the discriminator, matching the wire
format accepted by :meth:`Cognee.add` and related methods.

Options types
-------------
All ``*Opts`` classes use ``total=False`` (all keys optional) unless a field
is always required.  Both ``snake_case`` and ``camelCase`` spellings are
accepted by the native layer at runtime; the types here document the
``snake_case`` spellings that the Python layer normalises and forwards.

Result types (snake_case view)
-------------------------------
The native module returns results with **camelCase** keys (matching the C / JS
wire contract for cross-binding uniformity).  The :mod:`cognee_py.compat`
module re-keys these to **snake_case** before returning them to Python callers;
the ``*Result`` types here describe the snake_case view.

To access the raw camelCase dict (e.g. for cross-binding parity tests), call
the native method directly on a :class:`~cognee_py.Cognee` instance
instead of using the compat-layer functions.
"""

from __future__ import annotations

from typing import Any, Literal, TypedDict, Union


# ---------------------------------------------------------------------------
# Input TypedDicts
# ---------------------------------------------------------------------------


class TextInput(TypedDict):
    """A plain-text input descriptor.  All keys are required."""

    type: Literal["text"]
    text: str


class FileInput(TypedDict):
    """A local file-path input descriptor.  All keys are required."""

    type: Literal["file"]
    path: str


class UrlInput(TypedDict):
    """A URL input descriptor (http/https/ftp).  All keys are required."""

    type: Literal["url"]
    url: str


class BinaryInput(TypedDict):
    """A raw binary input descriptor.  All keys are required."""

    type: Literal["binary"]
    #: Raw bytes; also accepts ``bytearray`` or a base64-encoded ``str``.
    bytes: Union[bytes, bytearray, str]
    name: str


#: Discriminated union of all supported input types.
DataInput = Union[TextInput, FileInput, UrlInput, BinaryInput]


# ---------------------------------------------------------------------------
# Options TypedDicts  (total=False — all keys optional)
# ---------------------------------------------------------------------------


class AddOpts(TypedDict, total=False):
    """Options accepted by :meth:`Cognee.add` / :func:`cognee_py.compat.add`."""

    #: Tenant UUID string (multi-tenant scoping).
    tenant: str


class CognifyOpts(TypedDict, total=False):
    """Options accepted by :meth:`Cognee.cognify` / :func:`cognee_py.compat.cognify`."""

    tenant: str
    chunk_size: int
    #: Accepted but inert on the default chunking path: no chunker reads it, so it changes no chunk boundary (Python's DefaultChunkEngine does consume it, but that engine is unreachable). Only validation reads it, rejecting overlap >= chunk size. Kept for API compatibility; see CognifyConfig::chunk_overlap in crates/cognify/src/config.rs.
    chunk_overlap: int
    summarization: bool


class SearchOpts(TypedDict, total=False):
    """Options accepted by :meth:`Cognee.search` / :func:`cognee_py.compat.search`."""

    #: One of the 16 :class:`~cognee_py.SearchType` strings.
    #: Defaults to ``"HYBRID_COMPLETION"``.
    search_type: str
    datasets: list[str]
    dataset_ids: list[str]
    top_k: int
    system_prompt: str
    session_id: str
    node_type: str
    node_name: list[str]
    node_name_filter_operator: str
    only_context: bool
    use_combined_context: bool
    verbose: bool
    auto_feedback_detection: bool
    #: Graph searches only: rank triplets this many hops around the nearest
    #: hits. A positive integer.
    neighborhood_depth: int
    #: With ``neighborhood_depth``: at most this many hits to expand from,
    #: nearest first (all of them when omitted).
    neighborhood_seed_top_k: int
    #: Completion searches attach structured ``evidence`` references (and RAG
    #: text answers gain an ``Evidence:`` block). Default ``False``.
    include_references: bool
    user_id: str


class RecallOpts(TypedDict, total=False):
    """Options accepted by :meth:`Cognee.recall`."""

    search_type: str
    datasets: list[str]
    top_k: int
    auto_route: bool
    session_id: str
    #: ``"auto"`` | ``"graph"`` | ``"session"`` | ``"session_first"`` |
    #: ``"session_context"`` | ``"trace"`` | ``"all"`` (``"graph_context"`` is
    #: a deprecated alias of ``"graph"``; ``"tools"`` and ``"code"`` are
    #: accepted but raise ``CogneeValidationError`` as not supported yet). An
    #: empty list runs no source and returns no items, as in Python.
    scope: Union[str, list[str]]
    neighborhood_depth: int
    neighborhood_seed_top_k: int
    #: As ``SearchOpts.include_references``; evidence lands in each result's
    #: ``metadata["evidence"]``.
    include_references: bool
    #: Profile the ``"session_context"`` scope renders: ``"qa"`` (default) or
    #: ``"agent"``. Not validated, as in Python: an unknown profile matches no
    #: entries and renders nothing.
    context_profile: str
    #: ``"always"`` (default) or ``"on_empty"``: when the ``"tools"`` scope
    #: runs. Validated on every call; no effect until that scope exists.
    tools_trigger: str
    #: What the ``node_name`` values are: a graph node class name such as
    #: ``"Entity"``. Omitted, the names are node-set tags.
    node_type: str
    #: Scope the graph source to these node names. A graph ``node_type`` is
    #: honoured by the GRAPH_COMPLETION family and HYBRID_COMPLETION; CHUNKS
    #: and RAG_COMPLETION apply node-set names only.
    node_name: list[str]
    #: ``"OR"`` (default) or ``"AND"``: how ``node_name`` combines.
    node_name_filter_operator: str
    #: Keep the graph search's context and graph in its response.
    verbose: bool


class RememberOpts(TypedDict, total=False):
    """Options accepted by :meth:`Cognee.remember`."""

    session_id: str
    # Run ``improve()`` after the remember (default ``True``, as in Python).
    self_improvement: bool
    tenant: str


class ImproveOpts(TypedDict, total=False):
    """Options accepted by :meth:`Cognee.improve`.

    ``dataset_name`` is required; all other keys are optional.
    """

    dataset_name: str  # required — missing raises CogneeValidationError at runtime
    # UUID of a dataset the caller already resolved (possibly shared with it),
    # as Python's ``improve(dataset=<UUID>)``.
    dataset_id: str
    session_ids: list[str]
    feedback_alpha: float
    tenant: str
    # Run the ``global_context_index`` stage; omitted follows the
    # ``improve.global_context_index`` setting (default on), an explicit value
    # wins either way.
    build_global_context_index: bool
    # Run the ``review_conflicts`` stage; omitted follows the
    # ``improve.review_conflicts`` setting (default on), an explicit value
    # wins either way.
    review_conflicts: bool
    # Opt in to the ``build_truth_subspace`` stage (default ``False``).
    build_truth_subspace: bool
    # Resolve the dataset and validate the options, then return a ``running``
    # result at once (``background`` true, ``finished`` false, no stages) and
    # run the improve as a process-wide background task. Wait for it with
    # ``Cognee.wait_for_background_tasks()``; ``close()`` waits too. No per-run
    # handle (default ``False``).
    run_in_background: bool


class WaitForBackgroundTasksOpts(TypedDict, total=False):
    """Options accepted by :meth:`Cognee.wait_for_background_tasks`."""

    # Bound the wait, in seconds (non-negative); nothing is cancelled when it
    # elapses. Absent waits until every task finished.
    timeout_seconds: float


class ForgetTarget(TypedDict, total=False):
    """Deletion target for :meth:`Cognee.forget`.

    ``kind`` is required (``"all"``, ``"dataset"``, or ``"item"``).
    """

    #: ``"all"`` | ``"dataset"`` | ``"item"``  (required)
    kind: str
    #: Required when ``kind == "item"``.  Also accepts camelCase ``"dataId"``.
    data_id: str
    #: Required when ``kind`` is ``"dataset"`` or ``"item"``.
    dataset: dict[str, str]


class UpdateOpts(TypedDict, total=False):
    """Options accepted by :meth:`Cognee.update`."""

    tenant: str


class PruneSystemOpts(TypedDict, total=False):
    """Options accepted by :meth:`Cognee.prune_system`."""

    prune_graph: bool
    prune_vector: bool
    prune_metadata: bool
    prune_cache: bool


class DeleteDataOpts(TypedDict, total=False):
    """Options accepted by :meth:`CogneeDatasets.delete_data`."""

    soft_delete: bool
    delete_dataset_if_empty: bool


class VisualizeOpts(TypedDict, total=False):
    """Options accepted by :meth:`Cognee.visualize_to_file`."""

    #: Absolute path for the output HTML file.
    destination_path: str


# ---------------------------------------------------------------------------
# Result TypedDicts  — snake_case view (as returned by the compat layer)
# ---------------------------------------------------------------------------


class AddResult(TypedDict):
    """Result of :func:`cognee_py.compat.add` (snake_case view).

    The native :meth:`Cognee.add` returns the same data with camelCase keys
    (``datasetName``, ``addedCount``, ``deduplicatedCount``).
    """

    dataset_name: str
    added: list[Any]
    added_count: int
    deduplicated: list[Any]
    deduplicated_count: int


class CognifyResult(TypedDict):
    """Result of :func:`cognee_py.compat.cognify` (snake_case view).

    The native :meth:`Cognee.cognify` returns the same data with camelCase
    keys (``alreadyCompleted``, ``priorPipelineRunId``).
    """

    chunks: int
    entities: int
    edges: int
    summaries: int
    embeddings: int
    already_completed: bool
    prior_pipeline_run_id: str | None


class SearchResult(TypedDict):
    """A single item in the list returned by :func:`cognee_py.compat.search`.

    The exact fields depend on the ``search_type`` used.  Common keys are
    ``text`` / ``score`` for chunk-based types and ``answer`` for completion
    types.
    """

    pass  # open-ended: fields vary by search_type


class RecallResult(TypedDict):
    """Result of :meth:`Cognee.recall` (snake_case view).

    The native method returns the same data with camelCase keys
    (``searchTypeUsed``, ``autoRouted``, ``searchResponse``,
    ``normalizedResults``).
    """

    items: list[Any]
    search_type_used: str | None
    auto_routed: bool
    search_response: Any
    # One typed entry (kind, text, score, metadata, raw, ...) per graph item.
    normalized_results: list[Any]


class ForgetResult(TypedDict):
    """Result of :meth:`Cognee.forget` (snake_case view).

    The native method returns the same data with camelCase key
    ``deleteResult``.
    """

    target: str
    delete_result: Any


class UpdateResult(TypedDict):
    """Result of :meth:`Cognee.update` (snake_case view).

    The native method returns the same data with camelCase keys
    (``dataId``, ``deleteResult``, ``cognifyResult``).
    """

    data_id: str
    delete_result: Any
    cognify_result: Any


class PruneResult(TypedDict):
    """Result of :meth:`Cognee.prune_system` (snake_case view).

    The native method returns the same data with camelCase keys
    (``dataPruned``, ``graphPruned``, etc.).
    """

    data_pruned: bool
    graph_pruned: bool
    vector_pruned: bool
    metadata_pruned: bool
    cache_pruned: bool


class ImproveStageResult(TypedDict):
    """What one improve stage did (Python core's ``StageResult``).

    A nested record, so it keeps its wire keys (``durationMs``) in both the
    native and the snake_case view.
    """

    stage: str
    status: str  # "completed" | "already_completed" | "skipped" | "errored"
    reason: str | None
    error: str | None
    counts: dict[str, int]
    durationMs: int


class ImproveResult(TypedDict):
    """Result of :meth:`Cognee.improve` (snake_case view).

    The native method returns the same data with camelCase keys.
    """

    dataset_id: str | None
    dataset_name: str
    session_ids: list[str]
    # One record per improve stage, in registry order.
    stages: list[ImproveStageResult]
    background: bool
    finished: bool
    error: str | None
    status: str  # "completed" | "errored" | "skipped" | "running"


class WaitForBackgroundTasksResult(TypedDict):
    """Result of :meth:`Cognee.wait_for_background_tasks`."""

    # False only when the timeout elapsed with work still running.
    finished: bool
    # Background tasks still running.
    pending: int


__all__ = [
    # Input types
    "TextInput",
    "FileInput",
    "UrlInput",
    "BinaryInput",
    "DataInput",
    # Options types
    "AddOpts",
    "CognifyOpts",
    "SearchOpts",
    "RecallOpts",
    "RememberOpts",
    "ImproveOpts",
    "ForgetTarget",
    "UpdateOpts",
    "PruneSystemOpts",
    "DeleteDataOpts",
    "VisualizeOpts",
    "WaitForBackgroundTasksOpts",
    # Result types
    "AddResult",
    "CognifyResult",
    "SearchResult",
    "RecallResult",
    "ForgetResult",
    "UpdateResult",
    "PruneResult",
    "ImproveResult",
    "ImproveStageResult",
    "WaitForBackgroundTasksResult",
]
