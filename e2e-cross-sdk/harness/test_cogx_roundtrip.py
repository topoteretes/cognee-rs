"""Rust COGX export → Python import roundtrip.

The Rust SDK exports a knowledge graph as a COGX archive; Python cognee reads
one back through ``COGXArchiveSource`` in ``preserve`` mode. COGX is the only
one of Python's five export formats with a reader on the other side (``json``,
``graphml`` and ``cypher`` are one-way egress; ``pydantic`` is an in-process
Python object graph), so this pair is the whole Rust→Python memory-migration
path — and, packed as ``.cogx.tar.gz``, the payload of
``POST /api/v1/remember`` with ``content_type=cogx-archive``.

Two things are asserted, in order of how quietly they fail:

1. **Ids survive.** Python derives ``preserve_source_ids`` from
   ``manifest.source_system == "cognee"``. Get that string wrong and the import
   still succeeds — it just recomputes every entity id from its *name* via
   ``Entity.id_for``, so a Rust node and its imported counterpart stop being the
   same node and later re-cognifies fork a parallel graph. Nothing errors.

2. **No edge is dropped.** ``_build_graph_batches`` skips any fact whose
   subject/object UUID is not among the archive's nodes, logging a warning and
   carrying on. An exporter that filtered its node set would lose edges with a
   green test run.

The archive-shape checks run without an LLM; the end-to-end import is gated on
a key because producing a graph to export at all requires cognify.
"""

import json
import os
from pathlib import Path

import pytest

from conftest import requires_openai
from helpers import (
    DATASET_NAME,
    NLP_TEXT_FILE,
    _normalize_uuid,
    run_python_script,
    run_rust_cli,
    write_rust_config,
)

RESULT_START = ">>>GRAPH<<<"
RESULT_END = ">>>END<<<"


# ── Archive readers ──────────────────────────────────────────────────────────


def read_jsonl(archive: Path, name: str) -> list[dict]:
    """Parse one JSONL record file; absent files are empty, not an error."""
    path = archive / name
    if not path.exists():
        return []
    return [
        json.loads(line)
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]


def read_manifest(archive: Path) -> dict:
    return json.loads((archive / "manifest.json").read_text(encoding="utf-8"))


def exported_node_ids(archive: Path) -> set[str]:
    """Every id the archive makes resolvable — typed entities and raw nodes."""
    ids = {record["external_id"] for record in read_jsonl(archive, "entities.jsonl")}
    ids |= {record["id"] for record in read_jsonl(archive, "nodes.jsonl")}
    return ids


def exported_triples(archive: Path) -> set[tuple[str, str, str]]:
    return {
        (
            _normalize_uuid(fact["subject_ref"]),
            _normalize_uuid(fact["object_ref"]),
            fact["predicate"],
        )
        for fact in read_jsonl(archive, "facts.jsonl")
    }


# ── Fixtures ─────────────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def rust_cogx_archive(tmp_path_factory):
    """Rust add + cognify + ``export --pack``. Returns (archive_dir, tarball).

    Module-scoped, like the sibling suite's fixture: four tests consume this,
    and function scope would re-run cognify — an LLM call per test — four times
    over the same input, four chances to trip run_rust_cli's 120s timeout.
    """
    rust_ws = tmp_path_factory.mktemp("cogx_roundtrip") / "rust"
    rust_ws.mkdir()
    write_rust_config(rust_ws)

    input_file = rust_ws / "input.txt"
    input_file.write_text(NLP_TEXT_FILE.read_text())

    result = run_rust_cli(
        rust_ws, ["add", str(input_file), "-d", DATASET_NAME], check=False
    )
    assert result.returncode == 0, f"Rust add failed:\n{result.stdout}\n{result.stderr}"

    result = run_rust_cli(rust_ws, ["cognify", "-d", DATASET_NAME], check=False)
    assert result.returncode == 0, (
        f"Rust cognify failed:\n{result.stdout}\n{result.stderr}"
    )

    archive = rust_ws / "archive"
    result = run_rust_cli(
        rust_ws,
        ["export", "-d", DATASET_NAME, "-o", str(archive), "--pack"],
        check=False,
    )
    assert result.returncode == 0, (
        f"Rust export failed:\n{result.stdout}\n{result.stderr}"
    )

    tarball = archive.with_name(archive.name + ".cogx.tar.gz")
    return archive, tarball


def python_supports_cogx(workdir: Path) -> bool:
    """Whether the image's Python cognee is new enough to read a COGX archive.

    Returns False to let the caller skip — unless ``COGX_REQUIRE_PYTHON_SUPPORT``
    is set, in which case a missing migration package fails outright. A skip
    still exits 0, so a lane that can never run these tests would otherwise
    report green forever.
    """
    probe = run_python_script(
        workdir,
        "from cognee.modules.migration import COGXArchiveSource, import_memory_source\n"
        "print('HAS_COGX')\n",
        check=False,
    )
    supported = probe.returncode == 0 and "HAS_COGX" in probe.stdout
    if not supported and os.environ.get("COGX_REQUIRE_PYTHON_SUPPORT"):
        pytest.fail(
            "the image's Python cognee has no cognee.modules.migration. Bump the "
            "`topoteretes/cognee` ref in .github/workflows/http-parity.yml.\n"
            f"probe stderr: {probe.stderr[-600:]}"
        )
    return supported


