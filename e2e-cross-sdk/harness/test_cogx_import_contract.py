"""Python cognee imports a Rust-written COGX archive — without an LLM.

``test_cogx_roundtrip.py`` drives the real thing (Rust cognifies, exports, and
Python imports the result), but producing a graph to export needs an LLM, so it
only runs on key-gated lanes — never on a fork PR.

This suite closes that gap. ``harness/golden/cogx_archive/`` is a COGX archive
written by the Rust exporter itself (regenerate with ``COGX_REGENERATE_GOLDEN=1
cargo test -p cognee-migration --test python_import_contract``, which also
fails if it drifts from the writer). Running Python's real migration loader
over it exercises the whole Rust→Python contract — record parsing, id
preservation, edge reconstruction — deterministically and for free.

``translate_records`` is the function ``import_memory_source`` delegates to; it
builds the DataPoint nodes and edge tuples that ``add_data_points`` then
persists. Calling it directly keeps the test off the database, the embedding
provider, and the LLM, while still covering the semantics that matter.
"""

import json
import os
from pathlib import Path

import pytest

from helpers import run_python_script

GOLDEN_ARCHIVE = Path(__file__).parent / "golden" / "cogx_archive"

RESULT_START = ">>>IMPORT<<<"
RESULT_END = ">>>END<<<"

# Node ids the Rust exporter wrote into the golden archive. Python must
# reproduce these exactly — that is what id preservation means.
EXPECTED_NODE_IDS = {
    "0198c1e0-0000-7000-8000-00000000d0c0": "TextDocument",
    "0198c1e0-0000-7000-8000-00000000c8c8": "DocumentChunk",
    "0198c1e0-0000-7000-8000-0000000a11ce": "Entity",
    "0198c1e0-0000-7000-8000-00000000b0b0": "Entity",
    "0198c1e0-0000-7000-8000-000000009e50": "EntityType",
    "0198c1e0-0000-7000-8000-0000000c0115": "NodeSet",
}

EXPECTED_EDGES = {
    ("0198c1e0-0000-7000-8000-00000000c8c8", "0198c1e0-0000-7000-8000-00000000d0c0", "is_part_of"),
    ("0198c1e0-0000-7000-8000-00000000c8c8", "0198c1e0-0000-7000-8000-0000000a11ce", "contains"),
    ("0198c1e0-0000-7000-8000-00000000c8c8", "0198c1e0-0000-7000-8000-00000000b0b0", "contains"),
    ("0198c1e0-0000-7000-8000-0000000a11ce", "0198c1e0-0000-7000-8000-000000009e50", "is_a"),
    ("0198c1e0-0000-7000-8000-00000000b0b0", "0198c1e0-0000-7000-8000-000000009e50", "is_a"),
    ("0198c1e0-0000-7000-8000-0000000a11ce", "0198c1e0-0000-7000-8000-00000000b0b0", "knows"),
    (
        "0198c1e0-0000-7000-8000-0000000a11ce",
        "0198c1e0-0000-7000-8000-0000000c0115",
        "belongs_to_set",
    ),
}


