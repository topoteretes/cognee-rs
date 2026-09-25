"""HTTP response diff helpers for the cross-SDK parity harness.

Provides:
- ``DEFAULT_IGNORE``     — frozenset of JSONPath-like patterns stripped before
                           comparison (see e2e-parity.md §6.2).
- ``strip_paths``        — walk a JSON value and remove keys matching patterns.
- ``assert_responses_match`` — strict status + Content-Type + body equality,
                           with structured failure messages that name the
                           diverging field.
- ``WS_YIELD_TOLERANCE`` — accepted frame-count delta for WebSocket tests
                           (e2e-parity.md §12 Q5).

Inline self-tests (pytest) are at the bottom of this file.
"""

from __future__ import annotations

import hashlib
import json
import re
from typing import Any

import pytest

# ── Constants ─────────────────────────────────────────────────────────────────

WS_YIELD_TOLERANCE = 2  # acceptable ±delta in WebSocket intermediate frame counts

DEFAULT_IGNORE: frozenset[str] = frozenset(
    {
        "$..created_at",
        "$..updated_at",
        # camelCase variants — several DTOs (datasets, etc.) serialize
        # timestamps as camelCase, and these are always wall-clock volatile.
        "$..createdAt",
        "$..updatedAt",
        "$..pipeline_run_id",
        "$..run_info.duration_ms",
        "$..access_token",
        "$..token_type",
        "$..session.id",
        "$..run_id",
        "$..id",
    }
)


# ── JSONPath-lite strip walker ────────────────────────────────────────────────


def strip_paths(value: Any, paths: frozenset[str] | set[str] | tuple[str, ...]) -> Any:
    """Remove keys from *value* that match any pattern in *paths*.

    Supported patterns:
    - ``$.key``          — remove ``key`` from the root object only.
    - ``$..key``         — remove ``key`` recursively everywhere in the tree.
    - ``$.list[*].key``  — remove ``key`` from every element of the top-level
                           ``list`` array.

    The function is non-mutating: it returns a new object (deep copy of the
    stripped subtree).  Lists and dicts are always copied; primitives are
    returned as-is.
    """
    if not paths:
        return value

    # Pre-parse patterns once for speed
    parsed = [_parse_pattern(p) for p in paths]
    return _strip(value, parsed, path=())


def _parse_pattern(pattern: str) -> dict:
    """Turn a pattern string into a structured dict for matching.

    Returns one of:
    - {"kind": "root_key",     "key": str}
    - {"kind": "deep_key",     "key": str}
    - {"kind": "list_key",     "list": str, "key": str}
    """
    # $..key — deep recursive
    m = re.fullmatch(r"\$\.\.(\w+)", pattern)
    if m:
        return {"kind": "deep_key", "key": m.group(1)}

    # $.list[*].key — list element key
    m = re.fullmatch(r"\$\.(\w+)\[\*\]\.(\w+)", pattern)
    if m:
        return {"kind": "list_key", "list": m.group(1), "key": m.group(2)}

    # $.key — root key
    m = re.fullmatch(r"\$\.(\w+)", pattern)
    if m:
        return {"kind": "root_key", "key": m.group(1)}

    # Unknown pattern — ignore silently (forward-compatible).
    return {"kind": "unknown", "pattern": pattern}


def _strip(value: Any, parsed: list[dict], path: tuple) -> Any:
    if isinstance(value, dict):
        result = {}
        for k, v in value.items():
            current_path = path + (k,)
            if _should_remove_key(k, current_path, parsed):
                continue
            result[k] = _strip(v, parsed, current_path)
        return result
    if isinstance(value, list):
        return [_strip(item, parsed, path + (i,)) for i, item in enumerate(value)]
    return value


def _should_remove_key(key: str, path: tuple, parsed: list[dict]) -> bool:
    for spec in parsed:
        kind = spec["kind"]
        if kind == "deep_key":
            if key == spec["key"]:
                return True
        elif kind == "root_key":
            # Remove only if this is a direct child of root (path length == 1)
            if len(path) == 1 and key == spec["key"]:
                return True
        elif kind == "list_key":
            # Remove ``key`` when parent is an integer index inside ``spec["list"]``
            # i.e. path == (spec["list"], <int>, key)
            if (
                len(path) == 3
                and path[0] == spec["list"]
                and isinstance(path[1], int)
                and key == spec["key"]
            ):
                return True
    return False


