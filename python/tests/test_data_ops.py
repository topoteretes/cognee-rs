"""Tests for PyCognee.forget, .update, .prune_data, .prune_system (T5).

All result dicts use camelCase keys (matching the C API / TS wire shape).

Environment requirements:
- ``MOCK_EMBEDDING=true`` — avoids downloading an ONNX model; required for
  all tests that call warm() / add() (i.e. anything that initialises services).
- A non-empty ``LLM_API_KEY`` (or ``OPENAI_TOKEN``) — required for any test
  that reaches add(). A dummy value like ``"sk-test"`` is sufficient for
  add-only tests because ``OpenAIAdapter::new`` does no network I/O at
  construction time.
- ``OPENAI_URL`` — required for tests that actually invoke the LLM (update
  re-cognify step).

Tests skip gracefully when the required env vars are absent.
"""

import os
import pytest
import cognee_pipeline as cp


# ---------------------------------------------------------------------------
# Env-var guards
# ---------------------------------------------------------------------------

def _add_vars_present() -> bool:
    """True when the minimal env vars needed for add() / prune are set."""
    llm_key = os.environ.get("LLM_API_KEY") or os.environ.get("OPENAI_TOKEN", "")
    mock_emb = os.environ.get("MOCK_EMBEDDING", "")
    return bool(llm_key) and bool(mock_emb)


def _update_vars_present() -> bool:
    """True when env vars needed for update() (real LLM cognify) are set."""
    return _add_vars_present() and bool(os.environ.get("OPENAI_URL", ""))


SKIP_IF_NO_ADD = pytest.mark.skipif(
    not _add_vars_present(),
    reason="tests require MOCK_EMBEDDING=true and a non-empty LLM_API_KEY / OPENAI_TOKEN",
)

SKIP_IF_NO_UPDATE = pytest.mark.skipif(
    not _update_vars_present(),
    reason="update() tests require MOCK_EMBEDDING=true, LLM_API_KEY, and OPENAI_URL",
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
    """Warm handle with one text item added. Returns (cognee, data_id)."""
    c = await _make_cognee(tmp_path)
    result = await c.add({"type": "text", "text": "The quick brown fox."}, "test_ds")
    # added[0] has an "id" field (camelCase from Data serialisation)
    data_id = result["added"][0]["id"]
    return c, data_id


# ---------------------------------------------------------------------------
# forget() — "all" target
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_forget_all(tmp_path):
    """forget({"kind": "all"}) returns a result dict with target == "all"."""
    c, _ = await _cognee_with_data(tmp_path)
    result = await c.forget({"kind": "all"})
    assert isinstance(result, dict)
    assert result["target"] == "all"
    assert "deleteResult" in result


# ---------------------------------------------------------------------------
# forget() — "dataset" target
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_forget_dataset(tmp_path):
    """forget({"kind": "dataset", ...}) returns a result dict."""
    c, _ = await _cognee_with_data(tmp_path)
    result = await c.forget({"kind": "dataset", "dataset": {"name": "test_ds"}})
    assert isinstance(result, dict)
    assert "deleteResult" in result


# ---------------------------------------------------------------------------
# forget() — unknown kind raises CogneeValidationError
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_forget_bad_kind(tmp_path):
    """forget() with an unknown kind raises CogneeValidationError."""
    c = await _make_cognee(tmp_path)
    with pytest.raises(cp.CogneeValidationError):
        await c.forget({"kind": "unknown_kind_xyz"})


# ---------------------------------------------------------------------------
# forget() — snake_case key normalisation
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_forget_item_snake_case_data_id(tmp_path):
    """forget() accepts snake_case 'data_id' key (normalised to 'dataId')."""
    c, data_id = await _cognee_with_data(tmp_path)
    # "data_id" snake_case → should be normalised to "dataId" before dispatch.
    result = await c.forget({
        "kind": "item",
        "data_id": data_id,
        "dataset": {"name": "test_ds"},
    })
    assert isinstance(result, dict)
    assert "deleteResult" in result


# ---------------------------------------------------------------------------
# prune_data()
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_prune_data_returns_none(tmp_path):
    """prune_data() should not raise and should return None."""
    c, _ = await _cognee_with_data(tmp_path)
    result = await c.prune_data()
    assert result is None


# ---------------------------------------------------------------------------
# prune_system() — defaults
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_prune_system_default_opts(tmp_path):
    """prune_system() with no opts returns a dict with expected camelCase keys."""
    c, _ = await _cognee_with_data(tmp_path)
    result = await c.prune_system()
    assert isinstance(result, dict)
    for key in ("dataPruned", "graphPruned", "vectorPruned", "metadataPruned", "cachePruned"):
        assert key in result, f"missing key: {key}"


# ---------------------------------------------------------------------------
# prune_system() — snake_case opts normalisation
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_prune_system_snake_case_opts(tmp_path):
    """prune_system() accepts snake_case opts keys."""
    c, _ = await _cognee_with_data(tmp_path)
    result = await c.prune_system({"prune_graph": True, "prune_vector": True})
    assert result["graphPruned"] is True
    assert result["vectorPruned"] is True


# ---------------------------------------------------------------------------
# prune_system() — camelCase opts
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_prune_system_camelcase_opts(tmp_path):
    """prune_system() accepts camelCase opts keys equivalently."""
    c, _ = await _cognee_with_data(tmp_path)
    result = await c.prune_system({"pruneGraph": True, "pruneVector": False})
    assert result["graphPruned"] is True
    assert result["vectorPruned"] is False


# ---------------------------------------------------------------------------
# update() — requires real LLM
# ---------------------------------------------------------------------------

@SKIP_IF_NO_UPDATE
@pytest.mark.asyncio
async def test_update_result_shape(tmp_path):
    """update() returns a dict with deletedDataId, deleteResult, newData, cognifyResult."""
    c, data_id = await _cognee_with_data(tmp_path)
    result = await c.update(
        data_id,
        {"type": "text", "text": "Updated content for testing."},
        "test_ds",
    )
    assert isinstance(result, dict)
    for key in ("deletedDataId", "deleteResult", "newData", "cognifyResult"):
        assert key in result, f"missing key: {key}"
    assert result["deletedDataId"] == data_id


@SKIP_IF_NO_UPDATE
@pytest.mark.asyncio
async def test_update_invalid_uuid_raises(tmp_path):
    """update() with a non-UUID data_id raises CogneeValidationError."""
    c = await _make_cognee(tmp_path)
    with pytest.raises(cp.CogneeValidationError):
        await c.update("not-a-uuid", {"type": "text", "text": "x"}, "ds")
