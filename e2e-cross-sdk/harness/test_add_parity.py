"""Tests that the ``add`` operation produces identical output in both SDKs.

All tests in this file are **deterministic** — they compare MD5 hashes,
dataset UUID5 IDs, filenames, metadata fields, and stored file bytes.  No LLM
or API key is required.

Data ids are NOT compared across SDKs: both mint a random ``uuid4`` for a new
``data`` row and dedup by ``(content_hash, dataset_id, owner_id, tenant_id)``
(Python ``cognee/tasks/ingestion/ingest_data.py:364-389`` with
``identify_many``; ``get_unique_data_id``'s uuid5 is used only by dlt sources
and code_repo). The tests check that shape instead (SDK-1039 D7).
"""

import json
import threading
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

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
    write_rust_config,
    NLP_TEXT_FILE,
    QC_TEXT_FILE,
    DATASET_NAME,
)


_URL_FIXTURE_HTML = b"""\
<html>
  <head>
    <title>Cross SDK URL Fixture</title>
    <style>.hidden { display: none; }</style>
  </head>
  <body>
    <h1>Cross SDK URL heading</h1>
    <p>Local URL fixture body for add parity.</p>
    <script>window.secret = true;</script>
  </body>
</html>
"""


class _UrlFixtureHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/robots.txt":
            self.send_response(404)
            self.end_headers()
            return
        if self.path == "/page.html":
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(_URL_FIXTURE_HTML)))
            self.end_headers()
            self.wfile.write(_URL_FIXTURE_HTML)
            return
        self.send_response(404)
        self.end_headers()

    def log_message(self, *_args):
        pass


def _json_metadata(row):
    raw = row.get("external_metadata")
    assert raw, f"expected external_metadata in row: {row}"
    return json.loads(raw)


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


def _uuid_version(value) -> int:
    return uuid.UUID(str(value)).version


