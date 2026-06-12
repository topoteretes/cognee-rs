"""Tests for PyCognee.add, .cognify, and .add_and_cognify (T3b).

Environment requirements:
- ``MOCK_EMBEDDING=true`` — avoids downloading an ONNX model; required for
  all tests that call warm() / add() (i.e. anything that initialises services).
- A non-empty ``LLM_API_KEY`` (or ``OPENAI_TOKEN``) — required for any test
  that reaches cognify (LLM calls).  A dummy value like ``"sk-test"`` is
  sufficient for add-only tests because ``OpenAIAdapter::new`` does no network
  I/O at construction time.
- ``OPENAI_URL`` — required for tests that actually invoke the LLM (cognify).

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


def _cognify_vars_present() -> bool:
    """True when the env vars needed for cognify() (real LLM) are set."""
    return _add_vars_present() and bool(os.environ.get("OPENAI_URL", ""))


SKIP_IF_NO_ADD = pytest.mark.skipif(
    not _add_vars_present(),
    reason="add() tests require MOCK_EMBEDDING=true and a non-empty LLM_API_KEY / OPENAI_TOKEN",
)

SKIP_IF_NO_COGNIFY = pytest.mark.skipif(
    not _cognify_vars_present(),
    reason="cognify() tests require MOCK_EMBEDDING=true, LLM_API_KEY, and OPENAI_URL",
)


# ---------------------------------------------------------------------------
# Helper — build a fresh, warm Cognee handle in a tmp dir.
# ---------------------------------------------------------------------------

async def _make_cognee(tmp_path) -> cp.Cognee:
    """Create and warm a Cognee handle backed by an isolated tmp database."""
    db = str(tmp_path / "cognee.db")
    # Use the correct Settings field name.  ``db_path`` is not a known key and
    # would be silently ignored, leaving all tests sharing the default DB.
    # ``relational_db_url`` is the canonical SQLite URL field; ``mode=rwc``
    # tells SQLite to create the file if it does not exist yet.
    # We also isolate storage directories so tests do not share file blobs or
    # the graph/vector artefacts written under ``.cognee_system``/``.data_storage``.
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
# add() — single dict input
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_add_text_single(tmp_path):
    """add() with a single text dict returns addedCount == 1."""
    c = await _make_cognee(tmp_path)
    result = await c.add({"type": "text", "text": "Hello world"}, "test_ds")
    assert isinstance(result, dict)
    assert result["addedCount"] == 1
    assert result["deduplicatedCount"] == 0
    assert result["datasetName"] == "test_ds"


# ---------------------------------------------------------------------------
# add() — list input
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_add_text_list(tmp_path):
    """add() with a list of two text dicts returns addedCount == 2."""
    c = await _make_cognee(tmp_path)
    inputs = [
        {"type": "text", "text": "Alpha"},
        {"type": "text", "text": "Beta"},
    ]
    result = await c.add(inputs, "test_ds")
    assert result["addedCount"] == 2
    assert result["deduplicatedCount"] == 0


# ---------------------------------------------------------------------------
# add() — binary input (Python bytes are converted to base64 on the wire)
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_add_binary_bytes(tmp_path):
    """add() accepts the documented binary shape with a Python ``bytes`` value."""
    c = await _make_cognee(tmp_path)
    payload = "Binary payload contents".encode("utf-8")
    result = await c.add(
        {"type": "binary", "bytes": payload, "name": "payload.txt"}, "test_ds"
    )
    assert result["addedCount"] == 1
    assert result["deduplicatedCount"] == 0


# ---------------------------------------------------------------------------
# add() — deduplication
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_add_deduplication(tmp_path):
    """Second add of the same text returns deduplicatedCount == 1."""
    c = await _make_cognee(tmp_path)
    await c.add({"type": "text", "text": "Duplicate me"}, "ds")
    result = await c.add({"type": "text", "text": "Duplicate me"}, "ds")
    assert result["deduplicatedCount"] == 1
    assert result["addedCount"] == 0


# ---------------------------------------------------------------------------
# add() — unsupported input type
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_add_unsupported_type_raises(tmp_path):
    """An unsupported input type (s3) raises CogneeUnsupportedError."""
    c = await _make_cognee(tmp_path)
    with pytest.raises(cp.CogneeUnsupportedError):
        await c.add({"type": "s3", "bucket": "foo", "key": "bar"}, "ds")


# ---------------------------------------------------------------------------
# add() — result shape
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_add_result_keys(tmp_path):
    """add() result contains all expected camelCase keys."""
    c = await _make_cognee(tmp_path)
    result = await c.add({"type": "text", "text": "Shape check"}, "ds")
    for key in ("datasetName", "added", "addedCount", "deduplicated", "deduplicatedCount"):
        assert key in result, f"missing key: {key}"
    assert isinstance(result["added"], list)
    assert isinstance(result["deduplicated"], list)


# ---------------------------------------------------------------------------
# add() — opts argument (no error expected — opts are advisory)
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_add_with_opts(tmp_path):
    """add() accepts an opts dict without raising."""
    c = await _make_cognee(tmp_path)
    result = await c.add(
        {"type": "text", "text": "With opts"},
        "opts_ds",
        opts={"chunkSize": 512},
    )
    assert result["addedCount"] == 1


# ---------------------------------------------------------------------------
# cognify() — requires real LLM
# ---------------------------------------------------------------------------

@SKIP_IF_NO_COGNIFY
@pytest.mark.asyncio
async def test_cognify_after_add(tmp_path):
    """cognify() after add() returns a dict with the expected camelCase keys."""
    c = await _make_cognee(tmp_path)
    await c.add({"type": "text", "text": "Alice met Bob at the park."}, "cg_ds")
    result = await c.cognify("cg_ds")
    assert isinstance(result, dict)
    for key in ("chunks", "entities", "edges", "summaries", "embeddings",
                "alreadyCompleted", "priorPipelineRunId"):
        assert key in result, f"missing key: {key}"
    assert isinstance(result["alreadyCompleted"], bool)


@SKIP_IF_NO_COGNIFY
@pytest.mark.asyncio
async def test_cognify_missing_dataset_raises(tmp_path):
    """cognify() on a non-existent dataset raises CogneeValidationError."""
    c = await _make_cognee(tmp_path)
    with pytest.raises(cp.CogneeValidationError):
        await c.cognify("nonexistent_dataset_xyz")


# ---------------------------------------------------------------------------
# add_and_cognify() — requires real LLM
# ---------------------------------------------------------------------------

@SKIP_IF_NO_COGNIFY
@pytest.mark.asyncio
async def test_add_and_cognify_result_shape(tmp_path):
    """add_and_cognify() returns a dict with both 'add' and 'cognify' sub-keys."""
    c = await _make_cognee(tmp_path)
    result = await c.add_and_cognify(
        {"type": "text", "text": "Carol studies machine learning."},
        "combined_ds",
    )
    assert isinstance(result, dict)
    assert "add" in result
    assert "cognify" in result
    assert result["add"]["addedCount"] == 1


# ---------------------------------------------------------------------------
# add_and_cognify() — duplicate skip (add-only, no LLM needed)
# ---------------------------------------------------------------------------

@SKIP_IF_NO_ADD
@pytest.mark.asyncio
async def test_add_and_cognify_all_duplicates(tmp_path):
    """add_and_cognify() with all-duplicate inputs skips cognify and returns
    a zeroed CognifyResult."""
    c = await _make_cognee(tmp_path)
    text = "Exact duplicate text for add_and_cognify."
    await c.add({"type": "text", "text": text}, "dup_ds")
    result = await c.add_and_cognify({"type": "text", "text": text}, "dup_ds")
    assert isinstance(result, dict)
    assert "add" in result and "cognify" in result
    assert result["add"]["deduplicatedCount"] == 1
    assert result["add"]["addedCount"] == 0
    # Cognify was skipped — all counts should be zero.
    cog = result["cognify"]
    assert cog["chunks"] == 0
    assert cog["entities"] == 0
    assert cog["edges"] == 0
