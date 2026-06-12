"""Tests for PyCognee.sessions.* and PyCognee.get_or_create_default_user (T8).

Environment requirements:
- ``MOCK_EMBEDDING=true`` — avoids downloading an ONNX model; required for
  all tests that call warm() (i.e. anything that initialises services).
- A non-empty ``LLM_API_KEY`` (or ``OPENAI_TOKEN``) — required for any test
  that initialises services. A dummy value like ``"sk-test"`` is sufficient
  for tests that do not actually call the LLM.

Tests skip gracefully when the required env vars are absent.
"""

import os
import pytest
import cognee_pipeline as cp


# ---------------------------------------------------------------------------
# Env-var guards
# ---------------------------------------------------------------------------

def _base_vars_present() -> bool:
    """True when the minimal env vars needed for service initialisation are set."""
    llm_key = os.environ.get("LLM_API_KEY") or os.environ.get("OPENAI_TOKEN", "")
    mock_emb = os.environ.get("MOCK_EMBEDDING", "")
    return bool(llm_key) and bool(mock_emb)


SKIP_IF_NO_BASE = pytest.mark.skipif(
    not _base_vars_present(),
    reason="session ops tests require MOCK_EMBEDDING=true and a non-empty LLM_API_KEY / OPENAI_TOKEN",
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


# ---------------------------------------------------------------------------
# cognee.sessions.get()
# ---------------------------------------------------------------------------

@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_get_session_empty(tmp_path):
    """get() returns an empty list for a nonexistent session."""
    c = await _make_cognee(tmp_path)
    result = await c.sessions.get("nonexistent-session")
    assert isinstance(result, list)
    assert len(result) == 0


@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_get_session_with_opts(tmp_path):
    """get() with lastN opt doesn't raise and returns a list."""
    c = await _make_cognee(tmp_path)
    result = await c.sessions.get("any-session", {"lastN": 5})
    assert isinstance(result, list)


# ---------------------------------------------------------------------------
# cognee.sessions.get_graph_context() / set_graph_context()
# ---------------------------------------------------------------------------

@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_get_graph_context_none(tmp_path):
    """get_graph_context() returns None for a new/nonexistent session."""
    c = await _make_cognee(tmp_path)
    result = await c.sessions.get_graph_context("nonexistent-session")
    assert result is None


@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_set_and_get_graph_context(tmp_path):
    """set_graph_context() then get_graph_context() returns the stored value."""
    c = await _make_cognee(tmp_path)
    session_id = "test-session-ctx"
    await c.sessions.set_graph_context(session_id, "some context string")
    result = await c.sessions.get_graph_context(session_id)
    assert result == "some context string"


@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_set_graph_context_returns_none(tmp_path):
    """set_graph_context() returns None (void op)."""
    c = await _make_cognee(tmp_path)
    result = await c.sessions.set_graph_context("session-void", "ctx")
    assert result is None


# ---------------------------------------------------------------------------
# cognee.get_or_create_default_user()
# ---------------------------------------------------------------------------

@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_get_or_create_default_user(tmp_path):
    """get_or_create_default_user() returns a dict with an 'id' field."""
    c = await _make_cognee(tmp_path)
    user = await c.get_or_create_default_user()
    assert isinstance(user, dict)
    assert "id" in user


@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_get_or_create_default_user_idempotent(tmp_path):
    """Calling get_or_create_default_user() twice returns the same id."""
    c = await _make_cognee(tmp_path)
    user1 = await c.get_or_create_default_user()
    user2 = await c.get_or_create_default_user()
    assert user1["id"] == user2["id"]
