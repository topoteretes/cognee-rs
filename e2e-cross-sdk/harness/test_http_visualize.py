"""Phase-3 parity tests for GET /api/v1/visualize.

Per p8-e2e-parity.md Step 11 and routers/visualize.md:
Calls GET /api/v1/visualize?dataset_id={...} after seed_cognify, strips the
JSON-island region from both HTML bodies, and bytewise-diffs the remaining
HTML scaffold.  The JSON-island contents are diffed separately with a
structural compare (entity-name set Jaccard ≥ 0.5).
"""

from __future__ import annotations

import re

import pytest

from conftest import requires_openai
from http_helpers import DEFAULT_IGNORE, WS_YIELD_TOLERANCE, _JSON_ISLAND_RE
from seed import seed_cognify, seed_dataset_with_text

pytestmark = [requires_openai]

_SEED_TEXT = (
    "The solar system consists of the Sun and eight planets.  "
    "Earth is the third planet from the Sun.  "
    "Mars is often called the Red Planet due to its iron oxide surface.  "
    "Jupiter is the largest planet in the solar system."
)


def _extract_json_island(html: str) -> str | None:
    """Extract the JSON island content between island markers."""
    import json
    match = re.search(
        r"<!--JSON_ISLAND_START-->(.*?)<!--JSON_ISLAND_END-->",
        html,
        re.DOTALL,
    )
    if match:
        try:
            return json.loads(match.group(1).strip())
        except Exception:
            return match.group(1).strip()
    return None


def _entity_names(island_data) -> set[str]:
    """Extract entity names from the JSON island graph data."""
    if isinstance(island_data, dict):
        nodes = island_data.get("nodes") or island_data.get("entities") or []
        return {n.get("name") or n.get("label") or "" for n in nodes if isinstance(n, dict)}
    return set()


def _jaccard(a: set, b: set) -> float:
    if not a and not b:
        return 1.0
    return len(a & b) / len(a | b)


def test_visualize_html_scaffold(authed_clients, unique_dataset_name):
    """GET /api/v1/visualize returns HTML with matching scaffold (non-JSON-island parts)."""
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
        pytest.skip("Could not obtain dataset IDs for visualize test")

    py = authed_clients["py"].get(f"/api/v1/visualize?dataset_id={py_ds}")
    rs = authed_clients["rs"].get(f"/api/v1/visualize?dataset_id={rs_ds}")

    if py.status_code == 404 and rs.status_code == 404:
        pytest.skip("/api/v1/visualize not yet implemented")

    assert py.status_code == rs.status_code, (
        f"visualize status mismatch: py={py.status_code} rs={rs.status_code}"
    )

    # Strip JSON island from both sides and compare scaffold
    py_scaffold = _JSON_ISLAND_RE.sub("<!--JSON_ISLAND-->", py.text)
    rs_scaffold = _JSON_ISLAND_RE.sub("<!--JSON_ISLAND-->", rs.text)
    assert py_scaffold == rs_scaffold, (
        f"HTML scaffold mismatch after stripping JSON islands.\n"
        f"py len={len(py_scaffold)} rs len={len(rs_scaffold)}\n"
        f"py[:500]: {py_scaffold[:500]}\n"
        f"rs[:500]: {rs_scaffold[:500]}"
    )


def test_visualize_json_island_structural(authed_clients, unique_dataset_name):
    """The JSON island in /visualize has entity-name Jaccard ≥ 0.5 between both servers."""
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
        pytest.skip("Could not obtain dataset IDs for visualize JSON island test")

    py = authed_clients["py"].get(f"/api/v1/visualize?dataset_id={py_ds}")
    rs = authed_clients["rs"].get(f"/api/v1/visualize?dataset_id={rs_ds}")

    if py.status_code == 404 and rs.status_code == 404:
        pytest.skip("/api/v1/visualize not yet implemented")

    py_island = _extract_json_island(py.text)
    rs_island = _extract_json_island(rs.text)

    if py_island is None and rs_island is None:
        pytest.skip("No JSON island found in either visualize response")

    py_names = _entity_names(py_island)
    rs_names = _entity_names(rs_island)

    if py_names or rs_names:
        j = _jaccard(py_names, rs_names)
        assert j >= 0.5, (
            f"Visualize JSON island entity Jaccard {j:.2f} < 0.5\n"
            f"py entities: {sorted(py_names)}\nrs entities: {sorted(rs_names)}"
        )
