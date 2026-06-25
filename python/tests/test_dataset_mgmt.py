"""Tests for PyCogneeDatasets — cognee.datasets sub-object (T6).

Operations covered:
  list, list_data, has, status, empty, delete_data, delete_all.

Environment requirements:
- ``MOCK_EMBEDDING=true`` — avoids downloading an ONNX model.
- A non-empty ``LLM_API_KEY`` (or ``OPENAI_TOKEN``) — required for tests
  that call add().  A dummy value like ``"sk-test"`` is sufficient for
  add-only tests (no network I/O at construction time).

Tests skip gracefully when required env vars are absent.
"""

import os
import uuid

import pytest
import cognee_py as cp


# ---------------------------------------------------------------------------
# Env-var guards
# ---------------------------------------------------------------------------

def _add_vars_present() -> bool:
    """True when the minimal env vars needed for warm() / add() are set."""
    llm_key = os.environ.get("LLM_API_KEY") or os.environ.get("OPENAI_TOKEN", "")
    mock_emb = os.environ.get("MOCK_EMBEDDING", "")
    return bool(llm_key) and bool(mock_emb)


SKIP_IF_NO_ADD = pytest.mark.skipif(
    not _add_vars_present(),
    reason="tests require MOCK_EMBEDDING=true and a non-empty LLM_API_KEY / OPENAI_TOKEN",
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


async def _cognee_with_data(tmp_path) -> tuple:
    """Warm handle with one text item added. Returns (cognee, dataset_id, data_id)."""
    c = await _make_cognee(tmp_path)
    result = await c.add({"type": "text", "text": "The quick brown fox."}, "test_ds")
    # result["added"][0] has an "id" field (camelCase from Data serialisation).
    data_id = result["added"][0]["id"]
    # list() to get the dataset id.
    datasets = await c.datasets.list()
    ds_id = datasets[0]["id"]
    return c, ds_id, data_id


# ---------------------------------------------------------------------------
# cognee.datasets attribute — basic structural test
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_datasets_attribute_exists(tmp_path):
    """cognee.datasets is accessible and is a CogneeDatasets instance."""
    c = await _make_cognee(tmp_path)
    assert hasattr(c, "datasets")
    assert isinstance(c.datasets, cp.CogneeDatasets)


# ---------------------------------------------------------------------------
# list() — empty store
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_list_datasets_empty(tmp_path):
    """datasets.list() returns an empty list when no data has been added."""
    c = await _make_cognee(tmp_path)
    result = await c.datasets.list()
    assert isinstance(result, list)
    assert len(result) == 0


# ---------------------------------------------------------------------------
# has() — non-existent UUID returns False
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_has_data_false_for_nonexistent(tmp_path):
    """datasets.has() returns False for a random UUID that was never added."""
    c = await _make_cognee(tmp_path)
    ds_id = str(uuid.uuid4())
    result = await c.datasets.has(ds_id)
    assert result is False


# ---------------------------------------------------------------------------
# list() and has() — after add()
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_list_and_has_after_add(tmp_path):
    """After add(), list() shows the dataset and has() returns True."""
    c = await _make_cognee(tmp_path)
    await c.add({"type": "text", "text": "Hello world."}, "my_ds")
    datasets = await c.datasets.list()
    assert isinstance(datasets, list)
    assert any(ds.get("name") == "my_ds" for ds in datasets)
    ds_id = next(ds["id"] for ds in datasets if ds.get("name") == "my_ds")
    has = await c.datasets.has(ds_id)
    assert has is True


# ---------------------------------------------------------------------------
# list_data() — after add()
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_list_data_after_add(tmp_path):
    """datasets.list_data() returns at least one item after add()."""
    c, ds_id, _ = await _cognee_with_data(tmp_path)
    items = await c.datasets.list_data(ds_id)
    assert isinstance(items, list)
    assert len(items) >= 1


# ---------------------------------------------------------------------------
# status() — after add()
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_dataset_status_after_add(tmp_path):
    """datasets.status() returns a dict for the given UUIDs.

    Datasets with no pipeline runs are omitted from the result (matching
    Python's "not started" behaviour).  After a bare add() with no cognify()
    the map may be empty — the important thing is that the return type is a
    dict and no error is raised.
    """
    c, ds_id, _ = await _cognee_with_data(tmp_path)
    status = await c.datasets.status([ds_id])
    assert isinstance(status, dict)
    # Every key that IS present must be a string UUID.
    for key in status:
        assert isinstance(key, str)


# ---------------------------------------------------------------------------
# status() — empty list
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_dataset_status_empty_list(tmp_path):
    """datasets.status([]) returns an empty dict."""
    c = await _make_cognee(tmp_path)
    result = await c.datasets.status([])
    assert isinstance(result, dict)
    assert len(result) == 0


# ---------------------------------------------------------------------------
# empty() — after add()
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_empty_dataset(tmp_path):
    """datasets.empty() returns a delete result dict."""
    c, ds_id, _ = await _cognee_with_data(tmp_path)
    result = await c.datasets.empty(ds_id)
    assert result is not None


# ---------------------------------------------------------------------------
# delete_all() — after add()
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_delete_all_datasets(tmp_path):
    """datasets.delete_all() returns a list (of delete results)."""
    c = await _make_cognee(tmp_path)
    await c.add({"type": "text", "text": "To be deleted."}, "ephemeral_ds")
    result = await c.datasets.delete_all()
    assert isinstance(result, list)


# ---------------------------------------------------------------------------
# UUID validation — has() rejects non-UUIDs
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_has_invalid_uuid_raises(tmp_path):
    """datasets.has() raises CogneeValidationError for an invalid UUID."""
    c = await _make_cognee(tmp_path)
    with pytest.raises(cp.CogneeValidationError):
        await c.datasets.has("not-a-uuid")


# ---------------------------------------------------------------------------
# UUID validation — list_data() rejects non-UUIDs
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_list_data_invalid_uuid_raises(tmp_path):
    """datasets.list_data() raises CogneeValidationError for an invalid UUID."""
    c = await _make_cognee(tmp_path)
    with pytest.raises(cp.CogneeValidationError):
        await c.datasets.list_data("not-a-uuid")


# ---------------------------------------------------------------------------
# delete_data() — opts snake_case normalisation
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_delete_data_snake_case_opts(tmp_path):
    """datasets.delete_data() accepts snake_case opts keys."""
    c, ds_id, data_id = await _cognee_with_data(tmp_path)
    result = await c.datasets.delete_data(
        ds_id,
        data_id,
        {"soft_delete": False, "delete_dataset_if_empty": False},
    )
    assert result is not None
