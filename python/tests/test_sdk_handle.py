"""Tests for the PyCognee SDK handle (T1 — sdk-handle task).

Requirements:
- ``MOCK_EMBEDDING=true`` must be set for warm() tests (avoids ONNX model download).
- A non-empty ``LLM_API_KEY`` (or ``OPENAI_TOKEN``) must be set for warm() tests.
  A dummy value like ``"sk-test"`` is sufficient — ``OpenAIAdapter::new`` does no
  network I/O at construction time.

Tests that require warm() skip gracefully when the env vars are absent.
"""

import os
import uuid as _uuid_mod

import pytest
import cognee_py as cp


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _warm_vars_present() -> bool:
    """Return True when the minimal env vars for warm() are configured."""
    llm_key = os.environ.get("LLM_API_KEY") or os.environ.get("OPENAI_TOKEN", "")
    mock_emb = os.environ.get("MOCK_EMBEDDING", "")
    # MOCK_EMBEDDING must be truthy, not merely set: "false"/"0" means a real
    # embedding engine, which these tests must not silently fall back to.
    return bool(llm_key) and mock_emb.strip().lower() in ("1", "true", "yes")


SKIP_IF_NO_WARM = pytest.mark.skipif(
    not _warm_vars_present(),
    reason="warm() tests require MOCK_EMBEDDING=true and a non-empty LLM_API_KEY / OPENAI_TOKEN",
)


def _isolated_cognee(tmp_path, extra_json_fields: str = "") -> "cp.Cognee":
    """Build a Cognee handle isolated to ``tmp_path``.

    warm() bootstraps a relational DB and storage directories; without
    isolation those land in the pytest working directory (``cognee.db``,
    ``.cognee_system/``) and pollute the repository.
    """
    db = str(tmp_path / "cognee.db")
    data_dir = str(tmp_path / "data")
    system_dir = str(tmp_path / "system")
    settings = (
        f'{{"relational_db_url": "sqlite://{db}?mode=rwc",'
        f' "data_root_directory": "{data_dir}",'
        f' "system_root_directory": "{system_dir}"'
        f"{extra_json_fields}}}"
    )
    return cp.Cognee(settings)


# ---------------------------------------------------------------------------
# Constructor tests (sync, no env vars required)
# ---------------------------------------------------------------------------


def test_cognee_instantiation_no_args():
    """Cognee() with no arguments must construct without error."""
    cognee = cp.Cognee()
    assert cognee is not None


def test_cognee_instantiation_with_settings_json():
    """Cognee(settings_json) applies the JSON overlay over env defaults."""
    # Overriding a known string field; no network I/O happens here.
    cognee = cp.Cognee('{"llm_model": "gpt-4o-mini"}')
    assert cognee is not None


def test_cognee_instantiation_empty_settings():
    """Cognee('{}') (empty overlay) is equivalent to no-args construction."""
    cognee = cp.Cognee("{}")
    assert cognee is not None


def test_cognee_malformed_json_raises_validation_error():
    """Passing malformed JSON must raise CogneeValidationError."""
    with pytest.raises(cp.CogneeValidationError):
        cp.Cognee("{not valid json}")


def test_cognee_non_object_json_raises_validation_error():
    """Passing a JSON array (not an object) must raise CogneeValidationError."""
    with pytest.raises(cp.CogneeValidationError):
        cp.Cognee("[1, 2, 3]")


# ---------------------------------------------------------------------------
# Exception hierarchy
# ---------------------------------------------------------------------------


def test_cognee_error_base_importable():
    """All SDK exception classes must be importable from cognee_py."""
    assert issubclass(cp.CogneeError, Exception)
    assert issubclass(cp.CogneeComponentError, cp.CogneeError)
    assert issubclass(cp.CogneeServiceBuildError, cp.CogneeError)
    assert issubclass(cp.CogneeUserBootstrapError, cp.CogneeError)
    assert issubclass(cp.CogneeRuntimeError, cp.CogneeError)
    assert issubclass(cp.CogneeValidationError, cp.CogneeError)
    assert issubclass(cp.CogneeUnsupportedError, cp.CogneeError)
    assert issubclass(cp.CogneeFeatureNotBuiltError, cp.CogneeError)
    assert issubclass(cp.CogneeUnknownConfigKeyError, cp.CogneeError)
    assert issubclass(cp.CogneeConfigTypeMismatchError, cp.CogneeError)


def test_validation_error_caught_as_cognee_error():
    """CogneeValidationError must be catchable as CogneeError (base class)."""
    with pytest.raises(cp.CogneeError):
        cp.Cognee("{bad json}")


# ---------------------------------------------------------------------------
# Async tests (require warm env vars)
# ---------------------------------------------------------------------------


@SKIP_IF_NO_WARM
@pytest.mark.asyncio
async def test_warm_returns_none(tmp_path):
    """warm() must be awaitable and return None."""
    cognee = _isolated_cognee(tmp_path)
    result = await cognee.warm()
    assert result is None


@SKIP_IF_NO_WARM
@pytest.mark.asyncio
async def test_warm_idempotent(tmp_path):
    """Calling warm() twice on the same handle must succeed."""
    cognee = _isolated_cognee(tmp_path)
    await cognee.warm()
    await cognee.warm()


@SKIP_IF_NO_WARM
@pytest.mark.asyncio
async def test_owner_id_returns_valid_uuid(tmp_path):
    """owner_id() must return a valid UUID string."""
    cognee = _isolated_cognee(tmp_path)
    oid = await cognee.owner_id()
    assert isinstance(oid, str)
    # Validate it parses as a UUID.
    parsed = _uuid_mod.UUID(oid)
    assert str(parsed) == oid


@SKIP_IF_NO_WARM
@pytest.mark.asyncio
async def test_owner_id_warms_lazily(tmp_path):
    """owner_id() must warm the handle lazily (no prior warm() call needed)."""
    cognee = _isolated_cognee(tmp_path)
    oid = await cognee.owner_id()
    assert isinstance(oid, str)
    _uuid_mod.UUID(oid)  # raises ValueError if not a valid UUID


@SKIP_IF_NO_WARM
@pytest.mark.asyncio
async def test_owner_id_stable_across_calls(tmp_path):
    """owner_id() must return the same UUID on repeated calls."""
    cognee = _isolated_cognee(tmp_path)
    oid1 = await cognee.owner_id()
    oid2 = await cognee.owner_id()
    assert oid1 == oid2


@SKIP_IF_NO_WARM
@pytest.mark.asyncio
async def test_warm_with_settings_override(tmp_path):
    """warm() must succeed when settings were provided at construction."""
    cognee = _isolated_cognee(tmp_path, ', "llm_model": "gpt-4o-mini"')
    result = await cognee.warm()
    assert result is None
