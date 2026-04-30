"""Phase-3 parity tests for GET /api/v1/visualize.

Per docs/http-api-v2/tasks/e-07-visualize.md §4 and Decision 11:
The HTML response embeds graph data as seven JS variable assignments produced by
substituting Python's seven `__*_DATA__` placeholders into the template.  This
harness extracts those JSON literals from both backends and structurally diffs
them with a stable sort.

Out of scope per Decision 11: d3.js bundle hash, CDN URLs, theme/CSS, layout
coordinate randomness — none of these affect the seven extracted payloads, and
attempting to byte-diff the surrounding HTML scaffold produces noisy failures.

NOTE: an earlier version of this file greped for `<!--JSON_ISLAND_START/END-->`
markers that exist neither in Python (`cognee_network_visualization.py`) nor in
Rust (`crates/visualization/assets/graph_template.html`); it has been rewritten
in full per Decision 11.
"""

from __future__ import annotations

import json
import re

import pytest

from conftest import requires_openai
from seed import seed_cognify, seed_dataset_with_text

pytestmark = [requires_openai]

_SEED_TEXT = (
    "The solar system consists of the Sun and eight planets.  "
    "Earth is the third planet from the Sun.  "
    "Mars is often called the Red Planet due to its iron oxide surface.  "
    "Jupiter is the largest planet in the solar system."
)

# Alternative seed text used in the negative test — picks a different domain so
# the extracted entity sets diverge meaningfully and the harness can prove it
# detects real differences.
_OTHER_SEED_TEXT = (
    "Photosynthesis converts sunlight into chemical energy in chloroplasts.  "
    "Mitochondria produce ATP through cellular respiration.  "
    "Ribosomes synthesize proteins from messenger RNA templates."
)

# Each tuple maps a JS-variable line in the template to a (regex, dict-key) pair.
# Variable names are verified against
# `crates/visualization/assets/graph_template.html` lines 404-409 and 227.
#
# We capture lazily up to the trailing `;` at end-of-line.  `re.DOTALL` allows
# the JSON literal to span multiple lines (pretty-printed payloads are unlikely
# but not forbidden by Python's `json.dumps`, which the safe_json_embed helper
# does not invoke — still, defensive).
_PAYLOAD_PATTERNS: tuple[tuple[str, str, str], ...] = (
    ("nodes", "var nodes", r"var\s+nodes\s*=\s*(.*?);\s*$"),
    ("links", "var links", r"var\s+links\s*=\s*(.*?);\s*$"),
    ("task_colors", "var taskColors", r"var\s+taskColors\s*=\s*(.*?);\s*$"),
    ("pipeline_colors", "var pipelineColors", r"var\s+pipelineColors\s*=\s*(.*?);\s*$"),
    ("nodeset_colors", "var nodesetColors", r"var\s+nodesetColors\s*=\s*(.*?);\s*$"),
    ("user_colors", "var userColors", r"var\s+userColors\s*=\s*(.*?);\s*$"),
    ("schema", "const schemaData", r"const\s+schemaData\s*=\s*(.*?);\s*$"),
)


def _extract_payload(body: str) -> dict:
    """Extract the seven JSON-literal payloads embedded in a /visualize HTML body.

    Reverses the Python `</` → `<\\/` escape applied by `safe_json_embed`
    (Rust: `crates/visualization/src/html.rs:23-26`; Python: the matching
    `replace("</", "<\\/")` in `cognee_network_visualization.py`) before
    `json.loads`-ing each captured literal.

    Returns a dict keyed by ``nodes``, ``links``, ``task_colors``,
    ``pipeline_colors``, ``nodeset_colors``, ``user_colors``, ``schema``.
    The ``schema`` value is ``None`` when the template emitted the literal
    ``null`` (i.e. the dataset has no schema attached).
    """
    payload: dict = {}
    for key, marker, pattern in _PAYLOAD_PATTERNS:
        m = re.search(pattern, body, flags=re.MULTILINE | re.DOTALL)
        if m is None:
            raise AssertionError(
                f"could not locate JS variable line {marker!r} in /visualize HTML "
                f"(body length {len(body)}; first 500 chars: {body[:500]!r})"
            )
        raw = m.group(1).strip()
        # Reverse the `</` escape applied by safe_json_embed before parsing.
        unescaped = raw.replace("<\\/", "</")
        try:
            payload[key] = json.loads(unescaped)
        except json.JSONDecodeError as exc:
            raise AssertionError(
                f"failed to json-decode {marker!r} payload: {exc}\n"
                f"raw literal (first 500 chars): {unescaped[:500]!r}"
            ) from exc
    return payload


def _normalize_payload(payload: dict) -> dict:
    """Stable-sort the list-shaped payloads so structural equality is order-insensitive.

    - ``nodes`` sorted by ``id``.
    - ``links`` sorted by ``(source, target, label)`` (label may be missing → "").
    - color maps and schema are objects; ``json.loads`` already preserves
      insertion order but ``dict ==`` is order-insensitive in CPython, so they
      need no sort.
    """
    out = dict(payload)

    nodes = out.get("nodes")
    if isinstance(nodes, list):
        out["nodes"] = sorted(
            nodes,
            key=lambda n: str(n.get("id", "")) if isinstance(n, dict) else str(n),
        )

    links = out.get("links")
    if isinstance(links, list):
        out["links"] = sorted(
            links,
            key=lambda l: (
                str(l.get("source", "")) if isinstance(l, dict) else str(l),
                str(l.get("target", "")) if isinstance(l, dict) else "",
                str(l.get("label", "")) if isinstance(l, dict) else "",
            ),
        )

    return out


