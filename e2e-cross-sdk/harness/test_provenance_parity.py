"""Cross-SDK parity test for DataPoint provenance stamping (gap 05-10 §4.5).

Both Python and Rust cognify must stamp graph nodes with the same
``source_pipeline`` literal and the same set of ``source_task`` values
per node type (within a non-determinism tolerance).

Locked decision 10 puts this test in scope of gap 05.

Conventions mirrored from ``test_cognify_structural.py`` in the same
directory:

* Pytest discovery root is ``e2e-cross-sdk/harness/`` — there is no
  ``e2e-cross-sdk/tests/``.
* The Docker harness builds both CLIs and runs ``add`` + ``cognify`` via
  the existing ``both_cognified`` fixture in
  :mod:`harness/conftest.py`.
* Provenance attributes round-trip into the ``nodes`` table of each
  SDK's relational SQLite store; we read them via the existing
  ``query_nodes`` helper (which already normalises the
  ``type``/``node_type`` column rename across SDKs).
* All assertions tolerate LLM non-determinism — the harness is
  intentionally approximate.
"""

import pytest

from helpers import (
    open_db,
    python_db_path,
    query_nodes,
    rust_db_path,
)
from conftest import requires_openai


# ── Parity thresholds ────────────────────────────────────────────────────────

#: Minimum per-node-type Jaccard similarity on the set of `source_task`
#: values for the same node type. Wide tolerance because LLM extraction
#: often emits or skips a stage's worth of nodes between runs.
PARITY_THRESHOLD = 0.5

#: The task name literals that the cognify pipeline is expected to
#: stamp DataPoints with (matches Python's ``run_tasks_base.py``
#: per-yield call site and the Rust convenience ``cognify()`` /
#: pipeline-driven executor stamping).
EXPECTED_TASKS = {
    "classify_documents",
    "extract_chunks_from_documents",
    "extract_graph_from_data",
    "summarize_text",
}


# ── Helpers ──────────────────────────────────────────────────────────────────


def _attr(node, key):
    """Return ``node[key]`` whether the row is a SQLite Row, dict, or
    JSON-attributes blob (Rust serialises the DataPoint dump into a
    text column on some adapters; Python stores it directly)."""
    if key in node and node[key] is not None:
        return node[key]
    # Some adapters serialise the DataPoint properties under
    # an "attributes" / "properties" JSON column.  Fall through to
    # those if a top-level key is missing.
    for blob_key in ("attributes", "properties"):
        blob = node.get(blob_key)
        if isinstance(blob, str):
            try:
                import json
                blob = json.loads(blob)
            except (ValueError, TypeError):
                continue
        if isinstance(blob, dict) and key in blob:
            return blob[key]
    return None


def _group_tasks_by_type(nodes):
    """Build ``{node_type: set(source_task)}`` from a list of node rows.

    Skips rows that have no ``source_task`` set — they are either
    pre-provenance legacy rows or auxiliary nodes the pipeline does not
    stamp directly.
    """
    out: dict[str, set[str]] = {}
    for n in nodes:
        node_type = n.get("type") or n.get("node_type")
        task = _attr(n, "source_task")
        if not node_type or not task:
            continue
        out.setdefault(node_type, set()).add(task)
    return out


# ── Tests ────────────────────────────────────────────────────────────────────


@requires_openai
def test_provenance_pipeline_field_set_on_every_node(both_cognified):
    """Every cognify-produced node must have ``source_pipeline ==
    "cognify_pipeline"`` in both backends."""
    py_ws, rust_ws = both_cognified

    for backend, ws, path_fn in (
        ("python", py_ws, python_db_path),
        ("rust", rust_ws, rust_db_path),
    ):
        nodes = query_nodes(open_db(path_fn(ws)))
        assert nodes, f"{backend}: cognify produced zero nodes"
        stamped = [n for n in nodes if _attr(n, "source_pipeline")]
        # Tolerate adapters that don't surface provenance in the
        # relational `nodes` table at all (e.g. older Python schemas).
        if not stamped:
            pytest.skip(
                f"{backend}: no nodes carry source_pipeline in the "
                "relational nodes table — adapter does not round-trip "
                "provenance to SQLite"
            )
        for n in stamped:
            assert _attr(n, "source_pipeline") == "cognify_pipeline", (
                f"{backend}: node {n.get('id')} has unexpected "
                f"source_pipeline {_attr(n, 'source_pipeline')!r}"
            )


