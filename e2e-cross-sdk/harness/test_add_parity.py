"""Tests that the ``add`` operation produces identical output in both SDKs.

All tests in this file are **deterministic** — they compare MD5 hashes,
UUID5 IDs, filenames, metadata fields, and stored file bytes.  No LLM
or API key is required.
"""

from helpers import (
    open_db,
    query_data,
    query_datasets,
    query_dataset_data,
    query_rows,
    python_db_path,
    rust_db_path,
    read_stored_file,
    run_python_cli,
    run_rust_cli,
    NLP_TEXT_FILE,
    QC_TEXT_FILE,
    DATASET_NAME,
)


# ── Content hash ─────────────────────────────────────────────────────────────


def test_add_text_content_hash_matches(both_added):
    """Both SDKs must compute the same MD5 content_hash for identical input."""
    py_ws, rust_ws, *_ = both_added

    py_data = query_data(open_db(python_db_path(py_ws)))
    rust_data = query_data(open_db(rust_db_path(rust_ws)))

    assert len(py_data) == 1, f"Expected 1 Python data row, got {len(py_data)}"
    assert len(rust_data) == 1, f"Expected 1 Rust data row, got {len(rust_data)}"

    assert py_data[0]["content_hash"] == rust_data[0]["content_hash"], (
        f"content_hash mismatch:\n"
        f"  Python: {py_data[0]['content_hash']}\n"
        f"  Rust:   {rust_data[0]['content_hash']}"
    )


# ── Data ID ──────────────────────────────────────────────────────────────────


def test_add_text_data_id_matches(both_added):
    """With synced user_id + tenant_id, data.id must be identical (UUID5)."""
    py_ws, rust_ws, *_ = both_added

    py_data = query_data(open_db(python_db_path(py_ws)))
    rust_data = query_data(open_db(rust_db_path(rust_ws)))

    assert py_data[0]["id"] == rust_data[0]["id"], (
        f"data.id mismatch:\n"
        f"  Python: {py_data[0]['id']}\n"
        f"  Rust:   {rust_data[0]['id']}"
    )


# ── Name ─────────────────────────────────────────────────────────────────────


def test_add_text_name_matches(both_added):
    """Both SDKs must produce the same data.name for a file input."""
    py_ws, rust_ws, *_ = both_added

    py_data = query_data(open_db(python_db_path(py_ws)))
    rust_data = query_data(open_db(rust_db_path(rust_ws)))

    assert py_data[0]["name"] == rust_data[0]["name"], (
        f"name mismatch:\n"
        f"  Python: {py_data[0]['name']}\n"
        f"  Rust:   {rust_data[0]['name']}"
    )


# ── Metadata fields ──────────────────────────────────────────────────────────


def test_add_text_metadata_matches(both_added):
    """mime_type, extension, and loader_engine must match."""
    py_ws, rust_ws, *_ = both_added

    py_row = query_data(open_db(python_db_path(py_ws)))[0]
    rust_row = query_data(open_db(rust_db_path(rust_ws)))[0]

    for field in ("mime_type", "extension", "loader_engine"):
        py_val = py_row.get(field)
        rust_val = rust_row.get(field)
        assert py_val == rust_val, (
            f"{field} mismatch:\n"
            f"  Python: {py_val!r}\n"
            f"  Rust:   {rust_val!r}"
        )


# ── Dataset ID ───────────────────────────────────────────────────────────────


def test_add_dataset_id_matches(both_added):
    """datasets.id must be identical (UUID5 of name + user + tenant)."""
    py_ws, rust_ws, *_ = both_added

    py_ds = query_datasets(open_db(python_db_path(py_ws)))
    rust_ds = query_datasets(open_db(rust_db_path(rust_ws)))

    assert len(py_ds) >= 1
    assert len(rust_ds) >= 1

    # Find the dataset by name
    py_ids = {d["name"]: d["id"] for d in py_ds}
    rust_ids = {d["name"]: d["id"] for d in rust_ds}

    assert DATASET_NAME in py_ids, f"Dataset '{DATASET_NAME}' not found in Python DB"
    assert DATASET_NAME in rust_ids, f"Dataset '{DATASET_NAME}' not found in Rust DB"

    assert py_ids[DATASET_NAME] == rust_ids[DATASET_NAME], (
        f"dataset.id mismatch for '{DATASET_NAME}':\n"
        f"  Python: {py_ids[DATASET_NAME]}\n"
        f"  Rust:   {rust_ids[DATASET_NAME]}"
    )


# ── Junction table ───────────────────────────────────────────────────────────


def test_add_junction_row_count(both_added):
    """dataset_data junction table must have the same number of rows."""
    py_ws, rust_ws, *_ = both_added

    py_junctions = query_dataset_data(open_db(python_db_path(py_ws)))
    rust_junctions = query_dataset_data(open_db(rust_db_path(rust_ws)))

    assert len(py_junctions) == len(rust_junctions), (
        f"dataset_data row count mismatch:\n"
        f"  Python: {len(py_junctions)}\n"
        f"  Rust:   {len(rust_junctions)}"
    )


