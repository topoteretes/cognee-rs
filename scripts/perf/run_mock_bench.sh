#!/usr/bin/env bash
# Run the shared Python percentile orchestrator against the Rust `cognee-cli bench`.
#
# Reuses cognee/tests/performance/statistics_percentile_report.py from the
# sibling `../cognee` (Python) checkout — see T7 — driving it via the BENCH_CMD
# override so the Rust SDK renders through the identical percentile table + HTML
# report. Runs fully offline in `--mock-llm` mode: deterministic mock embeddings
# and a record/replay LLM cassette, no API key required.
#
# Usage:
#   scripts/perf/run_mock_bench.sh
#   RUNS=3 scripts/perf/run_mock_bench.sh
#   COGNEE_PY=/path/to/cognee scripts/perf/run_mock_bench.sh
#
# Overridable environment variables:
#   COGNEE_PY   Path to the Python cognee checkout (default: ../cognee).
#   RUNS        Number of sequential bench runs (default: 10).
#   BENCH_BIN   Pre-built cognee-cli binary to use (default: build a release one).
#   CASSETTE    LLM replay cassette path (default: scripts/perf/fixtures/cassette.json).
#   MEMORIES    Memory corpus path (default: scripts/perf/fixtures/memories.json).
#   OUT_DIR     Output directory for the report (default: target/perf).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

cd "$PROJECT_ROOT"

COGNEE_PY="${COGNEE_PY:-$PROJECT_ROOT/../cognee}"
REPORT="$COGNEE_PY/cognee/tests/performance/statistics_percentile_report.py"
RUNS="${RUNS:-10}"
CASSETTE="${CASSETTE:-$SCRIPT_DIR/fixtures/cassette.json}"
MEMORIES="${MEMORIES:-$SCRIPT_DIR/fixtures/memories.json}"
OUT_DIR="${OUT_DIR:-$PROJECT_ROOT/target/perf}"

if [[ ! -f "$REPORT" ]]; then
    echo "error: orchestrator not found at $REPORT" >&2
    echo "       set COGNEE_PY to the Python cognee checkout root." >&2
    exit 1
fi
if [[ ! -f "$CASSETTE" ]]; then
    echo "error: cassette not found at $CASSETTE (provided by T8)" >&2
    echo "       set CASSETTE=... to override." >&2
    exit 1
fi
if [[ ! -f "$MEMORIES" ]]; then
    echo "error: memory corpus not found at $MEMORIES (provided by T8)" >&2
    echo "       set MEMORIES=... to override." >&2
    exit 1
fi

if [[ -n "${BENCH_BIN:-}" ]]; then
    BIN="$BENCH_BIN"
else
    echo "Building cognee-cli with --features bench (set BENCH_BIN to skip)..." >&2
    cargo build --release -p cognee-cli --features bench
    BIN="$PROJECT_ROOT/target/release/cognee-cli"
fi

mkdir -p "$OUT_DIR"

echo "Orchestrator : $REPORT" >&2
echo "Bench binary : $BIN" >&2
echo "Runs         : $RUNS" >&2
echo "Cassette     : $CASSETTE" >&2
echo "Memories     : $MEMORIES" >&2
echo "Output dir   : $OUT_DIR" >&2

# Offline: deterministic mock embeddings, replay cassette, no API key.
#
# `--mock-llm` and `--mock-memories` are NOT placed in BENCH_CMD: passing
# `--mock-llm`/`--mock-memories` to the orchestrator makes it forward both to the
# bench subcommand (and skip the 60s inter-run cooldown). Duplicating `--mock-llm`
# in BENCH_CMD would trip clap's "argument cannot be used multiple times".
BENCH_CMD="$BIN bench --memories $MEMORIES" \
    python3 "$REPORT" --runs "$RUNS" --mock-llm --mock-memories "$CASSETTE" \
        --html "$OUT_DIR/report.html" \
        -o "$OUT_DIR/report.json"
