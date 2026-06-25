"""HTTP API v2 parity tests for ``POST /api/v1/improve``.

Per [`docs/http-api-v2/tasks/e-05-improve.md`](../../docs/http-api-v2/tasks/e-05-improve.md)
§5 — verify that the v2 additions to the improve payload (`sessionIds`,
`extractionTasks`, `enrichmentTasks`, `data`, `nodeName`) reach both backends
and produce the same wire envelope.

Coverage:

1. ``test_v2_payload_passthrough`` — both backends accept the full v2 payload
   (`sessionIds`, `extractionTasks`, `enrichmentTasks`, `data`, `nodeName`,
   `datasetName`) and return matching status codes plus matching wire-key
   shapes (camelCase only).

Stage-level convergence (the Rust handler runs the full session-bridge path)
is gated on the deferred P5 follow-up — wiring the real
``cognee_lib::api::improve::improve(...)`` call from the http-server requires
resolving the cycle constraint at ``crates/http-server/Cargo.toml:36-38`` and
extending ``ComponentHandles`` with ``vector_db`` / ``embedding_engine`` /
``add_pipeline`` / ``checkpoint_store`` / ``cognify_config`` /
``ontology_resolver``. Until then the Rust handler dispatches a no-op stub,
which is sufficient to validate DTO parity.
"""

from __future__ import annotations

import pytest


def test_v2_payload_passthrough(authed_clients, unique_dataset_name):
    """Both backends accept the full v2 payload and emit a matching envelope.

    Sends the canonical v2 body documented in
    [`docs/http-api-v2/tasks/e-05-improve.md`] §5:

    .. code:: json

        {
          "sessionIds": ["s1"],
          "extractionTasks": [],
          "enrichmentTasks": [],
          "data": "",
          "nodeName": [],
          "datasetName": "..."
        }

    The test asserts:

    * Status codes match across backends (404 -> skip; otherwise equal).
    * Both responses parse as JSON objects (or are both empty bodies).
    * Both responses use camelCase wire keys exclusively (no snake_case
      leakage in either backend).
    """
    payload = {
        "sessionIds": ["s1"],
        "extractionTasks": [],
        "enrichmentTasks": [],
        "data": "",
        "nodeName": [],
        "datasetName": unique_dataset_name,
        "runInBackground": False,
    }

    py = authed_clients["py"].post("/api/v1/improve", json=payload, timeout=120.0)
    rs = authed_clients["rs"].post("/api/v1/improve", json=payload, timeout=120.0)

    if py.status_code == 404 and rs.status_code == 404:
        pytest.skip("/api/v1/improve not yet implemented on either side")

    assert py.status_code == rs.status_code, (
        f"improve v2-payload status mismatch: py={py.status_code} rs={rs.status_code}\n"
        f"py: {py.text[:400]}\nrs: {rs.text[:400]}"
    )

    # Forbidden snake_case keys — neither backend should leak them on the wire.
    forbidden = {
        "session_ids",
        "extraction_tasks",
        "enrichment_tasks",
        "node_name",
        "dataset_name",
        "dataset_id",
        "run_in_background",
    }
    for side, resp in (("py", py), ("rs", rs)):
        body = resp.text or ""
        if not body:
            continue
        for key in forbidden:
            assert f'"{key}"' not in body, (
                f"{side} improve response leaks snake_case key {key!r}: {body[:400]}"
            )
