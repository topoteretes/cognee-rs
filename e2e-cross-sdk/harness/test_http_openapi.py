"""Phase-1 structural diff tests for GET /openapi.json.

Fetches /openapi.json from both servers and runs four structural diffs:
1. Path-set diff: sets of registered path strings.
2. Method-set diff: per shared path, the set of HTTP methods must match.
3. Security-scheme diff: ``components.securitySchemes`` key sets must match.
4. Top-level ``components.schemas`` key-set diff.

A normalizer is applied before each diff per plan.md §7 Q5 (collapses path
parameter name variants, e.g. ``{dataset_id}`` vs ``{datasetId}``).

Per p8-e2e-parity.md Step 12: the normalizer allowlist must be approved before
the test is un-skipped.  The test currently has a pytest.mark.skip and will be
enabled in a follow-up commit once the allowlist is approved.

Also commits ``harness/golden/openapi.python.json`` as a reviewer aid (not
asserted by these tests).
"""

from __future__ import annotations

import json
import re
from pathlib import Path

import pytest

# ── Normalizer ────────────────────────────────────────────────────────────────

# Canonical parameter name re-writing: collapse camelCase and snake_case
# parameter names to a single lowercase_snake form so structural diffs
# don't trip on naming-style differences.
_PARAM_RE = re.compile(r"\{([^}]+)\}")


def _camel_to_snake(name: str) -> str:
    """Convert camelCase or PascalCase to snake_case."""
    s = re.sub(r"([A-Z]+)([A-Z][a-z])", r"\1_\2", name)
    s = re.sub(r"([a-z\d])([A-Z])", r"\1_\2", s)
    return s.lower()


def _normalise_path(path: str) -> str:
    """Collapse ``{paramName}`` / ``{param_name}`` variants to snake_case."""
    return _PARAM_RE.sub(lambda m: "{" + _camel_to_snake(m.group(1)) + "}", path)


def _normalise_paths(paths_dict: dict) -> dict:
    """Return a new dict with all path keys normalised."""
    return {_normalise_path(k): v for k, v in paths_dict.items()}


def _normalise_methods(path_item: dict) -> set[str]:
    """Return the set of HTTP method names for a path item (upper-cased)."""
    http_methods = {"get", "post", "put", "patch", "delete", "head", "options", "trace"}
    return {m.upper() for m in path_item if m.lower() in http_methods}


# ── Helpers ───────────────────────────────────────────────────────────────────


def _fetch_openapi(client, url: str = "/openapi.json") -> dict:
    r = client.get(url)
    assert r.status_code == 200, f"GET {url} returned {r.status_code}: {r.text[:300]}"
    return r.json()


# ── Tests (currently skipped pending normalizer allowlist approval) ───────────

pytestmark = pytest.mark.skip(
    reason="normalizer allowlist pending — plan.md §7 Q5; remove skip once approved"
)


def test_openapi_path_set(py_client, rs_client):
    """The set of registered paths must match after normalisation."""
    py_spec = _fetch_openapi(py_client)
    rs_spec = _fetch_openapi(rs_client)

    py_paths = set(_normalise_paths(py_spec.get("paths", {})).keys())
    rs_paths = set(_normalise_paths(rs_spec.get("paths", {})).keys())

    only_in_py = py_paths - rs_paths
    only_in_rs = rs_paths - py_paths

    assert not only_in_py and not only_in_rs, (
        f"OpenAPI path-set mismatch:\n"
        f"  paths only in Python ({len(only_in_py)}): {sorted(only_in_py)[:20]}\n"
        f"  paths only in Rust   ({len(only_in_rs)}): {sorted(only_in_rs)[:20]}"
    )


def test_openapi_method_set(py_client, rs_client):
    """For each path present on both sides, the HTTP method sets must match."""
    py_spec = _fetch_openapi(py_client)
    rs_spec = _fetch_openapi(rs_client)

    py_paths = _normalise_paths(py_spec.get("paths", {}))
    rs_paths = _normalise_paths(rs_spec.get("paths", {}))
    shared = set(py_paths) & set(rs_paths)

    mismatches: list[str] = []
    for path in sorted(shared):
        py_methods = _normalise_methods(py_paths[path])
        rs_methods = _normalise_methods(rs_paths[path])
        if py_methods != rs_methods:
            mismatches.append(
                f"  {path}: py={sorted(py_methods)} rs={sorted(rs_methods)}"
            )

    assert not mismatches, (
        f"OpenAPI method-set mismatches ({len(mismatches)}):\n" + "\n".join(mismatches)
    )


def test_openapi_security_schemes(py_client, rs_client):
    """``components.securitySchemes`` key sets must match."""
    py_spec = _fetch_openapi(py_client)
    rs_spec = _fetch_openapi(rs_client)

    py_schemes = set(
        (py_spec.get("components") or {}).get("securitySchemes", {}).keys()
    )
    rs_schemes = set(
        (rs_spec.get("components") or {}).get("securitySchemes", {}).keys()
    )

    assert py_schemes == rs_schemes, (
        f"Security scheme mismatch:\n"
        f"  only in py: {py_schemes - rs_schemes}\n"
        f"  only in rs: {rs_schemes - py_schemes}"
    )


def test_openapi_schema_keys(py_client, rs_client):
    """``components.schemas`` key sets must match (per-schema field diff is a follow-up)."""
    py_spec = _fetch_openapi(py_client)
    rs_spec = _fetch_openapi(rs_client)

    py_schemas = set(
        (py_spec.get("components") or {}).get("schemas", {}).keys()
    )
    rs_schemas = set(
        (rs_spec.get("components") or {}).get("schemas", {}).keys()
    )

    only_in_py = py_schemas - rs_schemas
    only_in_rs = rs_schemas - py_schemas

    assert not only_in_py and not only_in_rs, (
        f"Schema key mismatch:\n"
        f"  only in py ({len(only_in_py)}): {sorted(only_in_py)[:20]}\n"
        f"  only in rs ({len(only_in_rs)}): {sorted(only_in_rs)[:20]}"
    )


# ── Golden snapshot writer (informational, not an assertion) ──────────────────
# Uncomment to regenerate: pytest -vs test_http_openapi.py::_write_golden_snapshot

def _write_golden_snapshot(py_client):
    """Write a golden snapshot of the Python /openapi.json for reviewer reference.

    This is NOT a test assertion — it's a developer helper to refresh the
    committed snapshot when the Python API surface changes intentionally.
    """
    golden_dir = Path(__file__).parent / "golden"
    golden_dir.mkdir(exist_ok=True)
    spec = _fetch_openapi(py_client)
    snapshot_path = golden_dir / "openapi.python.json"
    snapshot_path.write_text(json.dumps(spec, indent=2, sort_keys=True))
    print(f"Golden snapshot written to {snapshot_path}")
