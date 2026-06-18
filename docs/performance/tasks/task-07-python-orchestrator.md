# T7 — Reuse the Python orchestrator

**Status:** Not implemented
**Location:** `../cognee` (Python repo) + `scripts/perf/` (Rust repo wrapper)
**Depends on:** T6
**Unblocks:** T8 (running multi-iteration reports)

---

## Rationale

The percentile math and Chart.js HTML report in
[`statistics_percentile_report.py`](../../../../cognee/cognee/tests/performance/statistics_percentile_report.py)
are pure stdlib and SDK-agnostic — re-implementing them in Rust would be wasted
effort and would risk the Python/Rust numbers diverging in presentation. Instead
we **reuse the orchestrator from the `../cognee` checkout** and feed it the Rust
`cognee-cli bench` subcommand. Both SDKs then render through the identical table
and HTML, giving a true apples-to-apples comparison.

## Expected output

- A minimal, upstreamable change to the orchestrator in `../cognee`: a `BENCH_CMD`
  env override of the hardcoded invocation.
- `scripts/perf/run_mock_bench.sh` in this repo: builds the CLI and runs the
  `../cognee` orchestrator with `BENCH_CMD` pointed at the Rust binary in
  `--mock-llm` mode.
- No vendored copy of the orchestrator — it is referenced from `../cognee`.

## Step-by-step implementation

1. **`BENCH_CMD` override** in
   [`../cognee/cognee/tests/performance/statistics_percentile_report.py`](../../../../cognee/cognee/tests/performance/statistics_percentile_report.py).
   Replace the hardcoded command construction (around line 63) with:
   ```python
   import shlex, os
   BENCH_CMD = os.environ.get("BENCH_CMD")
   base = shlex.split(BENCH_CMD) if BENCH_CMD else [sys.executable, str(BENCH_SCRIPT)]
   cmd = base + ["--output", tmp_path] + extra_args
   ```
   This is backward-compatible (no `BENCH_CMD` ⇒ original Python behavior) and is
   the kind of change worth proposing upstream so the Rust repo doesn't carry a
   private patch. Coordinate landing it in the `../cognee` checkout.

2. **Flag compatibility.** The orchestrator forwards `--memories`, `--llm-model`,
   `--llm-provider`, `--embedding-model`, `--embedding-provider`,
   `--embedding-dims`, `--num-memories`, `--mock-llm`, `--mock-memories`. T6's
   clap parser must accept all of these (honoring what matters, tolerating the
   rest). Cross-check the two flag lists before running.

3. **Wrapper script** `scripts/perf/run_mock_bench.sh`:
   ```sh
   #!/usr/bin/env bash
   set -euo pipefail
   COGNEE_PY="${COGNEE_PY:-../cognee}"
   REPORT="$COGNEE_PY/cognee/tests/performance/statistics_percentile_report.py"
   RUNS="${RUNS:-10}"

   cargo build --release -p cognee-cli --features bench
   BIN="$(pwd)/target/release/cognee-cli"

   BENCH_CMD="$BIN bench --mock-llm \
       --mock-memories $(pwd)/scripts/perf/fixtures/cassette.json \
       --memories $(pwd)/scripts/perf/fixtures/memories.json" \
     python3 "$REPORT" --runs "$RUNS" --mock-llm \
       --html "$(pwd)/target/perf/report.html" \
       -o "$(pwd)/target/perf/report.json"
   ```
   (Fixtures `cassette.json` / `memories.json` come from T8.) Make it executable;
   document `COGNEE_PY` and `RUNS` overrides at the top of the file.

4. **`--mock-llm` to the orchestrator** ensures the 60s inter-run cooldown is
   skipped (it keys on `args.mock_llm`), so a 10-run mock report finishes in
   seconds-to-minutes rather than 10+ minutes.

5. **cwd note.** The orchestrator runs the subprocess with `cwd = COGNEE_DIR` (the
   Python repo root). T6's bench pins its own temp state dirs, so cwd is
   irrelevant to the Rust binary — verify the bench does not write state relative
   to cwd.

## Acceptance / verification

- `RUNS=3 scripts/perf/run_mock_bench.sh` completes offline (no API key), prints
  the percentile table, and writes `target/perf/report.html` + `report.json`.
- The HTML opens and shows the percentile bar chart + per-run line chart.
- Running the same script against the Python `bench_cognee.py`
  (`BENCH_CMD` unset, `--mock-llm`) still works — confirming the override is
  non-breaking.
