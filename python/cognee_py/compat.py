"""Drop-in compatibility layer mirroring the upstream ``cognee`` Python SDK.

Import this module to use module-level functions that match the upstream API
shape so that basic ``cognee`` scripts run unmodified (or with just an import
alias change):

.. code-block:: python

    import cognee_py.compat as cognee

    await cognee.add("Hello world")
    await cognee.cognify()
    results = await cognee.search("What is this about?",
                                  query_type=cognee.SearchType.CHUNKS)
    await cognee.prune.prune_data()

The compat layer maintains a lazily-initialised, process-global
:class:`~cognee_py.Cognee` handle that is shared across all
module-level calls, mirroring the implicit global state of the upstream SDK.
The handle-based API (:class:`~cognee_py.Cognee`) remains fully
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

Unsupported types raise :exc:`~cognee_py.CogneeValidationError`.

SearchType
----------
:data:`SearchType` is re-exported here so ``cognee.SearchType.CHUNKS`` works
after ``import cognee_py.compat as cognee``.

Result-key casing
-----------------
The native ``_native`` module returns result dicts with **camelCase** keys
(e.g. ``addedCount``, ``priorPipelineRunId``) for cross-binding uniformity
with the C and JavaScript APIs.  This compat layer re-keys all top-level result
dict keys to **snake_case** (e.g. ``added_count``, ``prior_pipeline_run_id``)
so that Python callers receive idiomatic names.

To access the raw camelCase dict, pass ``raw=True`` to the compat functions
that support it, or call the corresponding method directly on a
:class:`~cognee_py.Cognee` instance.

Notes on unsupported upstream types
------------------------------------
The upstream ``cognee`` SDK also defines ``SearchType.AGENTIC_COMPLETION``.
It is **not yet supported** by the Rust core and therefore absent from
:class:`SearchType`.  Passing the string value ``"AGENTIC_COMPLETION"`` raises
:exc:`~cognee_py.CogneeValidationError` with a descriptive message.
"""

from __future__ import annotations

import os
import pathlib
import re
from typing import Any, Optional, Union

from . import Cognee, CogneeValidationError, SearchType, graph_spec_to_json_schema

