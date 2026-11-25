#!/usr/bin/env python3
"""Convert a ``cognee-cli export`` directory to the CSV layout ``type_distribution.py`` reads.

``type_distribution.py`` was written against ``\\copy`` dumps of the Postgres graph tables
(``graph_node.csv.gz``: id, name, type, properties; ``graph_edge.csv.gz``:
source_id, target_id, relationship_name, properties). ``cognee-cli export``
writes the same graph as ``nodes.jsonl`` and ``facts.jsonl``; this writes both
CSVs into ``<out_dir>/db/`` so one analysis script reads either source.

Usage: export_to_csv.py <export_dir> <out_dir>
"""

import csv
import gzip
import json
import sys
from pathlib import Path


def jsonl(path: Path):
    if not path.exists():
        return
    with path.open() as f:
        for line in f:
            if line.strip():
                yield json.loads(line)


def main(export_dir: str, out_dir: str) -> None:
    src, db = Path(export_dir), Path(out_dir) / "db"
    db.mkdir(parents=True, exist_ok=True)
    nodes = edges = 0
    with gzip.open(db / "graph_node.csv.gz", "wt", newline="") as f:
        w = csv.writer(f)
        w.writerow(["id", "name", "type", "properties"])
        for n in jsonl(src / "nodes.jsonl"):
            rest = {k: v for k, v in n.items() if k not in ("id", "name", "type")}
            w.writerow([n["id"], n.get("name") or "", n.get("type") or "", json.dumps(rest)])
            nodes += 1
    with gzip.open(db / "graph_edge.csv.gz", "wt", newline="") as f:
        w = csv.writer(f)
        w.writerow(["source_id", "target_id", "relationship_name", "properties"])
        for e in jsonl(src / "facts.jsonl"):
            rest = {
                k: v for k, v in e.items() if k not in ("subject_ref", "object_ref", "predicate")
            }
            w.writerow([e["subject_ref"], e["object_ref"], e["predicate"], json.dumps(rest)])
            edges += 1
    print(f"wrote {nodes} nodes and {edges} edges to {db}")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
