"""Delete one document: its entity types go with it (SDK-1694 O6).

Python removes an EntityType when the last document that reached it is
deleted (``get_data_related_nodes``, or the graph's source refs on a
provenance-carrying graph). Rust did not until O6: every EntityType carried a
nil-``data_id`` ledger row that claimed it for every document, so a type only
the deleted document declared stayed in the graph, with no ``is_a`` edge left
pointing at it.

Each SDK adds two documents with disjoint subjects to one dataset, cognifies
them, then deletes one document by its data id. Extraction is an LLM's, so the
checks are invariants over each SDK's own graph rather than exact sets:

1. every ``EntityType`` left has an ``is_a`` edge pointing at it — no type
   outlives the documents that declared it;
2. every type that only the deleted document's entities pointed at is gone;
3. every type an entity only the kept document reached points at stays.

All tests require an OpenAI API key (cognify invokes the LLM).
"""

from helpers import (
    open_db,
    query_data,
    query_datasets,
    _normalize_uuid,
    python_db_path,
    python_graph,
    rust_db_path,
    rust_graph,
    run_python_cli,
    run_rust_cli,
    write_rust_config,
)
from conftest import requires_openai

DATASET = "o6_type_delete"

KEPT_TEXT = (
    "Apple Inc. is an American technology company headquartered in Cupertino, "
    "California. Tim Cook is the chief executive officer of Apple Inc., and "
    "the company designs the iPhone smartphone.\n"
)
DELETED_TEXT = (
    "The banana is a tropical fruit grown in Ecuador and the Philippines. "
    "Bananas are rich in potassium, a mineral nutrient, and the Cavendish "
    "variety is the most widely exported cultivar.\n"
)


def _document_entities(nodes, edges, document_ids):
    """Entity ids a document's chunks contain: document <-is_part_of- chunk
    -contains-> entity, whatever the edge direction an SDK reports."""
    types = {n["id"]: n.get("type") for n in nodes}
    chunks = set()
    for e in edges:
        s, d = e["source_node_id"], e["destination_node_id"]
        if e["relationship_name"] == "is_part_of" and d in document_ids:
            chunks.add(s)
    entities = set()
    for e in edges:
        s, d = e["source_node_id"], e["destination_node_id"]
        if s in chunks and types.get(d) == "Entity":
            entities.add(d)
    return entities


def _is_a(nodes, edges):
    """(entity, type) pairs of the graph's ``is_a`` edges into EntityTypes."""
    types = {n["id"] for n in nodes if n.get("type") == "EntityType"}
    return {
        (e["source_node_id"], e["destination_node_id"])
        for e in edges
        if e["relationship_name"] == "is_a" and e["destination_node_id"] in types
    }


def _data_id(db_path, stem):
    rows = [d for d in query_data(open_db(db_path)) if stem in (d.get("name") or "")]
    assert len(rows) == 1, f"expected one data row named like {stem!r}: {rows}"
    return _normalize_uuid(rows[0]["id"])


@requires_openai
def test_deleting_a_document_takes_the_types_only_it_declared(tmp_path):
    workspaces = {}
    for sdk in ("Python", "Rust"):
        ws = tmp_path / sdk.lower()
        ws.mkdir()
        kept = ws / "o6_kept.txt"
        kept.write_text(KEPT_TEXT)
        deleted = ws / "o6_deleted.txt"
        deleted.write_text(DELETED_TEXT)
        run = run_python_cli if sdk == "Python" else run_rust_cli
        if sdk == "Rust":
            write_rust_config(ws)
        result = run(ws, ["add", str(kept), str(deleted), "-d", DATASET], check=False)
        assert result.returncode == 0, f"{sdk} add failed:\n{result.stdout}\n{result.stderr}"
        result = run(ws, ["cognify", "-d", DATASET], check=False)
        assert result.returncode == 0, (
            f"{sdk} cognify failed:\n{result.stdout}\n{result.stderr}"
        )
        workspaces[sdk] = ws

    for sdk, ws in workspaces.items():
        db_path = python_db_path(ws) if sdk == "Python" else rust_db_path(ws)
        read_graph = python_graph if sdk == "Python" else rust_graph
        run = run_python_cli if sdk == "Python" else run_rust_cli
        kept_id = _data_id(db_path, "o6_kept")
        deleted_id = _data_id(db_path, "o6_deleted")
        assert {d["name"] for d in query_datasets(open_db(db_path))} >= {DATASET}

        nodes, edges = read_graph(ws)
        kept_entities = _document_entities(nodes, edges, {kept_id})
        deleted_entities = _document_entities(nodes, edges, {deleted_id})
        assert kept_entities and deleted_entities, (
            f"{sdk}: both documents have entities before the delete "
            f"(kept {len(kept_entities)}, deleted {len(deleted_entities)})"
        )
        pairs = _is_a(nodes, edges)
        sources = {}
        for entity, kind in pairs:
            sources.setdefault(kind, set()).add(entity)
        only_deleted = {
            kind
            for kind, entities in sources.items()
            if entities <= deleted_entities - kept_entities
        }
        only_kept = {
            kind
            for kind, entities in sources.items()
            if entities & (kept_entities - deleted_entities)
        }
        assert only_deleted, (
            f"{sdk}: the deleted document declared no type of its own; "
            f"types by entity: {sources}"
        )

        args = ["delete", "-d", DATASET, "--data-id", deleted_id, "-f"]
        if sdk == "Rust":
            args = ["delete", "--dataset-name", DATASET, "--data-id", deleted_id, "-f"]
        result = run(ws, args, check=False)
        assert result.returncode == 0, (
            f"{sdk} delete of one document failed:\n{result.stdout}\n{result.stderr}"
        )

        nodes, edges = read_graph(ws)
        left_types = {n["id"]: n.get("name") for n in nodes if n.get("type") == "EntityType"}
        typed = {kind for _, kind in _is_a(nodes, edges)}
        orphans = {left_types[kind] for kind in left_types.keys() - typed}
        assert not orphans, (
            f"{sdk}: EntityType node(s) with no is_a edge left after the delete: "
            f"{sorted(map(str, orphans))}"
        )
        survived = only_deleted & left_types.keys()
        assert not survived, (
            f"{sdk}: type(s) only the deleted document declared survive it: "
            f"{sorted(str(left_types[kind]) for kind in survived)}"
        )
        lost = only_kept - left_types.keys()
        assert not lost, (
            f"{sdk}: the delete removed {len(lost)} type(s) the kept document "
            f"still declares"
        )
