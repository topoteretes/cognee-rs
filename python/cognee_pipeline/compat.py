"""Drop-in compatibility layer mirroring the upstream ``cognee`` Python SDK.

Import this module to use module-level functions that match the upstream API
shape so that basic ``cognee`` scripts run unmodified (or with just an import
alias change):

.. code-block:: python

    import cognee_pipeline.compat as cognee

    await cognee.add("Hello world")
    await cognee.cognify()
    results = await cognee.search("What is this about?",
                                  query_type=cognee.SearchType.CHUNKS)
    await cognee.prune.prune_data()

The compat layer maintains a lazily-initialised, process-global
:class:`~cognee_pipeline.Cognee` handle that is shared across all
module-level calls, mirroring the implicit global state of the upstream SDK.
The handle-based API (:class:`~cognee_pipeline.Cognee`) remains fully
available for applications that need per-tenant isolation or explicit
lifecycle control.

Input coercion
--------------
:func:`add` accepts the same input shapes as the upstream SDK:

- ``str``                      — ingested as plain text
- :class:`pathlib.Path` / path-like — ingested as a file path
- URL string (``http://`` / ``https://``) — ingested as a URL
- ``bytes`` / ``bytearray``   — ingested as binary content
- ``dict``                     — forwarded as a single typed descriptor
                                 (must already have ``"type"`` key)
- ``list`` / ``tuple``         — each element is coerced independently

Unsupported types raise :exc:`~cognee_pipeline.CogneeValidationError`.

SearchType
----------
:data:`SearchType` is re-exported here so ``cognee.SearchType.CHUNKS`` works
after ``import cognee_pipeline.compat as cognee``.

Result-key casing
-----------------
The native ``_native`` module returns result dicts with **camelCase** keys
(e.g. ``addedCount``, ``priorPipelineRunId``) for cross-binding uniformity
with the C and JavaScript APIs.  This compat layer re-keys all top-level result
dict keys to **snake_case** (e.g. ``added_count``, ``prior_pipeline_run_id``)
so that Python callers receive idiomatic names.

To access the raw camelCase dict, pass ``raw=True`` to the compat functions
that support it, or call the corresponding method directly on a
:class:`~cognee_pipeline.Cognee` instance.

Notes on unsupported upstream types
------------------------------------
The upstream ``cognee`` SDK also defines ``SearchType.AGENTIC_COMPLETION``
and ``SearchType.GRAPH_COMPLETION_DECOMPOSITION``.  These are **not yet
supported** by the Rust core and therefore absent from :class:`SearchType`.
Passing the string values ``"AGENTIC_COMPLETION"`` or
``"GRAPH_COMPLETION_DECOMPOSITION"`` raises
:exc:`~cognee_pipeline.CogneeValidationError` with a descriptive message.
"""

from __future__ import annotations

import os
import pathlib
import re
from typing import Any, Union

from . import Cognee, CogneeValidationError, SearchType

__all__ = [
    "add",
    "cognify",
    "add_and_cognify",
    "search",
    "prune",
    "SearchType",
]

# ---------------------------------------------------------------------------
# Result re-keying: camelCase → snake_case
# ---------------------------------------------------------------------------

_CAMEL_RE = re.compile(r"(?<=[a-z0-9])(?=[A-Z])|(?<=[A-Z])(?=[A-Z][a-z])")


def _camel_to_snake(name: str) -> str:
    """Convert a camelCase or PascalCase string to snake_case.

    Examples::

        _camel_to_snake("addedCount")       == "added_count"
        _camel_to_snake("priorPipelineRunId") == "prior_pipeline_run_id"
        _camel_to_snake("datasetName")      == "dataset_name"
    """
    return _CAMEL_RE.sub("_", name).lower()


def _rekey(obj: Any) -> Any:
    """Recursively re-key the top-level keys of a dict from camelCase to snake_case.

    Non-dict values (lists, scalars) are returned unchanged.  Only the
    *outermost* dict level is re-keyed — nested dicts keep their original
    keys so the wire format for nested objects (e.g. ``deleteResult``,
    ``cognifyResult``) is preserved and readable at one layer.

    This is intentionally shallow: deep-re-keying all nested dicts would
    break field access patterns that callers already use for the raw wire
    format.  Callers who need snake_case all the way through should apply
    ``_rekey`` themselves at each nesting level, or use ``raw=True`` and work
    with the camelCase dict directly.
    """
    if not isinstance(obj, dict):
        return obj
    return {_camel_to_snake(k): v for k, v in obj.items()}


