"""Tests for the PyCogneeConfig config surface (T2 — config-surface task).

All tests are synchronous (no ``warm()`` / no LLM / no embedding model needed).
They only require the binding to compile correctly, so they run under any
environment — including CI with ``MOCK_EMBEDDING=true`` and no real API keys.
"""

import pytest
import cognee_pipeline as cp


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def make_cognee() -> cp.Cognee:
    """Return a freshly constructed Cognee instance (no warm needed)."""
    return cp.Cognee()


# ---------------------------------------------------------------------------
# config attribute presence
# ---------------------------------------------------------------------------


def test_cognee_has_config_attribute():
    """Cognee() must expose a ``config`` attribute."""
    cognee = make_cognee()
    assert hasattr(cognee, "config")


def test_config_is_cognee_config_type():
    """``cognee.config`` must be a ``CogneeConfig`` instance."""
    cognee = make_cognee()
    assert isinstance(cognee.config, cp.CogneeConfig)


def test_config_attribute_importable():
    """``CogneeConfig`` class must be importable directly from ``cognee_pipeline``."""
    assert cp.CogneeConfig is not None


# ---------------------------------------------------------------------------
# set_str — string convenience setter
# ---------------------------------------------------------------------------


def test_set_str_known_key_succeeds():
    """``config.set_str`` with a known string key must not raise."""
    cognee = make_cognee()
    # ``llm_provider`` is a well-known string field in Settings.
    cognee.config.set_str("llm_provider", "openai")


def test_set_str_unknown_key_raises():
    """``config.set_str`` with an unknown key must raise ``CogneeUnknownConfigKeyError``."""
    cognee = make_cognee()
    with pytest.raises(cp.CogneeUnknownConfigKeyError):
        cognee.config.set_str("totally_unknown_key_xyz", "value")


def test_set_str_error_is_cognee_error_subclass():
    """``CogneeUnknownConfigKeyError`` must be catchable as ``CogneeError``."""
    cognee = make_cognee()
    with pytest.raises(cp.CogneeError):
        cognee.config.set_str("totally_unknown_key_xyz", "value")


# ---------------------------------------------------------------------------
# set — generic setter (accepts Python objects)
# ---------------------------------------------------------------------------


def test_set_float_known_key_succeeds():
    """``config.set`` with a float value for ``llm_temperature`` must not raise."""
    cognee = make_cognee()
    cognee.config.set("llm_temperature", 0.7)


def test_set_bool_known_key_succeeds():
    """``config.set`` with a bool value for ``llm_streaming`` must not raise."""
    cognee = make_cognee()
    cognee.config.set("llm_streaming", True)


def test_set_int_known_key_succeeds():
    """``config.set`` with an int for ``llm_max_retries`` must not raise."""
    cognee = make_cognee()
    cognee.config.set("llm_max_retries", 5)


def test_set_unknown_key_raises():
    """``config.set`` with an unknown key must raise ``CogneeUnknownConfigKeyError``."""
    cognee = make_cognee()
    with pytest.raises(cp.CogneeUnknownConfigKeyError):
        cognee.config.set("unknown_key_abc", "value")


# ---------------------------------------------------------------------------
# get — read-back
# ---------------------------------------------------------------------------


def test_get_returns_dict():
    """``config.get()`` must return a Python ``dict``."""
    cognee = make_cognee()
    cfg = cognee.config.get()
    assert isinstance(cfg, dict)


def test_get_contains_llm_provider():
    """The dict returned by ``get()`` must contain ``llm_provider``."""
    cognee = make_cognee()
    cfg = cognee.config.get()
    assert "llm_provider" in cfg


def test_get_reflects_set_str():
    """``get()`` must reflect values changed via ``set_str``."""
    cognee = make_cognee()
    cognee.config.set_str("llm_provider", "ollama")
    cfg = cognee.config.get()
    assert cfg["llm_provider"] == "ollama"


