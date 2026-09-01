"""Phase-1 parity tests for POST /api/v1/add.

Covers: text upload, multi-file upload, URL ingestion, deduplication,
validation error on missing file.

Ignore extension: ``{"$..tenant_id", "$..data_id", "$..dataset_id",
"$..raw_data_location"}`` — file paths under /py vs /rs differ.
Note that ``content_hash`` lives only inside ``data_ingestion_info``,
which ``_ADD_IGNORE`` strips — so the response-diff tests here do NOT
compare it. ``test_add_content_hash_matches_across_sdks`` is what pins it.
"""

from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

from http_helpers import DEFAULT_IGNORE, assert_responses_match

_ADD_IGNORE = DEFAULT_IGNORE | {
    "$..tenant_id",
    "$..data_id",
    "$..dataset_id",
    "$..raw_data_location",
    # The per-item ingestion detail under `data_ingestion_info` is
    # SDK-version-specific: the pinned Python build returns a nested
    # `{run_info: PipelineRunInfo}` (no content_hash), while Rust returns its
    # documented flat `{content_hash, name, extension, mime_type}`. Both report
    # the same top-level PipelineRunInfo (status + dataset_name), which is the
    # stable parity contract; ignore the divergent detail representation.
    "$..data_ingestion_info",
    "$..payload",
}

_SAMPLE_TEXT = (
    "Cognee is an AI memory pipeline that transforms raw data into "
    "persistent, queryable knowledge graphs.  This test file is used "
    "for cross-SDK parity verification."
)

_URL_FIXTURE_HTML = b"""\
<html>
  <head><title>HTTP URL Fixture</title></head>
  <body>
    <h1>HTTP add URL heading</h1>
    <p>Served from the local pytest fixture.</p>
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


@contextmanager
def local_url_fixture():
    server = ThreadingHTTPServer(("127.0.0.1", 0), _UrlFixtureHandler)
    try:
        import threading

        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        yield f"http://127.0.0.1:{server.server_port}/page.html"
    finally:
        server.shutdown()
        server.server_close()


@pytest.mark.xfail(
    strict=True,
    reason=(
        "The pinned Python build returns a nested {run_info: PipelineRunInfo} "
        "from /add carrying no content_hash in any position, while Rust returns "
        "one per item in data_ingestion_info[]. This test "
        "is expected to fail. When it XPASSes, Python has started returning "
        "content_hash: drop this test and make the comparison in "
        "seed.py::seed_both unconditional, since that is the HTTP lane's only "
        "content-addressed equality oracle."
    ),
)
def test_python_add_omits_content_hash(authed_clients, unique_dataset_name):
    """Tripwire for the asymmetry that disabled seed_both's hash comparison."""
    from seed import extract_content_hash, seed_dataset_with_text

    py = seed_dataset_with_text(
        authed_clients["py"], name=unique_dataset_name, text="content hash probe"
    )
    assert extract_content_hash(py) is not None


def test_add_content_hash_matches_across_sdks(authed_clients, unique_dataset_name):
    """Same bytes in, same content_hash out of both SDKs.

    This is the HTTP lane's only content-addressed equality oracle: every other
    add test runs through ``assert_responses_match`` with ``_ADD_IGNORE``, which
    strips ``data_ingestion_info`` and with it the hash. ``seed_both`` carries
    the assertion; calling it here is what makes it run.

    Like the rest of this module it is auth-gated, so it skips on an OSS build
    (see the run's auth-skip summary). That gating is a known harness limit, not
    a property of this check.
    """
    from seed import seed_both

    seed_both(
        authed_clients,
        name=unique_dataset_name,
        text="cross-SDK content-addressed identity probe",
    )


def test_add_text_upload(authed_clients, unique_dataset_name):
    """POST /api/v1/add with a single text/plain file returns 200 on both servers."""
    py = authed_clients["py"].post(
        "/api/v1/add",
        files={"data": ("test.txt", _SAMPLE_TEXT.encode(), "text/plain")},
        data={"datasetName": unique_dataset_name},
    )
    rs = authed_clients["rs"].post(
        "/api/v1/add",
        files={"data": ("test.txt", _SAMPLE_TEXT.encode(), "text/plain")},
        data={"datasetName": unique_dataset_name},
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
        data={"datasetName": unique_dataset_name},
    )
    rs = authed_clients["rs"].post(
        "/api/v1/add",
        files=files,
        data={"datasetName": unique_dataset_name},
    )
    assert_responses_match(py, rs, ignore=_ADD_IGNORE)


def test_add_url_ingestion(authed_clients, unique_dataset_name):
    """POST /api/v1/add with a URL source ingests the page on both servers."""
    with local_url_fixture() as url:
        files = {"data": ("url.txt", url.encode(), "text/plain")}
        data = {"datasetName": unique_dataset_name}
        py = authed_clients["py"].post("/api/v1/add", files=files, data=data)
        rs = authed_clients["rs"].post("/api/v1/add", files=files, data=data)
    # Status codes must agree (both may error or both succeed)
    assert py.status_code == rs.status_code, (
        f"URL ingest status mismatch: py={py.status_code} rs={rs.status_code}\n"
        f"py body: {py.text[:300]}\nrs body: {rs.text[:300]}"
    )


def test_add_deduplication(authed_clients, unique_dataset_name):
    """Posting the same file twice should produce the same response (dedup)."""
    files = [("data", ("dup.txt", _SAMPLE_TEXT.encode(), "text/plain"))]
    kw = {"files": files, "data": {"datasetName": unique_dataset_name}}

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


@pytest.mark.xfail(
    reason=(
        "Behavioural divergence on a no-file add. Rust validates the request and "
        "returns 400 (nothing to ingest); the pinned Python build accepts it and "
        "returns 200, creating an empty dataset. Rust is the stricter/more-correct "
        "side. (Python's lenient behaviour is also what makes its dataset count "
        "higher in forget_everything.)"
    ),
    # Deterministic divergence, so an XPASS means one side changed its
    # validation behaviour — worth a CI failure rather than a silent pass.
    strict=True,
)
def test_add_validation_error_on_missing_file(authed_clients, unique_dataset_name):
    """POST /api/v1/add with no file and no URL returns a 4xx error on both."""
    py = authed_clients["py"].post(
        "/api/v1/add",
        data={"datasetName": unique_dataset_name},
    )
    rs = authed_clients["rs"].post(
        "/api/v1/add",
        data={"datasetName": unique_dataset_name},
    )
    assert py.status_code == rs.status_code, (
        f"Validation-error status mismatch: py={py.status_code} rs={rs.status_code}"
    )
    assert py.status_code >= 400, f"Expected 4xx, got py={py.status_code}"
