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


# ─────────────────────────────────────────────────────────────────────────────
# POST /api/v1/visualize/multi — superuser-only multi-(user,dataset) aggregation
# ─────────────────────────────────────────────────────────────────────────────
#
# Per docs/http-api-v2/tasks/e-08-visualize-multi.md §4 (Decision 16):
# - Both backends share the snake_case `[{"user_id": <uuid>, "dataset_id": <uuid>}]`
#   wire shape (Python uses raw `BaseModel`, not the camelCase `InDTO/OutDTO`).
# - Non-superusers receive `403 {"error": "Superuser privileges required for
#   multi-user visualization"}` from both sides.
# - On success: `render_multi_user` deduplicates nodes by `str(node_id)` (first-
#   write-wins) and edges by `(source, target, relation)` (mirror Python
#   `cognee_network_visualization.py:142,150-155`).
# - Each node is tagged with `source_user = user.email or str(user.id)` so the
#   `userColors` palette key matches Python.
#
# The harness here exercises what is reachable without DB-level superuser
# bootstrap: the 403-envelope parity. The dedupe + email-label parity tests
# require a superuser session that the current harness cannot manufacture
# cross-backend (the Python default user has a password, the Rust default user
# does not — see `crates/database/src/migrator/m20250422_*.rs`). They probe for
# the capability and skip cleanly if missing, so adding a superuser fixture
# later turns them green automatically.

_MULTI_SUPERUSER_CREDS = {
    "username": "default_user@example.com",
    "password": "default_password",
}


def _try_superuser_login(client) -> bool:
    """Attempt to authenticate as the well-known superuser on a backend.

    Returns True if both `/auth/login` and a follow-up `/users/me` confirm the
    session belongs to a superuser; False otherwise. Used to gracefully skip
    the multi-visualize parity tests when the harness's DB bootstrap does not
    seed a known-password superuser.
    """
    r = client.post("/api/v1/auth/login", data=_MULTI_SUPERUSER_CREDS)
    if r.status_code != 200:
        return False
    me = client.get("/api/v1/users/me")
    if me.status_code != 200:
        return False
    body = me.json()
    return bool(body.get("is_superuser"))


def _resolve_user_id(client) -> str | None:
    """Return the calling user's id via `/users/me`, or None on failure."""
    r = client.get("/api/v1/users/me")
    if r.status_code != 200:
        return None
    body = r.json()
    return body.get("id")


def test_visualize_multi_non_superuser_403_parity(authed_clients):
    """A regular caller hitting `POST /multi` receives a parity-shaped 403.

    The caller is the test user registered by the `authed_clients` fixture
    (not a superuser). Both backends must reject with status 403 and the
    Python-defined `{"error": "Superuser privileges required for multi-user
    visualization"}` envelope (NOT FastAPI's canonical `{"detail": …}`).
    """
    body = "[]"
    py = authed_clients["py"].post(
        "/api/v1/visualize/multi", content=body, headers={"content-type": "application/json"}
    )
    rs = authed_clients["rs"].post(
        "/api/v1/visualize/multi", content=body, headers={"content-type": "application/json"}
    )

    if py.status_code == 404 and rs.status_code == 404:
        pytest.skip("/api/v1/visualize/multi not yet implemented on either backend")

    assert py.status_code == 403, f"py status {py.status_code}: {py.text[:400]}"
    assert rs.status_code == 403, f"rs status {rs.status_code}: {rs.text[:400]}"
    py_body = py.json()
    rs_body = rs.json()
    assert py_body == {"error": "Superuser privileges required for multi-user visualization"}, (
        f"py 403 envelope divergence: {py_body!r}"
    )
    assert rs_body == {"error": "Superuser privileges required for multi-user visualization"}, (
        f"rs 403 envelope divergence: {rs_body!r}"
    )


def _bootstrap_multi_superuser(both_clients) -> dict[str, dict] | None:
    """Try to log in as superuser on both backends and return the resolved
    `(user_id, dataset_id)` for a freshly-seeded dataset on each side.

    Returns ``None`` if either backend cannot mint a superuser session — the
    caller should `pytest.skip` in that case. This isolates the
    capability-probing logic so each multi-visualize parity test stays focused.
    """
    out: dict[str, dict] = {}
    for side, client in both_clients.items():
        if not _try_superuser_login(client):
            return None
        uid = _resolve_user_id(client)
        if not uid:
            return None
        out[side] = {"user_id": uid, "client": client}
    return out


