#!/usr/bin/env python3
"""Diff the two SDKs' failure observations and report divergences.

Reads ``observations_python.jsonl`` and ``observations_rust.jsonl`` and, for
every (scenario, config) cell, compares a fixed vector of facts. Exits non-zero
when any compared fact differs, so the harness can fail a run rather than
merely describe one.

WHAT IS COMPARED, AND WHY THOSE:
  caller_kind ......... exception vs value — one of the six axes asked about.
  terminal_status ..... the last pipeline_runs row's status.
  terminal_run_info ... its run_info key set and whether it carries an error.
  markers ............. per data-item role: the completion-marker VALUE, plus
                        the KEY the marker is filed under (dataset key format).
  graph_empty ......... whether any graph node / live edge survived.
  vector_empty ........ whether any vector point survived.
  doc_node_present .... per role, whether the item's own document node survived.

WHAT IS NOT COMPARED, ON EVIDENCE:
  absolute node/edge/point COUNTS — the two SDKs run different extraction
  vocabularies and different embedding widths, so equality would be meaningless
  and inequality uninformative. Emptiness IS compared, which is what a rollback
  claim is about.
  relational ownership rows — the Python probe records
  ``stores_provenance_in_graph`` and it comes back True on ladybug 1.5.3, so
  Python keeps no relational Node/Edge ledger to compare with Rust's. The rows
  are printed for both sides and reported as an architectural difference.

NON-DETERMINISM: the Python probe is run several times because Python's default
failure path races (see run_python.sh). A cell whose Python samples disagree is
reported as UNSTABLE, and comparison for that fact uses the set of observed
Python values: Rust matches if its value is among them, and the instability is
reported as a finding in its own right.
"""

import collections
import json
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent


def load(name):
    path = HERE / name
    if not path.exists():
        sys.exit(f"missing observation file: {path}")
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def marker_view(obs):
    """Role -> "<key-token>=<value>" or "" — the marker as a cross-SDK reader
    sees it.

    The KEY is part of the comparison on purpose: a marker filed under a key the
    other SDK does not read is not a marker, it is a silent re-processing bug —
    each SDK would re-cognify what the other finished. The key is rendered as a
    token rather than literally, because the dataset id is freshly minted per
    run and would otherwise differ trivially on every comparison. The token
    names the ENCODING, which is the thing that has to match:
      <dataset_id:dashed> — Python's `str(dataset.id)`, 36 chars with hyphens
      <dataset_id:hex>    — the 32-char dashless form Rust uses for every other id
      anything else is rendered literally.
    """
    dataset_id = obs["dataset_id"]
    dashed = str(dataset_id)
    hexed = dashed.replace("-", "")
    out = {}
    for role, entry in obs["markers"].items():
        parts = []
        for key, value in sorted(entry.items()):
            if key == dashed:
                token = "<dataset_id:dashed>"
            elif key == hexed:
                token = "<dataset_id:hex>"
            else:
                token = f"<literal:{key}>"
            parts.append(f"{token}={value}")
        out[role] = ";".join(parts)
    return out


def facts(obs):
    runs = obs["pipeline_runs"]
    terminal = runs[-1] if runs else {}
    return {
        "caller_kind": obs["caller"]["kind"],
        "terminal_status": terminal.get("status"),
        "terminal_run_info_keys": tuple(terminal.get("run_info_keys", [])),
        "terminal_has_error": terminal.get("run_info_error_present"),
        "markers": tuple(sorted(marker_view(obs).items())),
        "run_status_sequence": tuple(r["status"] for r in runs),
        "marker_key_format_ok": obs["marker_dataset_key_matches_dashed_uuid"],
        "graph_has_nodes": obs["graph"]["node_count"] > 0,
        "graph_has_edges": obs["graph"]["edge_count"] > 0,
        "vector_has_points": obs["vector"]["point_count"] > 0,
        "doc_node_present": tuple(sorted(obs["graph_document_node_present"].items())),
    }


def main():
    py = load("observations_python.jsonl")
    rs = load("observations_rust.jsonl")

    py_by_cell = collections.defaultdict(list)
    for obs in py:
        py_by_cell[(obs["scenario"], obs["config"])].append(obs)
    rs_by_cell = {(obs["scenario"], obs["config"]): obs for obs in rs}

    cells = sorted(set(py_by_cell) | set(rs_by_cell))
    divergences = []
    unstable = []

    for cell in cells:
        scenario, config = cell
        if cell not in py_by_cell or cell not in rs_by_cell:
            divergences.append((cell, "cell", "present in only one SDK", "", ""))
            continue

        py_samples = [facts(o) for o in py_by_cell[cell]]
        rs_facts = facts(rs_by_cell[cell])

        print(f"\n=== {scenario} / {config} ===")
        for key in rs_facts:
            py_values = {s[key] for s in py_samples}
            is_unstable = len(py_values) > 1
            if is_unstable:
                unstable.append((cell, key, sorted(map(str, py_values))))
            match = rs_facts[key] in py_values
            flag = "OK " if match else "DIFF"
            if is_unstable:
                flag += "*"
            py_shown = (
                " | ".join(sorted(map(str, py_values))) if is_unstable else str(next(iter(py_values)))
            )
            print(f"  {flag} {key:28} py={py_shown}    rs={rs_facts[key]}")
            if not match:
                divergences.append((cell, key, "", py_shown, str(rs_facts[key])))

        pyo = py_by_cell[cell][0]
        rso = rs_by_cell[cell]
        print(
            "  --- not compared (architecture): "
            f"py ownership rows={pyo['ownership']['node_rows']}/"
            f"{pyo['ownership']['edge_rows']} "
            f"(provenance_in_graph={pyo['stores_provenance_in_graph']}), "
            f"rs={rso['ownership']['node_rows']}/{rso['ownership']['edge_rows']}; "
            f"py points={pyo['vector']['point_count']} rs={rso['vector']['point_count']}; "
            f"rs dangling edges={rso['graph'].get('edge_count_raw_including_dangling')}"
        )

    print("\n" + "=" * 72)
    if unstable:
        print(f"UNSTABLE (python samples disagree across repeats): {len(unstable)}")
        for cell, key, values in unstable:
            print(f"  {cell[0]}/{cell[1]}  {key}: {values}")
    if divergences:
        print(f"\nDIVERGENCES: {len(divergences)}")
        for cell, key, note, pv, rv in divergences:
            print(f"  {cell[0]}/{cell[1]}  {key}: {note or f'python={pv}  rust={rv}'}")
        return 1
    print("\nNo divergences on the compared facts.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
