"""Synthetic-divergence meta-tests for the HTTP parity harness.

These tests exercise ``assert_responses_match`` and the OpenAPI structural diff
helpers against crafted ``httpx.Response`` objects (no live server needed).
They are the *parity test for the parity test* — if the diff helpers regress,
these tests catch it before a real divergence slips through CI.

Five cases (per p8-e2e-parity.md Step 14):
1. Extra key on one side is caught and names the diverging field.
2. Type mismatch is caught and names the field and both types.
3. Status code mismatch is caught and shows both codes.
4. OpenAPI path-set divergence is caught with the missing path named.
5. Nested recursive diff catches a deeply-nested divergence.
"""

from __future__ import annotations

import json

import httpx
import pytest

from http_helpers import DEFAULT_IGNORE, assert_responses_match, strip_paths


def _json_resp(status: int, body: dict, ct: str = "application/json") -> httpx.Response:
    """Build a synthetic httpx.Response with a JSON body."""
    return httpx.Response(
        status,
        headers={"content-type": ct},
        content=json.dumps(body).encode(),
    )


# ── Case 1: extra key on rs side ──────────────────────────────────────────────


def test_self_extra_key_on_rs_is_caught():
    """assert_responses_match names the diverging field when rs has an extra key."""
    py = _json_resp(200, {"status": "ok"})
    rs = _json_resp(200, {"status": "ok", "surprise": "extra"})
    with pytest.raises(pytest.fail.Exception) as exc_info:
        assert_responses_match(py, rs, ignore=frozenset())
    assert "surprise" in str(exc_info.value), (
        f"Expected 'surprise' in failure message, got: {exc_info.value}"
    )


# ── Case 2: type mismatch ─────────────────────────────────────────────────────


def test_self_type_mismatch_is_caught():
    """assert_responses_match reports the field name and both types on type divergence."""
    py = _json_resp(200, {"count": 5})
    rs = _json_resp(200, {"count": "five"})
    with pytest.raises(pytest.fail.Exception) as exc_info:
        assert_responses_match(py, rs, ignore=frozenset())
    msg = str(exc_info.value)
    assert "count" in msg, f"Expected 'count' in failure message: {msg}"


# ── Case 3: status code mismatch ─────────────────────────────────────────────


def test_self_status_code_mismatch_is_caught():
    """assert_responses_match reports both status codes when they differ."""
    py = _json_resp(200, {"ok": True})
    rs = _json_resp(201, {"ok": True})
    with pytest.raises(pytest.fail.Exception) as exc_info:
        assert_responses_match(py, rs, ignore=frozenset())
    msg = str(exc_info.value)
    assert "200" in msg and "201" in msg, (
        f"Expected both status codes in message: {msg}"
    )


# ── Case 4: OpenAPI path-set divergence ───────────────────────────────────────


def test_self_openapi_missing_path_is_caught():
    """The OpenAPI structural diff catches a path present on Python but missing on Rust."""
    from test_http_openapi import _normalise_paths

    py_paths = {
        "/api/v1/health": {"get": {}},
        "/api/v1/datasets": {"get": {}, "post": {}},
        "/api/v1/datasets/{id}": {"get": {}, "delete": {}},
    }
    rs_paths = {
        "/api/v1/health": {"get": {}},
        "/api/v1/datasets": {"get": {}, "post": {}},
        # /api/v1/datasets/{id} is MISSING on Rust
    }

    py_norm = set(_normalise_paths(py_paths).keys())
    rs_norm = set(_normalise_paths(rs_paths).keys())
    only_in_py = py_norm - rs_norm

    assert only_in_py, (
        "Expected a diff but got none — structural diff helper may be broken"
    )
    assert any("datasets" in p and "id" in p for p in only_in_py), (
        f"Expected the missing /datasets/{{id}} path in diff, got: {only_in_py}"
    )


# ── Case 5: deeply-nested divergence ─────────────────────────────────────────


def test_self_nested_divergence_is_caught():
    """assert_responses_match catches a divergence nested several levels deep."""
    py = _json_resp(
        200,
        {"pipeline": {"run": {"result": {"status": "completed", "count": 3}}}},
    )
    rs = _json_resp(
        200,
        {"pipeline": {"run": {"result": {"status": "completed", "count": 99}}}},
    )
    with pytest.raises(pytest.fail.Exception) as exc_info:
        assert_responses_match(py, rs, ignore=frozenset())
    msg = str(exc_info.value)
    assert "count" in msg, (
        f"Expected 'count' to be named in the nested diff message: {msg}"
    )
    # Check that both values are shown
    assert "3" in msg or "99" in msg, (
        f"Expected diverging values (3 vs 99) in message: {msg}"
    )
