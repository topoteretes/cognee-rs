"""Tests for module-level serve / disconnect cloud operations.

Since T3-pre the ``cloud`` Cargo feature is **opt-in** rather than a
default for the OSS Python binding (closed `cognee-http-cloud` will
restore it via the closed bindings crate in T6). The standard
``python/scripts/check.sh`` build therefore omits the feature, so the
success-path tests assert the feature-not-built error envelope and the
``_feature_not_built`` companion tests un-skip and pass.

Closed binding builds (which re-enable ``cloud``) will exercise the
success paths in their own test suite.
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
    """OSS build (cloud feature off): disconnect() raises feature-not-built."""
    with pytest.raises(cp.CogneeFeatureNotBuiltError):
        await cp.disconnect()


@pytest.mark.asyncio
async def test_disconnect_with_wipe_credentials(isolated_home):
    """OSS build (cloud off): disconnect({wipe_credentials: True}) raises feature-not-built."""
    with pytest.raises(cp.CogneeFeatureNotBuiltError):
        await cp.disconnect({"wipe_credentials": True})  # wiped the isolated file, not the real one


@pytest.mark.asyncio
async def test_disconnect_camel_case_opts(isolated_home):
    """OSS build (cloud off): camelCase opts also surface feature-not-built."""
    with pytest.raises(cp.CogneeFeatureNotBuiltError):
        await cp.disconnect({"wipeCredentials": False})


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

@pytest.mark.asyncio
async def test_serve_feature_not_built():
    """When cloud is not compiled in, serve() raises CogneeFeatureNotBuiltError."""
    with pytest.raises(cp.CogneeFeatureNotBuiltError):
        await cp.serve()


@pytest.mark.asyncio
async def test_disconnect_feature_not_built():
    """When cloud is not compiled in, disconnect() raises CogneeFeatureNotBuiltError."""
    with pytest.raises(cp.CogneeFeatureNotBuiltError):
        await cp.disconnect()