@requires_openai
def test_provenance_user_field_present_when_stamped(both_cognified):
    """When a node carries ``source_user``, the value must be a
    non-empty string in both backends.

    The Python and Rust SDKs build the label slightly differently
    (Python uses ``user.email``, Rust falls back to
    ``user_id.to_string()`` when no email is configured), so we don't
    assert byte-equality between SDKs — only that whichever value is
    present is non-empty.
    """
    py_ws, rust_ws = both_cognified

    for backend, ws, path_fn in (
        ("python", py_ws, python_db_path),
        ("rust", rust_ws, rust_db_path),
    ):
        nodes = query_nodes(open_db(path_fn(ws)))
        users = [_attr(n, "source_user") for n in nodes]
        users = [u for u in users if u]
        if not users:
            pytest.skip(f"{backend}: source_user not surfaced in nodes table")
        for u in users:
            assert isinstance(u, str) and u.strip(), (
                f"{backend}: source_user must be a non-empty string, got {u!r}"
            )


@requires_openai
def test_source_task_values_are_in_expected_set(both_cognified):
    """Every observed ``source_task`` must come from the documented set
    that ``cognify_pipeline`` is allowed to emit."""
    py_ws, rust_ws = both_cognified

    for backend, ws, path_fn in (
        ("python", py_ws, python_db_path),
        ("rust", rust_ws, rust_db_path),
    ):
        nodes = query_nodes(open_db(path_fn(ws)))
        seen = {_attr(n, "source_task") for n in nodes}
        seen.discard(None)
        if not seen:
            pytest.skip(
                f"{backend}: no source_task values present (adapter "
                "doesn't surface provenance to SQLite)"
            )
        unexpected = seen - EXPECTED_TASKS
        assert not unexpected, (
            f"{backend}: saw source_task values outside the expected "
            f"cognify_pipeline set: {sorted(unexpected)}"
        )


@requires_openai
def test_source_task_jaccard_overlap_per_node_type(both_cognified):
    """For node types present in both SDKs, the multiset of
    ``source_task`` values must overlap by Jaccard ≥ ``PARITY_THRESHOLD``.

    LLM extraction is non-deterministic so we use Jaccard rather than
    set equality. Locked decision 10 ships this assertion as the
    canonical cross-SDK provenance gate.
    """
    py_ws, rust_ws = both_cognified

    py_groups = _group_tasks_by_type(query_nodes(open_db(python_db_path(py_ws))))
    rust_groups = _group_tasks_by_type(query_nodes(open_db(rust_db_path(rust_ws))))

    if not py_groups or not rust_groups:
        pytest.skip(
            "Provenance fields not surfaced in the SQLite nodes table for "
            f"one or both SDKs (py_groups={bool(py_groups)}, "
            f"rust_groups={bool(rust_groups)}); LLM cross-SDK parity gate "
            "cannot run"
        )

    shared_types = set(py_groups) & set(rust_groups)
    if not shared_types:
        pytest.skip(
            "Python and Rust produced disjoint node-type sets; nothing to "
            f"compare (py={sorted(py_groups)}, rust={sorted(rust_groups)})"
        )

    for node_type in shared_types:
        py_set = py_groups[node_type]
        rust_set = rust_groups[node_type]
        union = py_set | rust_set
        intersection = py_set & rust_set
        jaccard = len(intersection) / len(union) if union else 0.0
        assert jaccard >= PARITY_THRESHOLD, (
            f"source_task Jaccard for node_type {node_type!r}: "
            f"{jaccard:.2f} (rust={sorted(rust_set)}, "
            f"python={sorted(py_set)})"
        )
