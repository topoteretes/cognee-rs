"""Phase-1 parity tests for /health and /health/detailed.

No auth required.  Ignore extension: ``{"$..version"}`` — release versions
differ between the Python and Rust SDKs.
"""

import os

import pytest

from http_helpers import DEFAULT_IGNORE, assert_responses_match

# The detailed report's per-component breakdown is intrinsically
# backend-specific: Python runs lancedb/ladybug, Rust runs qdrant/ladybug, the
# `provider`/`details` strings and `response_time_ms` differ, and local ONNX
# embeddings can be healthy on one SDK while the other reports the provider
# unavailable. The meaningful cross-SDK contract is the *overall* verdict
# (status code + top-level `status`), so we ignore the volatile/backend-specific
# fields (`components`, `timestamp`, `uptime`, `version`).
_HEALTH_IGNORE = DEFAULT_IGNORE | {
    "$..version",
    "$.components",
    "$..timestamp",
    "$..uptime",
}


# ── Happy-path health checks ──────────────────────────────────────────────────


def test_health_basic(py_client, rs_client):
    """GET /health returns 200 HEALTHY on both servers."""
    py = py_client.get("/health")
    rs = rs_client.get("/health")
    assert_responses_match(py, rs, ignore=_HEALTH_IGNORE)


def test_health_detailed(py_client, rs_client):
    """GET /health/detailed returns 200 with component breakdown."""
    py = py_client.get("/health/detailed")
    rs = rs_client.get("/health/detailed")
    assert_responses_match(py, rs, ignore=_HEALTH_IGNORE)


# ── Forced-UNHEALTHY mode (requires COGNEE_TEST_FORCE_UNHEALTHY=1) ────────────


@pytest.mark.skipif(
    not os.environ.get("COGNEE_TEST_FORCE_UNHEALTHY"),
    reason="COGNEE_TEST_FORCE_UNHEALTHY not set — skipping forced-unhealthy tests",
)
def test_health_basic_unhealthy(py_client, rs_client):
    """GET /health returns a non-200 UNHEALTHY response when forced unhealthy."""
    py = py_client.get("/health")
    rs = rs_client.get("/health")
    # Both should agree on an unhealthy status code (typically 503)
    assert py.status_code == rs.status_code, (
        f"Health status mismatch: py={py.status_code} rs={rs.status_code}"
    )
    assert py.status_code != 200, "Expected UNHEALTHY (non-200) but got 200"


@pytest.mark.skipif(
    not os.environ.get("COGNEE_TEST_FORCE_UNHEALTHY"),
    reason="COGNEE_TEST_FORCE_UNHEALTHY not set — skipping forced-unhealthy tests",
)
def test_health_detailed_unhealthy(py_client, rs_client):
    """GET /health/detailed returns UNHEALTHY on both servers when forced."""
    py = py_client.get("/health/detailed")
    rs = rs_client.get("/health/detailed")
    assert py.status_code == rs.status_code, (
        f"Detailed health status mismatch: py={py.status_code} rs={rs.status_code}"
    )
    assert py.status_code != 200, "Expected UNHEALTHY (non-200) but got 200"
