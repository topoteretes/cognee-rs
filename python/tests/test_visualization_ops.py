"""Tests for PyCognee.visualize and .visualize_to_file (T9).

Environment requirements:
- ``MOCK_EMBEDDING=true`` — avoids downloading an ONNX model; required for
  all tests that call warm() (i.e. anything that initialises services).
- A non-empty ``LLM_API_KEY`` (or ``OPENAI_TOKEN``) — required for warm();
  a dummy value like ``"sk-test"`` is sufficient because OpenAIAdapter
  does no network I/O at construction time.

Tests skip gracefully when the required env vars are absent.

Note on the feature-not-built test: it requires a build compiled *without*
the ``visualization`` Cargo feature, which is not possible in the standard
``python/scripts/check.sh`` run (which builds with defaults). That test is
unconditionally skipped in CI.
"""

import os
import pytest
import cognee_pipeline as cp


# ---------------------------------------------------------------------------
# Env-var guards
# ---------------------------------------------------------------------------

def _warm_vars_present() -> bool:
    """True when the minimal env vars needed for warm() are set."""
    llm_key = os.environ.get("LLM_API_KEY") or os.environ.get("OPENAI_TOKEN", "")
    mock_emb = os.environ.get("MOCK_EMBEDDING", "")
    return bool(llm_key) and bool(mock_emb)


SKIP_IF_NO_WARM = pytest.mark.skipif(
    not _warm_vars_present(),
    reason="visualization tests require MOCK_EMBEDDING=true and a non-empty LLM_API_KEY / OPENAI_TOKEN",
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
# visualize() — smoke test
# ---------------------------------------------------------------------------

@SKIP_IF_NO_WARM
@pytest.mark.asyncio
async def test_visualize_returns_html(tmp_path):
    """visualize() returns a non-empty HTML string."""
    c = await _make_cognee(tmp_path)
    html = await c.visualize()
    assert isinstance(html, str)
    assert len(html) > 0
    assert "<!DOCTYPE html>" in html or "<html" in html


# ---------------------------------------------------------------------------
# visualize_to_file() — writes a file and returns its path
# ---------------------------------------------------------------------------

@SKIP_IF_NO_WARM
@pytest.mark.asyncio
async def test_visualize_to_file(tmp_path):
    """visualize_to_file() writes an HTML file and returns its path."""
    c = await _make_cognee(tmp_path)
    out = str(tmp_path / "graph.html")
    path = await c.visualize_to_file({"destination_path": out})
    assert isinstance(path, str)
    assert path.endswith(".html")
    assert os.path.isfile(path)
    content = open(path).read()
    assert "<!DOCTYPE html>" in content or "<html" in content


# ---------------------------------------------------------------------------
# visualize_to_file() — camelCase opts key also accepted
# ---------------------------------------------------------------------------

@SKIP_IF_NO_WARM
@pytest.mark.asyncio
async def test_visualize_to_file_camel_case_opts(tmp_path):
    """visualize_to_file() accepts camelCase opts keys."""
    c = await _make_cognee(tmp_path)
    out = str(tmp_path / "graph_camel.html")
    path = await c.visualize_to_file({"destinationPath": out})
    assert isinstance(path, str)
    assert os.path.isfile(path)


# ---------------------------------------------------------------------------
# visualize() — no opts (default behaviour)
# ---------------------------------------------------------------------------

@SKIP_IF_NO_WARM
@pytest.mark.asyncio
async def test_visualize_no_opts(tmp_path):
    """visualize() called with no arguments returns valid HTML."""
    c = await _make_cognee(tmp_path)
    html = await c.visualize()
    assert isinstance(html, str)
    assert len(html) > 0


# ---------------------------------------------------------------------------
# Feature-not-built test (unconditionally skipped in CI — requires a build
# without the visualization feature, which check.sh cannot provide).
# ---------------------------------------------------------------------------

@pytest.mark.skip(
    reason="requires a build compiled without the visualization feature; skip in standard CI"
)
@pytest.mark.asyncio
async def test_visualize_feature_not_built(tmp_path):
    """When visualization is not compiled in, both methods raise CogneeFeatureNotBuiltError."""
    c = await _make_cognee(tmp_path)
    with pytest.raises(cp.CogneeFeatureNotBuiltError):
        await c.visualize()
    with pytest.raises(cp.CogneeFeatureNotBuiltError):
        await c.visualize_to_file()
