# cognify failure-parity harness

Answers one question empirically: **do Rust and Python behave the same when a
cognify run fails?**

Both halves are executed, not modelled. The Python half runs the real
`cognee.cognify()` from cognee 1.5.3 (verified byte-identical, on every file in
the failure path, to the reference tree at `690c0ec`); the Rust half runs the
real `cognee_cognify::cognify()` from this branch. Only the AI calls are
replaced, with the same canned graph and summary on both sides.

## Why this is not the existing `e2e-cross-sdk` docker harness

The `e2e-cross-sdk` image bakes both CLIs into one container. Its Rust binary
is a release build of the tree at image-build time, so it cannot answer what
*this branch* does without a full rebuild, and the image on this machine also
pins cognee **1.0.9** — a version whose `run_tasks.py` predates the failure
handling under test. This harness therefore splits the two halves: Python runs
in its own minimal container at the reference version, Rust runs natively as a
cargo test against the working tree. The two meet in a shared JSON observation
format.

## Layout

| file | what it is |
|---|---|
| `Dockerfile` | Python 3.12 + `cognee==1.5.3`. Build once. |
| `py_probe.py` | Runs ONE (scenario, config) cell and prints one `@@OBS@@<json>` line. |
| `run_python.sh` | Drives the Python matrix, fresh process + fresh workspace per cell, `REPS` repeats. |
| `../../crates/cognify/tests/failure_parity_probe.rs` | The Rust probe: same matrix, same observation shape. |
| `run_rust.sh` | Drives the Rust matrix. |
| `compare.py` | Diffs the two observation files. Exits non-zero on any divergence. |
| `observations_*.jsonl` | The recorded runs. |
| `baseline_report.txt` | `compare.py` on the recorded runs — the 14 divergences as found. |
| `seeded_report*.txt` | The same comparison with a defect seeded on the Rust side, so the report is known to move when behaviour does. |

## Running it

```bash
docker build -t cognee-failure-parity-py:1.5.3 e2e-cross-sdk/failure-parity
REPS=3 e2e-cross-sdk/failure-parity/run_python.sh   # ~25 min, needs network on first run
e2e-cross-sdk/failure-parity/run_rust.sh            # seconds
python3 e2e-cross-sdk/failure-parity/compare.py
```

Network is needed only because ladybug downloads its `json` extension on first
use inside the container.

## The matrix

Scenarios — each poisons the MIDDLE data item, so neither "first" nor "last"
dispatch order can hide a divergence:

* `clean` — control.
* `unreadable_file` — the item's stored bytes are gone; the read fails on the
  way into the chunker.
* `extraction_failure` — the LLM raises for that item's chunk during graph
  extraction only.
* `summarization_failure` — the LLM raises for that item's chunk during
  summarization only.
* `second_run_after_success` — an earlier run completed two items; a later run
  fails on a third. Tests whether a failure damages a *previous* run's markers
  and artifacts.

Configs — Python exposes exactly one knob here,
`RAISE_INCREMENTAL_LOADING_ERRORS` (`run_tasks_data_item.py`, default `true`).
The Rust probe feeds the same value through the SDK's own
`FailureStop::from_env`, so the mapping under test is the shipped one rather
than a hand-written translation.

## Non-determinism is recorded, not smoothed

Python's `run_tasks` gathers the per-item chains with a bare `asyncio.gather`
(no `return_exceptions`), so the first failure propagates **while the sibling
items are still running**, and the rollback races them. `REPS` defaults to 3
and `compare.py` reports a cell whose Python samples disagree as `UNSTABLE`,
comparing Rust against the *set* of observed Python values.

## What is compared, and what is not

Compared: what the caller receives (exception vs value), the terminal
`pipeline_runs` status / `run_info` keys / error presence, the full run-status
sequence, per-item completion markers **including the key encoding**, and
whether any graph node, live graph edge or vector point survived.

Not compared, on recorded evidence rather than assumption:

* **Absolute counts.** The two SDKs extract with different vocabularies and
  embed at different widths, so equality would be meaningless. Emptiness is
  compared, which is what a rollback claim is about.
* **Relational ownership rows.** The Python probe records
  `stores_provenance_in_graph`, and on ladybug it comes back `true`: Python
  keeps its cognify provenance in the graph and has no relational `Node`/`Edge`
  ledger to line up against Rust's. Both sides' row counts are printed anyway.
* **Dangling graph edges on the Rust side.** `MockGraphDB::delete_nodes`
  removes nodes only, while the real adapters issue `DETACH DELETE`
  (`LadybugAdapter::delete_node`), so the mock keeps edges no real backend
  would. The Rust probe reports `edge_count` filtered to edges with both
  endpoints alive and keeps the raw count beside it as
  `edge_count_raw_including_dangling`.