# ---------------------------------------------------------------------------
# Process-global default handle
# ---------------------------------------------------------------------------

_default_handle: Cognee | None = None


def _handle() -> Cognee:
    """Return (creating if necessary) the process-global :class:`Cognee` handle."""
    global _default_handle
    if _default_handle is None:
        _default_handle = Cognee()
    return _default_handle


def reset_default_handle() -> None:
    """Replace the process-global handle with a fresh one.

    Useful in tests that need a clean state without reloading the module.
    After calling this the next module-level call will allocate a new
    :class:`Cognee` handle using the current environment variables.
    """
    global _default_handle
    _default_handle = None


# ---------------------------------------------------------------------------
# Input coercion helpers
# ---------------------------------------------------------------------------

_URL_PREFIXES = ("http://", "https://", "ftp://")


def _coerce_single(data: Any) -> dict:
    """Coerce a single data item to a typed descriptor dict.

    Handles:

    - ``str`` that looks like a URL → ``{"type": "url", "url": "..."}``
    - ``str``                       → ``{"type": "text", "text": "..."}``
    - :class:`pathlib.Path` / path-like → ``{"type": "file", "path": "..."}``
    - ``bytes`` / ``bytearray``     → ``{"type": "binary", "bytes": data}``
    - ``dict``                      → forwarded unchanged (must have ``"type"``)
    """
    if isinstance(data, dict):
        if "type" not in data:
            raise CogneeValidationError(
                'input dict must have a "type" key '
                "(got: {!r})".format(list(data.keys()))
            )
        return data

    if isinstance(data, (bytes, bytearray)):
        return {"type": "binary", "bytes": data, "name": "upload.bin"}

    if isinstance(data, str):
        if any(data.startswith(p) for p in _URL_PREFIXES):
            return {"type": "url", "url": data}
        return {"type": "text", "text": data}

    # pathlib.Path and anything with __fspath__ (os.PathLike)
    if isinstance(data, os.PathLike) or isinstance(data, pathlib.Path):
        return {"type": "file", "path": str(data)}

    raise CogneeValidationError(
        "add() received an input of type {!r} which cannot be coerced to a "
        "cognee data descriptor.  Accepted types: str, pathlib.Path, "
        "bytes/bytearray, dict, or a list of these.".format(type(data).__name__)
    )


def _coerce_inputs(
    data: Union[str, bytes, bytearray, os.PathLike, dict, list, tuple, Any],
) -> list:
    """Coerce *data* to a list of typed descriptor dicts."""
    if isinstance(data, (list, tuple)):
        return [_coerce_single(item) for item in data]
    return [_coerce_single(data)]


# ---------------------------------------------------------------------------
# Module-level SDK functions
# ---------------------------------------------------------------------------


async def add(
    data: Any,
    dataset_name: str = "main_dataset",
    *,
    raw: bool = False,
    **kwargs,
) -> Any:
    """Ingest one or more data items into *dataset_name*.

    Accepts the same input shapes as the upstream ``cognee.add()``:
    plain strings, file paths, URL strings, bytes, dicts, or lists of these.

    By default returns the result dict with **snake_case** keys:
    ``dataset_name``, ``added``, ``added_count``, ``deduplicated``,
    ``deduplicated_count``.

    Pass ``raw=True`` to receive the native camelCase dict (``datasetName``,
    ``addedCount``, ``deduplicatedCount``) for cross-binding parity tests or
    when interoperating with the C / JS APIs.
    """
    descriptors = _coerce_inputs(data)
    # Single-element list: pass the dict directly (handle.add normalises it).
    inputs = descriptors if len(descriptors) != 1 else descriptors[0]
    opts = kwargs if kwargs else None
    result = await _handle().add(inputs, dataset_name, opts or None)
    return result if raw else _rekey(result)


async def cognify(
    datasets: Union[str, list, None] = None,
    *,
    raw: bool = False,
    **kwargs,
) -> Any:
    """Extract a knowledge graph from previously-ingested data.

    *datasets* mirrors the upstream argument name.  When ``None`` the default
    dataset (``"main_dataset"``) is processed.

    By default returns the result dict with **snake_case** keys: ``chunks``,
    ``entities``, ``edges``, ``summaries``, ``embeddings``,
    ``already_completed``, ``prior_pipeline_run_id``.

    Pass ``raw=True`` to receive the native camelCase dict
    (``alreadyCompleted``, ``priorPipelineRunId``).
    """
    dataset_name = datasets if isinstance(datasets, str) else "main_dataset"
    opts = kwargs if kwargs else None
    result = await _handle().cognify(dataset_name, opts or None)
    return result if raw else _rekey(result)


