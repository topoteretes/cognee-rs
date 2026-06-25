"""Delete shared-entity parity: partial delete with overlapping entities (Gap E3).

Both SDKs add two documents whose content shares entities (e.g. NLP and
quantum computing both mention "computer"), cognify them, then delete one
document's dataset.  We compare the remaining provenance (nodes) between
SDKs with tolerance-based Jaccard similarity, since LLM extraction is
non-deterministic.

All tests require an OpenAI API key (cognify invokes the LLM).
"""

import pytest

from helpers import (
    open_db,
    query_data,
    query_datasets,
    query_nodes,
    query_edges,
    python_db_path,
    rust_db_path,
    run_python_cli,
    run_rust_cli,
    write_rust_config,
    NLP_TEXT_FILE,
    QC_TEXT_FILE,
)
from conftest import requires_openai

DATASET_A = "shared_entities_a"
DATASET_B = "shared_entities_b"


@requires_openai
def test_shared_entity_delete_parity(tmp_path):
    """After deleting one of two overlapping datasets, remaining node types
    should overlap between SDKs (Jaccard >= 0.3).

    Flow per SDK:
      1. Add doc A (NLP text) to dataset_a, add doc B (quantum computing
         text) to dataset_b.
      2. Cognify both datasets.
      3. Delete dataset_a only.
      4. Verify dataset_b's data still exists.
      5. Collect remaining node types and compare across SDKs.
    """
    py_ws = tmp_path / "python"
    py_ws.mkdir()
    rust_ws = tmp_path / "rust"
    rust_ws.mkdir()

    # Write input files
    input_a_py = py_ws / "input_nlp.txt"
    input_a_py.write_text(NLP_TEXT_FILE.read_text())
    input_b_py = py_ws / "input_qc.txt"
    input_b_py.write_text(QC_TEXT_FILE.read_text())

    input_a_rust = rust_ws / "input_nlp.txt"
    input_a_rust.write_text(NLP_TEXT_FILE.read_text())
    input_b_rust = rust_ws / "input_qc.txt"
    input_b_rust.write_text(QC_TEXT_FILE.read_text())

    # ── Python: add both datasets ────────────────────────────────────────
    result = run_python_cli(
        py_ws, ["add", str(input_a_py), "-d", DATASET_A], check=False
    )
    assert result.returncode == 0, (
        f"Python add dataset_a failed:\n{result.stdout}\n{result.stderr}"
    )
    result = run_python_cli(
        py_ws, ["add", str(input_b_py), "-d", DATASET_B], check=False
    )
    assert result.returncode == 0, (
        f"Python add dataset_b failed:\n{result.stdout}\n{result.stderr}"
    )

    # Python: cognify both
    result = run_python_cli(py_ws, ["cognify", "-d", DATASET_A], check=False)
    assert result.returncode == 0, (
        f"Python cognify dataset_a failed:\n{result.stdout}\n{result.stderr}"
    )
    result = run_python_cli(py_ws, ["cognify", "-d", DATASET_B], check=False)
    assert result.returncode == 0, (
        f"Python cognify dataset_b failed:\n{result.stdout}\n{result.stderr}"
    )

    # ── Rust: add both datasets ──────────────────────────────────────────
    write_rust_config(rust_ws)

    result = run_rust_cli(
        rust_ws, ["add", str(input_a_rust), "-d", DATASET_A], check=False
    )
    assert result.returncode == 0, (
        f"Rust add dataset_a failed:\n{result.stdout}\n{result.stderr}"
    )
    result = run_rust_cli(
        rust_ws, ["add", str(input_b_rust), "-d", DATASET_B], check=False
    )
    assert result.returncode == 0, (
        f"Rust add dataset_b failed:\n{result.stdout}\n{result.stderr}"
    )

    # Rust: cognify both
    result = run_rust_cli(rust_ws, ["cognify", "-d", DATASET_A], check=False)
    assert result.returncode == 0, (
        f"Rust cognify dataset_a failed:\n{result.stdout}\n{result.stderr}"
    )
    result = run_rust_cli(rust_ws, ["cognify", "-d", DATASET_B], check=False)
    assert result.returncode == 0, (
        f"Rust cognify dataset_b failed:\n{result.stdout}\n{result.stderr}"
    )

    # Pre-delete sanity: both SDKs should have 2 datasets
    py_db = python_db_path(py_ws)
    rust_db = rust_db_path(rust_ws)
    assert len(query_datasets(open_db(py_db))) >= 2, "Python has < 2 datasets"
    assert len(query_datasets(open_db(rust_db))) >= 2, "Rust has < 2 datasets"

    # ── Delete dataset_a only ────────────────────────────────────────────
    result = run_python_cli(
        py_ws, ["delete", "-d", DATASET_A, "-f"], check=False
    )
    assert result.returncode == 0, (
        f"Python delete dataset_a failed:\n{result.stdout}\n{result.stderr}"
    )

    result = run_rust_cli(
        rust_ws, ["delete", "-d", DATASET_A, "-f"], check=False
    )
    assert result.returncode == 0, (
        f"Rust delete dataset_a failed:\n{result.stdout}\n{result.stderr}"
    )

    # ── Verify dataset_b data survives ───────────────────────────────────
    py_conn = open_db(py_db)
    py_datasets = query_datasets(py_conn)
    py_data = query_data(py_conn)
    py_nodes = query_nodes(py_conn)
    py_conn.close()

    rust_conn = open_db(rust_db)
    rust_datasets = query_datasets(rust_conn)
    rust_data = query_data(rust_conn)
    rust_nodes = query_nodes(rust_conn)
    rust_conn.close()

    # At least dataset_b should survive
    py_ds_names = {d["name"] for d in py_datasets}
    rust_ds_names = {d["name"] for d in rust_datasets}

    assert DATASET_B in py_ds_names, (
        f"Python lost dataset_b after deleting dataset_a. "
        f"Remaining datasets: {py_ds_names}"
    )
    assert DATASET_B in rust_ds_names, (
        f"Rust lost dataset_b after deleting dataset_a. "
        f"Remaining datasets: {rust_ds_names}"
    )

    # dataset_a should be gone
    assert DATASET_A not in py_ds_names, (
        f"Python still has dataset_a after delete"
    )
    assert DATASET_A not in rust_ds_names, (
        f"Rust still has dataset_a after delete"
    )

    # Data rows: at least one should remain (from dataset_b)
    assert len(py_data) >= 1, "Python has no data rows after partial delete"
    assert len(rust_data) >= 1, "Rust has no data rows after partial delete"

    # ── Tolerance-based node type comparison ─────────────────────────────
    py_types = {
        n["type"] for n in py_nodes if n.get("type")
    }
    rust_types = {
        n["type"] for n in rust_nodes if n.get("type")
    }

    if not py_types and not rust_types:
        pytest.skip("Both SDKs produced zero typed nodes after partial delete")

    intersection = py_types & rust_types
    union = py_types | rust_types
    jaccard = len(intersection) / len(union) if union else 0

    assert jaccard >= 0.3, (
        f"Remaining node type Jaccard similarity too low ({jaccard:.2f}) "
        f"after partial delete:\n"
        f"  Python types: {sorted(py_types)}\n"
        f"  Rust types:   {sorted(rust_types)}\n"
        f"  Overlap:      {sorted(intersection)}"
    )