def _seed_multi_pair(client, *, name: str, text: str) -> str:
    """Seed one dataset on `client` and return its dataset id."""
    r = seed_dataset_with_text(client, name=name, text=text)
    ds_id = r.get("dataset_id") or r.get("id")
    assert ds_id, f"could not obtain dataset_id from add response: {r!r}"
    seed_cognify(client, dataset_id=ds_id)
    return ds_id


def test_visualize_multi_smoke(both_clients, unique_dataset_name):
    """`POST /multi` with a two-pair array returns 200 text/html with all 7 markers."""
    creds = _bootstrap_multi_superuser(both_clients)
    if creds is None:
        pytest.skip("multi-visualize harness needs a known-password superuser on both backends")

    pairs_per_side: dict[str, list[dict]] = {}
    for side, info in creds.items():
        client = info["client"]
        ds1 = _seed_multi_pair(client, name=f"{unique_dataset_name}_a", text=_SEED_TEXT)
        ds2 = _seed_multi_pair(client, name=f"{unique_dataset_name}_b", text=_OTHER_SEED_TEXT)
        pairs_per_side[side] = [
            {"user_id": info["user_id"], "dataset_id": ds1},
            {"user_id": info["user_id"], "dataset_id": ds2},
        ]

    py = creds["py"]["client"].post("/api/v1/visualize/multi", json=pairs_per_side["py"])
    rs = creds["rs"]["client"].post("/api/v1/visualize/multi", json=pairs_per_side["rs"])

    assert py.status_code == 200, f"py /multi status {py.status_code}: {py.text[:400]}"
    assert rs.status_code == 200, f"rs /multi status {rs.status_code}: {rs.text[:400]}"

    for side, resp in (("py", py), ("rs", rs)):
        ctype = resp.headers.get("content-type", "")
        assert ctype.lower().startswith("text/html"), f"{side} content-type {ctype!r}"
        for _key, marker, _pattern in _PAYLOAD_PATTERNS:
            assert marker in resp.text, f"{side} /multi missing marker {marker!r}"


def test_visualize_multi_payload_equality_disjoint(both_clients, unique_dataset_name):
    """Disjoint graphs across two pairs: structural payload equality required."""
    creds = _bootstrap_multi_superuser(both_clients)
    if creds is None:
        pytest.skip("multi-visualize harness needs a known-password superuser on both backends")

    pairs_per_side: dict[str, list[dict]] = {}
    for side, info in creds.items():
        client = info["client"]
        ds1 = _seed_multi_pair(client, name=f"{unique_dataset_name}_a", text=_SEED_TEXT)
        ds2 = _seed_multi_pair(client, name=f"{unique_dataset_name}_b", text=_OTHER_SEED_TEXT)
        pairs_per_side[side] = [
            {"user_id": info["user_id"], "dataset_id": ds1},
            {"user_id": info["user_id"], "dataset_id": ds2},
        ]

    py = creds["py"]["client"].post("/api/v1/visualize/multi", json=pairs_per_side["py"])
    rs = creds["rs"]["client"].post("/api/v1/visualize/multi", json=pairs_per_side["rs"])
    assert py.status_code == rs.status_code == 200, (
        f"multi-disjoint status mismatch: py={py.status_code} rs={rs.status_code}"
    )

    py_payload = _normalize_payload(_extract_payload(py.text))
    rs_payload = _normalize_payload(_extract_payload(rs.text))

    for key, _marker, _pattern in _PAYLOAD_PATTERNS:
        assert py_payload[key] == rs_payload[key], (
            f"/multi disjoint payload divergence for key {key!r}\n"
            f"py: {json.dumps(py_payload[key], sort_keys=True)[:1000]}\n"
            f"rs: {json.dumps(rs_payload[key], sort_keys=True)[:1000]}"
        )