def test_add_text_data_id_is_dataset_scoped_uuid4(both_added):
    """Both SDKs give a new data row a random uuid4 owned by its dataset.

    Replaces the old cross-SDK ``data.id`` equality check, whose premise (a
    uuid5 of content + owner + tenant) Python no longer has: ``ingest_data``
    mints ``uuid4()`` for a new row and stores its dataset in
    ``data.dataset_id`` (SDK-1039 D7).
    """
    py_ws, rust_ws, *_ = both_added

    for sdk, db in (("Python", python_db_path(py_ws)), ("Rust", rust_db_path(rust_ws))):
        conn = open_db(db)
        data = query_data(conn)
        datasets = {d["name"]: d["id"] for d in query_datasets(conn)}
        conn.close()
        assert len(data) == 1, f"{sdk}: expected 1 data row, got {len(data)}"
        assert _uuid_version(data[0]["id"]) == 4, (
            f"{sdk}: data.id {data[0]['id']} is not a uuid4"
        )
        assert data[0].get("dataset_id") == datasets[DATASET_NAME], (
            f"{sdk}: data.dataset_id {data[0].get('dataset_id')!r} is not the id of "
            f"'{DATASET_NAME}' ({datasets[DATASET_NAME]!r})"
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
    """Both SDKs must record the same number of dataset memberships."""
    py_ws, rust_ws, *_ = both_added

    py_junctions = query_dataset_data(open_db(python_db_path(py_ws)))
    rust_junctions = query_dataset_data(open_db(rust_db_path(rust_ws)))

    assert len(py_junctions) == len(rust_junctions), (
        f"dataset membership count mismatch:\n"
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


# ── URL inputs ───────────────────────────────────────────────────────────────


def _url_add_both(tmp_path, url, dataset):
    """Run ``add <url>`` in both SDKs; return (py_ws, rust_ws, py_result, rust_result)."""
    py_ws = tmp_path / "python_url"
    rust_ws = tmp_path / "rust_url"
    py_ws.mkdir()
    rust_ws.mkdir()

    py_result = run_python_cli(py_ws, ["add", url, "-d", dataset], check=False)
    if py_result.returncode == 0:
        py_conn = open_db(python_db_path(py_ws))
        py_owner = query_datasets(py_conn)[0]["owner_id"]
        py_tenant = query_datasets(py_conn)[0].get("tenant_id")
        py_conn.close()
    else:
        py_owner, py_tenant = None, None

    write_rust_config(rust_ws, **({"user_id": str(py_owner)} if py_owner else {}))
    rust_args = ["add", url, "-d", dataset]
    if py_tenant:
        rust_args.extend(["--tenant-id", str(py_tenant)])
    rust_result = run_rust_cli(rust_ws, rust_args, check=False)
    return py_ws, rust_ws, py_result, rust_result


def _data_rows(workspace, db_path_fn):
    path = db_path_fn(workspace)
    if not path.exists():
        return []
    conn = open_db(path)
    try:
        if conn.execute(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='data'"
        ).fetchone() is None:
            return []
        return query_data(conn)
    finally:
        conn.close()


def test_add_loopback_url_rejected_by_both(tmp_path):
    """Both SDKs refuse a loopback URL, and neither stores anything for it.

    SDK-1039 D9. Both SDKs block loopback/private hosts with no bypass:
    Python ``cognee/tasks/web_scraper/ssrf_protection.py:53-70``
    (``SSRFProtectionError``, ``URL host ... points to a non-public address``;
    the only switch is ``ALLOW_HTTP_REQUESTS=false``, which disables all
    fetching) and Rust ``crates/ingestion/src/url_crawler/fetcher.rs``
    (``URL targets a blocked address``; private hosts are allowed only under
    ``cfg(test)``). The fixture is a real server on 127.0.0.1, so a rejection
    is the policy and not a connection failure: the server must see no request.
    """
    hits = []

    class _Counting(_UrlFixtureHandler):
        def do_GET(self):
            hits.append(self.path)
            super().do_GET()

    server = ThreadingHTTPServer(("127.0.0.1", 0), _Counting)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        url = f"http://127.0.0.1:{server.server_port}/page.html"
        py_ws, rust_ws, py_result, rust_result = _url_add_both(
            tmp_path, url, "url_rejection"
        )
    finally:
        server.shutdown()
        server.server_close()

    py_out = f"{py_result.stdout}\n{py_result.stderr}"
    rust_out = f"{rust_result.stdout}\n{rust_result.stderr}"

    assert py_result.returncode != 0, f"Python accepted a loopback URL:\n{py_out}"
    assert rust_result.returncode != 0, f"Rust accepted a loopback URL:\n{rust_out}"
    # Same reason on both sides: a policy rejection of a non-public address.
    assert "SSRFProtectionError" in py_out or "non-public address" in py_out, py_out
    assert "blocked address" in rust_out, rust_out
    # Neither SDK reached the server, and neither stored a data row.
    assert hits == [], f"a loopback URL was fetched: {hits}"
    assert _data_rows(py_ws, python_db_path) == []
    assert _data_rows(rust_ws, rust_db_path) == []


_PUBLIC_URL = "https://example.com/"


def _public_network_available():
    import socket

    try:
        with socket.create_connection(("example.com", 443), timeout=5):
            return True
    except OSError:
        return False


def test_add_public_url_ingests_same_page_in_both(tmp_path):
    """Both SDKs fetch the same public URL and keep the page's raw bytes.

    Needs outbound network access; skips cleanly when there is none
    (SDK-1039 D9). The fixture is a public page because both SDKs refuse
    loopback/private hosts (see the rejection test above).

    This compares only what the two SDKs share. They intentionally differ in
    how a fetched page is stored, and the test does not hide that:

    * Python (1d3ed15, ``save_data_item_to_storage.py:82-90``) stores the raw
      HTML as the document (``text_loader``; ``content_hash`` is the HTML's
      hash) and records only ``external_metadata._cognee.source_uri``.
    * Rust extracts the page text at add time (``beautiful_soup_loader``;
      ``content_hash`` is over the extracted text) and records
      ``source``/``url``/``final_url``/``content_type``/``title``.

    So the checks are: both succeed and write exactly one row; the original
    (``original_data_location``) bytes are the same fetched HTML; and each
    SDK records the requested URL as the source, in its own metadata shape.
    """
    if not _public_network_available():
        pytest.skip("no outbound network access to example.com:443")

    py_ws, rust_ws, py_result, rust_result = _url_add_both(
        tmp_path, _PUBLIC_URL, "url_parity"
    )
    assert py_result.returncode == 0, (
        f"Python URL add failed:\n{py_result.stdout}\n{py_result.stderr}"
    )
    assert rust_result.returncode == 0, (
        f"Rust URL add failed:\n{rust_result.stdout}\n{rust_result.stderr}"
    )

    py_rows = _data_rows(py_ws, python_db_path)
    rust_rows = _data_rows(rust_ws, rust_db_path)
    assert len(py_rows) == len(rust_rows) == 1
    py_row, rust_row = py_rows[0], rust_rows[0]

    for row in (py_row, rust_row):
        assert row["original_data_location"].endswith(".html")
        assert row["raw_data_location"]

    py_raw = read_stored_file(py_ws / ".data_storage", py_row["original_data_location"])
    rust_raw = read_stored_file(rust_ws / ".data_storage", rust_row["original_data_location"])
    assert py_raw == rust_raw, "the two SDKs stored different bytes for the same page"
    assert b"Example Domain" in py_raw

    py_meta = _json_metadata(py_row)
    rust_meta = _json_metadata(rust_row)
    assert py_meta["_cognee"]["source_uri"] == _PUBLIC_URL
    assert rust_meta["source"] == "url"
    assert rust_meta["url"] == _PUBLIC_URL


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
    """Adding 2 different texts produces 2 dataset-scoped rows in both SDKs.

    The rows hold the same contents (content hashes) in both SDKs; their ids
    are each SDK's own uuid4 and are not compared (SDK-1039 D7).
    """
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

    # Query only the multi_test dataset's data. Both SDKs keep membership in
    # data.dataset_id (Python alembic d6e8f0a2b4c6; Rust
    # m20261028_000001_dataset_scoped_data).
    query = (
        f"SELECT d.* FROM data d "
        f"JOIN datasets ds ON d.dataset_id = ds.id "
        f"WHERE ds.name = '{ds}' ORDER BY d.name"
    )
    py_conn = open_db(python_db_path(py_ws))
    rust_conn = open_db(rust_db_path(rust_ws))
    py_data = query_rows(py_conn, query)
    rust_data = query_rows(rust_conn, query)
    py_conn.close()
    rust_conn.close()

    assert len(py_data) == 2, f"Python: expected 2 data rows, got {len(py_data)}"
    assert len(rust_data) == 2, f"Rust: expected 2 data rows, got {len(rust_data)}"

    py_hashes = sorted(d["content_hash"] for d in py_data)
    rust_hashes = sorted(d["content_hash"] for d in rust_data)
    assert py_hashes == rust_hashes, (
        f"content hashes mismatch for multi-item add:\n"
        f"  Python: {py_hashes}\n"
        f"  Rust:   {rust_hashes}"
    )
    for sdk, rows in (("Python", py_data), ("Rust", rust_data)):
        assert len({d["id"] for d in rows}) == 2, f"{sdk}: the two rows share an id"
        assert all(_uuid_version(d["id"]) == 4 for d in rows), (
            f"{sdk}: data ids are not uuid4: {[d['id'] for d in rows]}"
        )