__all__ = [
    "add",
    "cognify",
    "add_and_cognify",
    "search",
    "prune",
    "SearchType",
    "graph_spec_to_json_schema",
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


def _default_dataset_name() -> str:
    """The handle's ``default_dataset_name`` setting
    (``COGNEE_DEFAULT_DATASET_NAME``, default ``"main_dataset"``). A blank
    value falls back to ``"main_dataset"``, as the Rust
    ``Settings::effective_default_dataset_name`` the CLI uses does."""
    name = _handle().config.get().get("default_dataset_name")
    return name if isinstance(name, str) and name.strip() else "main_dataset"


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
# Custom graph models: ``graph_model`` → the ``graph_schema`` JSON Schema
# ---------------------------------------------------------------------------


def _is_default_graph_model(model: type) -> bool:
    """Whether *model* is upstream's default ``KnowledgeGraph``.

    Upstream ``cognify(graph_model=KnowledgeGraph)`` is the default pipeline,
    not a custom model, so passing it explicitly must not turn into a runtime
    schema.
    """
    return (
        getattr(model, "__name__", None) == "KnowledgeGraph"
        and getattr(model, "__module__", None) == "cognee.shared.data_models"
    )


def _basemodel_json_schema(model: type) -> dict:
    """``model.model_json_schema()`` without the default per-field titles.

    The same generator upstream's ``graph_model_to_graph_schema`` uses, for a
    plain ``pydantic.BaseModel`` (which that function passes through
    unchanged). Pydantic is the caller's dependency, not this package's, so it
    is imported only here.
    """
    try:
        from pydantic._internal._core_utils import is_core_schema
        from pydantic.json_schema import GenerateJsonSchema
    except ImportError:  # pragma: no cover - a pydantic without these helpers
        return model.model_json_schema()

    class _WithoutDefaultFieldTitles(GenerateJsonSchema):
        def field_title_should_be_set(self, schema):  # type: ignore[override]
            should_set = super().field_title_should_be_set(schema)
            if should_set and is_core_schema(schema):
                return False
            return should_set

    return model.model_json_schema(schema_generator=_WithoutDefaultFieldTitles)


def _graph_model_to_schema(graph_model: Any) -> Any:
    """Convert a ``graph_model`` argument into the native ``graph_schema``.

    Accepts what upstream ``cognify(graph_model=...)`` takes — a Pydantic
    model class — plus the JSON Schema itself, as a ``dict`` or a JSON
    string (what upstream's HTTP route accepts). ``None`` and upstream's
    ``KnowledgeGraph`` both mean the default pipeline.

    A class is converted the way upstream's ``graph_model_to_graph_schema``
    does it: through that very function when the upstream ``cognee`` package
    is importable (so a ``DataPoint`` subclass drops the infrastructure fields
    ``DataPoint`` declares), else with ``model_json_schema()`` minus the
    default field titles. The schema is validated natively; an invalid one
    raises :exc:`~cognee_py.CogneeValidationError`.
    """
    if graph_model is None or isinstance(graph_model, (dict, str)):
        return graph_model
    if isinstance(graph_model, type) and hasattr(graph_model, "model_json_schema"):
        if _is_default_graph_model(graph_model):
            return None
        try:
            from cognee.shared.graph_model_utils import graph_model_to_graph_schema
        except ImportError:
            # Upstream ``cognee`` is absent, or ``cognee`` is this repo's
            # drop-in shim, which has no ``shared`` package.
            return _basemodel_json_schema(graph_model)
        return graph_model_to_graph_schema(graph_model)
    raise CogneeValidationError(
        "graph_model must be a Pydantic model class, a JSON Schema dict or a "
        f"JSON string, got {type(graph_model).__name__}"
    )


def _upstream_datapoint() -> Optional[type]:
    """Upstream ``cognee``'s ``DataPoint`` class, or ``None`` when absent.

    ``None`` when upstream ``cognee`` is not importable, including when
    ``cognee`` is this repo's drop-in shim, which defines no ``DataPoint``.
    """
    try:
        from cognee.infrastructure.engine import DataPoint
    except ImportError:
        return None
    return DataPoint


def _check_chunk_attachment_model(graph_model: Any, chunk_attachment: Any) -> None:
    """Raise as upstream ``cognify`` does for ``chunk_attachment`` with a
    model class that is not a ``DataPoint`` subclass.

    Upstream (``cognify.py``) requires ``issubclass(graph_model, DataPoint)``
    whenever ``chunk_attachment`` is set. That check is only possible when
    upstream's ``DataPoint`` is importable, so it applies then and only to a
    class: a JSON Schema dict or string, like upstream's HTTP route, always
    describes a ``DataPoint``-like model, and without upstream there is no
    ``DataPoint`` to subclass, so a class is accepted as its schema. A missing
    model (or the default ``KnowledgeGraph``) is rejected natively, because
    ``chunk_attachment`` without a graph schema fails validation.
    """
    # Upstream rejects an unusable value before it looks at the model, so
    # leave that error to the native validation.
    if chunk_attachment not in ("direct", "all") or not isinstance(graph_model, type):
        return
    if _is_default_graph_model(graph_model):
        return
    data_point = _upstream_datapoint()
    if data_point is not None and not issubclass(graph_model, data_point):
        raise CogneeValidationError(
            "chunk_attachment requires a custom DataPoint graph_model; "
            f"{graph_model.__name__!r} is not a DataPoint subclass."
        )


def _cognify_opts(kwargs: dict) -> Optional[dict]:
    """The native cognify ``opts`` for compat ``**kwargs``.

    ``graph_model`` is converted by :func:`_graph_model_to_schema` and sent as
    ``graph_schema``; ``chunk_attachment`` and every other keyword pass
    through (snake_case keys are normalised natively); a ``chunk_attachment``
    with a non-``DataPoint`` class raises as upstream does, when upstream's
    ``DataPoint`` is importable (see :func:`_check_chunk_attachment_model`).
    Unknown keywords are
    logged as a warning natively — upstream forwards them into the LLM call
    rather than raising — and wrongly-typed or invalid values raise.
    """
    opts = dict(kwargs)
    if "graph_model" in opts:
        if opts.get("graph_schema") is not None:
            raise CogneeValidationError("pass either graph_model or graph_schema, not both")
        graph_model = opts.pop("graph_model")
        _check_chunk_attachment_model(graph_model, opts.get("chunk_attachment"))
        opts["graph_schema"] = _graph_model_to_schema(graph_model)
    return opts or None


# ---------------------------------------------------------------------------
# Module-level SDK functions
# ---------------------------------------------------------------------------


async def add(
    data: Any,
    dataset_name: Optional[str] = None,
    *,
    raw: bool = False,
    **kwargs,
) -> Any:
    """Ingest one or more data items into *dataset_name*.

    When *dataset_name* is ``None`` the ``default_dataset_name`` setting
    (``COGNEE_DEFAULT_DATASET_NAME``, default ``"main_dataset"``) is used.

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
    if dataset_name is None:
        dataset_name = _default_dataset_name()
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
    dataset (the ``default_dataset_name`` setting, ``"main_dataset"`` unless
    configured) is processed.

    By default returns the result dict with **snake_case** keys: ``chunks``,
    ``entities``, ``edges``, ``summaries``, ``embeddings``,
    ``already_completed``, ``prior_pipeline_run_id``.

    Pass ``raw=True`` to receive the native camelCase dict
    (``alreadyCompleted``, ``priorPipelineRunId``).

    ``graph_model`` takes a custom Pydantic graph model class (or its JSON
    Schema as a dict or JSON string) and ``chunk_attachment`` takes
    ``"direct"`` or ``"all"``, as upstream; see :func:`_graph_model_to_schema`.
    A model written in the spec DSL can be passed as
    ``graph_model=graph_spec_to_json_schema(spec)`` — the upstream spelling,
    with ``cognee.low_level.graph_spec_to_json_schema`` — or directly as
    ``graph_spec=spec``. An invalid value raises
    :exc:`~cognee_py.CogneeValidationError`.
    """
    dataset_name = datasets if isinstance(datasets, str) else _default_dataset_name()
    opts = _cognify_opts(kwargs)
    result = await _handle().cognify(dataset_name, opts)
    return result if raw else _rekey(result)


async def add_and_cognify(
    data: Any,
    dataset_name: Optional[str] = None,
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

    Accepts the same ``graph_model`` / ``chunk_attachment`` keywords as
    :func:`cognify`.
    """
    descriptors = _coerce_inputs(data)
    inputs = descriptors if len(descriptors) != 1 else descriptors[0]
    opts = _cognify_opts(kwargs)
    if dataset_name is None:
        dataset_name = _default_dataset_name()
    result = await _handle().add_and_cognify(inputs, dataset_name, opts)
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
    query_type: Union[SearchType, str] = SearchType.HYBRID_COMPLETION,
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
