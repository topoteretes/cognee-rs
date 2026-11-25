"""Entity types across documents, and relationship labels, match Python (SDK-1694 O7).

Python builds each document's graph on its own: within a document the first
type an entity is given wins, and an entity two documents type differently
keeps both types (one ``is_a`` edge each). Relationship labels go through
``generate_edge_name`` (lower case, spaces to ``_``, apostrophes dropped)
before the edge is keyed, so a later document that spells a stored label
differently adds no edge. Rust kept one type per entity for the whole run
until O1 and stored raw labels until O2.

Real LLM output would make an exact comparison flaky, so both SDKs are pointed
at :class:`scripted_llm.ScriptedLlm`, which returns the same canned graph for
a chunk carrying a given marker, whichever SDK asks. No API key is needed, so
the case runs on every PR.

Each SDK gets its own workspace and runs:

1. ``add`` + ``cognify`` of two documents. ``Jaguar`` is a car manufacturer
   (and, second within the same document, a company) in one, and an animal in
   the other. Both documents use a relationship label that is not normalised.
2. ``add`` + ``cognify`` of a third document that repeats one edge under a
   differently spelled label (the "second run" check from the O2 review).

After each run, each SDK's graph must hold exactly the expected entities (one
node each), entity types and semantic edges, and the two SDKs' graphs must
agree, entity ids included (both derive an entity's id from its name).
"""

from helpers import (
    python_graph,
    rust_graph,
    run_python_cli,
    run_rust_cli,
    write_rust_config,
)
from scripted_llm import ScriptedLlm

DATASET = "o7_type_parity"

ALPHA, BRAVO, CHARLIE = "O7-ALPHA", "O7-BRAVO", "O7-CHARLIE"

DOCUMENTS = {
    ALPHA: (
        "o7_alpha.txt",
        f"{ALPHA}. Jaguar is a British car manufacturer headquartered in "
        "Coventry, England.\n",
    ),
    BRAVO: (
        "o7_bravo.txt",
        f"{BRAVO}. The jaguar is a large cat that lives in the Amazon "
        "rainforest of South America.\n",
    ),
    CHARLIE: (
        "o7_charlie.txt",
        f"{CHARLIE}. Jaguar still builds its cars at its headquarters in "
        "Coventry.\n",
    ),
}


def _node(node_id, name, kind):
    return {"id": node_id, "name": name, "type": kind, "description": f"{name}, a {kind}."}


def _edge(source, target, label):
    return {"source_node_id": source, "target_node_id": target, "relationship_name": label}


GRAPHS = {
    ALPHA: {
        "nodes": [
            _node("Jaguar", "Jaguar", "Car Manufacturer"),
            _node("Coventry", "Coventry", "City"),
            # Second type for the same entity within one document: the first
            # one wins, in Python and in Rust, and this one is dropped.
            _node("Jaguar", "Jaguar", "Company"),
        ],
        "edges": [_edge("Jaguar", "Coventry", "Headquartered In")],
    },
    BRAVO: {
        "nodes": [
            _node("Jaguar", "Jaguar", "Animal"),
            _node("Amazon Rainforest", "Amazon Rainforest", "Forest"),
        ],
        "edges": [_edge("Jaguar", "Amazon Rainforest", "Lives In")],
    },
    CHARLIE: {
        "nodes": [
            _node("Jaguar", "Jaguar", "Car Manufacturer"),
            _node("Coventry", "Coventry", "City"),
        ],
        # Normalises to the label ALPHA's edge was stored under.
        "edges": [_edge("Jaguar", "Coventry", "HEADQUARTERED IN")],
    },
}

EXPECTED_TYPES = {
    "jaguar": {"car manufacturer", "animal"},
    "coventry": {"city"},
    "amazon rainforest": {"forest"},
}
EXPECTED_EDGES = {
    ("jaguar", "headquartered_in", "coventry"),
    ("jaguar", "lives_in", "amazon rainforest"),
}


