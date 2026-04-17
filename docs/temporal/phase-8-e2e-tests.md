# Phase 8 — E2E Cross-SDK Tests

**Files:** `e2e-cross-sdk/harness/test_temporal_search.py` (new), `e2e-cross-sdk/harness/helpers.py` (extend)  
**Status:** Done

---

## Goal

Verify that the Rust temporal cognify pipeline produces graph structures compatible with the Python SDK: both SDKs must produce `Event` and `Timestamp` nodes from the same input, node counts must be within 50% of each other, and temporal search must return non-empty results on both sides.

---

## Python Reference — Existing Pattern

The existing cross-SDK tests (`test_cognify_structural.py`) establish the pattern:
- Both CLIs run `add` then `cognify` on the same text file.
- SQLite databases are opened and node/edge counts compared.
- Tolerance: 50% divergence allowed (LLM is non-deterministic).
- `@requires_openai` decorator skips when `OPENAI_API_KEY`/`OPENAI_TOKEN` is absent.

---

## Test Data Fixture

```python
# e2e-cross-sdk/test_data/biography_temporal.txt  (new file)
# A 500-word excerpt from the Python test biographies with dense date references.
# Must contain at least 10 explicit dates to ensure ≥ 5 Event/Timestamp nodes.
BIOGRAPHY_TEXT = (TEST_DATA_DIR / "biography_temporal.txt").read_text()
```

Copy the Arnulf Øverland or Attaphol Buspakom biography from the Python test suite (`/tmp/cognee-python/cognee/tests/test_temporal_graph.py`) into `e2e-cross-sdk/test_data/biography_temporal.txt`.

---

## Helper Addition: `query_nodes_by_type`

Add to `e2e-cross-sdk/harness/helpers.py`:

```python
def query_nodes_by_type(conn: sqlite3.Connection, node_type: str) -> list[dict]:
    """Return all graph nodes of a given type from the graph SQLite store.

    The Rust Ladybug adapter and the Python Kuzu adapter both write node
    properties as JSON; the exact column names differ but both expose a
    filterable ``type`` property.
    """
    rows = conn.execute(
        "SELECT id, properties FROM graph_node "
        "WHERE json_extract(properties, '$.type') = ?",
        (node_type,),
    ).fetchall()
    return [{"id": r[0], **json.loads(r[1])} for r in rows]
```

**Note:** Verify the Ladybug SQLite schema column names (`id`, `properties`) match before running. Adjust the query if the column is named differently (e.g. `node_id`, `data`).

---

## New File: `e2e-cross-sdk/harness/test_temporal_search.py`

