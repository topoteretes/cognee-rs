"""Tests for PyCognee.remember, .remember_entry, .improve and
.wait_for_background_tasks (T7, SDK-1051).

Environment requirements:
- ``MOCK_EMBEDDING=true`` — avoids downloading an ONNX model; required for
  all tests that call warm() / add() (i.e. anything that initialises services).
- A non-empty ``LLM_API_KEY`` (or ``OPENAI_TOKEN``) — required for any test
  that initialises services.  A dummy value like ``"sk-test"`` is sufficient
  for tests that do not actually call the LLM.
- ``OPENAI_URL`` — required for tests that actually invoke the LLM
  (remember, remember_entry).

Tests skip gracefully when the required env vars are absent.
"""

import os
import pytest
import cognee_py as cp


# ---------------------------------------------------------------------------
# Env-var guards
# ---------------------------------------------------------------------------

def _base_vars_present() -> bool:
    """True when the minimal env vars needed for service initialisation are set."""
    llm_key = os.environ.get("LLM_API_KEY") or os.environ.get("OPENAI_TOKEN", "")
    mock_emb = os.environ.get("MOCK_EMBEDDING", "")
    return bool(llm_key) and bool(mock_emb)


def _llm_vars_present() -> bool:
    """True when the env vars needed for real LLM calls are set."""
    return _base_vars_present() and bool(os.environ.get("OPENAI_URL", ""))


SKIP_IF_NO_BASE = pytest.mark.skipif(
    not _base_vars_present(),
    reason="memory ops tests require MOCK_EMBEDDING=true and a non-empty LLM_API_KEY / OPENAI_TOKEN",
)

