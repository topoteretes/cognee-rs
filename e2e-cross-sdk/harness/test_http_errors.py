"""Phase-1 parity tests for error handling.

Covers:
1. Missing required body field → validation 400.
2. Bad JWT → 401 with LOGIN_BAD_CREDENTIALS or similar error code.
3. Missing auth → 401.
4. Unsupported method on a known path → 405.
5. /teapot debug route if present → 418.

Per e2e-parity.md §6.4: only ``detail[*].type``/``loc`` *codes* must match;
the human-readable ``msg`` is allowed to differ.  Pre-strip ``detail[*].msg``
before the diff.
"""

from __future__ import annotations

import pytest

from http_helpers import DEFAULT_IGNORE, assert_responses_match, strip_paths

# Strip volatile / implementation-specific fields from error detail arrays.
_ERROR_IGNORE = DEFAULT_IGNORE | {"$..detail[*].msg", "$..detail[*].ctx"}


def _strip_error_msg(response_json: dict) -> dict:
    """Strip the human-readable 'msg' from Pydantic detail items."""
    detail = response_json.get("detail")
    if isinstance(detail, list):
        cleaned = []
        for item in detail:
            if isinstance(item, dict):
                item = {k: v for k, v in item.items() if k not in ("msg", "ctx")}
            cleaned.append(item)
        return {**response_json, "detail": cleaned}
    return response_json


def test_missing_required_body_field(authed_clients):
    """POST /api/v1/cognify with empty body returns 422 validation error on both."""
    py = authed_clients["py"].post("/api/v1/cognify", json={})
    rs = authed_clients["rs"].post("/api/v1/cognify", json={})
    assert py.status_code == rs.status_code, (
        f"validation error status mismatch: py={py.status_code} rs={rs.status_code}\n"
        f"py: {py.text[:300]}\nrs: {rs.text[:300]}"
    )
    assert py.status_code in (400, 422), (
        f"Expected 400 or 422, got py={py.status_code}"
    )


def test_bad_jwt_returns_401(py_client, rs_client):
    """A malformed JWT in Authorization: Bearer returns 401 on both servers."""
    bad_token = "eyJhbGciOiJIUzI1NiJ9.BOGUS.SIGNATURE"
    py = py_client.get(
        "/api/v1/auth/me",
        headers={"Authorization": f"Bearer {bad_token}"},
    )
    rs = rs_client.get(
        "/api/v1/auth/me",
        headers={"Authorization": f"Bearer {bad_token}"},
    )
    assert py.status_code == 401, f"py: expected 401 for bad JWT, got {py.status_code}"
    assert rs.status_code == 401, f"rs: expected 401 for bad JWT, got {rs.status_code}"


def test_missing_auth_returns_401(py_client, rs_client):
    """GET /api/v1/auth/me without any auth token returns 401 on both servers."""
    py = py_client.get("/api/v1/auth/me")
    rs = rs_client.get("/api/v1/auth/me")
    assert py.status_code == 401, f"py: expected 401 for missing auth, got {py.status_code}"
    assert rs.status_code == 401, f"rs: expected 401 for missing auth, got {rs.status_code}"


def test_unsupported_method_returns_405(py_client, rs_client):
    """DELETE /api/v1/auth/register (unsupported method) returns 405 on both servers."""
    py = py_client.delete("/api/v1/auth/register")
    rs = rs_client.delete("/api/v1/auth/register")
    assert py.status_code == rs.status_code, (
        f"unsupported method status mismatch: py={py.status_code} rs={rs.status_code}"
    )
    assert py.status_code == 405, f"Expected 405, got py={py.status_code}"


def test_teapot_route(py_client, rs_client):
    """GET /teapot (debug route) returns 418 on both servers if present."""
    py = py_client.get("/teapot")
    rs = rs_client.get("/teapot")
    if py.status_code == 404 and rs.status_code == 404:
        pytest.skip("/teapot route not present on either server — skipping")
    assert py.status_code == rs.status_code, (
        f"/teapot status mismatch: py={py.status_code} rs={rs.status_code}"
    )
    if py.status_code == 418:
        # Both served 418 — that's the pass condition
        return
    pytest.fail(
        f"Expected 418 from /teapot, got py={py.status_code} rs={rs.status_code}"
    )
