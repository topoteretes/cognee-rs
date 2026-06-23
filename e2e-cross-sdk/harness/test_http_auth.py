"""Phase-1 parity tests for /api/v1/auth/*.

Covers: register, login, /me, logout, /me post-logout (must 401), and the
JWT cross-server canary (issue token on Python, present to Rust, assert 200;
reverse direction too).

Per e2e-parity.md §7: the JWT cross-server canary is the fail-fast sentinel —
if it fails, every auth-using test is suspect.  It is not a structural diff
(``assert_responses_match``) — it asserts a 200 and checks the ``email`` field.

Skip behaviour (plan §6.1 / Phase-0 follow-up F3): every test in this module
is skipped against the OSS ``cognee-http-server`` build because the
``/api/v1/auth/*`` routes were moved to the closed
``cognee-cloud-rust/crates/cognee-http-cloud`` crate. The session-scoped
``auth_endpoints_available`` fixture (see ``conftest.py``) probes each server
once and the autouse ``_skip_when_auth_endpoints_missing`` fixture below
applies the skip uniformly to module-level tests, including the ones that
hit the auth endpoints directly via ``py_client``/``rs_client`` rather than
via the ``authed_clients`` fixture.
"""

import uuid

import pytest

from http_helpers import DEFAULT_IGNORE, assert_responses_match


@pytest.fixture(autouse=True)
def _skip_when_auth_endpoints_missing(auth_endpoints_available):
    """Skip every test in this module when either server lacks /api/v1/auth/*.

    The probe is session-scoped, so the per-test overhead is just a dict
    lookup. See ``conftest.py::auth_endpoints_available`` for the probe
    logic.
    """
    missing = [name for name, ok in auth_endpoints_available.items() if not ok]
    if missing:
        pytest.skip(
            "Cognee server(s) do not expose /api/v1/auth/* "
            f"(missing on: {', '.join(missing)}) — likely an OSS build. "
            "These tests cover the closed cognee-http-cloud auth surface; run them "
            "against a closed cognee-cloud-rust deployment (plan §6.1)."
        )


def _unique_email() -> str:
    return f"parity_{uuid.uuid4().hex[:8]}@example.com"


def _make_creds(email: str | None = None) -> dict:
    email = email or _unique_email()
    return {"email": email, "password": "StrongPass123!", "username": email}


# ── Register ──────────────────────────────────────────────────────────────────


def test_register(py_client, rs_client):
    """POST /api/v1/auth/register returns 200 on both servers."""
    creds = _make_creds()
    py = py_client.post(
        "/api/v1/auth/register",
        json={"email": creds["email"], "password": creds["password"], "is_verified": True},
    )
    rs = rs_client.post(
        "/api/v1/auth/register",
        json={"email": creds["email"], "password": creds["password"], "is_verified": True},
    )
    assert_responses_match(py, rs, ignore=DEFAULT_IGNORE)


# ── Login ─────────────────────────────────────────────────────────────────────


def test_login(py_client, rs_client):
    """POST /api/v1/auth/login (OAuth2 form) returns 200 with a token."""
    creds = _make_creds()
    # Register first
    for c in (py_client, rs_client):
        c.post(
            "/api/v1/auth/register",
            json={"email": creds["email"], "password": creds["password"], "is_verified": True},
        )
    # Login uses OAuth2 form (username field, not email)
    py = py_client.post(
        "/api/v1/auth/login",
        data={"username": creds["email"], "password": creds["password"]},
    )
    rs = rs_client.post(
        "/api/v1/auth/login",
        data={"username": creds["email"], "password": creds["password"]},
    )
    assert_responses_match(py, rs, ignore=DEFAULT_IGNORE)


# ── /me ───────────────────────────────────────────────────────────────────────


def test_me(authed_clients):
    """GET /api/v1/auth/me returns 200 with user info on both servers."""
    py = authed_clients["py"].get("/api/v1/auth/me")
    rs = authed_clients["rs"].get("/api/v1/auth/me")
    assert_responses_match(py, rs, ignore=DEFAULT_IGNORE | {"$..tenant_id", "$..owner_id"})


# ── Logout ────────────────────────────────────────────────────────────────────


