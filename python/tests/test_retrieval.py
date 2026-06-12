"""Tests for PyCognee.search and .recall (T4 — hoist + Python search/recall).

Environment requirements:
- ``MOCK_EMBEDDING=true`` — avoids downloading an ONNX model; required for
  all tests that call warm() / add() (i.e. anything that initialises services).
- A non-empty ``LLM_API_KEY`` (or ``OPENAI_TOKEN``) — required for any test
  that reaches add(). A dummy value like ``"sk-test"`` is sufficient for
  add-only tests because ``OpenAIAdapter::new`` does no network I/O at
  construction time.
- ``OPENAI_URL`` — required for tests that actually invoke the LLM (search,
  recall with real knowledge graph).

Tests skip gracefully when the required env vars are absent.
"""

import os
import pytest
import cognee_pipeline as cp


# ---------------------------------------------------------------------------
# Env-var guards
# ---------------------------------------------------------------------------

def _add_vars_present() -> bool:
    """True when the minimal env vars needed for add() are set."""
    llm_key = os.environ.get("LLM_API_KEY") or os.environ.get("OPENAI_TOKEN", "")
    mock_emb = os.environ.get("MOCK_EMBEDDING", "")
    return bool(llm_key) and bool(mock_emb)


def _search_vars_present() -> bool:
    """True when env vars needed for search/recall (real LLM) are set."""
    return _add_vars_present() and bool(os.environ.get("OPENAI_URL", ""))


SKIP_IF_NO_ADD = pytest.mark.skipif(
    not _add_vars_present(),
    reason="tests require MOCK_EMBEDDING=true and a non-empty LLM_API_KEY / OPENAI_TOKEN",
)

SKIP_IF_NO_SEARCH = pytest.mark.skipif(
    not _search_vars_present(),
    reason=(
        "search/recall tests require MOCK_EMBEDDING=true, LLM_API_KEY, and OPENAI_URL"
    ),
)


# ---------------------------------------------------------------------------
# Helper — build a fresh, warm Cognee handle in a tmp dir.
# ---------------------------------------------------------------------------

async def _make_cognee(tmp_path) -> cp.Cognee:
    """Create and warm a Cognee handle backed by an isolated tmp database."""
    db = str(tmp_path / "cognee.db")
    data_dir = str(tmp_path / "data")
    system_dir = str(tmp_path / "system")
    settings = (
        f'{{"relational_db_url": "sqlite://{db}?mode=rwc",'
        f' "data_root_directory": "{data_dir}",'
        f' "system_root_directory": "{system_dir}"}}'
    )
    c = cp.Cognee(settings)
    await c.warm()
    return c


async def _cognee_with_data(tmp_path) -> cp.Cognee:
    """Warm handle with one text item added (but NOT cognified)."""
    c = await _make_cognee(tmp_path)
    await c.add({"type": "text", "text": "Alice studies artificial intelligence."}, "ds")
    return c


# ---------------------------------------------------------------------------
# SearchType constants — pure Python, no Rust call needed.
# ---------------------------------------------------------------------------

def test_search_type_importable():
    """SearchType must be importable from cognee_pipeline."""
    assert hasattr(cp, "SearchType")
    st = cp.SearchType
    assert st.GRAPH_COMPLETION == "GRAPH_COMPLETION"
    assert st.RAG_COMPLETION == "RAG_COMPLETION"
    assert st.CHUNKS == "CHUNKS"
    assert st.SUMMARIES == "SUMMARIES"
    assert st.TEMPORAL == "TEMPORAL"
    assert st.CYPHER == "CYPHER"
    assert st.NATURAL_LANGUAGE == "NATURAL_LANGUAGE"
    assert st.GRAPH_COMPLETION_COT == "GRAPH_COMPLETION_COT"
    assert st.GRAPH_COMPLETION_CONTEXT_EXTENSION == "GRAPH_COMPLETION_CONTEXT_EXTENSION"
    assert st.GRAPH_SUMMARY_COMPLETION == "GRAPH_SUMMARY_COMPLETION"
    assert st.TRIPLET_COMPLETION == "TRIPLET_COMPLETION"
    assert st.FEELING_LUCKY == "FEELING_LUCKY"
    assert st.FEEDBACK == "FEEDBACK"
    assert st.CODING_RULES == "CODING_RULES"
    assert st.CHUNKS_LEXICAL == "CHUNKS_LEXICAL"


def test_search_type_in_all():
    """SearchType must be listed in __all__."""
    assert "SearchType" in cp.__all__


# ---------------------------------------------------------------------------
# search() — validation tests (no LLM required, just service warm-up).
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_search_unknown_type_raises(tmp_path):
    """search() with an unknown searchType raises CogneeValidationError."""
    c = await _make_cognee(tmp_path)
    with pytest.raises(cp.CogneeValidationError):
        await c.search("anything", {"search_type": "NOT_A_VALID_TYPE"})