SKIP_IF_NO_LLM = pytest.mark.skipif(
    not _llm_vars_present(),
    reason="remember/remember_entry tests require MOCK_EMBEDDING=true, LLM_API_KEY, and OPENAI_URL",
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
# remember() — requires real LLM.
# ---------------------------------------------------------------------------

@SKIP_IF_NO_LLM
@pytest.mark.asyncio
async def test_remember_text(tmp_path):
    """remember() with a text input does not raise and returns a non-None result."""
    c = await _make_cognee(tmp_path)
    result = await c.remember({"type": "text", "text": "Fact A: cats meow."}, "mem_ds")
    assert result is not None


@SKIP_IF_NO_LLM
@pytest.mark.asyncio
async def test_remember_list_input(tmp_path):
    """remember() with a list of inputs does not raise."""
    c = await _make_cognee(tmp_path)
    result = await c.remember(
        [
            {"type": "text", "text": "Fact B: dogs bark."},
            {"type": "text", "text": "Fact C: birds sing."},
        ],
        "mem_list_ds",
    )
    assert result is not None


# ---------------------------------------------------------------------------
# remember_entry() — requires real LLM.
# ---------------------------------------------------------------------------

@SKIP_IF_NO_LLM
@pytest.mark.asyncio
async def test_remember_entry_qa(tmp_path):
    """remember_entry() with a QA entry does not raise."""
    c = await _make_cognee(tmp_path)
    entry = {"type": "qa", "question": "What is cognee?", "answer": "A memory system."}
    result = await c.remember_entry(entry, "ds", "session-1")
    assert result is not None


@SKIP_IF_NO_LLM
@pytest.mark.asyncio
async def test_remember_entry_trace(tmp_path):
    """remember_entry() with a trace entry does not raise."""
    c = await _make_cognee(tmp_path)
    entry = {
        "type": "trace",
        "originFunction": "my_function",
        "status": "success",
        "memoryQuery": "What happened?",
    }
    result = await c.remember_entry(entry, "ds", "session-2")
    assert result is not None


# ---------------------------------------------------------------------------
# remember_entry() — validation tests (run with MOCK_EMBEDDING only).
# ---------------------------------------------------------------------------

@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_remember_entry_bad_type(tmp_path):
    """remember_entry() with an unknown type raises CogneeValidationError."""
    c = await _make_cognee(tmp_path)
    with pytest.raises(cp.CogneeValidationError):
        await c.remember_entry({"type": "unknown_entry_type"}, "ds", "s")


@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_remember_entry_missing_type(tmp_path):
    """remember_entry() with no type key raises CogneeValidationError."""
    c = await _make_cognee(tmp_path)
    with pytest.raises(cp.CogneeValidationError):
        await c.remember_entry({"question": "No type here"}, "ds", "s")


@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_remember_entry_trace_missing_origin_function(tmp_path):
    """remember_entry() trace without originFunction raises CogneeValidationError."""
    c = await _make_cognee(tmp_path)
    with pytest.raises(cp.CogneeValidationError):
        await c.remember_entry({"type": "trace"}, "ds", "s")


@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_remember_entry_feedback_missing_qa_id(tmp_path):
    """remember_entry() feedback without qaId raises CogneeValidationError."""
    c = await _make_cognee(tmp_path)
    with pytest.raises(cp.CogneeValidationError):
        await c.remember_entry({"type": "feedback"}, "ds", "s")


# ---------------------------------------------------------------------------
# improve() — validation tests (run with MOCK_EMBEDDING only).
# ---------------------------------------------------------------------------

@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_improve_missing_dataset_name(tmp_path):
    """improve({}) with no datasetName raises CogneeValidationError."""
    c = await _make_cognee(tmp_path)
    with pytest.raises(cp.CogneeValidationError):
        await c.improve({})


@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_improve_missing_dataset_name_null(tmp_path):
    """improve() with wrong type for datasetName raises CogneeValidationError."""
    c = await _make_cognee(tmp_path)
    with pytest.raises(cp.CogneeValidationError):
        # Passing None for opts would normally be a Python-side type error, but
        # passing an empty dict (no datasetName) must raise CogneeValidationError.
        await c.improve({"sessionIds": ["abc"]})


# ---------------------------------------------------------------------------
# improve() — requires real LLM.
# ---------------------------------------------------------------------------

@SKIP_IF_NO_LLM
@pytest.mark.asyncio
async def test_improve_result_keys(tmp_path):
    """improve() returns a dict with all expected camelCase keys."""
    c = await _make_cognee(tmp_path)
    # First add some data so the dataset exists.
    await c.add({"type": "text", "text": "Fact for improve."}, "improve_ds")
    result = await c.improve({"datasetName": "improve_ds"})
    assert isinstance(result, dict)
    for key in ("datasetId", "datasetName", "sessionIds", "stages", "background",
                "finished", "error", "status"):
        assert key in result, f"missing key: {key}"
    assert isinstance(result["stages"], list)
    assert result["stages"][0]["stage"] == "feedback_weights"


# ---------------------------------------------------------------------------
# improve(runInBackground) + wait_for_background_tasks() (SDK-1051).
# ---------------------------------------------------------------------------

def _cold_cognee(tmp_path) -> cp.Cognee:
    """A handle that is never warmed: the wait needs no services."""
    db = str(tmp_path / "cognee.db")
    return cp.Cognee(f'{{"relational_db_url": "sqlite://{db}?mode=rwc"}}')


@pytest.mark.asyncio
async def test_wait_for_background_tasks_idle(tmp_path):
    """With nothing running the wait returns at once, finished, none pending."""
    c = _cold_cognee(tmp_path)
    assert await c.wait_for_background_tasks() == {"finished": True, "pending": 0}
    # snake_case and camelCase spellings of the timeout are both accepted.
    assert (await c.wait_for_background_tasks({"timeout_seconds": 5}))["finished"] is True
    assert (await c.wait_for_background_tasks({"timeoutSeconds": 0.5}))["pending"] == 0


@pytest.mark.asyncio
async def test_wait_for_background_tasks_rejects_bad_timeout(tmp_path):
    """A negative or non-numeric timeout raises CogneeValidationError."""
    c = _cold_cognee(tmp_path)
    for bad in (-1, "5"):
        with pytest.raises(cp.CogneeValidationError):
            await c.wait_for_background_tasks({"timeout_seconds": bad})


@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_improve_in_background_returns_running_then_drains(tmp_path):
    """runInBackground returns the running result; the wait drains the run."""
    c = await _make_cognee(tmp_path)
    result = await c.improve({"dataset_name": "bg_ds", "run_in_background": True})
    assert result["status"] == "running"
    assert result["background"] is True
    assert result["finished"] is False
    assert result["datasetName"] == "bg_ds"
    assert isinstance(result["datasetId"], str)
    waited = await c.wait_for_background_tasks({"timeout_seconds": 60})
    assert waited == {"finished": True, "pending": 0}


@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_improve_in_background_unknown_dataset_id_raises(tmp_path):
    """An unknown datasetId is refused before anything is dispatched."""
    c = await _make_cognee(tmp_path)
    with pytest.raises(cp.CogneeValidationError):
        await c.improve({
            "dataset_name": "bg_ds",
            "dataset_id": "00000000-0000-0000-0000-0000000000aa",
            "run_in_background": True,
        })