# ── Archive shape (no LLM beyond producing the graph) ────────────────────────


@requires_openai
def test_archive_declares_itself_as_cognee_origin(rust_cogx_archive):
    """The manifest fields the Python importer branches on."""
    archive, tarball = rust_cogx_archive
    manifest = read_manifest(archive)

    assert manifest["source_system"] == "cognee", (
        "source_system must be exactly 'cognee' — Python derives "
        "preserve_source_ids from this string, and any other value silently "
        "downgrades the import to name-derived entity ids."
    )
    assert manifest["cogx_version"] == "0.1"
    assert manifest.get("migration_revision") is None, (
        "Rust must not claim a Python Alembic revision; None means 'unknown', "
        "which leaves the importing store's own stamp alone."
    )
    assert tarball.exists(), "--pack did not produce the .cogx.tar.gz upload form"


@requires_openai
def test_every_fact_endpoint_is_an_exported_node(rust_cogx_archive):
    """Python drops facts with unresolvable UUID refs, and only logs a warning."""
    archive, _ = rust_cogx_archive
    resolvable = exported_node_ids(archive)
    facts = read_jsonl(archive, "facts.jsonl")
    assert facts, "cognify produced no edges to export"

    dangling = [
        (fact["external_id"], reference)
        for fact in facts
        for reference in (fact["subject_ref"], fact["object_ref"])
        if reference not in resolvable
    ]
    assert not dangling, (
        f"{len(dangling)} fact endpoint(s) reference nodes absent from the "
        f"archive; Python would silently skip these edges: {dangling[:5]}"
    )


@requires_openai
def test_document_chunks_are_written_typed_and_raw(rust_cogx_archive):
    """Both records are needed: raw keeps topology, document carries content."""
    archive, _ = rust_cogx_archive
    documents = read_jsonl(archive, "documents.jsonl")
    if not documents:
        pytest.skip("cognify produced no DocumentChunk nodes")

    raw_ids = {record["id"] for record in read_jsonl(archive, "nodes.jsonl")}
    for document in documents:
        assert document["external_id"] in raw_ids, (
            f"DocumentChunk {document['external_id']} was written as a typed "
            "document but not as a raw node — facts pointing at the chunk "
            "would dangle and be skipped on import."
        )
        assert document["content"], "document record carries no content"


# ── The roundtrip ────────────────────────────────────────────────────────────


@requires_openai
def test_python_imports_the_rust_archive_preserving_ids_and_edges(
    rust_cogx_archive, tmp_path
):
    """Rust exports, Python imports, and the graph arrives intact."""
    archive, _ = rust_cogx_archive

    py_ws = tmp_path / "python"
    py_ws.mkdir()

    if not python_supports_cogx(py_ws):
        pytest.skip("image's Python cognee predates COGX migration support")

    script = f"""
import asyncio, json
from cognee.modules.migration import COGXArchiveSource, import_memory_source
from cognee.infrastructure.databases.graph import get_graph_engine

async def main():
    source = COGXArchiveSource({str(archive)!r}, mode="preserve")
    await import_memory_source(source, dataset_name={DATASET_NAME!r})

    engine = await get_graph_engine()
    nodes, edges = await engine.get_graph_data()
    payload = {{
        "node_ids": [str(node_id) for node_id, _ in nodes],
        "edges": [
            [str(source_id), str(target_id), str(relationship)]
            for source_id, target_id, relationship, *_ in edges
        ],
    }}
    print({RESULT_START!r} + json.dumps(payload) + {RESULT_END!r})

asyncio.run(main())
"""
    result = run_python_script(py_ws, script, check=False)
    assert result.returncode == 0, (
        f"Python COGX import failed (exit {result.returncode}):\n"
        f"--- stdout ---\n{result.stdout}\n--- stderr ---\n{result.stderr}"
    )
    assert RESULT_START in result.stdout, (
        f"import produced no graph payload:\n{result.stdout}\n{result.stderr}"
    )

    payload = json.loads(
        result.stdout.split(RESULT_START)[1].split(RESULT_END)[0]
    )
    imported_ids = {_normalize_uuid(node_id) for node_id in payload["node_ids"]}
    imported_edges = {
        (_normalize_uuid(source), _normalize_uuid(target), relationship)
        for source, target, relationship in payload["edges"]
    }

    # 1. Entity ids carried across verbatim.
    exported_entity_ids = {
        _normalize_uuid(record["external_id"])
        for record in read_jsonl(archive, "entities.jsonl")
    }
    assert exported_entity_ids, "archive contains no entities to check"
    missing = exported_entity_ids - imported_ids
    assert not missing, (
        f"{len(missing)} of {len(exported_entity_ids)} exported entity ids are "
        f"absent from the imported graph — id preservation is off, so the "
        f"importer recomputed ids from names. Missing: {sorted(missing)[:5]}"
    )

    # 2. Every exported edge is present. Superset, not equality: the importer
    #    also links everything to an `import:cognee` NodeSet of its own.
    exported = exported_triples(archive)
    dropped = exported - imported_edges
    assert not dropped, (
        f"{len(dropped)} of {len(exported)} exported facts did not become "
        f"edges in the imported graph: {sorted(dropped)[:5]}"
    )