@pytest.fixture(scope="module")
def imported(tmp_path_factory):
    """Run Python's migration loader over the Rust-written golden archive."""
    assert GOLDEN_ARCHIVE.is_dir(), (
        f"golden COGX archive missing at {GOLDEN_ARCHIVE}. Regenerate it with "
        "COGX_REGENERATE_GOLDEN=1 cargo test -p cognee-migration "
        "--test python_import_contract"
    )

    workdir = tmp_path_factory.mktemp("cogx_import")

    probe = run_python_script(
        workdir,
        "from cognee.modules.migration import COGXArchiveSource\nprint('HAS_COGX')\n",
        check=False,
    )
    if probe.returncode != 0 or "HAS_COGX" not in probe.stdout:
        message = (
            "the image's Python cognee has no cognee.modules.migration, so the "
            "COGX import contract cannot be checked. Bump the `topoteretes/cognee` "
            "ref in .github/workflows/http-parity.yml to a SHA that contains it.\n"
            f"probe stderr: {probe.stderr[-600:]}"
        )
        # In CI this must be a failure, not a skip: a skipped suite still exits
        # 0, which is precisely how a "required" gate ends up asserting nothing.
        if os.environ.get("COGX_REQUIRE_PYTHON_SUPPORT"):
            pytest.fail(message)
        pytest.skip(message)

    script = f"""
import asyncio, json
from cognee.modules.migration import COGXArchiveSource
from cognee.modules.migration.loader import translate_records

async def main():
    source = COGXArchiveSource({str(GOLDEN_ARCHIVE)!r}, mode="preserve")
    records = [record async for record in source.records()]

    # The exact predicate import_memory_source computes from the manifest.
    preserve_source_ids = source.source_system == "cognee"
    result = translate_records(
        records, mode="preserve", preserve_source_ids=preserve_source_ids
    )

    nodes = [node for batch in result.graph_batches for node in batch["nodes"]]
    edges = [edge for batch in result.graph_batches for edge in batch["edges"]]

    payload = {{
        "source_system": source.source_system,
        "mode": source.mode,
        "migration_revision": source.migration_revision,
        "preserve_source_ids": preserve_source_ids,
        "record_count": len(records),
        "skipped_facts": result.skipped_facts,
        "nodes": {{str(node.id): type(node).__name__ for node in nodes}},
        "chunk_text": next(
            (getattr(node, "text", None) for node in nodes
             if type(node).__name__ == "DocumentChunk"),
            None,
        ),
        "edges": [
            [str(subject), str(obj), predicate, properties]
            for subject, obj, predicate, properties in edges
        ],
    }}
    print({RESULT_START!r} + json.dumps(payload, default=str) + {RESULT_END!r})

asyncio.run(main())
"""
    result = run_python_script(workdir, script, check=False)
    assert result.returncode == 0, (
        f"Python COGX import failed (exit {result.returncode}):\n"
        f"--- stdout ---\n{result.stdout}\n--- stderr ---\n{result.stderr}"
    )
    assert RESULT_START in result.stdout, (
        f"loader produced no payload:\n{result.stdout}\n{result.stderr}"
    )
    return json.loads(result.stdout.split(RESULT_START)[1].split(RESULT_END)[0])


def test_manifest_enables_id_preservation(imported):
    """`preserve_source_ids` is derived from the manifest's source_system."""
    assert imported["source_system"] == "cognee"
    assert imported["preserve_source_ids"] is True, (
        "Python would recompute entity ids from names — a Rust node and its "
        "imported counterpart would stop being the same node."
    )
    assert imported["mode"] == "preserve", "restore must be zero-LLM by default"
    assert imported["migration_revision"] is None


def test_every_rust_node_id_survives_the_import(imported):
    """Ids come across verbatim, with their original DataPoint types."""
    imported_nodes = imported["nodes"]

    missing = set(EXPECTED_NODE_IDS) - set(imported_nodes)
    assert not missing, f"node ids lost in import: {sorted(missing)}"

    wrong_type = {
        node_id: (expected, imported_nodes[node_id])
        for node_id, expected in EXPECTED_NODE_IDS.items()
        if imported_nodes[node_id] != expected
    }
    assert not wrong_type, f"nodes rehydrated as the wrong class: {wrong_type}"


def test_no_edge_is_dropped(imported):
    """The loader skips unresolvable facts silently; assert none were."""
    assert imported["skipped_facts"] == 0, (
        f"{imported['skipped_facts']} facts were dropped — some endpoint did "
        "not resolve to an exported node."
    )

    rebuilt = {(subject, obj, predicate) for subject, obj, predicate, _ in imported["edges"]}
    missing = EXPECTED_EDGES - rebuilt
    assert not missing, f"edges lost in import: {sorted(missing)}"


def test_edge_temporal_validity_and_annotations_survive(imported):
    """COGX carries fact_text, valid_at, invalid_at and confidence."""
    knows = next(
        properties
        for subject, obj, predicate, properties in imported["edges"]
        if predicate == "knows"
    )
    assert knows.get("edge_text") == "Alice knows Bob"
    assert str(knows.get("valid_at", "")).startswith("2026-01-11T20:51:23"), knows
    assert float(knows.get("confidence")) == pytest.approx(0.87)


def test_document_chunk_text_survives_as_a_typed_node(imported):
    """The raw node keeps topology; its text must come back with it."""
    assert imported["chunk_text"] == "Alice knows Bob.", (
        "DocumentChunk text did not survive rehydration — searches over "
        "imported chunks would return empty content."
    )