# ── Stored file content ─────────────────────────────────────────────────────


def test_add_stored_file_content_matches(both_added):
    """The actual file stored on disk must be byte-for-byte identical."""
    py_ws, rust_ws, *_ = both_added

    py_row = query_data(open_db(python_db_path(py_ws)))[0]
    rust_row = query_data(open_db(rust_db_path(rust_ws)))[0]

    py_storage = py_ws / ".data_storage"
    rust_storage = rust_ws / ".data_storage"

    py_bytes = read_stored_file(py_storage, py_row["raw_data_location"])
    rust_bytes = read_stored_file(rust_storage, rust_row["raw_data_location"])

    assert py_bytes == rust_bytes, (
        f"Stored file content differs.\n"
        f"  Python file size: {len(py_bytes)} bytes\n"
        f"  Rust file size:   {len(rust_bytes)} bytes\n"
        f"  Python location:  {py_row['raw_data_location']}\n"
        f"  Rust location:    {rust_row['raw_data_location']}"
    )


# ── Deduplication ────────────────────────────────────────────────────────────


def test_add_deduplication(python_add_result, synced_rust_workspace):
    """Adding the same content twice must still produce exactly 1 data row."""
    py_ws, owner_id, tenant_id = python_add_result
    rust_ws, _, _ = synced_rust_workspace

    # Write input file
    input_file = py_ws / "input.txt"
    input_file.write_text(NLP_TEXT_FILE.read_text())
    rust_input = rust_ws / "input.txt"
    rust_input.write_text(NLP_TEXT_FILE.read_text())

    # Python: add the same file again
    run_python_cli(py_ws, ["add", str(input_file), "-d", DATASET_NAME])

    # Rust: add twice
    rust_args = ["add", str(rust_input), "-d", DATASET_NAME]
    if tenant_id:
        rust_args.extend(["--tenant-id", tenant_id])
    run_rust_cli(rust_ws, rust_args)
    run_rust_cli(rust_ws, rust_args)

    py_data = query_data(open_db(python_db_path(py_ws)))
    rust_data = query_data(open_db(rust_db_path(rust_ws)))

    assert len(py_data) == 1, f"Python dedup failed: {len(py_data)} rows"
    assert len(rust_data) == 1, f"Rust dedup failed: {len(rust_data)} rows"


# ── Multiple items ───────────────────────────────────────────────────────────


def test_add_multiple_items(python_add_result, synced_rust_workspace):
    """Adding 2 different texts must produce 2 data rows with matching IDs."""
    py_ws, owner_id, tenant_id = python_add_result
    rust_ws, _, _ = synced_rust_workspace

    # Write input files
    file1_py = py_ws / "input1.txt"
    file1_py.write_text(NLP_TEXT_FILE.read_text())
    file2_py = py_ws / "input2.txt"
    file2_py.write_text(QC_TEXT_FILE.read_text())

    file1_rust = rust_ws / "input1.txt"
    file1_rust.write_text(NLP_TEXT_FILE.read_text())
    file2_rust = rust_ws / "input2.txt"
    file2_rust.write_text(QC_TEXT_FILE.read_text())

    ds = "multi_test"

    # Python: add two files to a new dataset
    run_python_cli(py_ws, ["add", str(file1_py), str(file2_py), "-d", ds])

    # Rust: same
    rust_args = ["add", str(file1_rust), str(file2_rust), "-d", ds]
    if tenant_id:
        rust_args.extend(["--tenant-id", tenant_id])
    run_rust_cli(rust_ws, rust_args)

    # Query only the multi_test dataset's data via junction table
    py_conn = open_db(python_db_path(py_ws))
    rust_conn = open_db(rust_db_path(rust_ws))

    py_data = query_rows(
        py_conn,
        f"SELECT d.* FROM data d "
        f"JOIN dataset_data dd ON d.id = dd.data_id "
        f"JOIN datasets ds ON dd.dataset_id = ds.id "
        f"WHERE ds.name = '{ds}' ORDER BY d.name",
    )
    rust_data = query_rows(
        rust_conn,
        f"SELECT d.* FROM data d "
        f"JOIN dataset_data dd ON d.id = dd.data_id "
        f"JOIN datasets ds ON dd.dataset_id = ds.id "
        f"WHERE ds.name = '{ds}' ORDER BY d.name",
    )
    py_conn.close()
    rust_conn.close()

    assert len(py_data) == 2, f"Python: expected 2 data rows, got {len(py_data)}"
    assert len(rust_data) == 2, f"Rust: expected 2 data rows, got {len(rust_data)}"

    # IDs should match (same content, same user, same tenant)
    py_ids = sorted(d["id"] for d in py_data)
    rust_ids = sorted(d["id"] for d in rust_data)
    assert py_ids == rust_ids, (
        f"data IDs mismatch for multi-item add:\n"
        f"  Python: {py_ids}\n"
        f"  Rust:   {rust_ids}"
    )