```python
"""Cross-SDK parity tests for the temporal cognify pipeline and TEMPORAL search.

Both CLIs are run with --temporal-cognify on the same biography text.
Node counts are compared with 50% tolerance (LLM is non-deterministic).
"""

import pytest
from pathlib import Path

from helpers import (
    open_db,
    python_db_path,
    rust_db_path,
    run_python_cli,
    run_rust_cli,
    query_nodes_by_type,
    NLP_TEXT_FILE,
    TEST_DATA_DIR,
    DATASET_NAME,
)
from conftest import requires_openai

TEMPORAL_DATASET = "temporal_e2e"
BIOGRAPHY_FILE = TEST_DATA_DIR / "biography_temporal.txt"


# ── Fixtures ─────────────────────────────────────────────────────────────────


@pytest.fixture
def both_temporal_cognified(python_workspace, rust_workspace):
    """Run temporal cognify on both SDKs with the same biography text."""
    # Python
    run_python_cli(python_workspace, [
        "add", str(BIOGRAPHY_FILE), "--dataset", TEMPORAL_DATASET,
    ])
    run_python_cli(python_workspace, [
        "cognify", "--dataset", TEMPORAL_DATASET, "--temporal-cognify",
    ])

    # Rust
    run_rust_cli(rust_workspace, [
        "add", str(BIOGRAPHY_FILE), "--dataset", TEMPORAL_DATASET,
    ])
    run_rust_cli(rust_workspace, [
        "cognify", "--dataset", TEMPORAL_DATASET, "--temporal-cognify",
    ])

    return python_workspace, rust_workspace


# ── Tests ─────────────────────────────────────────────────────────────────────


@requires_openai
def test_temporal_cognify_produces_event_nodes(both_temporal_cognified):
    """Both SDKs must produce Event nodes in the graph database."""
    py_ws, rust_ws = both_temporal_cognified
    py_events   = query_nodes_by_type(open_db(python_db_path(py_ws)), "Event")
    rust_events = query_nodes_by_type(open_db(rust_db_path(rust_ws)),  "Event")
    assert len(py_events)   >= 5, f"Python produced only {len(py_events)} Event nodes"
    assert len(rust_events) >= 5, f"Rust produced only {len(rust_events)} Event nodes"


@requires_openai
def test_temporal_cognify_produces_timestamp_nodes(both_temporal_cognified):
    """Both SDKs must produce Timestamp nodes in the graph database."""
    py_ws, rust_ws = both_temporal_cognified
    py_ts   = query_nodes_by_type(open_db(python_db_path(py_ws)), "Timestamp")
    rust_ts = query_nodes_by_type(open_db(rust_db_path(rust_ws)),  "Timestamp")
    assert len(py_ts)   >= 5, f"Python produced only {len(py_ts)} Timestamp nodes"
    assert len(rust_ts) >= 5, f"Rust produced only {len(rust_ts)} Timestamp nodes"


@requires_openai
def test_temporal_event_count_within_tolerance(both_temporal_cognified):
    """Event node counts must be within 50% of each other."""
    py_ws, rust_ws = both_temporal_cognified
    py_count   = len(query_nodes_by_type(open_db(python_db_path(py_ws)), "Event"))
    rust_count = len(query_nodes_by_type(open_db(rust_db_path(rust_ws)),  "Event"))
    avg   = (py_count + rust_count) / 2
    ratio = abs(py_count - rust_count) / avg if avg > 0 else 0
    assert ratio <= 0.5, (
        f"Event count divergence too large ({ratio:.0%}): "
        f"Python={py_count}, Rust={rust_count}"
    )


@requires_openai
def test_temporal_timestamp_count_within_tolerance(both_temporal_cognified):
    """Timestamp node counts must be within 50% of each other."""
    py_ws, rust_ws = both_temporal_cognified
    py_count   = len(query_nodes_by_type(open_db(python_db_path(py_ws)), "Timestamp"))
    rust_count = len(query_nodes_by_type(open_db(rust_db_path(rust_ws)),  "Timestamp"))
    avg   = (py_count + rust_count) / 2
    ratio = abs(py_count - rust_count) / avg if avg > 0 else 0
    assert ratio <= 0.5, (
        f"Timestamp count divergence too large ({ratio:.0%}): "
        f"Python={py_count}, Rust={rust_count}"
    )


@requires_openai
def test_temporal_search_returns_non_empty_results(both_temporal_cognified):
    """TEMPORAL search must return non-empty output on both SDKs."""
    py_ws, rust_ws = both_temporal_cognified

    py_out   = run_python_cli(py_ws,   ["search", "TEMPORAL", "What events happened?"])
    rust_out = run_rust_cli(rust_ws,   ["search", "TEMPORAL", "What events happened?"])

    assert py_out.returncode == 0,   f"Python search failed: {py_out.stderr}"
    assert rust_out.returncode == 0, f"Rust search failed: {rust_out.stderr}"
    assert len(py_out.stdout.strip())   > 0, "Python TEMPORAL search returned empty output"
    assert len(rust_out.stdout.strip()) > 0, "Rust TEMPORAL search returned empty output"


@requires_openai
def test_temporal_search_with_year_filter(both_temporal_cognified):
    """TEMPORAL search with a year filter must return results on both SDKs."""
    py_ws, rust_ws = both_temporal_cognified

    # The biography fixture contains events from the 1960s–2010s.
    py_out   = run_python_cli(py_ws,   ["search", "TEMPORAL", "What happened in 1985?"])
    rust_out = run_rust_cli(rust_ws,   ["search", "TEMPORAL", "What happened in 1985?"])

    assert py_out.returncode == 0
    assert rust_out.returncode == 0
    # We do not compare exact text — LLM output is non-deterministic.
    # Just verify both CLIs completed and produced output.
    assert len(py_out.stdout.strip())   > 0
    assert len(rust_out.stdout.strip()) > 0
```

---

## Dockerfile Changes

The cross-SDK Dockerfile builds both CLIs into one image. No changes are required to the build stages. The `biography_temporal.txt` fixture goes into `e2e-cross-sdk/test_data/` which is already copied into the image (check the `COPY test_data /test_data` line in the Dockerfile).

Verify the `--temporal-cognify` flag is available in both the Python and Rust CLIs before pushing to CI.

---

## Running the Tests

```bash
cd e2e-cross-sdk
OPENAI_API_KEY=sk-... docker compose up --build
```

Or targeting only the temporal suite:

```bash
docker compose run tests pytest harness/test_temporal_search.py -v
```

---

## Verification Checklist

- [ ] `biography_temporal.txt` fixture added to `e2e-cross-sdk/test_data/`
- [ ] `query_nodes_by_type` helper added to `helpers.py`
- [ ] SQLite column names verified against Ladybug adapter schema
- [ ] All 6 tests in `test_temporal_search.py` pass
- [ ] Python `--temporal-cognify` CLI flag confirmed (already exists in Python SDK)
- [ ] Rust `--temporal-cognify` CLI flag confirmed (added in Phase 5)
