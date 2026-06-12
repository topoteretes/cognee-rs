"""Tests for PyCognee.remember, .remember_entry, .memify, and .improve (T7).

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
import cognee_pipeline as cp


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
# memify() — runs on empty graph; requires MOCK_EMBEDDING + LLM_API_KEY only.
# ---------------------------------------------------------------------------

@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_memify_empty_graph(tmp_path):
    """memify() on an empty graph returns a dict with the expected camelCase keys."""
    c = await _make_cognee(tmp_path)
    result = await c.memify()
    assert isinstance(result, dict)
    # Must have at least one of the expected top-level keys (all camelCase).
    assert "tripletCount" in result or "alreadyCompleted" in result, (
        f"unexpected memify result shape: {result}"
    )


@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_memify_result_keys(tmp_path):
    """memify() result contains all expected camelCase keys."""
    c = await _make_cognee(tmp_path)
    result = await c.memify()
    for key in ("tripletCount", "indexedCount", "batchCount", "alreadyCompleted",
                "priorPipelineRunId"):
        assert key in result, f"missing key: {key}"
    assert isinstance(result["alreadyCompleted"], bool)
    assert isinstance(result["tripletCount"], int)


@SKIP_IF_NO_BASE
@pytest.mark.asyncio
async def test_memify_with_opts(tmp_path):
    """memify() accepts an opts dict without raising."""
    c = await _make_cognee(tmp_path)
    result = await c.memify(opts={"tripletBatchSize": 10})
    assert isinstance(result, dict)
    assert "tripletCount" in result


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
    for key in ("stagesRun", "memifyResult", "feedbackEntriesProcessed",
                "feedbackEntriesApplied", "sessionsPersisted", "edgesSynced"):
        assert key in result, f"missing key: {key}"
    assert isinstance(result["stagesRun"], list)
