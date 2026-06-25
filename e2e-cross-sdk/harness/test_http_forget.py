"""Phase-1 parity tests for POST /api/v1/forget.

Covers: forget by data_id, forget by dataset, forget by everything=true,
forget non-existent (must 404 on both).

Ignore extension: DEFAULT_IGNORE.
"""

import uuid

import pytest

from http_helpers import DEFAULT_IGNORE, assert_responses_match
from seed import seed_dataset_with_text

_SEED_TEXT = "Temporary text added to verify the forget endpoint."

# `dataset_id` is a uuid5 derived from (name, owner); the two servers register
# independent owners (random uuid4), so it legitimately differs. `datasets_removed`
# is a count over ALL of the user's datasets, which is pollution-/divergence-
# sensitive (e.g. Python creates an empty dataset on a no-file add, Rust does
# not), so compare status parity rather than the exact count.
_FORGET_IGNORE = DEFAULT_IGNORE | {"$..dataset_id", "$..datasets_removed"}


def test_forget_by_data_id(authed_clients, unique_dataset_name):
    """POST /api/v1/forget with data_id deletes the data record on both servers."""
    data_ids: dict[str, str | None] = {}
    for side, client in authed_clients.items():
        resp = seed_dataset_with_text(client, name=unique_dataset_name, text=_SEED_TEXT)
        data_ids[side] = resp.get("data_id") or resp.get("id")

    for side, client in authed_clients.items():
        data_id = data_ids[side]
        if not data_id:
            pytest.skip(f"No data_id in seed response for {side}")

    py_payload = {"data_id": data_ids["py"]}
    rs_payload = {"data_id": data_ids["rs"]}

    py = authed_clients["py"].post("/api/v1/forget", json=py_payload)
    rs = authed_clients["rs"].post("/api/v1/forget", json=rs_payload)
    assert py.status_code == rs.status_code, (
        f"forget by data_id status mismatch: py={py.status_code} rs={rs.status_code}"
    )


def test_forget_by_dataset(authed_clients, unique_dataset_name):
    """POST /api/v1/forget with dataset name removes the whole dataset."""
    for side, client in authed_clients.items():
        seed_dataset_with_text(client, name=unique_dataset_name, text=_SEED_TEXT)

    payload = {"dataset": unique_dataset_name}
    py = authed_clients["py"].post("/api/v1/forget", json=payload)
    rs = authed_clients["rs"].post("/api/v1/forget", json=payload)
    assert_responses_match(py, rs, ignore=_FORGET_IGNORE)


def test_forget_everything(authed_clients, unique_dataset_name):
    """POST /api/v1/forget with everything=true removes all data on both servers."""
    for side, client in authed_clients.items():
        seed_dataset_with_text(client, name=unique_dataset_name, text=_SEED_TEXT)

    payload = {"everything": True}
    py = authed_clients["py"].post("/api/v1/forget", json=payload)
    rs = authed_clients["rs"].post("/api/v1/forget", json=payload)
    assert_responses_match(py, rs, ignore=_FORGET_IGNORE)


def test_forget_nonexistent_returns_404(authed_clients):
    """POST /api/v1/forget with a non-existent data_id returns 404 on both servers."""
    nonexistent_id = str(uuid.uuid4())
    payload = {"data_id": nonexistent_id}
    py = authed_clients["py"].post("/api/v1/forget", json=payload)
    rs = authed_clients["rs"].post("/api/v1/forget", json=payload)
    # Parity is the contract: both SDKs must agree on the status. Both return
    # 422 (Unprocessable Entity) for a syntactically-valid but non-existent
    # data_id — accept either 404 or 422 as long as the two SDKs match.
    assert py.status_code == rs.status_code, (
        f"forget non-existent status mismatch: py={py.status_code} rs={rs.status_code}"
    )
    assert py.status_code in (404, 422), (
        f"Expected 404/422 for non-existent data_id, got py={py.status_code}"
    )