def test_get_redacts_llm_api_key():
    """Secret field ``llm_api_key`` must be redacted in the output of ``get()``."""
    cognee = make_cognee()
    # Set a real-looking key to make sure the redaction is applied at read-back,
    # not just when the field was already empty.
    cognee.config.set_str("llm_api_key", "sk-super-secret")
    cfg = cognee.config.get()
    assert cfg.get("llm_api_key") == "***REDACTED***"


def test_get_redacts_embedding_api_key():
    """Secret field ``embedding_api_key`` must be redacted."""
    cognee = make_cognee()
    cognee.config.set_str("embedding_api_key", "emb-secret")
    cfg = cognee.config.get()
    assert cfg.get("embedding_api_key") == "***REDACTED***"


# ---------------------------------------------------------------------------
# set_llm_config — bulk LLM setter
# ---------------------------------------------------------------------------


def test_set_llm_config_succeeds():
    """``set_llm_config`` with a valid dict must not raise."""
    cognee = make_cognee()
    cognee.config.set_llm_config({"llm_model": "gpt-4o"})


def test_set_llm_config_multiple_keys():
    """``set_llm_config`` with multiple keys must succeed."""
    cognee = make_cognee()
    cognee.config.set_llm_config(
        {"llm_model": "gpt-4o", "llm_provider": "openai", "llm_temperature": 0.0}
    )


def test_set_llm_config_reflected_in_get():
    """Changes made via ``set_llm_config`` must be visible in ``get()``."""
    cognee = make_cognee()
    cognee.config.set_llm_config({"llm_model": "gpt-4o-mini"})
    cfg = cognee.config.get()
    assert cfg["llm_model"] == "gpt-4o-mini"


def test_set_llm_config_non_dict_raises():
    """Passing a non-dict to ``set_llm_config`` must raise ``ValueError``."""
    cognee = make_cognee()
    with pytest.raises((ValueError, TypeError)):
        cognee.config.set_llm_config("not a dict")


# ---------------------------------------------------------------------------
# set_embedding_config
# ---------------------------------------------------------------------------


def test_set_embedding_config_succeeds():
    """``set_embedding_config`` with a valid dict must not raise."""
    cognee = make_cognee()
    cognee.config.set_embedding_config({"embedding_provider": "openai"})


# ---------------------------------------------------------------------------
# set_vector_db_config
# ---------------------------------------------------------------------------


def test_set_vector_db_config_succeeds():
    """``set_vector_db_config`` with a valid dict must not raise."""
    cognee = make_cognee()
    cognee.config.set_vector_db_config({"vector_db_provider": "qdrant"})


# ---------------------------------------------------------------------------
# set_graph_db_config
# ---------------------------------------------------------------------------


def test_set_graph_db_config_succeeds():
    """``set_graph_db_config`` with a valid dict must not raise."""
    cognee = make_cognee()
    cognee.config.set_graph_db_config({"graph_database_provider": "ladybug"})


# ---------------------------------------------------------------------------
# Exception hierarchy
# ---------------------------------------------------------------------------


def test_config_key_error_is_cognee_error():
    """``CogneeUnknownConfigKeyError`` must be a subclass of ``CogneeError``."""
    assert issubclass(cp.CogneeUnknownConfigKeyError, cp.CogneeError)


def test_config_type_error_is_cognee_error():
    """``CogneeConfigTypeMismatchError`` must be a subclass of ``CogneeError``."""
    assert issubclass(cp.CogneeConfigTypeMismatchError, cp.CogneeError)


# ---------------------------------------------------------------------------
# JSON conversion guards (py_to_serde)
# ---------------------------------------------------------------------------


def test_set_self_referential_dict_raises_value_error():
    """A reference cycle must raise ValueError, not abort the process."""
    cognee = make_cognee()
    cyclic = {}
    cyclic["self"] = cyclic
    with pytest.raises(ValueError):
        cognee.config.set("llm_provider", cyclic)


def test_set_deeply_nested_value_raises_value_error():
    """Nesting beyond the 128-level guard must raise ValueError."""
    cognee = make_cognee()
    deep = "leaf"
    for _ in range(200):
        deep = [deep]
    with pytest.raises(ValueError):
        cognee.config.set("llm_provider", deep)
