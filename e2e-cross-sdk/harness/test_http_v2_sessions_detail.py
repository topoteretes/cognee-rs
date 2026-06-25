"""HTTP API v2 parity test for ``GET /api/v1/sessions/{session_id}`` (E-12).

Per [`docs/http-api-v2/tasks/e-12-sessions-detail.md`](../../docs/http-api-v2/tasks/e-12-sessions-detail.md)
§5 — verify byte-level Python parity on the sessions detail endpoint.

Coverage:

1. **Unknown session id → 404 ``{"detail":"session not found"}``** on both
   backends. This is the **only** v2 endpoint that intentionally emits the
   ``{detail}`` envelope (Python ``HTTPException(404, ...)`` parity); every
   other catch-all in the sessions router uses ``{error}``.
2. **Happy-path structural parity** when a real session row exists. The
   parity surface is the row body keys (12 `SessionRecord.to_dict()` keys
   + ``effective_status``) plus the five extras (``label``, ``msg_count``,
   ``tool_calls``, ``qas``, ``traces``) — all snake_case (Python plain
   dict via ``jsonable_encoder``). Numeric counters and timestamps differ
   across backends; the parity assertion is over the key set + types.

Out of scope:

- Time-sensitive abandonment thresholds (covered by Rust-side integration
  tests in ``crates/http-server/tests/test_sessions_detail.rs``).
- Owner-aware cache content parity — fixtures aren't shared across
  backends so the actual ``qas`` / ``traces`` lists differ.
"""

from __future__ import annotations

from http_helpers import DEFAULT_IGNORE, assert_responses_match


_REQUIRED_DETAIL_KEYS = {
    # SessionRecord.to_dict() — 12 keys.
    "session_id",
    "user_id",
    "dataset_id",
    "status",
    "started_at",
    "last_activity_at",
    "ended_at",
    "tokens_in",
    "tokens_out",
    "cost_usd",
    "error_count",
    "last_model",
    # Read-time effective status from SessionRowWithStatus.
    "effective_status",
    # Five detail extras.
    "label",
    "msg_count",
    "tool_calls",
    "qas",
    "traces",
}


def test_sessions_detail_unknown_session_returns_404_with_detail_envelope(authed_clients):
    """Both backends 404 with ``{detail: "session not found"}`` when the
    session id is unknown. This is the only v2 endpoint that emits the
    ``{detail}`` envelope (Python FastAPI ``HTTPException`` parity).
    """
    sid = "cc_unknown_e2e_zzzzz"
    py = authed_clients["py"].get(f"/api/v1/sessions/{sid}")
    rs = authed_clients["rs"].get(f"/api/v1/sessions/{sid}")

    assert py.status_code == 404, f"py expected 404, got {py.status_code}: {py.text[:300]}"
    assert rs.status_code == 404, f"rs expected 404, got {rs.status_code}: {rs.text[:300]}"

    for side, body in (("py", py.json()), ("rs", rs.json())):
        assert isinstance(body, dict), f"{side}: expected dict, got {type(body).__name__}"
        assert body.get("detail") == "session not found", (
            f"{side} 404 envelope must be {{detail: 'session not found'}}: {body}"
        )
        # Must NOT use the {error} envelope (the rest of this router does).
        assert "error" not in body, (
            f"{side} 404 envelope must NOT include 'error' key (FastAPI HTTPException parity): {body}"
        )

    # Structural diff — both bodies should be identical.
    assert_responses_match(py, rs, ignore=DEFAULT_IGNORE)


