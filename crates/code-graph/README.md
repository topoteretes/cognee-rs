# cognee-code-graph

The deterministic code graph: what an [enola](https://github.com/enola-labs/enola)
snapshot of a repository becomes in cognee's graph. A port of Python cognee's
`cognee/tasks/code_graph` at the parity pin **a59433e6b** (enola **0.5.1**).

This crate is the pure half of the feature. It reads a snapshot directory and
returns nodes and edges. It runs no subprocess and writes nothing to a graph:
running `enola --generate`, writing the result and the `code_graph.*` config
keys come in a later stage (SDK-1053 O3).

## What it does

| Step | Function | Python |
|---|---|---|
| Read `.enola/` | `parse_enola_snapshot`, `ParsedSnapshot::read` | `enola.parse_enola_snapshot` |
| Reject an unknown artifact format | `validate_receipt` (`SUPPORTED_FORMAT_VERSIONS = [1]`) | `enola.validate_receipt` |
| Identity for incremental skip | `snapshot_identity` | `enola.snapshot_identity` |
| Facts to nodes | `map_facts_to_data_points` | `extract_code_graph.map_facts_to_data_points` |
| Relations to typed edges | `build_code_graph_edges` | `extract_code_graph.build_code_graph_edges` |
| Node identity | `fact_node_id(repo, kind, name)` | `extract_code_graph.fact_node_id` |

`parse_enola_snapshot` reads `facts.jsonl`. It also turns `insights.json`
findings into `insight` facts and `flows.json` entry points into `flow` facts.
A `flows.json` is read only when `receipt.json` vouches for it.

Facts stay plain JSON objects (`Fact = serde_json::Map`), as Python keeps them
as dicts. The mapping reads them key by key the way Python does, so a fact of
an unexpected shape degrades the same way in both SDKs: it is dropped, skipped
or partly read, and never fails the run.

## Nodes

There are 15 node types. `CodeRepository` is one. The other 14 are
`CodeModule`, `CodeSymbol`, `ApiEndpoint`, `StorageResource`,
`ExternalDependency`, `CodeService`, `CodeTestReference`, `CodeFileReference`,
`CodeInsight`, `CodeFlow`, `CodeIntent`, `CodeExtractionAccount`,
`CodeAssociation` and `CodeLintFinding`. They share one struct,
`CodeGraphEntity`. Its `base.data_type` is the Python class name, so the stored
`type` is the same in both SDKs. Every node indexes `["name"]`.

`CodeNode::graph_properties()` gives a node's properties as Python's graph
node carries them:

- `part_of` is not a property. Python stores it as a `part_of` edge to the
  repository; here it is `CodeGraphEntity::part_of`, for the writer to turn
  into that edge.
- `belongs_to_set` holds the node-set names. The `DataPoint` itself carries
  `NodeSet` entries (`{"id", "name", "type"}`), as the document pipeline does.

## Parity

Node ids, `fact_hash` values, descriptions and edges are byte-identical to
Python's. `fact_hash` is a SHA-256 over Python's `json.dumps(..., sort_keys=True)`,
so `pyjson` reproduces Python's JSON text: separators, `ensure_ascii` escapes
and `float.__repr__`.

`tests/golden.rs` compares the whole output with what Python produced for the
snapshots under `tests/fixtures/snapshots/`, untagged and tagged
(`node_set=["team-a", "shared"]`). To regenerate the goldens, run this with an
interpreter that has Python cognee installed at the pin:

```bash
python3 scripts/generate_code_graph_goldens.py \
    <python-cognee>/cognee/tests/unit/tasks/code_graph/fixtures
```

The other test files port Python's unit tests for these functions. Python's
own golden hashes are among them.

Known differences from Python, all on input enola does not write:

- **Non-standard JSON numbers.** `facts.jsonl` lines holding `NaN`/`Infinity`
  are skipped as corrupt; Python reads them. Integers beyond 64 bits are read
  as floats, so their hash differs.
- **Non-UTF-8 side files.** A non-UTF-8 `receipt.json` or `insights.json` is
  ignored with a warning; Python raises. A non-UTF-8 `facts.jsonl` is an
  `EnolaSnapshot` error in both.
- **Non-iterable `relations`.** A numeric `relations` value counts as no
  relations; Python raises `TypeError`.
- **`symbol_kind: null` on a symbol.** It is not serialised; Python's node
  stores it as `null`.