# ── Minimal JSON differ ───────────────────────────────────────────────────────


def _diff_json(py_val: Any, rs_val: Any, path: str = "$") -> list[str]:
    """Return a list of human-readable diff lines (empty = equal)."""
    diffs: list[str] = []
    if type(py_val) is not type(rs_val):
        # Allow int/float interop
        if not (isinstance(py_val, (int, float)) and isinstance(rs_val, (int, float))):
            diffs.append(
                f"  {path}: type mismatch py={type(py_val).__name__} rs={type(rs_val).__name__}"
                f" | py={_abbrev(py_val)} rs={_abbrev(rs_val)}"
            )
            return diffs

    if isinstance(py_val, dict):
        py_keys = set(py_val)
        rs_keys = set(rs_val)
        only_py = py_keys - rs_keys
        only_rs = rs_keys - py_keys
        if only_py:
            diffs.append(f"  {path}: keys only in py: {sorted(only_py)}")
        if only_rs:
            diffs.append(f"  {path}: keys only in rs: {sorted(only_rs)}")
        for k in sorted(py_keys & rs_keys):
            diffs.extend(_diff_json(py_val[k], rs_val[k], f"{path}.{k}"))
    elif isinstance(py_val, list):
        if len(py_val) != len(rs_val):
            diffs.append(
                f"  {path}: list length mismatch py={len(py_val)} rs={len(rs_val)}"
            )
        for i, (pv, rv) in enumerate(zip(py_val, rs_val)):
            diffs.extend(_diff_json(pv, rv, f"{path}[{i}]"))
    else:
        if py_val != rs_val:
            diffs.append(
                f"  {path}: value mismatch py={_abbrev(py_val)} rs={_abbrev(rs_val)}"
            )
    return diffs


def _abbrev(v: Any, max_len: int = 120) -> str:
    s = repr(v)
    return s if len(s) <= max_len else s[: max_len - 3] + "..."


# ── Main assertion ────────────────────────────────────────────────────────────


def _sort_json_lists(value):
    """Recursively sort every list by a canonical JSON key of its elements.

    Used for collection endpoints (e.g. GET /datasets) where the *set* of items
    is the contract but the order is not — two SDKs may return the same items in
    a different order when sort keys tie (e.g. equal created_at timestamps).
    """
    if isinstance(value, dict):
        return {k: _sort_json_lists(v) for k, v in value.items()}
    if isinstance(value, list):
        sorted_items = [_sort_json_lists(v) for v in value]
        return sorted(sorted_items, key=lambda v: json.dumps(v, sort_keys=True, default=str))
    return value


def assert_responses_match(
    py,
    rs,
    *,
    ignore: frozenset[str] | set[str] | tuple[str, ...] = DEFAULT_IGNORE,
    sort_lists: bool = False,
) -> None:
    """Assert that *py* and *rs* (``httpx.Response`` objects) are equivalent.

    Comparison order:
    1. Status code — must match exactly.
    2. Content-Type — must match (ignoring charset suffix differences).
    3. Body — depends on content type:
       - JSON: structural diff after applying ``strip_paths``.
       - HTML: strip ``<!--JSON_ISLAND_START-->...<!--JSON_ISLAND_END-->``
               from both sides and bytewise-compare the remainder.
       - Binary: SHA-256 equality.

    On failure, ``pytest.fail`` is called with a structured message that names
    the diverging field/path so CI logs are actionable.
    """
    # 1. Status code
    if py.status_code != rs.status_code:
        pytest.fail(
            f"Status code mismatch: py={py.status_code} rs={rs.status_code}\n"
            f"py body: {py.text[:500]}\n"
            f"rs body: {rs.text[:500]}"
        )

    # 2. Content-Type (normalised: strip charset, boundary, etc.)
    py_ct = _norm_content_type(py.headers.get("content-type", ""))
    rs_ct = _norm_content_type(rs.headers.get("content-type", ""))
    if py_ct != rs_ct:
        pytest.fail(
            f"Content-Type mismatch: py={py_ct!r} rs={rs_ct!r}\n"
            f"py body: {py.text[:300]}\n"
            f"rs body: {rs.text[:300]}"
        )

    # 3. Body
    if "html" in py_ct:
        _assert_html_match(py, rs)
    elif "json" in py_ct:
        _assert_json_match(py, rs, ignore=ignore, sort_lists=sort_lists)
    else:
        _assert_binary_match(py, rs)


