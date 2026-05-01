"""HTTP API v2 parity tests for ``GET /api/v1/sessions``.

Per [`docs/http-api-v2/tasks/e-09-sessions-list.md`](../../docs/http-api-v2/tasks/e-09-sessions-list.md)
§5 — verify byte-level Python parity on the sessions list endpoint.

Coverage:
1. Empty-list happy path: ``GET /sessions?range=30d&limit=10`` returns
   the snake_case envelope ``{sessions, total, limit, offset, has_more}``
   on both backends with a structural diff.
2. ``?limit=999`` → both backends return a 400 validation envelope. The
   shapes are not byte-identical (Pydantic vs. our handler) but both
   include ``loc=["query","limit"]`` and ``type`` ending in
   ``value_error``.

Out of scope (per §6 acceptance criteria + Decision 9 / D-1):
- ``?order_by=banana`` is **not** part of the shared cross-SDK suite —
  Python silently falls back, Rust returns 400 (the only acknowledged
  Rust-only divergence in v2). The Rust-side behavior is covered by
  ``crates/http-server/tests/test_sessions_list.rs::list_order_by_invalid_returns_400_with_python_validation_envelope``.
"""

from __future__ import annotations

from http_helpers import DEFAULT_IGNORE, assert_responses_match


def test_sessions_list_empty_happy_path(authed_clients):
    """Both backends return a 200 envelope with the documented snake_case
    keys, even when the user has no sessions yet.
    """
    py = authed_clients["py"].get("/api/v1/sessions?range=30d&limit=10")
    rs = authed_clients["rs"].get("/api/v1/sessions?range=30d&limit=10")

    assert py.status_code == 200, f"py /sessions failed: {py.status_code} {py.text[:300]}"
    assert rs.status_code == 200, f"rs /sessions failed: {rs.status_code} {rs.text[:300]}"

    py_body = py.json()
    rs_body = rs.json()

    # Envelope shape — Python emits a plain dict (not OutDTO), so wire keys
    # are snake_case. Both backends must agree.
    for side, body in (("py", py_body), ("rs", rs_body)):
        assert isinstance(body, dict), f"{side}: expected dict, got {type(body).__name__}"
        for key in ("sessions", "total", "limit", "offset", "has_more"):
            assert key in body, f"{side} envelope missing snake_case key {key!r}: {body}"
        assert isinstance(body["sessions"], list)
        assert body["limit"] == 10
        assert body["offset"] == 0
        assert body["has_more"] in (False, True)

    # Structural diff (clients on a fresh test fixture should both be
    # empty; if one side seeded sessions earlier in the run, ignore the
    # session_id contents).
    assert_responses_match(
        py,
        rs,
        ignore=DEFAULT_IGNORE
        | {
            "$..sessions",  # row identities are non-deterministic across backends
            "$..total",  # depends on prior in-fixture seeding
            "$..has_more",
        },
    )


def test_sessions_list_limit_out_of_range_returns_400(authed_clients):
    """Both backends 400 on ``?limit=999``.

    Python's FastAPI emits its native validation envelope; Rust emits the
    Python-shaped envelope via ``ValidatedQuery`` + the handler-side
    ``1..=500`` check (Decision 7). The byte shapes are not strictly
    identical but both include ``loc=["query","limit"]`` and a
    value-error type.
    """
    py = authed_clients["py"].get("/api/v1/sessions?limit=999")
    rs = authed_clients["rs"].get("/api/v1/sessions?limit=999")

    assert py.status_code == 400, f"py expected 400, got {py.status_code}: {py.text[:300]}"
    assert rs.status_code == 400, f"rs expected 400, got {rs.status_code}: {rs.text[:300]}"

    for side, body in (("py", py.json()), ("rs", rs.json())):
        detail = body.get("detail")
        assert isinstance(detail, list) and detail, f"{side} body must have detail array: {body}"
        first = detail[0]
        loc = first.get("loc") or []
        assert "limit" in loc, f"{side} detail[0].loc should include 'limit', got {loc}"
        assert "query" in loc, f"{side} detail[0].loc should include 'query', got {loc}"
        ty = first.get("type", "")
        assert ty.endswith("value_error"), (
            f"{side} detail[0].type should end with value_error, got {ty!r}"
        )
