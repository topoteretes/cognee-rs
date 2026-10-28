"""Vector payload parity: both SDKs store Python's ``IndexSchema`` rows (SDK-1039).

Python writes every vector row through ``IndexSchema`` (``LanceDBAdapter.py``):
the indexed ``text``, a few reference scalars (``document_id``, ``chunk_index``,
…) and the ``DataPoint`` base fields at their IndexSchema defaults. Its LanceDB
table schema excludes the ``metadata`` key. Rust builds the same row with
``cognee_models::index_schema_payload`` and adds one documented key,
``dataset_id`` (plus the ``dataset_ids`` union its adapters keep when a point
is indexed for a second dataset).

These tests cognify the same text with both CLIs and compare the stored LanceDB
payloads per collection:

* the key sets, Rust-only keys aside;
* the JSON type of every key (``null`` where the other side has ``null``);
* values only where they are deterministic: the IndexSchema defaults, and the
  chunk text and ``chunk_index`` of the one-chunk input. ``document_id`` is
  compared by presence and type only: data ids are ``uuid4`` in both SDKs
  (decision D7), so its value never matches.

They need ``OPENAI_API_KEY`` because cognify calls the LLM.
"""

import json
from pathlib import Path

import pytest

from conftest import requires_openai
from helpers import run_python_script

# Keys Rust adds to Python's IndexSchema row (cognee_models::RUST_ONLY_PAYLOAD_KEYS,
# plus the adapter-maintained dataset union).
RUST_ONLY_KEYS = {"dataset_id", "dataset_ids"}

# The collections default cognify writes in both SDKs.
CORE_COLLECTIONS = {
    "DocumentChunk_text",
    "Entity_name",
    "EntityType_name",
    "TextSummary_text",
    "EdgeType_relationship_name",
    "TextDocument_name",
}

# IndexSchema defaults: every stored row has exactly these values, whatever the
# source point carried.
INDEX_SCHEMA_DEFAULTS = {
    "type": "IndexSchema",
    "version": 1,
    "topological_rank": 0,
    "ontology_valid": False,
    "ontology_uri": None,
    "source_pipeline": None,
    "source_task": None,
    "source_node_set": None,
    "source_user": None,
    "source_content_hash": None,
    "feedback_weight": 0.5,
}

# Known value gaps, not shape gaps: the key is present on both sides, but Rust
# has no value yet. Each entry names the ticket that closes it.
KNOWN_TYPE_GAPS = {
    # Rust chunks do not carry their document's external_metadata (SDK-1057).
    ("DocumentChunk_text", "external_metadata"),
    # Both SDKs copy Document.external_metadata into the row as is, but the
    # Documents differ: Python's classify_documents stores
    # ``json.dumps(data.external_metadata)``, always a JSON string (``"null"``
    # or ``"{}"`` for an item added without metadata), while Rust leaves the
    # field None when the data item has no metadata. A
    # classify_documents gap, not a payload one (SDK-1039 P6 notes).
    ("TextDocument_name", "external_metadata"),
}

_START, _END = "<<<PAYLOADS>>>", "<<<END>>>"

# Reads every LanceDB table under a workspace. A LanceDB database is a
# directory of ``<table>.lance`` directories; each SDK may keep one per dataset,
# so rows of the same collection are pooled. Python's rows hold the payload in a
# ``payload`` struct column; Rust's in a ``metadata`` JSON string column.
_DUMP_SCRIPT = """
import json, pathlib, lancedb
root = pathlib.Path({root!r})
out = {{}}
for table_dir in sorted(root.rglob("*.lance")):
    if not table_dir.is_dir() or table_dir.parent.suffix == ".lance":
        continue
    db = lancedb.connect(str(table_dir.parent))
    for row in db.open_table(table_dir.stem).to_arrow().to_pylist():
        if "payload" in row:
            payload = row["payload"]
        else:
            payload = json.loads(row["metadata"])
        out.setdefault(table_dir.stem, []).append(payload)
print({start!r} + json.dumps(out, default=str) + {end!r})
"""


def lancedb_payloads(workspace: Path) -> dict[str, list[dict]]:
    """Stored payloads per collection, read with the lancedb Python package."""
    script = _DUMP_SCRIPT.format(root=str(workspace), start=_START, end=_END)
    result = run_python_script(workspace, script, check=False)
    out = result.stdout
    start, end = out.find(_START), out.find(_END)
    if result.returncode != 0 or start == -1 or end < start:
        raise RuntimeError(
            f"Reading LanceDB under {workspace} failed (exit {result.returncode}):\n"
            f"--- stdout ---\n{out}\n--- stderr ---\n{result.stderr}"
        )
    return json.loads(out[start + len(_START) : end])


