"""Phase-2 parity tests for ontology-enabled HTTP workflow.

Flow (both Python and Rust servers):
1. Upload ontology via POST /api/v1/ontologies.
2. Seed dataset via POST /api/v1/add.
3. Cognify with ontology key(s) via POST /api/v1/cognify (blocking).
4. Search via POST /api/v1/search.

Assertions are parity-oriented and tolerant to LLM nondeterminism:
- Status-code parity for each step.
- Unknown ontology key returns 404 on both servers.
- Search payloads are non-empty and contain at least one ontology marker.
"""

from __future__ import annotations

import uuid

import pytest

from conftest import requires_openai
from seed import seed_dataset_with_text

pytestmark = [requires_openai]

_ONTOLOGY_BODY = """@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix : <http://test.cognee.ai/ontology#> .

:LegalEntity a owl:Class ;
    rdfs:label \"LegalEntity\" .

:Organisation a owl:Class ;
    rdfs:subClassOf :LegalEntity ;
    rdfs:label \"Organisation\" .

:Technology a owl:Class ;
    rdfs:label \"Technology\" .

:Algorithm a owl:Class ;
    rdfs:subClassOf :Technology ;
    rdfs:label \"Algorithm\" .
"""

_SEED_TEXT = (
    "TechCorp is an Organisation building an Algorithm-driven platform. "
    "DeepSort is an Algorithm used by TechCorp for Technology ranking."
)

_ONTOLOGY_MARKERS = (
    "algorithm",
    "technology",
    "organisation",
    "legalentity",
    "is_a",
)


def _upload_ontology(client, key: str):
    return client.post(
        "/api/v1/ontologies",
        files={
            "ontology_file": ("tech.owl", _ONTOLOGY_BODY.encode(), "application/rdf+xml")
        },
        data={"ontology_key": key},
    )


def _extract_dataset_id(seed_response: dict) -> str | None:
    return seed_response.get("dataset_id") or seed_response.get("id")


def _payload_blob(value) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value.lower()
    return str(value).lower()


def _search_has_ontology_markers(search_json) -> bool:
    if not isinstance(search_json, list) or not search_json:
        return False

    blobs: list[str] = []
    for item in search_json:
        if not isinstance(item, dict):
            continue
        for key in ("searchResult", "search_result", "textResult", "text_result"):
            if key in item:
                blobs.append(_payload_blob(item.get(key)))

    haystack = " ".join(blobs)
    return any(marker in haystack for marker in _ONTOLOGY_MARKERS)


def test_ontology_upload_cognify_search_parity(authed_clients, unique_dataset_name):
    """Parity check for upload->cognify(ontologyKey)->search workflow."""
    ontology_key = f"tech-{uuid.uuid4().hex[:8]}"

    # 1. Upload ontology on both servers.
    py_upload = _upload_ontology(authed_clients["py"], ontology_key)
    rs_upload = _upload_ontology(authed_clients["rs"], ontology_key)

    assert py_upload.status_code == rs_upload.status_code, (
        f"ontology upload status mismatch: py={py_upload.status_code} rs={rs_upload.status_code}\n"
        f"py: {py_upload.text[:400]}\nrs: {rs_upload.text[:400]}"
    )
    assert py_upload.status_code == 200

    # 2. Seed datasets on both sides.
    dataset_ids: dict[str, str | None] = {}
    for side, client in authed_clients.items():
        seed_resp = seed_dataset_with_text(client, name=unique_dataset_name, text=_SEED_TEXT)
        dataset_ids[side] = _extract_dataset_id(seed_resp)

    for side in ("py", "rs"):
        if not dataset_ids[side]:
            pytest.skip(f"No dataset ID in seed response for {side}")

    # 3. Cognify with ontology key on both servers.
    py_cognify = authed_clients["py"].post(
        "/api/v1/cognify",
        json={
            "datasets": [dataset_ids["py"]],
            "ontologyKey": [ontology_key],
            "run_in_background": False,
        },
        timeout=300.0,
    )
    rs_cognify = authed_clients["rs"].post(
        "/api/v1/cognify",
        json={
            "datasets": [dataset_ids["rs"]],
            "ontologyKey": [ontology_key],
            "run_in_background": False,
        },
        timeout=300.0,
    )

    assert py_cognify.status_code == rs_cognify.status_code, (
        f"ontology cognify status mismatch: py={py_cognify.status_code} rs={rs_cognify.status_code}\n"
        f"py: {py_cognify.text[:400]}\nrs: {rs_cognify.text[:400]}"
    )
    assert py_cognify.status_code == 200

    # 4. Search on both servers and verify ontology-aware semantic markers.
    search_payload = {
        "query": "Algorithm Technology",
        "search_type": "GRAPH_COMPLETION",
    }
    py_search = authed_clients["py"].post(
        "/api/v1/search",
        json=search_payload,
        timeout=180.0,
    )
    rs_search = authed_clients["rs"].post(
        "/api/v1/search",
        json=search_payload,
        timeout=180.0,
    )

    assert py_search.status_code == rs_search.status_code, (
        f"ontology search status mismatch: py={py_search.status_code} rs={rs_search.status_code}\n"
        f"py: {py_search.text[:400]}\nrs: {rs_search.text[:400]}"
    )
    assert py_search.status_code == 200

    py_json = py_search.json()
    rs_json = rs_search.json()
    assert isinstance(py_json, list) and py_json, "Python search must return non-empty array"
    assert isinstance(rs_json, list) and rs_json, "Rust search must return non-empty array"

    assert _search_has_ontology_markers(py_json), (
        "Python search response does not include ontology markers"
    )
    assert _search_has_ontology_markers(rs_json), (
        "Rust search response does not include ontology markers"
    )


def test_ontology_unknown_key_parity_returns_404(authed_clients, unique_dataset_name):
    """Unknown ontology key in /api/v1/cognify returns 404 on both servers."""
    dataset_ids: dict[str, str | None] = {}
    for side, client in authed_clients.items():
        seed_resp = seed_dataset_with_text(client, name=unique_dataset_name, text=_SEED_TEXT)
        dataset_ids[side] = _extract_dataset_id(seed_resp)

    for side in ("py", "rs"):
        if not dataset_ids[side]:
            pytest.skip(f"No dataset ID in seed response for {side}")

    unknown_key = f"does-not-exist-{uuid.uuid4().hex[:8]}"

    py = authed_clients["py"].post(
        "/api/v1/cognify",
        json={
            "datasets": [dataset_ids["py"]],
            "ontologyKey": [unknown_key],
            "run_in_background": False,
        },
        timeout=180.0,
    )
    rs = authed_clients["rs"].post(
        "/api/v1/cognify",
        json={
            "datasets": [dataset_ids["rs"]],
            "ontologyKey": [unknown_key],
            "run_in_background": False,
        },
        timeout=180.0,
    )

    assert py.status_code == rs.status_code, (
        f"unknown-key status mismatch: py={py.status_code} rs={rs.status_code}\n"
        f"py: {py.text[:400]}\nrs: {rs.text[:400]}"
    )
    assert py.status_code == 404
