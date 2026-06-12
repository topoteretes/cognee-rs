"""Tests for module-level serve / disconnect cloud operations (T10).

Environment requirements:
- No special env vars are needed for ``test_disconnect`` — it is always run.
- ``test_serve_direct_mode`` requires ``COGNEE_TEST_SERVER_URL`` to be set to
  a running Cognee HTTP server URL; it is skipped otherwise.

Note on the Auth0 interactive flow: it cannot be tested in CI. Only the
direct mode (``url`` option) is testable in automated environments.

Note on the feature-not-built test: it requires a build compiled *without*
the ``cloud`` Cargo feature, which is not possible in the standard
``python/scripts/check.sh`` run (which builds with defaults). That test is
unconditionally skipped in CI.
"""

import os
import pytest
import cognee_pipeline as cp


# ---------------------------------------------------------------------------
# test_disconnect — always runs (no server needed)
# ---------------------------------------------------------------------------

@pytest.fixture
def isolated_home(tmp_path, monkeypatch):
    """Point HOME at a tmp dir for the duration of the test.

    The cloud credential store lives at ``~/.cognee/cloud_credentials.json``
    (resolved from ``$HOME`` at call time on Linux).  Without this isolation,
    ``disconnect({"wipe_credentials": True})`` would DELETE the developer's
    real credential file — shared byte-for-byte with the Python SDK — on
    every test run.
    """
    monkeypatch.setenv("HOME", str(tmp_path))
    return tmp_path


@pytest.mark.asyncio
async def test_disconnect(isolated_home):
    """disconnect() returns None without raising even if not connected."""
    result = await cp.disconnect()
    assert result is None


@pytest.mark.asyncio
async def test_disconnect_with_wipe_credentials(isolated_home):
    """disconnect({"wipe_credentials": True}) also returns None without raising."""
    creds = isolated_home / ".cognee" / "cloud_credentials.json"
    creds.parent.mkdir(parents=True)
    creds.write_text("{}")
    result = await cp.disconnect({"wipe_credentials": True})
    assert result is None
    assert not creds.exists()  # wiped the isolated file, not the real one


@pytest.mark.asyncio
async def test_disconnect_camel_case_opts(isolated_home):
    """disconnect() accepts camelCase opts keys."""
    result = await cp.disconnect({"wipeCredentials": False})
    assert result is None


# ---------------------------------------------------------------------------
# test_serve_direct_mode — skips unless COGNEE_TEST_SERVER_URL is set
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_serve_direct_mode():
    """serve({"url": server_url}) returns {"connected": True, "serviceUrl": ...}."""
    server_url = os.environ.get("COGNEE_TEST_SERVER_URL")
    if not server_url:
        pytest.skip("COGNEE_TEST_SERVER_URL not set — skipping direct-mode serve test")

    result = await cp.serve({"url": server_url})
    assert isinstance(result, dict)
    assert result.get("connected") is True
    assert "serviceUrl" in result  # camelCase — matches capi/neon shape
    assert isinstance(result["serviceUrl"], str)


# ---------------------------------------------------------------------------
# Feature-not-built test (unconditionally skipped in CI — requires a build
# without the cloud feature, which check.sh cannot provide).
# ---------------------------------------------------------------------------

@pytest.mark.skip(
    reason="requires a build compiled without the cloud feature; skip in standard CI"
)
@pytest.mark.asyncio
async def test_serve_feature_not_built():
    """When cloud is not compiled in, serve() raises CogneeFeatureNotBuiltError."""
    with pytest.raises(cp.CogneeFeatureNotBuiltError):
        await cp.serve()


@pytest.mark.skip(
    reason="requires a build compiled without the cloud feature; skip in standard CI"
)
@pytest.mark.asyncio
async def test_disconnect_feature_not_built():
    """When cloud is not compiled in, disconnect() raises CogneeFeatureNotBuiltError."""
    with pytest.raises(cp.CogneeFeatureNotBuiltError):
        await cp.disconnect()
