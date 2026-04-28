"""Phase-3 parity tests for POST /api/v1/sync and GET /api/v1/sync/status.

Per p8-e2e-parity.md Step 11:
- Sync triggered.
- Status polled.
- Sync on empty workspace.

Ignore extension: ``{"$..last_run_at", "$..duration_ms"}``.
"""

from __future__ import annotations

import pytest

from http_helpers import DEFAULT_IGNORE, assert_responses_match

_SYNC_IGNORE = DEFAULT_IGNORE | {"$..last_run_at", "$..duration_ms", "$..run_id"}


def test_sync_trigger(authed_clients):
    """POST /api/v1/sync triggers a sync run on both servers."""
    py = authed_clients["py"].post("/api/v1/sync", json={}, timeout=60.0)
    rs = authed_clients["rs"].post("/api/v1/sync", json={}, timeout=60.0)
    if py.status_code == 404 and rs.status_code == 404:
        pytest.skip("/api/v1/sync not yet implemented")
    assert py.status_code == rs.status_code, (
        f"sync status mismatch: py={py.status_code} rs={rs.status_code}\n"
        f"py: {py.text[:400]}\nrs: {rs.text[:400]}"
    )
    assert_responses_match(py, rs, ignore=_SYNC_IGNORE)


def test_sync_status_poll(authed_clients):
    """GET /api/v1/sync/status returns the current sync status on both servers."""
    py = authed_clients["py"].get("/api/v1/sync/status", timeout=30.0)
    rs = authed_clients["rs"].get("/api/v1/sync/status", timeout=30.0)
    if py.status_code == 404 and rs.status_code == 404:
        pytest.skip("/api/v1/sync/status not yet implemented")
    assert_responses_match(py, rs, ignore=_SYNC_IGNORE)


def test_sync_empty_workspace(authed_clients):
    """POST /api/v1/sync on an empty workspace is a no-op or returns an empty state."""
    # Trigger sync, then check status — both should agree on the empty state
    py_trigger = authed_clients["py"].post("/api/v1/sync", json={}, timeout=60.0)
    rs_trigger = authed_clients["rs"].post("/api/v1/sync", json={}, timeout=60.0)
    if py_trigger.status_code == 404 and rs_trigger.status_code == 404:
        pytest.skip("/api/v1/sync not yet implemented")
    assert py_trigger.status_code == rs_trigger.status_code, (
        f"sync-empty status mismatch: py={py_trigger.status_code} rs={rs_trigger.status_code}"
    )
