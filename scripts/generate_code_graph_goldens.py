#!/usr/bin/env python3
"""Regenerate the cognee-code-graph golden fixtures from Python cognee.

Python cognee is the source of truth for how an enola snapshot maps onto
graph nodes and edges. This script runs Python's own pipeline
(``parse_enola_snapshot`` -> ``map_facts_to_data_points`` ->
``build_code_graph_edges``) over the snapshot fixtures of Python's unit tests,
and over the snapshots written in this repo (LOCAL_SNAPSHOTS, under
crates/code-graph/tests/fixtures/snapshots/), and writes what it produced, so
the Rust port can be compared with it.

Run it with an interpreter that has Python cognee installed at the parity pin
(the commit named in crates/code-graph/README.md), from this repo's root:

    python3 scripts/generate_code_graph_goldens.py \
        <python-cognee-checkout>/cognee/tests/unit/tasks/code_graph/fixtures

It copies Python's input snapshots to
crates/code-graph/tests/fixtures/snapshots/ (the local ones are already
there) and writes one golden per (snapshot, node_set) case to
crates/code-graph/tests/fixtures/goldens/<snapshot>__<tag>.json:

    {
      "facts":      [...],         # parse_enola_snapshot's facts, synthesized ones included
      "skipped":    <int>,         # build_code_graph_edges' skip count
      "node_order": [id, ...],     # node ids in the order map_facts_to_data_points returns them
      "nodes":      [{"id", "type", "part_of", "belongs_to_set", "properties"}, ...],  # sorted by id
      "edges":      [{"source", "target", "relationship_name", "properties"}, ...]
    }

Node properties are the node's logical graph properties: the model's fields
minus the run-dependent timestamps (created_at, updated_at), minus part_of
(stored as an edge), with belongs_to_set reduced to the node-set names the
graph keeps. A None-valued field is kept as null. ``part_of`` is the id of the
node's CodeRepository (null on a repository), and ``belongs_to_set`` the
node's NodeSet points as ``{"id", "name"}`` (null when untagged). Edge
properties drop the run-dependent ``updated_at``. Edges keep
build_code_graph_edges' order.
"""

import json
import shutil
import sys
from pathlib import Path

from cognee.tasks.code_graph.enola import parse_enola_snapshot
from cognee.tasks.code_graph.extract_code_graph import (
    build_code_graph_edges,
    map_facts_to_data_points,
)

REPO_ROOT = Path(__file__).resolve().parent.parent
FIXTURES_OUT = REPO_ROOT / "crates" / "code-graph" / "tests" / "fixtures"
REPO_PATH = "/tmp/shop"

# Python's fixtures (relative to the directory given on the command line).
SNAPSHOTS = {"basic": ".", "docs_and_flows": "docs_and_flows"}
# Snapshots written in this repo, read in place:
#   insights   insights.json evidence by fact_id, by name, by bare file, and
#              evidence nothing resolves
#   all_kinds  test_ref, file_ref, intent, extraction, association and lint
#              facts, scalar and non-scalar props, a null prop, a -0 prop and
#              a symbol whose symbol_kind is null
#   cluster    two repositories (makes_request / reaches_route), with
#              relations and matched routes that do not resolve
LOCAL_SNAPSHOTS = ("insights", "all_kinds", "cluster")
SNAPSHOT_FILES = ("facts.jsonl", "receipt.json", "insights.json", "flows.json")
NODE_SETS = {"untagged": None, "tagged": ["team-a", "shared"]}


def node_properties(point):
    dumped = point.model_dump(mode="json")
    properties = {}
    for key, value in dumped.items():
        if key in ("created_at", "updated_at", "part_of"):
            continue
        if key == "belongs_to_set" and value is not None:
            value = [entry["name"] if isinstance(entry, dict) else entry for entry in value]
        properties[key] = value
    return properties


def node_entry(point):
    part_of = getattr(point, "part_of", None)
    belongs_to_set = point.belongs_to_set
    return {
        "id": str(point.id),
        "type": type(point).__name__,
        "part_of": str(part_of.id) if part_of is not None else None,
        "belongs_to_set": (
            [{"id": str(entry.id), "name": entry.name} for entry in belongs_to_set]
            if belongs_to_set is not None
            else None
        ),
        "properties": node_properties(point),
    }


def edge_entry(edge):
    source, target, relationship_name, properties = edge
    return {
        "source": str(source),
        "target": str(target),
        "relationship_name": relationship_name,
        "properties": {
            key: (str(value) if key in ("source_node_id", "target_node_id") else value)
            for key, value in properties.items()
            if key != "updated_at"
        },
    }


def write_goldens(snapshot, source_dir):
    facts, _receipt = parse_enola_snapshot(source_dir)
    edges, skipped = build_code_graph_edges(facts, repo_path=REPO_PATH)
    for tag, node_set in NODE_SETS.items():
        points = map_facts_to_data_points(facts, repo_path=REPO_PATH, node_set=node_set)
        golden = {
            "facts": facts,
            "skipped": skipped,
            "node_order": [str(point.id) for point in points],
            "nodes": sorted(
                (node_entry(point) for point in points),
                key=lambda node: node["id"],
            ),
            "edges": [edge_entry(edge) for edge in edges],
        }
        out = FIXTURES_OUT / "goldens" / f"{snapshot}__{tag}.json"
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(golden, indent=2, sort_keys=True, ensure_ascii=False) + "\n")
        print(f"wrote {out.relative_to(REPO_ROOT)}")


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    python_fixtures = Path(sys.argv[1])

    for snapshot, relative in SNAPSHOTS.items():
        source_dir = python_fixtures / relative
        target_dir = FIXTURES_OUT / "snapshots" / snapshot
        target_dir.mkdir(parents=True, exist_ok=True)
        for name in SNAPSHOT_FILES:
            if (source_dir / name).is_file():
                shutil.copyfile(source_dir / name, target_dir / name)
        write_goldens(snapshot, source_dir)

    for snapshot in LOCAL_SNAPSHOTS:
        write_goldens(snapshot, FIXTURES_OUT / "snapshots" / snapshot)


if __name__ == "__main__":
    main()
