"""Phase-2 parity tests for POST /api/v1/cognify (LLM-gated).

Requires: OPENAI_TOKEN or OPENAI_API_KEY in environment.

Per e2e-parity.md §5 phase-2 and p8-e2e-parity.md Step 10:
- Blocking cognify on a single dataset.
- Blocking cognify on multiple datasets.
- Error path: cognify on non-existent dataset → both must 404.

LLM output is non-deterministic; entity/relationship names are compared
using Jaccard similarity (≥ 50% overlap required) rather than strict equality,
matching the precedent in test_cognify_structural.py.
"""

from __future__ import annotations

import uuid

import pytest

from conftest import requires_openai
from http_helpers import DEFAULT_IGNORE, assert_responses_match
from seed import seed_dataset_with_text

pytestmark = [requires_openai]

_IGNORE = DEFAULT_IGNORE | {
    "$..pipeline_run_id",
    "$..started_at",
    "$..ended_at",
    "$..payload.entities[*].id",
    "$..payload.relationships[*].id",
    "$..run_info",
}

_SEED_TEXT = (
    "The knowledge graph represents relationships between entities.  "
    "Marie Curie discovered polonium and radium.  "
    "Albert Einstein developed the theory of relativity."
)


def _jaccard(a: list[str], b: list[str]) -> float:
    sa, sb = set(a), set(b)
    if not sa and not sb:
        return 1.0
    return len(sa & sb) / len(sa | sb)


def _extract_names(payload: dict | list | None, key: str = "name") -> list[str]:
    """Extract name strings from a payload list of dicts."""
    if isinstance(payload, list):
        return [item.get(key, "") for item in payload if isinstance(item, dict)]
    if isinstance(payload, dict):
        items = payload.get("entities") or payload.get("relationships") or []
        return [item.get(key, "") for item in items if isinstance(item, dict)]
    return []


def test_cognify_single_dataset_blocking(authed_clients, unique_dataset_name):
    """POST /api/v1/cognify blocking on a single dataset completes on both servers."""
    ds_ids: dict[str, str | None] = {}
    for side, client in authed_clients.items():
        resp = seed_dataset_with_text(client, name=unique_dataset_name, text=_SEED_TEXT)
        ds_ids[side] = resp.get("dataset_id") or resp.get("id")

    for side in ("py", "rs"):
        ds_id = ds_ids[side]
        if not ds_id:
            pytest.skip(f"No dataset_id in seed response for {side}")

    py = authed_clients["py"].post(
        "/api/v1/cognify",
        json={"datasets": [ds_ids["py"]], "run_in_background": False},
        timeout=300.0,
    )
    rs = authed_clients["rs"].post(
        "/api/v1/cognify",
        json={"datasets": [ds_ids["rs"]], "run_in_background": False},
        timeout=300.0,
    )
    assert py.status_code == rs.status_code, (
        f"cognify status mismatch: py={py.status_code} rs={rs.status_code}\n"
        f"py: {py.text[:400]}\nrs: {rs.text[:400]}"
    )
    assert py.status_code == 200

    # Structural equality (LLM names): Jaccard ≥ 0.5
    py_names = _extract_names(py.json().get("payload"), "name")
    rs_names = _extract_names(rs.json().get("payload"), "name")
    if py_names and rs_names:
        j = _jaccard(py_names, rs_names)
        assert j >= 0.5, (
            f"Entity-name Jaccard {j:.2f} < 0.5\n"
            f"py entities: {py_names}\nrs entities: {rs_names}"
        )


def test_cognify_multiple_datasets_blocking(authed_clients, unique_dataset_name):
    """POST /api/v1/cognify blocking on two datasets completes on both servers."""
    ds_ids_a: dict[str, str | None] = {}
    ds_ids_b: dict[str, str | None] = {}
    name_b = unique_dataset_name + "_b"
    for side, client in authed_clients.items():
        r_a = seed_dataset_with_text(client, name=unique_dataset_name, text=_SEED_TEXT)
        r_b = seed_dataset_with_text(client, name=name_b, text="Second dataset about space exploration.")
        ds_ids_a[side] = r_a.get("dataset_id") or r_a.get("id")
        ds_ids_b[side] = r_b.get("dataset_id") or r_b.get("id")

    py_datasets = [x for x in [ds_ids_a["py"], ds_ids_b["py"]] if x]
    rs_datasets = [x for x in [ds_ids_a["rs"], ds_ids_b["rs"]] if x]

    if not py_datasets or not rs_datasets:
        pytest.skip("Could not obtain dataset IDs for multi-dataset cognify")

    py = authed_clients["py"].post(
        "/api/v1/cognify",
        json={"datasets": py_datasets, "run_in_background": False},
        timeout=300.0,
    )
    rs = authed_clients["rs"].post(
        "/api/v1/cognify",
        json={"datasets": rs_datasets, "run_in_background": False},
        timeout=300.0,
    )
    assert py.status_code == rs.status_code, (
        f"multi-dataset cognify status mismatch: py={py.status_code} rs={rs.status_code}"
    )


def test_cognify_nonexistent_dataset_returns_404(authed_clients):
    """POST /api/v1/cognify on a non-existent dataset returns 404 on both servers."""
    nonexistent = str(uuid.uuid4())
    py = authed_clients["py"].post(
        "/api/v1/cognify",
        json={"datasets": [nonexistent], "run_in_background": False},
        timeout=60.0,
    )
    rs = authed_clients["rs"].post(
        "/api/v1/cognify",
        json={"datasets": [nonexistent], "run_in_background": False},
        timeout=60.0,
    )
    assert py.status_code == rs.status_code, (
        f"non-existent cognify status mismatch: py={py.status_code} rs={rs.status_code}"
    )
    assert py.status_code == 404, f"Expected 404, got py={py.status_code}"
