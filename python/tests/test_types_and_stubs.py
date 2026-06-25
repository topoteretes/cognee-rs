"""Tests for ID-2 / DOC-1: type stubs, TypedDict inputs/options/results,
and the snake_case result-key convention in the compat layer.

These tests do NOT require network access or an embedding model — they
exercise pure-Python structural properties of the new types and the
compat-layer re-keying logic.
"""

from __future__ import annotations

import re


# ---------------------------------------------------------------------------
# types.py — structural smoke tests (no imports of compiled _native needed)
# ---------------------------------------------------------------------------


def test_types_module_importable():
    """cognee_py.types must import without errors."""
    import cognee_py.types as t  # noqa: F401


def test_all_input_typeddicts_present():
    """TextInput, FileInput, UrlInput, BinaryInput, DataInput must all be exported."""
    from cognee_py import types as t
    for name in ("TextInput", "FileInput", "UrlInput", "BinaryInput", "DataInput"):
        assert hasattr(t, name), f"missing: {name}"


def test_all_opts_typeddicts_present():
    """All Opts TypedDicts must be exported."""
    from cognee_py import types as t
    expected = [
        "AddOpts", "CognifyOpts", "SearchOpts", "RecallOpts", "RememberOpts",
        "MemifyOpts", "ImproveOpts", "ForgetTarget", "UpdateOpts",
        "PruneSystemOpts", "DeleteDataOpts", "VisualizeOpts",
    ]
    for name in expected:
        assert hasattr(t, name), f"missing: {name}"


def test_all_result_typeddicts_present():
    """All Result TypedDicts must be exported."""
    from cognee_py import types as t
    expected = [
        "AddResult", "CognifyResult", "SearchResult", "RecallResult",
        "ForgetResult", "UpdateResult", "PruneResult", "MemifyResult",
        "ImproveResult",
    ]
    for name in expected:
        assert hasattr(t, name), f"missing: {name}"


def test_text_input_has_correct_annotations():
    """TextInput must have 'type' (Literal['text']) and 'text' (str) annotations."""
    from cognee_py.types import TextInput
    import typing
    hints = typing.get_type_hints(TextInput)
    assert "type" in hints
    assert "text" in hints


def test_add_result_snake_case_fields():
    """AddResult TypedDict must use snake_case field names."""
    from cognee_py.types import AddResult
    import typing
    hints = typing.get_type_hints(AddResult)
    assert "dataset_name" in hints
    assert "added_count" in hints
    assert "deduplicated_count" in hints
    # Must NOT have camelCase names.
    assert "datasetName" not in hints
    assert "addedCount" not in hints
    assert "deduplicatedCount" not in hints


def test_cognify_result_snake_case_fields():
    """CognifyResult TypedDict must use snake_case field names."""
    from cognee_py.types import CognifyResult
    import typing
    hints = typing.get_type_hints(CognifyResult)
    assert "already_completed" in hints
    assert "prior_pipeline_run_id" in hints
    assert "alreadyCompleted" not in hints
    assert "priorPipelineRunId" not in hints


def test_prune_result_snake_case_fields():
    """PruneResult TypedDict must use snake_case field names."""
    from cognee_py.types import PruneResult
    import typing
    hints = typing.get_type_hints(PruneResult)
    for field in ("data_pruned", "graph_pruned", "vector_pruned",
                  "metadata_pruned", "cache_pruned"):
        assert field in hints, f"missing: {field}"
    for bad in ("dataPruned", "graphPruned", "vectorPruned"):
        assert bad not in hints, f"unexpected camelCase field: {bad}"


# ---------------------------------------------------------------------------
# compat._camel_to_snake — unit tests
# ---------------------------------------------------------------------------


def test_camel_to_snake_conversions():
    """_camel_to_snake must correctly convert well-known camelCase keys."""
    from cognee_py.compat import _camel_to_snake
    cases = {
        "addedCount": "added_count",
        "datasetName": "dataset_name",
        "deduplicatedCount": "deduplicated_count",
        "alreadyCompleted": "already_completed",
        "priorPipelineRunId": "prior_pipeline_run_id",
        "dataPruned": "data_pruned",
        "graphPruned": "graph_pruned",
        "tripletCount": "triplet_count",
        "searchTypeUsed": "search_type_used",
        "autoRouted": "auto_routed",
        "deleteResult": "delete_result",
        "deletedDataId": "deleted_data_id",
        "serviceUrl": "service_url",
    }
    for camel, expected in cases.items():
        got = _camel_to_snake(camel)
        assert got == expected, f"{camel!r} -> {got!r} (expected {expected!r})"


