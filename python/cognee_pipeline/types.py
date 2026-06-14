"""Public TypedDict types for the cognee-pipeline Python binding.

These types document the shapes of inputs, option dicts, and result dicts
used throughout the :class:`~cognee_pipeline.Cognee` API.  They are importable
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
wire contract for cross-binding uniformity).  The :mod:`cognee_pipeline.compat`
module re-keys these to **snake_case** before returning them to Python callers;
the ``*Result`` types here describe the snake_case view.

To access the raw camelCase dict (e.g. for cross-binding parity tests), call
the native method directly on a :class:`~cognee_pipeline.Cognee` instance
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
    """Options accepted by :meth:`Cognee.add` / :func:`cognee_pipeline.compat.add`."""

    #: Tenant UUID string (multi-tenant scoping).
    tenant: str


class CognifyOpts(TypedDict, total=False):
    """Options accepted by :meth:`Cognee.cognify` / :func:`cognee_pipeline.compat.cognify`."""

    tenant: str
    chunk_size: int
    chunk_overlap: int
    summarization: bool
    temporal_cognify: bool
    #: Index ``"source → relation → target"`` triplet embeddings.
    triplet: bool


class SearchOpts(TypedDict, total=False):
    """Options accepted by :meth:`Cognee.search` / :func:`cognee_pipeline.compat.search`."""

    #: One of the 15 :class:`~cognee_pipeline.SearchType` strings.
    #: Defaults to ``"GRAPH_COMPLETION"``.
    search_type: str
    datasets: list[str]
    dataset_ids: list[str]
    top_k: int
    system_prompt: str
    session_id: str
    node_type: str
    node_name: list[str]
    only_context: bool
    use_combined_context: bool
    verbose: bool
    save_interaction: bool
    auto_feedback_detection: bool
    user_id: str


class RecallOpts(TypedDict, total=False):
    """Options accepted by :meth:`Cognee.recall`."""

    search_type: str
    datasets: list[str]
    top_k: int
    auto_route: bool
    session_id: str
    #: ``"auto"`` | ``"graph"`` | ``"session"`` | ``"trace"`` | ``"graph_context"`` | ``"all"``
    scope: Union[str, list[str]]


class RememberOpts(TypedDict, total=False):
    """Options accepted by :meth:`Cognee.remember`."""

    session_id: str
    self_improvement: bool
    tenant: str


class MemifyOpts(TypedDict, total=False):
    """Options accepted by :meth:`Cognee.memify`."""

    triplet_batch_size: int
    node_type_filter: str
    node_name_filter: list[str]
    #: ``"AND"`` or ``"OR"``
    node_name_filter_operator: str


class ImproveOpts(TypedDict, total=False):
    """Options accepted by :meth:`Cognee.improve`.

    ``dataset_name`` is required; all other keys are optional.
    """

    dataset_name: str  # required — missing raises CogneeValidationError at runtime
    session_ids: list[str]
    node_name: list[str]
    feedback_alpha: float
    tenant: str


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


class ServeOpts(TypedDict, total=False):
    """Options accepted by :func:`cognee_pipeline.serve`."""

    url: str
    api_key: str
    cloud_url: str
    auth0_domain: str
    auth0_client_id: str
    auth0_audience: str


class DisconnectOpts(TypedDict, total=False):
    """Options accepted by :func:`cognee_pipeline.disconnect`."""

    #: Delete on-disk credential cache when ``True`` (default ``False``).
    wipe_credentials: bool


# ---------------------------------------------------------------------------
# Result TypedDicts  — snake_case view (as returned by the compat layer)
# ---------------------------------------------------------------------------


class AddResult(TypedDict):
    """Result of :func:`cognee_pipeline.compat.add` (snake_case view).

    The native :meth:`Cognee.add` returns the same data with camelCase keys
    (``datasetName``, ``addedCount``, ``deduplicatedCount``).
    """

    dataset_name: str
    added: list[Any]
    added_count: int
    deduplicated: list[Any]
    deduplicated_count: int


class CognifyResult(TypedDict):
    """Result of :func:`cognee_pipeline.compat.cognify` (snake_case view).

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
    """A single item in the list returned by :func:`cognee_pipeline.compat.search`.

    The exact fields depend on the ``search_type`` used.  Common keys are
    ``text`` / ``score`` for chunk-based types and ``answer`` for completion
    types.
    """

    pass  # open-ended: fields vary by search_type


class RecallResult(TypedDict):
    """Result of :meth:`Cognee.recall` (snake_case view).

    The native method returns the same data with camelCase keys
    (``searchTypeUsed``, ``autoRouted``, ``searchResponse``).
    """

    items: list[Any]
    search_type_used: str | None
    auto_routed: bool
    search_response: Any


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
    (``deletedDataId``, ``deleteResult``, ``newData``, ``cognifyResult``).
    """

    deleted_data_id: str
    delete_result: Any
    new_data: list[Any]
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


class MemifyResult(TypedDict):
    """Result of :meth:`Cognee.memify` (snake_case view).

    The native method returns the same data with camelCase keys
    (``tripletCount``, ``indexedCount``, ``batchCount``,
    ``alreadyCompleted``, ``priorPipelineRunId``).
    """

    triplet_count: int
    indexed_count: int
    batch_count: int
    already_completed: bool
    prior_pipeline_run_id: str | None


class ImproveResult(TypedDict):
    """Result of :meth:`Cognee.improve` (snake_case view).

    The native method returns the same data with camelCase keys.
    """

    stages_run: list[str]
    memify_result: MemifyResult | None
    feedback_entries_processed: int
    feedback_entries_applied: int
    sessions_persisted: int
    edges_synced: int


class ServeResult(TypedDict):
    """Result of :func:`cognee_pipeline.serve`."""

    connected: bool
    service_url: str


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
    "MemifyOpts",
    "ImproveOpts",
    "ForgetTarget",
    "UpdateOpts",
    "PruneSystemOpts",
    "DeleteDataOpts",
    "VisualizeOpts",
    "ServeOpts",
    "DisconnectOpts",
    # Result types
    "AddResult",
    "CognifyResult",
    "SearchResult",
    "RecallResult",
    "ForgetResult",
    "UpdateResult",
    "PruneResult",
    "MemifyResult",
    "ImproveResult",
    "ServeResult",
]