def _norm_content_type(ct: str) -> str:
    """Return the MIME type portion only, lower-cased."""
    return ct.split(";")[0].strip().lower()


def _assert_json_match(py, rs, *, ignore, sort_lists: bool = False) -> None:
    try:
        py_json = py.json()
    except Exception as exc:
        pytest.fail(f"py response is not valid JSON: {exc}\nbody: {py.text[:500]}")
    try:
        rs_json = rs.json()
    except Exception as exc:
        pytest.fail(f"rs response is not valid JSON: {exc}\nbody: {rs.text[:500]}")

    py_stripped = strip_paths(py_json, ignore)
    rs_stripped = strip_paths(rs_json, ignore)

    if sort_lists:
        py_stripped = _sort_json_lists(py_stripped)
        rs_stripped = _sort_json_lists(rs_stripped)

    diffs = _diff_json(py_stripped, rs_stripped)
    if diffs:
        diff_text = "\n".join(diffs)
        pytest.fail(
            f"JSON body mismatch ({len(diffs)} difference(s)):\n"
            f"{diff_text}\n\n"
            f"py (full): {json.dumps(py_stripped, indent=2, default=str)[:1000]}\n"
            f"rs (full): {json.dumps(rs_stripped, indent=2, default=str)[:1000]}"
        )


_JSON_ISLAND_RE = re.compile(
    r"<!--JSON_ISLAND_START-->.*?<!--JSON_ISLAND_END-->", re.DOTALL
)


def _assert_html_match(py, rs) -> None:
    py_html = _JSON_ISLAND_RE.sub("<!--JSON_ISLAND-->", py.text)
    rs_html = _JSON_ISLAND_RE.sub("<!--JSON_ISLAND-->", rs.text)
    if py_html != rs_html:
        pytest.fail(
            f"HTML body mismatch (after stripping JSON islands).\n"
            f"py length={len(py_html)} rs length={len(rs_html)}\n"
            f"py[:500]: {py_html[:500]}\n"
            f"rs[:500]: {rs_html[:500]}"
        )


def _assert_binary_match(py, rs) -> None:
    py_hash = hashlib.sha256(py.content).hexdigest()
    rs_hash = hashlib.sha256(rs.content).hexdigest()
    if py_hash != rs_hash:
        pytest.fail(
            f"Binary body mismatch:\n"
            f"py SHA-256={py_hash} (len={len(py.content)})\n"
            f"rs SHA-256={rs_hash} (len={len(rs.content)})"
        )


# ─────────────────────────────────────────────────────────────────────────────
# Inline self-tests (pytest)
# ─────────────────────────────────────────────────────────────────────────────


def test_strip_paths_dollar_dot():
    """$..key removes the key recursively at every depth."""
    data = {
        "id": "abc",
        "nested": {"id": "def", "value": 42},
        "list": [{"id": "ghi", "x": 1}, {"x": 2}],
    }
    result = strip_paths(data, frozenset({"$..id"}))
    assert "id" not in result
    assert "id" not in result["nested"]
    assert "id" not in result["list"][0]
    assert result["nested"]["value"] == 42
    assert result["list"][0]["x"] == 1


def test_assert_match_passes_for_equal_dicts():
    """assert_responses_match does not raise for structurally equal JSON responses."""
    import httpx

    resp = httpx.Response(
        200,
        headers={"content-type": "application/json"},
        content=json.dumps({"status": "ok", "id": "volatile"}).encode(),
    )
    # id is in DEFAULT_IGNORE — should not trip
    assert_responses_match(resp, resp, ignore=DEFAULT_IGNORE)


def test_assert_match_fails_on_extra_key():
    """assert_responses_match raises pytest.fail when rs has an extra key."""
    import httpx

    py_resp = httpx.Response(
        200,
        headers={"content-type": "application/json"},
        content=json.dumps({"status": "ok"}).encode(),
    )
    rs_resp = httpx.Response(
        200,
        headers={"content-type": "application/json"},
        content=json.dumps({"status": "ok", "extra_key": "surprise"}).encode(),
    )
    with pytest.raises(pytest.fail.Exception) as exc_info:
        assert_responses_match(py_resp, rs_resp, ignore=frozenset())
    assert "extra_key" in str(exc_info.value)
