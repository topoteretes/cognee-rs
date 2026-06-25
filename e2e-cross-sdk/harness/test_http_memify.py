"""Phase-2 parity tests for POST /api/v1/memify (LLM-gated).

Requires: OPENAI_TOKEN or OPENAI_API_KEY in environment.

Per p8-e2e-parity.md Step 10:
- Memify on a cognified dataset.
- Memify on an empty dataset (graceful no-op).
"""

from __future__ import annotations

import uuid

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
}

_SEED_TEXT = (
    "Memify enriches the knowledge graph by creating triplet embeddings.  "
    "Each triplet represents a source entity, a relationship, and a target entity.  "
    "The embeddings are stored in the vector database for semantic retrieval."
)


def test_memify_on_cognified_dataset(authed_clients, unique_dataset_name):
    """POST /api/v1/memify on a cognified dataset completes on both servers."""
    ds_ids: dict[str, str | None] = {}
    for side, client in authed_clients.items():
        r = seed_dataset_with_text(client, name=unique_dataset_name, text=_SEED_TEXT)
        ds_id = r.get("dataset_id") or r.get("id")
        ds_ids[side] = ds_id
        if ds_id:
            seed_cognify(client, dataset_id=ds_id)

    py_ds = ds_ids.get("py")
    rs_ds = ds_ids.get("rs")
    if not py_ds or not rs_ds:
        pytest.skip("Could not obtain dataset IDs for memify test")

    py = authed_clients["py"].post(
        "/api/v1/memify",
        json={"dataset_id": py_ds, "run_in_background": False},
        timeout=300.0,
    )
    rs = authed_clients["rs"].post(
        "/api/v1/memify",
        json={"dataset_id": rs_ds, "run_in_background": False},
        timeout=300.0,
    )
    assert py.status_code == rs.status_code, (
        f"memify status mismatch: py={py.status_code} rs={rs.status_code}\n"
        f"py: {py.text[:400]}\nrs: {rs.text[:400]}"
    )


def test_memify_on_empty_dataset_is_noop(authed_clients, unique_dataset_name):
    """POST /api/v1/memify on an empty/nonexistent dataset is a graceful no-op on both servers."""
    nonexistent = str(uuid.uuid4())
    py = authed_clients["py"].post(
        "/api/v1/memify",
        json={"dataset_id": nonexistent, "run_in_background": False},
        timeout=60.0,
    )
    rs = authed_clients["rs"].post(
        "/api/v1/memify",
        json={"dataset_id": nonexistent, "run_in_background": False},
        timeout=60.0,
    )
    assert py.status_code == rs.status_code, (
        f"memify empty status mismatch: py={py.status_code} rs={rs.status_code}"
    )
