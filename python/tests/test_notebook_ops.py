"""Tests for PyCognee.notebooks.* (T8).

Environment requirements:
- ``MOCK_EMBEDDING=true`` — avoids downloading an ONNX model; required for
  all tests that call warm() (i.e. anything that initialises services).
- A non-empty ``LLM_API_KEY`` (or ``OPENAI_TOKEN``) — required for any test
  that initialises services. A dummy value like ``"sk-test"`` is sufficient
  for tests that do not actually call the LLM.

Tests skip gracefully when the required env vars are absent.
"""

import os
import uuid
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
    reason="notebook ops tests require MOCK_EMBEDDING=true and a non-empty LLM_API_KEY / OPENAI_TOKEN",
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
# cognee.notebooks.list()
# ---------------------------------------------------------------------------

@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_list_notebooks_empty(tmp_path):
    """list() returns a list (may be empty for a fresh database)."""
    c = await _make_cognee(tmp_path)
    result = await c.notebooks.list()
    assert isinstance(result, list)


# ---------------------------------------------------------------------------
# cognee.notebooks.create() / delete()
# ---------------------------------------------------------------------------

@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_create_notebook(tmp_path):
    """create() returns a Notebook dict with the expected name."""
    c = await _make_cognee(tmp_path)
    nb = await c.notebooks.create("My Notebook")
    assert isinstance(nb, dict)
    assert nb["name"] == "My Notebook"
    assert "id" in nb


@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_create_and_list_notebook(tmp_path):
    """A created notebook appears in the list."""
    c = await _make_cognee(tmp_path)
    nb = await c.notebooks.create("Listed Notebook")
    notebooks = await c.notebooks.list()
    ids = [n["id"] for n in notebooks]
    assert nb["id"] in ids


@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_create_and_delete_notebook(tmp_path):
    """create() then delete() returns True."""
    c = await _make_cognee(tmp_path)
    nb = await c.notebooks.create("Delete Me")
    deleted = await c.notebooks.delete(nb["id"])
    assert deleted is True


@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_delete_nonexistent_notebook(tmp_path):
    """delete() returns False for a nonexistent UUID."""
    c = await _make_cognee(tmp_path)
    deleted = await c.notebooks.delete(str(uuid.uuid4()))
    assert deleted is False


# ---------------------------------------------------------------------------
# cognee.notebooks.create() — cells parameter
# ---------------------------------------------------------------------------

@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_create_notebook_with_cells(tmp_path):
    """create() with a cells list does not raise."""
    c = await _make_cognee(tmp_path)
    nb = await c.notebooks.create("With Cells", cells=[{"type": "text", "content": "hello"}])
    assert isinstance(nb, dict)
    assert nb["name"] == "With Cells"


# ---------------------------------------------------------------------------
# cognee.notebooks.update()
# ---------------------------------------------------------------------------

@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_update_notebook_name(tmp_path):
    """update() with a new name returns a Notebook dict with the updated name."""
    c = await _make_cognee(tmp_path)
    nb = await c.notebooks.create("Original Name")
    updated = await c.notebooks.update(nb["id"], {"name": "Updated Name"})
    assert isinstance(updated, dict)
    assert updated["name"] == "Updated Name"


@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_update_nonexistent_notebook(tmp_path):
    """update() for a nonexistent UUID returns None."""
    c = await _make_cognee(tmp_path)
    result = await c.notebooks.update(str(uuid.uuid4()), {"name": "Ghost"})
    assert result is None