def test_visualize_multi_payload_equality_overlapping(both_clients, unique_dataset_name):
    """Overlapping pairs (same dataset twice): dedupe parity required.

    Each pair references the same dataset, guaranteeing every node id and edge
    `(source, target, relation)` triple appears in both halves of the input.
    Python and Rust must collapse these to a single entry per id and triple.
    """
    creds = _bootstrap_multi_superuser(both_clients)
    if creds is None:
        pytest.skip("multi-visualize harness needs a known-password superuser on both backends")

    pairs_per_side: dict[str, list[dict]] = {}
    for side, info in creds.items():
        client = info["client"]
        ds = _seed_multi_pair(client, name=f"{unique_dataset_name}_overlap", text=_SEED_TEXT)
        pairs_per_side[side] = [
            {"user_id": info["user_id"], "dataset_id": ds},
            {"user_id": info["user_id"], "dataset_id": ds},
        ]

    py = creds["py"]["client"].post("/api/v1/visualize/multi", json=pairs_per_side["py"])
    rs = creds["rs"]["client"].post("/api/v1/visualize/multi", json=pairs_per_side["rs"])
    assert py.status_code == rs.status_code == 200, (
        f"multi-overlap status mismatch: py={py.status_code} rs={rs.status_code}"
    )

    py_payload = _normalize_payload(_extract_payload(py.text))
    rs_payload = _normalize_payload(_extract_payload(rs.text))

    # Dedupe correctness: an overlapping payload must not have grown — the
    # repeated pair should collapse exactly back to the single-pair shape.
    py_single = creds["py"]["client"].post(
        "/api/v1/visualize/multi", json=[pairs_per_side["py"][0]]
    )
    rs_single = creds["rs"]["client"].post(
        "/api/v1/visualize/multi", json=[pairs_per_side["rs"][0]]
    )
    assert py_single.status_code == rs_single.status_code == 200
    py_single_payload = _normalize_payload(_extract_payload(py_single.text))
    rs_single_payload = _normalize_payload(_extract_payload(rs_single.text))

    for key in ("nodes", "links"):
        assert py_payload[key] == py_single_payload[key], (
            f"py /multi did not dedupe for key {key!r}: overlapped vs single diverged"
        )
        assert rs_payload[key] == rs_single_payload[key], (
            f"rs /multi did not dedupe for key {key!r}: overlapped vs single diverged"
        )

    for key, _marker, _pattern in _PAYLOAD_PATTERNS:
        assert py_payload[key] == rs_payload[key], (
            f"/multi overlapping payload divergence for key {key!r}\n"
            f"py: {json.dumps(py_payload[key], sort_keys=True)[:1000]}\n"
            f"rs: {json.dumps(rs_payload[key], sort_keys=True)[:1000]}"
        )


def test_visualize_multi_user_colors_keys_match(both_clients, unique_dataset_name):
    """`userColors` keys parity → proves the email-label semantics agree.

    Each backend should key the palette by `user.email or str(user.id)` per
    Python `cognee_network_visualization.py:138`. With both sides logged in as
    the same default-user email, the resulting palette key set must match.
    """
    creds = _bootstrap_multi_superuser(both_clients)
    if creds is None:
        pytest.skip("multi-visualize harness needs a known-password superuser on both backends")

    pairs_per_side: dict[str, list[dict]] = {}
    for side, info in creds.items():
        client = info["client"]
        ds = _seed_multi_pair(client, name=f"{unique_dataset_name}_colors", text=_SEED_TEXT)
        pairs_per_side[side] = [{"user_id": info["user_id"], "dataset_id": ds}]

    py = creds["py"]["client"].post("/api/v1/visualize/multi", json=pairs_per_side["py"])
    rs = creds["rs"]["client"].post("/api/v1/visualize/multi", json=pairs_per_side["rs"])
    assert py.status_code == rs.status_code == 200

    py_payload = _extract_payload(py.text)
    rs_payload = _extract_payload(rs.text)

    py_keys = set((py_payload.get("user_colors") or {}).keys())
    rs_keys = set((rs_payload.get("user_colors") or {}).keys())
    assert py_keys == rs_keys, (
        f"/multi userColors key divergence: py={py_keys!r} rs={rs_keys!r}\n"
        "Decision 16: both sides must label nodes with `user.email or str(user.id)`."
    )