def test_logout(authed_clients):
    """POST /api/v1/auth/logout returns 200 on both servers."""
    py = authed_clients["py"].post("/api/v1/auth/logout")
    rs = authed_clients["rs"].post("/api/v1/auth/logout")
    assert_responses_match(py, rs, ignore=DEFAULT_IGNORE)


def test_me_after_logout(py_client, rs_client):
    """GET /api/v1/auth/me returns 401 after logout."""
    creds = _make_creds()
    for c in (py_client, rs_client):
        c.post(
            "/api/v1/auth/register",
            json={"email": creds["email"], "password": creds["password"], "is_verified": True},
        )
        c.post("/api/v1/auth/login", data={"username": creds["email"], "password": creds["password"]})
        c.post("/api/v1/auth/logout")
    py = py_client.get("/api/v1/auth/me")
    rs = rs_client.get("/api/v1/auth/me")
    assert py.status_code == 401, f"Expected 401 after logout, got {py.status_code}"
    assert rs.status_code == 401, f"Expected 401 after logout, got {rs.status_code}"


# ── JWT cross-server canary ───────────────────────────────────────────────────


@pytest.mark.xfail(
    reason=(
        "Cross-SDK token portability requires a shared user identity. Both SDKs "
        "assign random uuid4 user ids (docs/http-server/routers/auth-register.md: "
        "'users.id is uuid4() ... Do not introduce uuid5(email)') and the harness "
        "runs each server against its own isolated DB, so a token minted by one "
        "server carries a sub that does not exist in the other server's DB -> 401. "
        "Making this pass needs a shared relational DB across both servers, which "
        "is a harness architecture change, not an SDK bug."
    ),
    strict=False,
)
def test_jwt_cross_compat(py_client, rs_client):
    """Issue a JWT on Python, present it to Rust's /me — must return 200.

    Then reverse: issue on Rust, present to Python.  This canary validates the
    shared JWT secret / audience contract.  If it fails, all auth-gated tests
    are suspect — fail immediately.

    Per e2e-parity.md §7: this test intentionally does NOT call
    ``assert_responses_match``; it only checks the 200 status and that the
    ``email`` in the response matches what was registered.
    """
    creds = _make_creds()
    for c in (py_client, rs_client):
        c.post(
            "/api/v1/auth/register",
            json={"email": creds["email"], "password": creds["password"], "is_verified": True},
        )

    # 1. Python issues the JWT → present to Rust
    py_login = py_client.post(
        "/api/v1/auth/login",
        data={"username": creds["email"], "password": creds["password"]},
    )
    assert py_login.status_code == 200, f"Python login failed: {py_login.text}"
    py_token = py_login.json().get("access_token")
    if py_token:
        me_on_rs = rs_client.get(
            "/api/v1/auth/me",
            headers={"Authorization": f"Bearer {py_token}"},
        )
        assert me_on_rs.status_code == 200, (
            f"JWT cross-compat FAILED: Python token rejected by Rust /me.\n"
            f"status={me_on_rs.status_code} body={me_on_rs.text[:300]}\n"
            "This means every auth-gated test is suspect."
        )
        assert me_on_rs.json().get("email") == creds["email"], (
            f"Email mismatch: expected {creds['email']!r}, got {me_on_rs.json().get('email')!r}"
        )

    # 2. Rust issues the JWT → present to Python
    rs_login = rs_client.post(
        "/api/v1/auth/login",
        data={"username": creds["email"], "password": creds["password"]},
    )
    assert rs_login.status_code == 200, f"Rust login failed: {rs_login.text}"
    rs_token = rs_login.json().get("access_token")
    if rs_token:
        me_on_py = py_client.get(
            "/api/v1/auth/me",
            headers={"Authorization": f"Bearer {rs_token}"},
        )
        assert me_on_py.status_code == 200, (
            f"JWT cross-compat FAILED: Rust token rejected by Python /me.\n"
            f"status={me_on_py.status_code} body={me_on_py.text[:300]}\n"
            "This means every auth-gated test is suspect."
        )
        assert me_on_py.json().get("email") == creds["email"], (
            f"Email mismatch: expected {creds['email']!r}, got {me_on_py.json().get('email')!r}"
        )

    if not py_token and not rs_token:
        pytest.skip(
            "Neither server returned access_token in login response — "
            "JWT cross-compat canary skipped (cookie-only auth mode?)"
        )