async def add_and_cognify(
    data: Any,
    dataset_name: str = "main_dataset",
    *,
    raw: bool = False,
    **kwargs,
) -> Any:
    """Ingest *data* and immediately extract the knowledge graph.

    Convenience wrapper combining :func:`add` + :func:`cognify` in a single
    native call, skipping re-cognification of deduplicated items.

    By default returns a dict with **snake_case** top-level keys: ``add``
    (snake_case sub-dict) and ``cognify`` (snake_case sub-dict).

    Pass ``raw=True`` to receive the native camelCase dict.
    """
    descriptors = _coerce_inputs(data)
    inputs = descriptors if len(descriptors) != 1 else descriptors[0]
    opts = kwargs if kwargs else None
    result = await _handle().add_and_cognify(inputs, dataset_name, opts or None)
    if raw:
        return result
    # Re-key both the outer dict and the nested add/cognify sub-dicts.
    outer = _rekey(result)
    if isinstance(outer.get("add"), dict):
        outer["add"] = _rekey(outer["add"])
    if isinstance(outer.get("cognify"), dict):
        outer["cognify"] = _rekey(outer["cognify"])
    return outer


async def search(
    query_text: str = "",
    query_type: Union[SearchType, str] = SearchType.GRAPH_COMPLETION,
    top_k: int = 10,
    *,
    datasets=None,
    raw: bool = False,
    **kwargs,
) -> Any:
    """Search the knowledge graph.

    Mirrors the upstream ``cognee.search()`` signature:

    .. code-block:: python

        results = await search("What is X?",
                               query_type=SearchType.CHUNKS,
                               top_k=5)

    *query_type* accepts :class:`SearchType` members or plain strings.
    *top_k* defaults to ``10`` (same as upstream).

    Returns a list of result dicts (shape depends on *query_type*).
    Individual result items are returned as-is (their keys vary by search type
    and are not re-keyed).

    Pass ``raw=True`` to skip any normalisation and receive the value exactly
    as returned by the native layer.
    """
    # Use .value to get the raw string when a SearchType enum member is passed.
    # Plain strings (no .value attribute) fall back to str().  This is safe
    # regardless of whether the caller passes a SearchType member or a bare
    # string like "CHUNKS".
    search_type_str = query_type.value if hasattr(query_type, "value") else str(query_type)
    opts: dict[str, Any] = {"search_type": search_type_str, "top_k": top_k}
    if datasets is not None:
        opts["datasets"] = datasets
    opts.update(kwargs)
    return await _handle().search(query_text, opts)


# ---------------------------------------------------------------------------
# prune object — matches upstream ``cognee.prune.prune_data()`` call shape
# ---------------------------------------------------------------------------


class _Prune:
    """Namespace object that matches ``cognee.prune.prune_data()``."""

    async def prune_data(self) -> None:
        """Delete all ingested data (files + relational metadata).

        Mirrors ``cognee.prune.prune_data()`` in the upstream SDK.
        """
        await _handle().prune_data()

    async def prune_system(
        self,
        graph: bool = True,
        vector: bool = True,
        metadata: bool = False,
        cache: bool = True,
        *,
        raw: bool = False,
    ) -> Any:
        """Wipe knowledge-graph / vector / metadata stores.

        Mirrors ``cognee.prune.prune_system()`` in the upstream SDK.
        Keyword arguments map to the ``prune_graph``, ``prune_vector``,
        ``prune_metadata``, and ``prune_cache`` opts keys.

        By default returns the result dict with **snake_case** keys:
        ``data_pruned``, ``graph_pruned``, ``vector_pruned``,
        ``metadata_pruned``, ``cache_pruned``.

        Pass ``raw=True`` to receive the native camelCase dict.
        """
        opts = {
            "prune_graph": graph,
            "prune_vector": vector,
            "prune_metadata": metadata,
            "prune_cache": cache,
        }
        result = await _handle().prune_system(opts)
        return result if raw else _rekey(result)


#: Namespace providing ``prune.prune_data()`` and ``prune.prune_system()``.
#: Usage: ``await cognee.prune.prune_data()``
prune = _Prune()
