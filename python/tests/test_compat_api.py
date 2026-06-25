"""Tests for the upstream-compatible cognee SDK compat layer (ID-1).

Exercises ``cognee_py.compat`` as a drop-in for upstream ``cognee``
module-level scripts.

Environment requirements (same gating as the other integration tests):
- ``MOCK_EMBEDDING=true``   — avoids downloading an ONNX model.
- A non-empty ``LLM_API_KEY`` / ``OPENAI_TOKEN`` — needed for add().
  A dummy value like ``"sk-test"`` is sufficient; no network I/O at
  construction time.
- ``OPENAI_URL``            — only for tests that exercise the LLM
  (search, cognify with real graph extraction).

Tests skip gracefully when the required env vars are absent.
"""

import os
import pytest
import cognee_py as cp
import cognee_py.compat as cognee


# ---------------------------------------------------------------------------
# Env-var guards (mirrors the pattern in test_sdk_handle.py)
# ---------------------------------------------------------------------------


def _add_vars_present() -> bool:
    llm_key = os.environ.get("LLM_API_KEY") or os.environ.get("OPENAI_TOKEN", "")
    mock_emb = os.environ.get("MOCK_EMBEDDING", "")
    return bool(llm_key) and mock_emb.strip().lower() in ("1", "true", "yes")


def _search_vars_present() -> bool:
    return _add_vars_present() and bool(os.environ.get("OPENAI_URL", ""))


SKIP_IF_NO_ADD = pytest.mark.skipif(
    not _add_vars_present(),
    reason="requires MOCK_EMBEDDING=true and a non-empty LLM_API_KEY / OPENAI_TOKEN",
)

SKIP_IF_NO_SEARCH = pytest.mark.skipif(
    not _search_vars_present(),
    reason="requires MOCK_EMBEDDING=true, LLM_API_KEY, and OPENAI_URL",
)


# ---------------------------------------------------------------------------
# Helpers — isolated handle so tests do not pollute the repository root.
# ---------------------------------------------------------------------------


def _patch_default_handle(tmp_path):
    """Replace the compat module's global handle with an isolated one."""
    db = str(tmp_path / "cognee.db")
    data_dir = str(tmp_path / "data")
    system_dir = str(tmp_path / "system")
    settings = (
        f'{{"relational_db_url": "sqlite://{db}?mode=rwc",'
        f' "data_root_directory": "{data_dir}",'
        f' "system_root_directory": "{system_dir}"}}'
    )
    cognee.reset_default_handle()
    # Directly set the module-level _default_handle to the isolated instance.
    import cognee_py.compat as _compat
    _compat._default_handle = cp.Cognee(settings)


# ---------------------------------------------------------------------------
# Step 4 — SearchType is a str-Enum (no network I/O needed)
# ---------------------------------------------------------------------------


def test_search_type_is_str_enum():
    """SearchType must be a (str, Enum) subclass."""
    from enum import Enum
    assert issubclass(cp.SearchType, str)
    assert issubclass(cp.SearchType, Enum)


def test_search_type_str_equality():
    """SearchType values must compare equal to their string forms."""
    assert cp.SearchType.GRAPH_COMPLETION == "GRAPH_COMPLETION"
    assert cp.SearchType.CHUNKS == "CHUNKS"
    assert cp.SearchType.RAG_COMPLETION == "RAG_COMPLETION"
    assert cp.SearchType.SUMMARIES == "SUMMARIES"


def test_search_type_usable_in_f_string():
    """SearchType members must render as their value strings (str inheritance)."""
    assert f"{cp.SearchType.CHUNKS}" == "CHUNKS"
    assert str(cp.SearchType.GRAPH_COMPLETION) == "GRAPH_COMPLETION"


def test_search_type_reexported_from_compat():
    """cognee_py.compat.SearchType must be the same object."""
    assert cognee.SearchType is cp.SearchType


def test_all_15_types_present():
    """All 15 Rust-backed search types must be present in SearchType."""
    expected = {
        "GRAPH_COMPLETION", "GRAPH_COMPLETION_COT", "GRAPH_COMPLETION_CONTEXT_EXTENSION",
        "GRAPH_SUMMARY_COMPLETION", "TRIPLET_COMPLETION", "RAG_COMPLETION",
        "CHUNKS", "SUMMARIES", "TEMPORAL", "CYPHER", "NATURAL_LANGUAGE",
        "FEELING_LUCKY", "FEEDBACK", "CODING_RULES", "CHUNKS_LEXICAL",
    }
    present = {m.name for m in cp.SearchType}
    assert expected <= present, f"missing: {expected - present}"


