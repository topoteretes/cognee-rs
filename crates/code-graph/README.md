# cognee-code-graph

The deterministic code graph: what an [enola](https://github.com/enola-labs/enola)
snapshot of a repository becomes in cognee's graph. A port of Python cognee's
`cognee/tasks/code_graph` at the parity pin **a59433e6b** (enola **0.5.1**).

It runs `enola --generate` on a repository, reads the snapshot it writes,
maps it to nodes and edges, and writes them to a graph. `load_code_graph`
does all four for one local repository.

## What it does

| Step | Function | Python |
|---|---|---|
| Read `.enola/` | `parse_enola_snapshot`, `ParsedSnapshot::read` | `enola.parse_enola_snapshot` |
| Reject an unknown artifact format | `validate_receipt` (`SUPPORTED_FORMAT_VERSIONS = [1]`) | `enola.validate_receipt` |
| Identity for incremental skip | `snapshot_identity` | `enola.snapshot_identity` |
| Facts to nodes | `map_facts_to_data_points` | `extract_code_graph.map_facts_to_data_points` |
| Relations to typed edges | `build_code_graph_edges` | `extract_code_graph.build_code_graph_edges` |
| Node identity | `fact_node_id(repo, kind, name)` | `extract_code_graph.fact_node_id` |
| Find the binary | `find_enola_binary` | `enola.find_enola_binary` |
| Run `enola --generate` | `run_enola_generate` | `enola.run_enola_generate` |
| Extract | `extract_code_graph` | `extract_code_graph.extract_code_graph` |
| Write nodes | `add_code_graph_data_points` | `extract_code_graph.add_code_graph_data_points` |
| Write edges, sweep, detag, stamp | `add_code_graph_edges` | `extract_code_graph.add_code_graph_edges` |
| Read the stamp | `stored_repository_state` | `extract_code_graph._stored_repository_state` |
| The whole load | `load_code_graph` | `extract_code_graph.get_code_graph_tasks` |
| The CODE_REPO cognify route | `extract_code_repo_graph` | `code_repo.extract_code_repo_graph` |

`parse_enola_snapshot` reads `facts.jsonl`. It also turns `insights.json`
findings into `insight` facts and `flows.json` entry points into `flow` facts.
A `flows.json` is read only when `receipt.json` vouches for it.

Facts stay plain JSON objects (`Fact = serde_json::Map`), as Python keeps them
as dicts. The mapping reads them key by key the way Python does, so a fact of
an unexpected shape degrades the same way in both SDKs: it is dropped, skipped
or partly read, and never fails the run.

## Running enola

enola is an external binary, and this is a production subprocess:

- **Which binary.** `code_graph.enola_path` (env `ENOLA_PATH`) names it; the
  file must exist. When it is empty, `enola` is looked up on `PATH` by the
  operating system when the process starts. This crate never reads the
  environment. A missing binary is `CodeGraphError::EnolaNotInstalled`
  (Python's `EnolaNotInstalledError`, HTTP 500). Python also looks in its
  own environment's scripts directory, where the `enola-cli` wheel installs
  the binary; Rust has no such directory.
- **How it runs.** `enola --generate <repo>`, with the repository as working
  directory (so a repo-local `mcp-arch.yaml` applies and the caller's never
  does), `ENOLA_NO_UPDATE_CHECK=1` and `ENOLA_NO_PROMPTS=1` on top of the
  inherited environment, stdin closed. enola writes `.enola/` inside the
  repository.
- **Failures.** A run longer than `code_graph.enola_timeout_secs` (default
  600) is killed. A timeout, a non-zero exit (with the last 2000 characters
  of stderr) or a run that leaves no `facts.jsonl` is
  `CodeGraphError::EnolaSnapshot` (Python's `EnolaSnapshotError`, HTTP 500).
  The child is spawned with `kill_on_drop`, so dropping the load's future
  kills it.

## Writing the graph

A load is incremental, as Python's is:

- **Unchanged snapshot.** `extract_code_graph` reads the repository node
  first. When it already carries this snapshot's id (`receipt.json`'s
  `snapshot_id`, else the hash of `facts.jsonl`) and the current mapping
  version, the snapshot is not mapped and nothing is written
  (`CodeGraphLoad::skipped`). Only an untagged repository is skipped: a tagged
  call, or a repository node that still carries a tag, always loads. A failed
  read of the stamp loads in full.
- **Nodes.** Writes are graph-only (no embedding call) and deltas against the
  graph: only the snapshot's own nodes are read (`get_nodes`). A fact node is
  written when the graph lacks it or its `fact_hash` changed; repository nodes
  are always written. Each written node goes through Python's storage walk: a
  `part_of` edge to its repository, and per node set a `NodeSet` node and a
  `belongs_to_set` edge, with Python's default edge properties
  (`edge_object_id`, `feedback_weight`, the fallback `edge_text`).
- **Typed relations** (`calls`, `imports`, ...) the graph does not have yet
  are added with `add_edges`.
- **Stale sweep.** What earlier loads of the same repositories derived and
  this snapshot no longer does is removed: code nodes of the snapshot's
  repositories that are not in it, and edges between the snapshot's nodes
  that it no longer has (`delete_nodes`, `GraphDBTrait::delete_edge_triples`).
  Other repositories and non-code nodes are never touched.
- **Detag.** Node-set tags the repository's nodes carry but this call does
  not are removed: the `belongs_to_set` edges (by node-set id) and the names
  in the `belongs_to_set` property (by exact name).
- **Stamp.** Last, every repository of the snapshot is stamped with
  `last_snapshot_id`, `last_mapping_version`, `last_receipt` (the receipt
  projection) and `last_delta` (added, updated, unchanged and removed counts
  with up to 20 sample names each, `edges_added`, `edges_removed`,
  `node_set_detagged`). A load that fails before the stamp is never recorded
  as done, so it is not skipped next time.

Python reads the whole graph (`get_graph_data`) for the sweep and the detag.
Here one scoped read is taken instead, after the writes: the repositories'
nodes and their one-hop neighbourhood (`get_neighborhood(repos, 1)`). Every
code node of a repository has a `part_of` edge to it, so that read holds them
all, plus the node sets the repository belongs to and every edge among them.
A code node that lost its `part_of` edge outside this crate is not seen.

With a `CodeGraphLedger`, every node and edge of the snapshot, written or left
unchanged, is recorded in the relational provenance ledger under the owning
`Data` row, before the graph write it describes, so deleting the row (or
rolling back the run) finds it. The row ids are the ones cognify uses
(`cognee_database::provenance_ids`). A skipped load records its rows too
(Python records none): its owner may be another dataset over the same graph,
which deleting the first owner must not strip. The sweep leaves the ledger
alone, as Python's does: a swept node's rows stay with their owner, and
deleting the owner later finds nothing more to delete. Deleting a repository's
`Data` row (`forget`) removes its code graph through these rows and leaves
other repositories' nodes, and node sets they share, in place.

## The CODE_REPO route

`add(<project dir>)` stores a code project as one repo manifest item
(`cognee_ingestion::code_repo`, Python's `resolve_code_repository`): a JSON
document naming the repository (`repo_path`) and its source files, stamped
`system_metadata.source = "code_repo"`. cognify routes that item to
`cognee_cognify::tasks::run_code_routes`, which reads the stored manifest and
calls `extract_code_repo_graph`: the repository directory must still exist
(`CodeGraphError::InvalidRepoItem` otherwise, Python's `ValueError`), and the
item's `external_metadata["node_set"]` tags every code node. The load runs in
the cognify run, so its ledger rows carry that run's id. One repository per
item for now; Python loads a dataset's repositories as one enola cluster
(SDK-1053 O8).

Python drops a deleted repository's survivors' completion stamps so the next
`cognify()` re-extracts them as a cluster (`mark_dataset_code_repos_for_rebuild`);
that comes with the clusters (SDK-1053 O8). Python also invalidates its CODE
retriever's snapshot cache after every write; Rust has no such cache yet.

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

`tests/load.rs` runs the whole load with a stand-in enola
(`tests/fixtures/bin/enola`, a shell script that copies a snapshot fixture)
and compares the graph it writes with the same goldens. Its ignored test runs
a real enola 0.5.1 on Python's e2e fixture (`tests/fixtures/code_repo_fixture`)
and checks Python's known answers:

```bash
ENOLA_PATH=/path/to/enola cargo test -p cognee-code-graph --test load -- --ignored
```

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
