"""Phase-2 parity tests for POST /api/v1/improve (LLM-gated).

Requires: OPENAI_TOKEN or OPENAI_API_KEY in environment.

Per p8-e2e-parity.md Step 10:
- Improve with sessions present.
- Improve with no sessions (synchronous return, per pipelines.md §2).
"""

from __future__ import annotations

import pytest

from conftest import requires_openai
from http_helpers import DEFAULT_IGNORE, assert_responses_match
from seed import seed_cognify, seed_dataset_with_text

pytestmark = [requires_openai]

_IGNORE = DEFAULT_IGNORE | {
    "$..pipeline_run_id",
    "$..started_at",
    "$..ended_at",
    "$..run_info",
    "$..session_id",
}

_SEED_TEXT = (
    "The Rust programming language emphasizes memory safety and performance.  "
    "Python is widely used for data science and machine learning workloads.  "
    "Go was designed for simplicity and efficient concurrency."
)


def test_improve_with_sessions(authed_clients, unique_dataset_name):
    """POST /api/v1/improve after a cognify run completes on both servers."""
    ds_ids: dict[str, str | None] = {}
    for side, client in authed_clients.items():
        r = seed_dataset_with_text(client, name=unique_dataset_name, text=_SEED_TEXT)
        ds_id = r.get("dataset_id") or r.get("id")
        ds_ids[side] = ds_id
        if ds_id:
            seed_cognify(client, dataset_id=ds_id)

    payload = {"run_in_background": False}
    py = authed_clients["py"].post("/api/v1/improve", json=payload, timeout=300.0)
    rs = authed_clients["rs"].post("/api/v1/improve", json=payload, timeout=300.0)

    if py.status_code == 404 and rs.status_code == 404:
        pytest.skip("/api/v1/improve not yet implemented")

    assert py.status_code == rs.status_code, (
        f"improve status mismatch: py={py.status_code} rs={rs.status_code}\n"
        f"py: {py.text[:400]}\nrs: {rs.text[:400]}"
    )


def test_improve_no_sessions(authed_clients):
    """POST /api/v1/improve with no sessions is a synchronous no-op or empty return."""
    payload = {"run_in_background": False}
    py = authed_clients["py"].post("/api/v1/improve", json=payload, timeout=60.0)
    rs = authed_clients["rs"].post("/api/v1/improve", json=payload, timeout=60.0)

    if py.status_code == 404 and rs.status_code == 404:
        pytest.skip("/api/v1/improve not yet implemented")

    assert py.status_code == rs.status_code, (
        f"improve-no-sessions status mismatch: py={py.status_code} rs={rs.status_code}"
    )