def test_unsupported_types_absent():
    """AGENTIC_COMPLETION and GRAPH_COMPLETION_DECOMPOSITION must NOT be present."""
    names = {m.name for m in cp.SearchType}
    assert "AGENTIC_COMPLETION" not in names
    assert "GRAPH_COMPLETION_DECOMPOSITION" not in names


# ---------------------------------------------------------------------------
# Step 3 — _coerce_inputs helper (no network I/O needed)
# ---------------------------------------------------------------------------


def test_coerce_str_to_text():
    """Plain strings must be coerced to {type: text} descriptors."""
    from cognee_py.compat import _coerce_inputs
    result = _coerce_inputs("Hello world")
    assert result == [{"type": "text", "text": "Hello world"}]


def test_coerce_url_string():
    """URL strings must be coerced to {type: url} descriptors."""
    from cognee_py.compat import _coerce_inputs
    result = _coerce_inputs("https://example.com")
    assert result == [{"type": "url", "url": "https://example.com"}]


def test_coerce_path():
    """pathlib.Path must be coerced to {type: file} descriptors."""
    import pathlib
    from cognee_py.compat import _coerce_inputs
    p = pathlib.Path("/tmp/test.txt")
    result = _coerce_inputs(p)
    assert result == [{"type": "file", "path": "/tmp/test.txt"}]


def test_coerce_bytes():
    """bytes must be coerced to {type: binary} descriptors."""
    from cognee_py.compat import _coerce_inputs
    result = _coerce_inputs(b"raw bytes")
    assert result[0]["type"] == "binary"
    assert result[0]["bytes"] == b"raw bytes"


def test_coerce_dict_passthrough():
    """dicts with a 'type' key must pass through unchanged."""
    from cognee_py.compat import _coerce_inputs
    d = {"type": "text", "text": "already a descriptor"}
    result = _coerce_inputs(d)
    assert result == [d]


def test_coerce_dict_missing_type_raises():
    """dicts without a 'type' key must raise CogneeValidationError."""
    from cognee_py.compat import _coerce_inputs
    with pytest.raises(cp.CogneeValidationError):
        _coerce_inputs({"no_type": "here"})


def test_coerce_list():
    """Lists are fan-out — each element is coerced independently."""
    from cognee_py.compat import _coerce_inputs
    result = _coerce_inputs(["hello", "https://example.com"])
    assert result[0] == {"type": "text", "text": "hello"}
    assert result[1] == {"type": "url", "url": "https://example.com"}


def test_coerce_unsupported_type_raises():
    """Unknown input types must raise CogneeValidationError."""
    from cognee_py.compat import _coerce_inputs
    with pytest.raises(cp.CogneeValidationError):
        _coerce_inputs(12345)


# ---------------------------------------------------------------------------
# Step 2 — default handle management (no network I/O needed)
# ---------------------------------------------------------------------------


def test_default_handle_lazily_created():
    """The default handle must be None before the first module-level call."""
    import cognee_py.compat as _compat
    _compat.reset_default_handle()
    assert _compat._default_handle is None
    # Calling _handle() must create it.
    h = _compat._handle()
    assert h is not None
    assert isinstance(h, cp.Cognee)
    _compat.reset_default_handle()


def test_default_handle_singleton():
    """_handle() must return the same object on repeated calls."""
    import cognee_py.compat as _compat
    _compat.reset_default_handle()
    h1 = _compat._handle()
    h2 = _compat._handle()
    assert h1 is h2
    _compat.reset_default_handle()


# ---------------------------------------------------------------------------
# Integration tests — require MOCK_EMBEDDING + LLM_API_KEY
# ---------------------------------------------------------------------------


@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_compat_add_plain_string(tmp_path):
    """await cognee.add('text') must succeed (upstream-style call)."""
    _patch_default_handle(tmp_path)
    result = await cognee.add("The quick brown fox.")
    assert isinstance(result, dict)
    assert "added" in result


