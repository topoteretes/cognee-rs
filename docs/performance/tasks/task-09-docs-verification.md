# T9 — Docs & verification

**Status:** Not implemented
**Location:** `docs/performance/`, `scripts/perf/`, workspace-wide
**Depends on:** T1–T8
**Unblocks:** —

---

## Rationale

The recorder + mock + bench are only useful if a developer can discover and run
them, and only trustworthy if the whole workspace still passes its checks. This
task writes the user-facing how-to, ensures unit/smoke coverage exists, and runs
the full check suite (including the language-binding checks) so nothing regressed.

## Expected output

- `docs/performance/mock-benchmark.md` — a how-to that ties the pieces together.
  This is the only **new** doc T9 writes. The regeneration entry point already
  exists: T8 committed `scripts/perf/README.md` (covers the "run offline
  benchmark", "regenerate the cassette / `perf-record`" commands and the
  `RUNS`/`COGNEE_PY`/`BENCH_BIN`/`CASSETTE`/`MEMORIES` env overrides). T9 must
  **link to** `scripts/perf/README.md` from the how-to, not duplicate it.
- One genuinely missing test (see step 3): a smoke test that `LlmCassette::load`
  accepts the committed T8 fixture `scripts/perf/fixtures/cassette.json`.
- Green: `scripts/check_all.sh`.
- Project bookkeeping updated (root README "Implemented" list, `CLAUDE.md` if a
  new feature/crate surface was added).

## Step-by-step implementation

1. **How-to doc** `docs/performance/mock-benchmark.md`, covering:
   - The record-once → replay-forever flow (link
     [python-approach.md](../python-approach.md) §6 for the diagram).
   - Env vars: `MOCK_LLM`, `MOCK_LLM_CASSETTE`, `COGNEE_RECORD_LLM`,
     `MOCK_EMBEDDING=deterministic`.
   - Commands: how to run a single mock bench (`cognee-cli bench --mock-llm
     --mock-memories <cassette> --memories <corpus> --output <file>`; the
     cassette path can also come from `MOCK_LLM_CASSETTE`), how to run the
     N-run percentile report (`scripts/perf/run_mock_bench.sh`, e.g.
     `RUNS=3 scripts/perf/run_mock_bench.sh`), and — for refreshing the cassette
     when prompts/corpus change — **link to** the `## Regenerating the cassette
     (perf-record)` section in [`scripts/perf/README.md`](../../scripts/perf/README.md)
     (the `COGNEE_RECORD_LLM=…` record command), rather than re-documenting it.
   - How to read the output (table columns, the HTML report location).
   - The feature flags: `mock` (in `cognee-llm`, surfaced as `mock-llm` in
     `cognee-lib`/`cognee-cli`) and `bench` (in `cognee-cli`) — both default-on,
     and how to turn them off.

2. **Cross-link.** In [README.md](../README.md) (the task index), flip the **T9**
   row to `Implemented` — T1–T8 are already marked `Implemented`. Add a one-line
   pointer to `docs/performance/` from the root `README.md` "Implemented" section
   / docs index if one exists.

3. **Test inventory.** The following coverage **already exists** from earlier
   tasks — confirm it still passes, do not re-add it:
   - T1 hashing + cassette serde — `crates/llm/src/mock/cassette.rs` `mod tests`
     (`input_hash_*`, `canonicalize_*`, `cassette_round_trips_through_save_load`).
   - T2/T3 record→replay round-trip — `crates/llm/src/mock/recording.rs` and
     `replay.rs` `mod tests` (`records_structured_output_entry`,
     `record_then_replay_round_trip`, …).
   - T3 miss-policy branches — `crates/llm/src/mock/replay.rs`
     (`miss_empty_graph_*`, `miss_error_returns_err`, …).
   - T5 deterministic vectors — `crates/embedding/src/mock.rs`
     (`test_deterministic_*`) and `config.rs`
     (`test_from_env_mock_embedding_deterministic`).
   - T6 CLI bench smoke test, offline — `crates/cli/tests/cli_bench.rs`
     (`test_bench_mock_offline_smoke`, `test_bench_help`,
     `test_bench_num_memories_truncates`).

   The **only genuinely missing** test T9 should add:
   - A test that `LlmCassette::load` accepts the committed T8 fixture
     `scripts/perf/fixtures/cassette.json` (no existing test references it).

4. **Feature-matrix check.** Verify builds with the mock/bench features both on
   and off:
   ```sh
   cargo check --all-targets
   cargo check -p cognee-llm                       # mock off
   cargo check -p cognee-llm --features mock
   cargo check -p cognee-cli                       # bench off
   cargo check -p cognee-cli --features bench
   ```

5. **Full suite.** Run `scripts/check_all.sh` (fmt → check → clippy → C API →
   Python binding → JS binding). Address any fallout (e.g. new `Settings` fields
   surfacing through bindings — the binding checks will catch drift).

6. **Bookkeeping.** If a new crate feature/CLI subcommand is now part of the
   default surface, reflect it in the project `CLAUDE.md` "Implemented" / crate
   descriptions and the root README, per repo convention.

## Acceptance / verification

- `scripts/check_all.sh` exits 0.
- `cargo test --workspace` passes (mock/bench unit + smoke tests included).
- A clean checkout, with **no API credentials**, can produce a percentile HTML
  report via `RUNS=3 scripts/perf/run_mock_bench.sh`.
- All task rows in [README.md](../README.md) are marked `Implemented`.
