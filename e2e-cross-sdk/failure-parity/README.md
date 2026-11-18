# cognify failure-parity harness

Answers one question empirically: **do Rust and Python behave the same when a
cognify run fails?**

Both halves are executed, not modelled. The Python half runs the real
`cognee.cognify()` from cognee at the `http-parity.yml` Python pin (`1d3ed15b`,
installed from source); the Rust half runs the
real `cognee_cognify::cognify()` from this branch. Only the AI calls are
replaced, with the same canned graph and summary on both sides.

## History

The first recorded runs (2026-09-01, cognee 1.5.3 against Rust `edbe807c`)
found 14 divergences (SDK-946, cognee-rs gh #170). Re-run on 2026-11-17 with
Python at the CI pin `1d3ed15b`, `baseline_report.txt` shows none (110 compared
facts over 10 cells). None went by changing what the probe asserts:

* **What the caller receives under `RAISE_INCREMENTAL_LOADING_ERRORS=false`
  (4 cells).** 1.5.3 returned a value; Rust raised. Python changed, not Rust:
  `cognify(raise_on_error=True)` (`66cf08245`, COG-6276, first released after
  1.5.3) is the default, and it raises `CognifyFailedError` on any errored run.
  The probe calls `cognify()` with defaults, so both sides now raise.
* **`pipeline_runs` rows.** Rust's leading `INITIATED` row and a tolerant run's
  extra `COMPLETED` row went in SDK-1039 (`672e1663`, `d731151b`); the
  `run_info.data` key went on both sides (Python COG-6089 `a88402500`, Rust
  `672e1663`). SDK-946 also removed the dataset-level `COMPLETED` row the
  `cognify_datasets` / `cognify_dataset_refs` loops appended, which this probe
  never saw: it calls `cognify()` directly. That one is pinned by
  `crates/cognify/tests/integration_repeat_cognify.rs`.

**Expect divergences when the CI pin moves past 1.6.5.** Running the same
matrix against cognee 1.6.6 differs in 7 cells, from two Python changes Rust
has not ported: summaries are LLM-free by default (SDK-1312, `d84a42fec`), so
`summarization_failure` no longer fails at all; and chunks, document nodes and
embeddings come from a separate `indexing_pipeline` run before extraction
(SDK-397, `137c5f08b`), so a failed cognify run no longer leaves the stores
empty.

## Why this is not the existing `e2e-cross-sdk` docker harness

The `e2e-cross-sdk` image bakes both CLIs into one container. Its Rust binary
is a release build of the tree at image-build time, so it cannot answer what
*this branch* does without a full rebuild; its Python is baked in at build time
too, so trying another cognee commit also means a full rebuild. This harness
therefore splits the two halves: Python runs in its own minimal container at the
chosen cognee version (see `Dockerfile`), Rust runs natively as a
cargo test against the working tree. The two meet in a shared JSON observation
format.

## Layout

| file | what it is |
|---|---|
| `Dockerfile` | Python 3.12 + cognee at `COGNEE_REF` (default: the CI pin) or any `COGNEE_SPEC`. Build once per version. |
| `py_probe.py` | Runs ONE (scenario, config) cell and prints one `@@OBS@@<json>` line. |
| `run_python.sh` | Drives the Python matrix, fresh process + fresh workspace per cell, `REPS` repeats. |
| `../../crates/cognify/tests/failure_parity_probe.rs` | The Rust probe: same matrix, same observation shape. |
| `run_rust.sh` | Drives the Rust matrix. |
| `compare.py` | Diffs the two observation files. Exits non-zero on any divergence. |
| `observations_*.jsonl` | The recorded runs. |
| `baseline_report.txt` | `compare.py` on the recorded runs. |

## Running it

```bash
docker build -t cognee-failure-parity-py:1d3ed15b e2e-cross-sdk/failure-parity
# another version: --build-arg COGNEE_SPEC=cognee==X -t …:X, then run with IMAGE=…:X
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