@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_compat_add_list_of_strings(tmp_path):
    """await cognee.add(['a', 'b']) must ingest all items.

    The compat layer returns snake_case keys by default.
    """
    _patch_default_handle(tmp_path)
    result = await cognee.add(["First item.", "Second item."])
    assert isinstance(result, dict)
    # Compat layer re-keys camelCase → snake_case by default.
    assert result.get("added_count", 0) >= 1


@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_compat_add_pathlib_path(tmp_path):
    """await cognee.add(Path('file.txt')) must ingest the file."""
    import pathlib
    _patch_default_handle(tmp_path)
    f = tmp_path / "sample.txt"
    f.write_text("Sample file content for testing.")
    result = await cognee.add(pathlib.Path(str(f)))
    assert isinstance(result, dict)


@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_compat_add_with_dataset_name(tmp_path):
    """await cognee.add('text', 'my_dataset') must use the provided dataset name.

    The compat layer returns snake_case keys by default.
    """
    _patch_default_handle(tmp_path)
    result = await cognee.add("Dataset-specific content.", dataset_name="my_dataset")
    assert isinstance(result, dict)
    # Compat layer re-keys camelCase → snake_case by default.
    assert result.get("dataset_name") == "my_dataset"


@SKIP_IF_NO_SEARCH
@pytest.mark.asyncio
async def test_compat_upstream_style_script(tmp_path):
    """Run a minimal upstream-style cognee script end-to-end."""
    _patch_default_handle(tmp_path)
    # Upstream pattern: add → cognify → search
    await cognee.add("Alice studies artificial intelligence.")
    await cognee.cognify()
    results = await cognee.search(
        "What does Alice study?",
        query_type=cp.SearchType.GRAPH_COMPLETION,
    )
    assert isinstance(results, (list, dict))


@SKIP_IF_NO_SEARCH
@pytest.mark.asyncio
async def test_compat_search_default_type(tmp_path):
    """search() with no query_type defaults to GRAPH_COMPLETION."""
    _patch_default_handle(tmp_path)
    await cognee.add("Bob likes Python programming.")
    await cognee.cognify()
    results = await cognee.search("What does Bob like?")
    assert isinstance(results, (list, dict))


@SKIP_IF_NO_SEARCH
@pytest.mark.asyncio
async def test_compat_search_chunks_type(tmp_path):
    """search() with query_type=SearchType.CHUNKS returns a list."""
    _patch_default_handle(tmp_path)
    await cognee.add("Carol works on machine learning.")
    await cognee.cognify()
    results = await cognee.search(
        "What does Carol do?",
        query_type=cp.SearchType.CHUNKS,
        top_k=5,
    )
    assert isinstance(results, list)


@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_compat_prune_data(tmp_path):
    """await cognee.prune.prune_data() must succeed without raising."""
    _patch_default_handle(tmp_path)
    await cognee.add("Temporary content to prune.")
    result = await cognee.prune.prune_data()
    assert result is None


@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_compat_prune_system(tmp_path):
    """await cognee.prune.prune_system() must return a result dict.

    The compat layer returns snake_case keys by default.
    """
    _patch_default_handle(tmp_path)
    await cognee.add("Content for system prune test.")
    result = await cognee.prune.prune_system()
    assert isinstance(result, dict)
    # Compat layer re-keys camelCase → snake_case by default.
    assert "data_pruned" in result or "graph_pruned" in result


# ---------------------------------------------------------------------------
# Step 5 — optional cognee alias package (import-only, no I/O)
# ---------------------------------------------------------------------------


def test_cognee_alias_package_importable():
    """The optional ``cognee`` top-level package must be importable."""
    import cognee as _cognee  # noqa: F401 — import is the test
    assert hasattr(_cognee, "add")
    assert hasattr(_cognee, "cognify")
    assert hasattr(_cognee, "search")
    assert hasattr(_cognee, "prune")
    assert hasattr(_cognee, "SearchType")


def test_cognee_alias_same_functions():
    """cognee.add / cognee.search must be the same objects as compat equivalents."""
    import cognee as _cognee
    import cognee_py.compat as _compat
    assert _cognee.add is _compat.add
    assert _cognee.search is _compat.search
    assert _cognee.SearchType is cp.SearchType