def _summary(nodes, edges):
    """What the checks compare, by stored name (both SDKs store the
    ``generate_node_name`` / ``generate_edge_name`` forms, so no case folding):

    * ``entities``: entity name -> sorted node ids (one id each, and the same
      name-derived id in both SDKs);
    * ``types``: entity name -> type names reached by ``is_a``;
    * ``semantic``: sorted (source, label, target) entity-to-entity edges;
    * ``entity_types``: sorted EntityType names.
    """
    by_id = {n["id"]: n for n in nodes}

    def name(node_id):
        return str(by_id[node_id].get("name") or "")

    def kind(node_id):
        return by_id.get(node_id, {}).get("type")

    entities: dict[str, list] = {}
    for n in nodes:
        if n.get("type") == "Entity":
            entities.setdefault(name(n["id"]), []).append(n["id"])
    types: dict[str, set] = {}
    semantic = []
    for e in edges:
        s, d, label = e["source_node_id"], e["destination_node_id"], e["relationship_name"]
        if kind(s) != "Entity":
            continue
        if label == "is_a" and kind(d) == "EntityType":
            types.setdefault(name(s), set()).add(name(d))
        elif kind(d) == "Entity":
            semantic.append((name(s), label, name(d)))
    entity_types = sorted(name(n["id"]) for n in nodes if n.get("type") == "EntityType")
    return {
        "entities": {k: sorted(v) for k, v in entities.items()},
        "types": types,
        "semantic": sorted(semantic),
        "entity_types": entity_types,
    }


def _run(sdk, ws, llm, markers):
    """``add`` the documents for *markers*, then ``cognify``, through *sdk*."""
    paths = []
    for marker in markers:
        filename, text = DOCUMENTS[marker]
        path = ws / filename
        path.write_text(text)
        paths.append(str(path))
    if sdk == "Python":
        run = run_python_cli
        env = {
            "LLM_ENDPOINT": llm.url,
            "LLM_API_KEY": "scripted",
            "EMBEDDING_ENDPOINT": llm.url,
            "EMBEDDING_API_KEY": "scripted",
        }
    else:
        run = run_rust_cli
        # The Rust CLI's environment beats its config.json, and docker compose
        # forwards a developer's `.env` (OPENAI_URL / OPENAI_TOKEN), so every
        # alias is pointed at the scripted endpoint.
        env = {
            "LLM_ENDPOINT": llm.url,
            "OPENAI_URL": llm.url,
            "LLM_API_KEY": "scripted",
            "OPENAI_TOKEN": "scripted",
        }
    llm.reset()
    for args in (["add", *paths, "-d", DATASET], ["cognify", "-d", DATASET]):
        result = run(ws, args, env=env, check=False)
        assert result.returncode == 0, (
            f"{sdk} {args[0]} failed:\n{result.stdout}\n{result.stderr}"
        )
    served = set(llm.graph_markers_served())
    assert set(markers) <= served, (
        f"{sdk}: the scripted graph was not requested for {sorted(set(markers) - served)}; "
        f"calls: {llm.calls}"
    )
    assert not llm.unscripted(), f"{sdk}: unscripted requests: {llm.unscripted()}"
    print(f"{sdk} {markers}: scripted calls {llm.call_counts()}")


def _graph(sdk, ws):
    nodes, edges = (python_graph if sdk == "Python" else rust_graph)(ws)
    return _summary(nodes, edges)


def _check(sdk, summary, stage):
    entities = summary["entities"]
    assert sorted(entities) == sorted(EXPECTED_TYPES), (
        f"{sdk} ({stage}): entity names {sorted(entities)}, "
        f"expected {sorted(EXPECTED_TYPES)}"
    )
    split = {k: v for k, v in entities.items() if len(v) != 1}
    assert not split, f"{sdk} ({stage}): an entity is stored as several nodes: {split}"
    assert summary["types"] == EXPECTED_TYPES, (
        f"{sdk} ({stage}): entity types {summary['types']}, expected {EXPECTED_TYPES}"
    )
    assert summary["semantic"] == sorted(EXPECTED_EDGES), (
        f"{sdk} ({stage}): semantic edges {summary['semantic']}, expected one each of "
        f"{sorted(EXPECTED_EDGES)}"
    )
    assert "company" not in summary["entity_types"], (
        f"{sdk} ({stage}): the type that lost within its document was stored: "
        f"{summary['entity_types']}"
    )


def test_entity_types_and_labels_match_python_across_documents_and_runs(tmp_path):
    with ScriptedLlm(GRAPHS) as llm:
        workspaces = {}
        for sdk in ("Python", "Rust"):
            ws = tmp_path / sdk.lower()
            ws.mkdir()
            if sdk == "Rust":
                write_rust_config(
                    ws, extra={"llm_endpoint": llm.url, "llm_api_key": "scripted"}
                )
            workspaces[sdk] = ws

        for stage, markers in (("first run", [ALPHA, BRAVO]), ("second run", [CHARLIE])):
            summaries = {}
            for sdk, ws in workspaces.items():
                _run(sdk, ws, llm, markers)
                summaries[sdk] = _graph(sdk, ws)
                _check(sdk, summaries[sdk], stage)
            assert summaries["Python"] == summaries["Rust"], (
                f"{stage}: the SDKs' graphs differ:\n"
                f"Python: {summaries['Python']}\nRust:   {summaries['Rust']}"
            )
