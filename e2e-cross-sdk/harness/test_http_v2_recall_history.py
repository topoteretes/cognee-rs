"""HTTP API v2 parity tests for ``GET /api/v1/recall`` (recall history).

Per [`docs/http-api-v2/tasks/e-03-recall-history.md`](../../docs/http-api-v2/tasks/e-03-recall-history.md)
§4.3 — verify byte-level Python parity on the recall-history endpoint, with
particular attention to the ``createdAt`` timestamp serialization (Decision 6,
``iso8601_offset`` helper).

Coverage:
1. Empty history on both backends -> ``200 []``.
2. Seeded history (via POST search) on both backends -> structural-diff plus
   byte-equality assertion on ``createdAt`` (must end with ``+00:00`` and parse
   identically as a Python ``datetime``).

The 500 error envelope path is non-deterministic across backends (Rust's
"orchestrator not wired" branch vs Python's exception-driven path) and is
covered by ``crates/http-server/tests/test_recall.rs`` instead.
"""

from __future__ import annotations

from datetime import datetime

import pytest

from http_helpers import DEFAULT_IGNORE, assert_responses_match
from seed import seed_cognify, seed_dataset_with_text

_SEED_TEXT = (
    "The knowledge graph stores entities and their relationships. "
    "Cognee ingests text, extracts facts, and links them in a graph. "
    "This enables semantic search over structured knowledge."
)

# Recall-history rows carry per-server primary keys + LLM-shaped result text.
# Strip those so the structural diff focuses on the wire envelope.
_HISTORY_IGNORE = DEFAULT_IGNORE | {
    "$..tenant_id",
    "$..owner_id",
}


def _parse_iso8601_offset(s: str) -> datetime:
    """Parse a Decision-6-shaped timestamp string into a ``datetime``.

    Both ``+00:00`` and ``Z`` suffixes are accepted (lenient deserialization
    parity), but Python's stdlib ``fromisoformat`` only handles ``+00:00`` /
    ``Z`` from 3.11+. We normalise ``Z`` -> ``+00:00`` for portability.
    """
    return datetime.fromisoformat(s.replace("Z", "+00:00"))


def test_recall_history_empty_returns_200_empty_array(authed_clients):
    """Both backends return ``200 []`` when the user has no recall history.

    Decision 6 helper: the empty array shape must match byte-for-byte.
    """
    py = authed_clients["py"].get("/api/v1/recall")
    rs = authed_clients["rs"].get("/api/v1/recall")

    assert py.status_code == 200, f"py /recall failed: {py.status_code} {py.text[:300]}"
    assert rs.status_code == 200, f"rs /recall failed: {rs.status_code} {rs.text[:300]}"
    assert py.json() == [], f"py recall history must be empty, got: {py.json()}"
    assert rs.json() == [], f"rs recall history must be empty, got: {rs.json()}"
    assert_responses_match(py, rs, ignore=_HISTORY_IGNORE)


def test_recall_history_after_search_has_createdat_offset(
    authed_clients, unique_dataset_name
):
    """Seed both backends with one POST /search, then GET /recall.

    Asserts:
    - status code 200 on both sides;
    - structural envelope equivalence;
    - every row's ``createdAt`` serializes with an explicit ``+00:00`` offset
      (Decision 6 — ``iso8601_offset`` helper, NOT chrono's default ``Z``).
    """
    # Bootstrap: ensure each backend has a dataset to search against. The
    # search itself is the "seed" for the recall-history table — POST /search
    # writes both a Query and a Result row to the user's history.
    for side, client in authed_clients.items():
        resp = seed_dataset_with_text(
            client, name=unique_dataset_name, text=_SEED_TEXT
        )
        ds_id = resp.get("dataset_id") or resp.get("id")
        if ds_id:
            # Best-effort cognify so Chunks search has something to retrieve;
            # if cognify is gated (e.g. no LLM), the history row from POST
            # /search will still be written even on a 0-result search.
            try:
                seed_cognify(client, dataset_id=ds_id)
            except AssertionError:
                pass

    # POST /search — Chunks is LLM-free per test_http_search.py.
    search_payload = {"search_type": "CHUNKS", "query": "knowledge graph"}
    py_post = authed_clients["py"].post("/api/v1/search", json=search_payload)
    rs_post = authed_clients["rs"].post("/api/v1/search", json=search_payload)
    assert py_post.status_code == 200, (
        f"py POST /search failed: {py_post.status_code} {py_post.text[:300]}"
    )
    assert rs_post.status_code == 200, (
        f"rs POST /search failed: {rs_post.status_code} {rs_post.text[:300]}"
    )

    # GET /recall on both backends.
    py = authed_clients["py"].get("/api/v1/recall")
    rs = authed_clients["rs"].get("/api/v1/recall")
    assert py.status_code == 200, f"py /recall failed: {py.text[:300]}"
    assert rs.status_code == 200, f"rs /recall failed: {rs.text[:300]}"

    py_body = py.json()
    rs_body = rs.json()
    assert isinstance(py_body, list) and len(py_body) >= 1, (
        f"py recall history must contain at least one row, got: {py_body}"
    )
    assert isinstance(rs_body, list) and len(rs_body) >= 1, (
        f"rs recall history must contain at least one row, got: {rs_body}"
    )

    # Structural envelope diff (createdAt left in by default since DEFAULT_IGNORE
    # only strips snake_case `created_at`, not the camelCase `createdAt` wire key).
    # Strip createdAt for the structural diff — its content is non-deterministic
    # across servers (clock skew + LLM latency); we re-assert byte-shape per row
    # below.
    assert_responses_match(
        py, rs, ignore=_HISTORY_IGNORE | {"$..createdAt", "$..text"}
    )

    # Decision 6: every wire-visible DateTime<Utc> must serialize with an
    # explicit `+00:00` offset (Python `datetime.isoformat()` parity).
    for side, body in (("py", py_body), ("rs", rs_body)):
        for i, row in enumerate(body):
            assert "createdAt" in row, (
                f"{side}[{i}] missing 'createdAt' field: {row!r}"
            )
            assert "created_at" not in row, (
                f"{side}[{i}] leaked snake_case 'created_at': {row!r}"
            )
            ts = row["createdAt"]
            assert isinstance(ts, str), (
                f"{side}[{i}].createdAt must be a string, got: {type(ts).__name__}"
            )
            assert ts.endswith("+00:00"), (
                f"{side}[{i}].createdAt must end with +00:00 offset (Decision 6), "
                f"got: {ts!r}"
            )
            # Round-trip parse to confirm byte-level shape is RFC 3339.
            _ = _parse_iso8601_offset(ts)
