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
- A regeneration entry point (`scripts/perf/README.md` and/or a justfile/Make
  target `perf-record` / `perf-report`).
- Green: `scripts/check_all.sh`.
- Project bookkeeping updated (root README "Implemented" list, `CLAUDE.md` if a
  new feature/crate surface was added).

## Step-by-step implementation

1. **How-to doc** `docs/performance/mock-benchmark.md`, covering:
   - The record-once → replay-forever flow (link
     [python-approach.md](../python-approach.md) §6 for the diagram).
   - Env vars: `MOCK_LLM`, `MOCK_LLM_CASSETTE`, `COGNEE_RECORD_LLM`,
     `MOCK_EMBEDDING=deterministic`.
   - Commands: how to run a single mock bench (`cognee-cli bench --mock-llm …`),
     how to run the N-run percentile report (`scripts/perf/run_mock_bench.sh`),
     and how to **refresh the cassette** when prompts/corpus change (the T8
     record command).
   - How to read the output (table columns, the HTML report location).
   - The feature flags (`mock-llm`, `bench`) and when they're on.

2. **Cross-link.** Update [README.md](../README.md) statuses to `Implemented` as
   each task lands, and add a one-line pointer to `docs/performance/` from the
   root `README.md` "Implemented" section / docs index if one exists.

3. **Test inventory.** Confirm coverage exists from earlier tasks and fill gaps:
   - T1 hashing + cassette serde (unit).
   - T2/T3 record→replay round-trip (unit).
   - T3 miss-policy branches (unit).
   - T5 deterministic vectors (unit).
   - T6 CLI bench smoke test, offline (integration).
   - A test that `LlmCassette::load` accepts the committed T8 fixture.

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