@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_search_unknown_type_camelcase_raises(tmp_path):
    """search() with an unknown searchType (camelCase key) raises CogneeValidationError."""
    c = await _make_cognee(tmp_path)
    with pytest.raises(cp.CogneeValidationError):
        await c.search("anything", {"searchType": "TOTALLY_WRONG"})


# ---------------------------------------------------------------------------
# search() — integration tests (require real LLM via OPENAI_URL).
# ---------------------------------------------------------------------------

@SKIP_IF_NO_SEARCH
@pytest.mark.asyncio
async def test_search_default_type_returns_list_or_dict(tmp_path):
    """search() with no opts returns a list or dict without raising."""
    c = await _cognee_with_data(tmp_path)
    result = await c.search("What does Alice study?")
    assert isinstance(result, (list, dict))


@SKIP_IF_NO_SEARCH
@pytest.mark.asyncio
async def test_search_chunks_snake_case_opts(tmp_path):
    """search() accepts snake_case opts and returns a list for CHUNKS type."""
    c = await _cognee_with_data(tmp_path)
    result = await c.search(
        "What does Alice study?",
        {"search_type": "CHUNKS", "top_k": 5},
    )
    assert isinstance(result, list)


@SKIP_IF_NO_SEARCH
@pytest.mark.asyncio
async def test_search_chunks_camelcase_opts(tmp_path):
    """search() accepts camelCase opts equivalently."""
    c = await _cognee_with_data(tmp_path)
    result = await c.search(
        "What does Alice study?",
        {"searchType": "CHUNKS", "topK": 5},
    )
    assert isinstance(result, list)


@SKIP_IF_NO_SEARCH
@pytest.mark.asyncio
async def test_search_symbolic_search_type(tmp_path):
    """search() works when using SearchType constants."""
    c = await _cognee_with_data(tmp_path)
    result = await c.search(
        "What does Alice study?",
        {"search_type": cp.SearchType.CHUNKS},
    )
    assert isinstance(result, list)


@SKIP_IF_NO_SEARCH
@pytest.mark.asyncio
async def test_search_no_opts_returns_result(tmp_path):
    """search() with opts=None (explicit) returns without raising."""
    c = await _cognee_with_data(tmp_path)
    result = await c.search("query", None)
    assert isinstance(result, (list, dict))


# ---------------------------------------------------------------------------
# recall() — integration tests (require real LLM via OPENAI_URL).
# ---------------------------------------------------------------------------

@SKIP_IF_NO_SEARCH
@pytest.mark.asyncio
async def test_recall_basic_result_shape(tmp_path):
    """recall() returns a dict with the expected camelCase keys."""
    c = await _cognee_with_data(tmp_path)
    result = await c.recall("What does Alice study?")
    assert isinstance(result, dict)
    assert "items" in result
    assert "searchTypeUsed" in result
    assert "autoRouted" in result
    assert "searchResponse" in result


@SKIP_IF_NO_SEARCH
@pytest.mark.asyncio
async def test_recall_auto_routed_is_bool(tmp_path):
    """recall() autoRouted field must be a bool."""
    c = await _cognee_with_data(tmp_path)
    result = await c.recall("What does Alice study?")
    assert isinstance(result["autoRouted"], bool)


@SKIP_IF_NO_SEARCH
@pytest.mark.asyncio
async def test_recall_items_is_list(tmp_path):
    """recall() items field must be a list."""
    c = await _cognee_with_data(tmp_path)
    result = await c.recall("What does Alice study?")
    assert isinstance(result["items"], list)


@SKIP_IF_NO_SEARCH
@pytest.mark.asyncio
async def test_recall_snake_case_opts(tmp_path):
    """recall() accepts snake_case opts (top_k, auto_route)."""
    c = await _cognee_with_data(tmp_path)
    result = await c.recall(
        "What does Alice study?",
        {"top_k": 5, "auto_route": False},
    )
    assert isinstance(result, dict)
    assert "items" in result


@SKIP_IF_NO_SEARCH
@pytest.mark.asyncio
async def test_recall_camelcase_opts(tmp_path):
    """recall() accepts camelCase opts equivalently."""
    c = await _cognee_with_data(tmp_path)
    result = await c.recall(
        "What does Alice study?",
        {"topK": 5, "autoRoute": False},
    )
    assert isinstance(result, dict)
    assert "items" in result


@SKIP_IF_NO_SEARCH
@pytest.mark.asyncio
async def test_recall_invalid_search_type_raises(tmp_path):
    """recall() with unknown search_type raises CogneeValidationError."""
    c = await _cognee_with_data(tmp_path)
    with pytest.raises(cp.CogneeValidationError):
        await c.recall("query", {"search_type": "BAD_TYPE"})
