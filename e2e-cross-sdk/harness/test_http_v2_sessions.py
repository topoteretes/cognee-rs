"""HTTP API v2 parity tests for ``GET /api/v1/sessions``,
``/sessions/stats`` and ``/sessions/cost-by-model``.

Per [`docs/http-api-v2/tasks/e-09-sessions-list.md`](../../docs/http-api-v2/tasks/e-09-sessions-list.md)
§5, [`e-10-sessions-stats.md`](../../docs/http-api-v2/tasks/e-10-sessions-stats.md)
§5, and [`e-11-sessions-cost-by-model.md`](../../docs/http-api-v2/tasks/e-11-sessions-cost-by-model.md)
§5 — verify byte-level Python parity on the sessions list + stats +
cost-by-model endpoints.

Coverage:
1. Empty-list happy path: ``GET /sessions?range=30d&limit=10`` returns
   the snake_case envelope ``{sessions, total, limit, offset, has_more}``
   on both backends with a structural diff.
2. ``?limit=999`` → both backends return a 400 validation envelope. The
   shapes are not byte-identical (Pydantic vs. our handler) but both
   include ``loc=["query","limit"]`` and ``type`` ending in
   ``value_error``.
3. ``GET /sessions/stats?range=30d`` returns the snake_case 14-field
   envelope (E-10). Structural diff against the Python backend.
4. ``GET /sessions/stats?range=all`` — same shape, different time window
   (the two stable input shapes; ``24h`` and ``7d`` are time-sensitive
   and skipped from the parity suite).
5. ``GET /sessions/cost-by-model?range=all`` — plain JSON array of
   5-field snake_case rows (E-11). Structural diff over the array shape
   (per-row keys + types); numeric values diverge across backends since
   fixtures aren't shared.

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


# ─── E-10 — GET /sessions/stats ───────────────────────────────────────────────
#
# 14-field snake_case envelope (Python parity carve-out — plain dict via
# `jsonable_encoder`, not an OutDTO). The numeric values depend on the test
# fixture's seeded session_records, which differ across backends, so the
# parity assertions are structural (key set + types) rather than value-level.
# Time-sensitive `?range=24h` / `?range=7d` are deliberately skipped from
# the cross-SDK suite (covered by Rust-side integration tests).

_STATS_KEYS = {
    "range",
    "sessions",
    "total_spend_usd",
    "avg_spend_per_session_usd",
    "tokens_in",
    "tokens_out",
    "tokens_total",
    "agent_time_s",
    "avg_session_s",
    "success_rate",
    "completed",
    "failed",
    "abandoned",
    "running",
}


def _assert_stats_envelope(side: str, body: dict, expected_range: str) -> None:
    """Common shape checks for the stats response — Python parity."""
    assert isinstance(body, dict), f"{side}: expected dict, got {type(body).__name__}"
    missing = _STATS_KEYS - set(body.keys())
    assert not missing, f"{side} stats envelope missing keys {missing!r}: {body}"
    assert body["range"] == expected_range, (
        f"{side} range echo mismatch: expected {expected_range!r}, got {body['range']!r}"
    )
    # Type checks — `int` / `float` Python parity. JSON numbers without a
    # fractional part decode as int in Python's json module, so allow both.
    for k in (
        "sessions",
        "tokens_in",
        "tokens_out",
        "tokens_total",
        "completed",
        "failed",
        "abandoned",
        "running",
    ):
        assert isinstance(body[k], int), f"{side} {k} must be int, got {type(body[k]).__name__}"
    for k in (
        "total_spend_usd",
        "avg_spend_per_session_usd",
        "agent_time_s",
        "avg_session_s",
        "success_rate",
    ):
        assert isinstance(body[k], (int, float)), (
            f"{side} {k} must be numeric, got {type(body[k]).__name__}"
        )


def test_sessions_stats_range_30d_structural_parity(authed_clients):
    """Both backends return the 14-field snake_case stats envelope for
    ``?range=30d`` (the default). Structural diff over the key set —
    numeric values diverge across backends since fixtures aren't shared.
    """
    py = authed_clients["py"].get("/api/v1/sessions/stats?range=30d")
    rs = authed_clients["rs"].get("/api/v1/sessions/stats?range=30d")

    assert py.status_code == 200, f"py /sessions/stats failed: {py.status_code} {py.text[:300]}"
    assert rs.status_code == 200, f"rs /sessions/stats failed: {rs.status_code} {rs.text[:300]}"

    py_body = py.json()
    rs_body = rs.json()

    _assert_stats_envelope("py", py_body, "30d")
    _assert_stats_envelope("rs", rs_body, "30d")

    # Structural diff — ignore numeric counter values; the empty fixture
    # may still differ because of side effects from earlier tests in the
    # run.
    assert_responses_match(
        py,
        rs,
        ignore=DEFAULT_IGNORE
        | {
            "$..sessions",
            "$..total_spend_usd",
            "$..avg_spend_per_session_usd",
            "$..tokens_in",
            "$..tokens_out",
            "$..tokens_total",
            "$..agent_time_s",
            "$..avg_session_s",
            "$..success_rate",
            "$..completed",
            "$..failed",
            "$..abandoned",
            "$..running",
        },
    )


# ─── E-11 — GET /sessions/cost-by-model ───────────────────────────────────────
#
# Plain JSON array of 5-field snake_case rows (Python parity carve-out — Python
# returns a list-of-dicts via `jsonable_encoder([...])`, not an OutDTO). Numeric
# values depend on the fixture's seeded `session_model_usage` rows, which differ
# across backends, so the parity assertions are structural (top-level array +
# per-row key set + types) rather than value-level.

_COST_BY_MODEL_KEYS = {
    "model",
    "session_count",
    "cost_usd",
    "tokens_in",
    "tokens_out",
}


def _assert_cost_by_model_envelope(side: str, body) -> None:
    """Common shape checks for the cost-by-model response — Python parity."""
    assert isinstance(body, list), f"{side}: expected list, got {type(body).__name__}"
    for i, row in enumerate(body):
        assert isinstance(row, dict), (
            f"{side} row[{i}]: expected dict, got {type(row).__name__}"
        )
        missing = _COST_BY_MODEL_KEYS - set(row.keys())
        assert not missing, f"{side} row[{i}] missing keys {missing!r}: {row}"
        assert isinstance(row["model"], str), (
            f"{side} row[{i}].model must be str, got {type(row['model']).__name__}"
        )
        for k in ("session_count", "tokens_in", "tokens_out"):
            assert isinstance(row[k], int), (
                f"{side} row[{i}].{k} must be int, got {type(row[k]).__name__}"
            )
        assert isinstance(row["cost_usd"], (int, float)), (
            f"{side} row[{i}].cost_usd must be numeric, got {type(row['cost_usd']).__name__}"
        )


def test_sessions_cost_by_model_range_all_structural_parity(authed_clients):
    """Both backends return the 5-field snake_case cost-by-model array
    for ``?range=all`` (no time filter). The body is a plain JSON array
    (not an envelope). Structural diff over the per-row key set —
    numeric values diverge across backends since fixtures aren't shared.
    """
    py = authed_clients["py"].get("/api/v1/sessions/cost-by-model?range=all")
    rs = authed_clients["rs"].get("/api/v1/sessions/cost-by-model?range=all")

    assert py.status_code == 200, (
        f"py /sessions/cost-by-model failed: {py.status_code} {py.text[:300]}"
    )
    assert rs.status_code == 200, (
        f"rs /sessions/cost-by-model failed: {rs.status_code} {rs.text[:300]}"
    )

    py_body = py.json()
    rs_body = rs.json()

    _assert_cost_by_model_envelope("py", py_body)
    _assert_cost_by_model_envelope("rs", rs_body)

    # Structural diff — ignore the array contents because fixtures aren't
    # shared across backends, but the top-level type (list) and per-row
    # shape are validated above.
    assert_responses_match(
        py,
        rs,
        ignore=DEFAULT_IGNORE
        | {
            # Drop the entire array body — row identities and numeric
            # values are non-deterministic across backends.
            "$",
        },
    )


def test_sessions_stats_range_all_structural_parity(authed_clients):
    """Same envelope shape for ``?range=all`` (no time filter). The
    ``range`` field echoes the input verbatim on both backends.
    """
    py = authed_clients["py"].get("/api/v1/sessions/stats?range=all")
    rs = authed_clients["rs"].get("/api/v1/sessions/stats?range=all")

    assert py.status_code == 200, f"py /sessions/stats failed: {py.status_code} {py.text[:300]}"
    assert rs.status_code == 200, f"rs /sessions/stats failed: {rs.status_code} {rs.text[:300]}"

    py_body = py.json()
    rs_body = rs.json()

    _assert_stats_envelope("py", py_body, "all")
    _assert_stats_envelope("rs", rs_body, "all")

    assert_responses_match(
        py,
        rs,
        ignore=DEFAULT_IGNORE
        | {
            "$..sessions",
            "$..total_spend_usd",
            "$..avg_spend_per_session_usd",
            "$..tokens_in",
            "$..tokens_out",
            "$..tokens_total",
            "$..agent_time_s",
            "$..avg_session_s",
            "$..success_rate",
            "$..completed",
            "$..failed",
            "$..abandoned",
            "$..running",
        },
    )
