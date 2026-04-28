"""Phase-1 parity tests for POST /api/v1/add.

Covers: text upload, multi-file upload, URL ingestion, deduplication,
validation error on missing file.

Ignore extension: ``{"$..tenant_id", "$..data_id", "$..dataset_id",
"$..raw_data_location"}`` — file paths under /py vs /rs differ.
The ``content_hash`` field is NOT ignored — it must match.
"""

from http_helpers import DEFAULT_IGNORE, assert_responses_match

_ADD_IGNORE = DEFAULT_IGNORE | {
    "$..tenant_id",
    "$..data_id",
    "$..dataset_id",
    "$..raw_data_location",
}

_SAMPLE_TEXT = (
    "Cognee is an AI memory pipeline that transforms raw data into "
    "persistent, queryable knowledge graphs.  This test file is used "
    "for cross-SDK parity verification."
)


def test_add_text_upload(authed_clients, unique_dataset_name):
    """POST /api/v1/add with a single text/plain file returns 200 on both servers."""
    py = authed_clients["py"].post(
        "/api/v1/add",
        files={"data": ("test.txt", _SAMPLE_TEXT.encode(), "text/plain")},
        data={"dataset_name": unique_dataset_name},
    )
    rs = authed_clients["rs"].post(
        "/api/v1/add",
        files={"data": ("test.txt", _SAMPLE_TEXT.encode(), "text/plain")},
        data={"dataset_name": unique_dataset_name},
    )
    assert_responses_match(py, rs, ignore=_ADD_IGNORE)


def test_add_multi_file_upload(authed_clients, unique_dataset_name):
    """POST /api/v1/add with multiple files returns 200 on both servers."""
    files = [
        ("data", ("file1.txt", b"Content of file one.", "text/plain")),
        ("data", ("file2.txt", b"Content of file two.", "text/plain")),
    ]
    py = authed_clients["py"].post(
        "/api/v1/add",
        files=files,
        data={"dataset_name": unique_dataset_name},
    )
    rs = authed_clients["rs"].post(
        "/api/v1/add",
        files=files,
        data={"dataset_name": unique_dataset_name},
    )
    assert_responses_match(py, rs, ignore=_ADD_IGNORE)


def test_add_url_ingestion(authed_clients, unique_dataset_name):
    """POST /api/v1/add with a URL source ingests the page on both servers."""
    payload = {
        "url": "https://raw.githubusercontent.com/topoteretes/cognee/main/README.md",
        "dataset_name": unique_dataset_name,
    }
    py = authed_clients["py"].post("/api/v1/add", json=payload)
    rs = authed_clients["rs"].post("/api/v1/add", json=payload)
    # Status codes must agree (both may error or both succeed)
    assert py.status_code == rs.status_code, (
        f"URL ingest status mismatch: py={py.status_code} rs={rs.status_code}\n"
        f"py body: {py.text[:300]}\nrs body: {rs.text[:300]}"
    )


def test_add_deduplication(authed_clients, unique_dataset_name):
    """Posting the same file twice should produce the same response (dedup)."""
    files = [("data", ("dup.txt", _SAMPLE_TEXT.encode(), "text/plain"))]
    kw = {"files": files, "data": {"dataset_name": unique_dataset_name}}

    # First upload
    py1 = authed_clients["py"].post("/api/v1/add", **kw)
    rs1 = authed_clients["rs"].post("/api/v1/add", **kw)
    assert py1.status_code == rs1.status_code, (
        f"First upload status mismatch: py={py1.status_code} rs={rs1.status_code}"
    )

    # Second upload of the same content
    py2 = authed_clients["py"].post("/api/v1/add", **kw)
    rs2 = authed_clients["rs"].post("/api/v1/add", **kw)
    assert py2.status_code == rs2.status_code, (
        f"Dedup upload status mismatch: py={py2.status_code} rs={rs2.status_code}"
    )


def test_add_validation_error_on_missing_file(authed_clients, unique_dataset_name):
    """POST /api/v1/add with no file and no URL returns a 4xx error on both."""
    py = authed_clients["py"].post(
        "/api/v1/add",
        data={"dataset_name": unique_dataset_name},
    )
    rs = authed_clients["rs"].post(
        "/api/v1/add",
        data={"dataset_name": unique_dataset_name},
    )
    assert py.status_code == rs.status_code, (
        f"Validation-error status mismatch: py={py.status_code} rs={rs.status_code}"
    )
    assert py.status_code >= 400, f"Expected 4xx, got py={py.status_code}"