def test_camel_to_snake_already_snake():
    """Keys that are already snake_case must pass through unchanged."""
    from cognee_py.compat import _camel_to_snake
    for key in ("chunks", "entities", "edges", "summaries", "embeddings",
                "items", "target", "added", "deduplicated"):
        assert _camel_to_snake(key) == key, f"altered: {key!r}"


# ---------------------------------------------------------------------------
# compat._rekey — unit tests
# ---------------------------------------------------------------------------


def test_rekey_dict():
    """_rekey must convert a flat camelCase dict to snake_case."""
    from cognee_py.compat import _rekey
    result = _rekey({
        "datasetName": "ds",
        "addedCount": 2,
        "deduplicatedCount": 0,
        "added": [],
        "deduplicated": [],
    })
    assert result == {
        "dataset_name": "ds",
        "added_count": 2,
        "deduplicated_count": 0,
        "added": [],
        "deduplicated": [],
    }


def test_rekey_non_dict_passthrough():
    """_rekey must return non-dict values unchanged."""
    from cognee_py.compat import _rekey
    assert _rekey([1, 2, 3]) == [1, 2, 3]
    assert _rekey("hello") == "hello"
    assert _rekey(42) == 42
    assert _rekey(None) is None


def test_rekey_shallow_only():
    """_rekey re-keys only the top level; nested dicts are preserved as-is."""
    from cognee_py.compat import _rekey
    inner = {"nestedKey": "value", "anotherKey": 1}
    result = _rekey({"outerCamel": inner})
    assert "outer_camel" in result
    # Inner dict is NOT re-keyed.
    assert result["outer_camel"] is inner
    assert "nestedKey" in result["outer_camel"]


# ---------------------------------------------------------------------------
# pyi stubs — syntax check (parse them as Python AST)
# ---------------------------------------------------------------------------


def test_init_pyi_parseable():
    """__init__.pyi must be valid Python syntax."""
    import ast, pathlib
    stub_path = pathlib.Path(__file__).parent.parent / "cognee_py" / "__init__.pyi"
    assert stub_path.exists(), f"stub not found: {stub_path}"
    ast.parse(stub_path.read_text())


def test_native_pyi_parseable():
    """_native.pyi must be valid Python syntax."""
    import ast, pathlib
    stub_path = pathlib.Path(__file__).parent.parent / "cognee_py" / "_native.pyi"
    assert stub_path.exists(), f"stub not found: {stub_path}"
    ast.parse(stub_path.read_text())


# ---------------------------------------------------------------------------
# py.typed — marker file must exist and be non-empty (backed by real stubs)
# ---------------------------------------------------------------------------


def test_py_typed_exists():
    """py.typed marker must exist in the package directory."""
    import pathlib
    marker = pathlib.Path(__file__).parent.parent / "cognee_py" / "py.typed"
    assert marker.exists(), "py.typed marker file not found"


# ---------------------------------------------------------------------------
# compat layer signature — keyword-argument forms
# ---------------------------------------------------------------------------


def test_search_accepts_raw_kwarg():
    """compat.search must accept a 'raw' keyword argument without error."""
    import inspect
    from cognee_py.compat import search
    sig = inspect.signature(search)
    assert "raw" in sig.parameters, "'raw' kwarg not in search() signature"


def test_add_accepts_raw_kwarg():
    """compat.add must accept a 'raw' keyword argument without error."""
    import inspect
    from cognee_py.compat import add
    sig = inspect.signature(add)
    assert "raw" in sig.parameters, "'raw' kwarg not in add() signature"


def test_cognify_accepts_raw_kwarg():
    """compat.cognify must accept a 'raw' keyword argument without error."""
    import inspect
    from cognee_py.compat import cognify
    sig = inspect.signature(cognify)
    assert "raw" in sig.parameters, "'raw' kwarg not in cognify() signature"


def test_prune_system_accepts_raw_kwarg():
    """compat.prune.prune_system must accept a 'raw' keyword argument."""
    import inspect
    from cognee_py.compat import prune
    sig = inspect.signature(prune.prune_system)
    assert "raw" in sig.parameters, "'raw' kwarg not in prune_system() signature"