def _seed_both_with(authed_clients, dataset_name: str, text: str) -> dict[str, str]:
    """Seed each backend with ``text`` under ``dataset_name``, run cognify, return ds_ids."""
    ds_ids: dict[str, str] = {}
    for side, client in authed_clients.items():
        r = seed_dataset_with_text(client, name=dataset_name, text=text)
        ds_id = r.get("dataset_id") or r.get("id")
        assert ds_id, f"could not obtain dataset_id from {side} add response: {r!r}"
        seed_cognify(client, dataset_id=ds_id)
        ds_ids[side] = ds_id
    return ds_ids


# ─────────────────────────────────────────────────────────────────────────────
# Smoke test: status, content-type, marker presence
# ─────────────────────────────────────────────────────────────────────────────


def test_visualize_smoke(authed_clients, unique_dataset_name):
    """GET /api/v1/visualize returns 200 text/html with all seven JS variable markers."""
    ds_ids = _seed_both_with(authed_clients, unique_dataset_name, _SEED_TEXT)

    py = authed_clients["py"].get(f"/api/v1/visualize?dataset_id={ds_ids['py']}")
    rs = authed_clients["rs"].get(f"/api/v1/visualize?dataset_id={ds_ids['rs']}")

    if py.status_code == 404 and rs.status_code == 404:
        pytest.skip("/api/v1/visualize not yet implemented on either backend")

    assert py.status_code == 200, f"py /visualize status {py.status_code}: {py.text[:400]}"
    assert rs.status_code == 200, f"rs /visualize status {rs.status_code}: {rs.text[:400]}"

    for side, resp in (("py", py), ("rs", rs)):
        ctype = resp.headers.get("content-type", "")
        assert ctype.lower().startswith("text/html"), (
            f"{side} content-type {ctype!r} is not text/html"
        )
        for _key, marker, _pattern in _PAYLOAD_PATTERNS:
            assert marker in resp.text, (
                f"{side} /visualize body missing marker {marker!r} "
                f"(body length {len(resp.text)})"
            )


# ─────────────────────────────────────────────────────────────────────────────
# Structural-equality test: extract + sort + diff each payload
# ─────────────────────────────────────────────────────────────────────────────


def test_visualize_payload_equality(authed_clients, unique_dataset_name):
    """The seven extracted JSON payloads structurally match between Python and Rust."""
    ds_ids = _seed_both_with(authed_clients, unique_dataset_name, _SEED_TEXT)

    py = authed_clients["py"].get(f"/api/v1/visualize?dataset_id={ds_ids['py']}")
    rs = authed_clients["rs"].get(f"/api/v1/visualize?dataset_id={ds_ids['rs']}")

    if py.status_code == 404 and rs.status_code == 404:
        pytest.skip("/api/v1/visualize not yet implemented on either backend")

    assert py.status_code == rs.status_code == 200, (
        f"visualize status mismatch: py={py.status_code} rs={rs.status_code}"
    )

    py_payload = _normalize_payload(_extract_payload(py.text))
    rs_payload = _normalize_payload(_extract_payload(rs.text))

    # Structural-diff each key in turn so failures point at the divergent payload
    # rather than dumping the full dict.
    for key, _marker, _pattern in _PAYLOAD_PATTERNS:
        assert py_payload[key] == rs_payload[key], (
            f"/visualize payload divergence for key {key!r}\n"
            f"py: {json.dumps(py_payload[key], sort_keys=True)[:1000]}\n"
            f"rs: {json.dumps(rs_payload[key], sort_keys=True)[:1000]}"
        )


# ─────────────────────────────────────────────────────────────────────────────
# Negative test: different graphs ⇒ different payloads
# ─────────────────────────────────────────────────────────────────────────────


def test_visualize_negative_detects_divergence(authed_clients, unique_dataset_name):
    """When the two backends are seeded with different content, the harness catches it.

    Sanity-verifies that ``_extract_payload`` is doing real work — a no-op
    extractor or an over-aggressive normalizer would silently report equality
    here and quietly mask real regressions in the positive test above.
    """
    py_client = authed_clients["py"]
    rs_client = authed_clients["rs"]

    py_seed = seed_dataset_with_text(py_client, name=unique_dataset_name, text=_SEED_TEXT)
    rs_seed = seed_dataset_with_text(rs_client, name=unique_dataset_name, text=_OTHER_SEED_TEXT)
    py_ds = py_seed.get("dataset_id") or py_seed.get("id")
    rs_ds = rs_seed.get("dataset_id") or rs_seed.get("id")
    assert py_ds and rs_ds, (
        f"could not obtain dataset_ids — py={py_seed!r} rs={rs_seed!r}"
    )
    seed_cognify(py_client, dataset_id=py_ds)
    seed_cognify(rs_client, dataset_id=rs_ds)

    py = py_client.get(f"/api/v1/visualize?dataset_id={py_ds}")
    rs = rs_client.get(f"/api/v1/visualize?dataset_id={rs_ds}")

    if py.status_code == 404 and rs.status_code == 404:
        pytest.skip("/api/v1/visualize not yet implemented on either backend")

    assert py.status_code == rs.status_code == 200, (
        f"negative-test status mismatch: py={py.status_code} rs={rs.status_code}"
    )

    py_payload = _normalize_payload(_extract_payload(py.text))
    rs_payload = _normalize_payload(_extract_payload(rs.text))

    assert py_payload != rs_payload, (
        "negative test: backends were seeded with intentionally different graphs "
        "but extracted payloads compared equal — the harness is not detecting real "
        "differences and the positive parity test cannot be trusted.\n"
        f"py nodes: {py_payload.get('nodes')}\n"
        f"rs nodes: {rs_payload.get('nodes')}"
    )