def json_kind(value) -> str:
    if value is None:
        return "null"
    if isinstance(value, bool):
        return "bool"
    if isinstance(value, (int, float)):
        return "number"
    if isinstance(value, str):
        return "string"
    if isinstance(value, list):
        return "array"
    return type(value).__name__


@pytest.fixture
def both_payloads(both_cognified):
    py_ws, rust_ws = both_cognified
    return lancedb_payloads(py_ws), lancedb_payloads(rust_ws)


def _common(py: dict, rust: dict) -> list[str]:
    missing = {
        "python": sorted(CORE_COLLECTIONS - set(py)),
        "rust": sorted(CORE_COLLECTIONS - set(rust)),
    }
    assert not missing["python"] and not missing["rust"], f"collections missing: {missing}"
    return sorted(set(py) & set(rust))


@requires_openai
def test_payload_key_sets_match_per_collection(both_payloads):
    """Same keys per collection; Rust adds only the documented dataset keys."""
    py, rust = both_payloads
    for collection in _common(py, rust):
        py_keys = set().union(*(row.keys() for row in py[collection]))
        rust_keys = set().union(*(row.keys() for row in rust[collection]))
        assert rust_keys - RUST_ONLY_KEYS == py_keys, (
            f"{collection}: Rust-only {sorted(rust_keys - RUST_ONLY_KEYS - py_keys)}, "
            f"Python-only {sorted(py_keys - rust_keys)}"
        )
        assert "dataset_id" in rust_keys, f"{collection}: Rust row lost dataset_id"


@requires_openai
def test_lancedb_rows_have_no_metadata_key(both_payloads):
    """D2: Python's LanceDB schema excludes ``metadata``; Rust drops it on write."""
    py, rust = both_payloads
    for collection in _common(py, rust):
        for side, rows in (("python", py[collection]), ("rust", rust[collection])):
            assert all("metadata" not in row for row in rows), f"{side} {collection}"


@requires_openai
def test_payload_value_types_match_per_collection(both_payloads):
    """Each key holds the same JSON type on both sides (``null`` vs ``null``)."""
    py, rust = both_payloads
    for collection in _common(py, rust):
        py_keys = set().union(*(row.keys() for row in py[collection]))
        for key in sorted(py_keys):
            py_kinds = {json_kind(row.get(key)) for row in py[collection]}
            rust_kinds = {json_kind(row.get(key)) for row in rust[collection]}
            if (collection, key) in KNOWN_TYPE_GAPS:
                assert rust_kinds == {"null"}, f"{collection}.{key}: {rust_kinds}"
                continue
            assert rust_kinds == py_kinds, (
                f"{collection}.{key}: Rust {sorted(rust_kinds)} vs Python {sorted(py_kinds)}"
            )


@requires_openai
def test_index_schema_defaults_match(both_payloads):
    """The base fields sit at their IndexSchema defaults in every row."""
    py, rust = both_payloads
    for collection in _common(py, rust):
        for side, rows in (("python", py[collection]), ("rust", rust[collection])):
            for row in rows:
                for key, expected in INDEX_SCHEMA_DEFAULTS.items():
                    assert row.get(key) == expected, (
                        f"{side} {collection}.{key} = {row.get(key)!r}, expected {expected!r}"
                    )


@requires_openai
def test_document_references_match(both_payloads):
    """Chunk rows carry ``document_id`` (by presence and type, D7) and
    ``chunk_index``; every other collection has ``null`` there. The one-chunk
    input gives both SDKs the same chunk text."""
    py, rust = both_payloads
    for collection in _common(py, rust):
        for side, rows in (("python", py[collection]), ("rust", rust[collection])):
            for row in rows:
                if collection == "DocumentChunk_text":
                    assert isinstance(row["document_id"], str) and row["document_id"], (
                        f"{side} chunk without document_id: {row}"
                    )
                    assert isinstance(row["chunk_index"], int), f"{side}: {row}"
                else:
                    assert row["document_id"] is None, f"{side} {collection}: {row}"
                    assert row["chunk_index"] is None, f"{side} {collection}: {row}"

    def chunks(rows):
        return sorted((row["chunk_index"], row["text"].strip()) for row in rows)

    assert chunks(rust["DocumentChunk_text"]) == chunks(py["DocumentChunk_text"])