def _assert_detail_envelope(side: str, body: dict, session_id: str) -> None:
    """Common shape checks for the detail response — Python parity."""
    assert isinstance(body, dict), f"{side}: expected dict, got {type(body).__name__}"
    missing = _REQUIRED_DETAIL_KEYS - set(body.keys())
    assert not missing, f"{side} detail envelope missing keys {missing!r}: {body}"
    assert body["session_id"] == session_id, (
        f"{side} session_id echo mismatch: expected {session_id!r}, got {body['session_id']!r}"
    )
    # Type checks.
    assert isinstance(body["status"], str)
    assert isinstance(body["effective_status"], str)
    assert isinstance(body["msg_count"], int), (
        f"{side} msg_count must be int, got {type(body['msg_count']).__name__}"
    )
    assert isinstance(body["tool_calls"], int), (
        f"{side} tool_calls must be int, got {type(body['tool_calls']).__name__}"
    )
    assert isinstance(body["qas"], list)
    assert isinstance(body["traces"], list)
    # Tail-truncation cap.
    assert len(body["qas"]) <= 20, f"{side} qas must cap at 20: got {len(body['qas'])}"
    assert len(body["traces"]) <= 20, f"{side} traces must cap at 20: got {len(body['traces'])}"
    # `label` is `Optional[str]`.
    assert body["label"] is None or isinstance(body["label"], str)
    # Numeric fields parity.
    for k in ("tokens_in", "tokens_out", "error_count"):
        assert isinstance(body[k], int), (
            f"{side} {k} must be int, got {type(body[k]).__name__}"
        )
    assert isinstance(body["cost_usd"], (int, float)), (
        f"{side} cost_usd must be numeric, got {type(body['cost_usd']).__name__}"
    )


def test_sessions_detail_known_session_structural_parity(authed_clients, http_e2e_helpers):
    """When a session row exists on both backends, the detail body shape
    matches across SDKs (snake_case keys, key set, types). Numeric counters
    and timestamps differ across fixtures, so the diff is structural.

    The fixture path used here mirrors the existing E-09/E-10/E-11 cross-
    SDK tests: ``http_e2e_helpers.bootstrap_session(...)`` writes a single
    ``session_records`` row through each backend's HTTP API (or skips the
    test if the helper isn't available, leaving the 404 case as the only
    parity assertion).
    """
    bootstrap = getattr(http_e2e_helpers, "bootstrap_session", None)
    if bootstrap is None:
        # Helper not present in this branch's harness; the 404 parity case
        # above is the operative parity assertion. Skip the happy-path
        # check rather than fail the cross-SDK suite over harness drift.
        import pytest

        pytest.skip("http_e2e_helpers.bootstrap_session not available")

    session_id = "cc_e2e_detail_known_1"
    bootstrap(authed_clients["py"], session_id)
    bootstrap(authed_clients["rs"], session_id)

    py = authed_clients["py"].get(f"/api/v1/sessions/{session_id}")
    rs = authed_clients["rs"].get(f"/api/v1/sessions/{session_id}")

    assert py.status_code == 200, f"py /sessions/{{id}} failed: {py.status_code} {py.text[:300]}"
    assert rs.status_code == 200, f"rs /sessions/{{id}} failed: {rs.status_code} {rs.text[:300]}"

    py_body = py.json()
    rs_body = rs.json()

    _assert_detail_envelope("py", py_body, session_id)
    _assert_detail_envelope("rs", rs_body, session_id)

    # `label`, `msg_count`, `tool_calls`, and per-row `effective_status`
    # parity is a function of the (synchronously-bootstrapped) row state —
    # both sides should agree on these for an empty cache.
    assert py_body["label"] == rs_body["label"], (
        f"label mismatch: py={py_body['label']!r} rs={rs_body['label']!r}"
    )
    assert py_body["msg_count"] == rs_body["msg_count"], (
        f"msg_count mismatch: py={py_body['msg_count']} rs={rs_body['msg_count']}"
    )
    assert py_body["tool_calls"] == rs_body["tool_calls"], (
        f"tool_calls mismatch: py={py_body['tool_calls']} rs={rs_body['tool_calls']}"
    )
    assert py_body["effective_status"] == rs_body["effective_status"], (
        f"effective_status mismatch: py={py_body['effective_status']!r} rs={rs_body['effective_status']!r}"
    )

    # Structural diff — ignore non-deterministic identifiers and timestamps
    # that differ across backends. The key set + types are already
    # validated by `_assert_detail_envelope` above.
    assert_responses_match(
        py,
        rs,
        ignore=DEFAULT_IGNORE
        | {
            "$.user_id",  # user UUIDs differ across backends
            "$.dataset_id",
            "$.started_at",
            "$.last_activity_at",
            "$.ended_at",
            "$.tokens_in",
            "$.tokens_out",
            "$.cost_usd",
            "$.error_count",
            "$.last_model",
            "$.qas",
            "$.traces",
        },
    )
