#!/usr/bin/env bash
#
# Alice in Wonderland end-to-end demo for the cognee-rust CLI.
#
# Pipeline: add (ingest the PDF) -> cognify (build the knowledge graph)
#           -> several searches across different query types.
#
# All state is isolated in a per-run work directory so this never pollutes
# the repo root or your global cognee state. LLM credentials are read from
# the project-root `.env` (OPENAI_URL / OPENAI_TOKEN / OPENAI_MODEL); ONNX
# embeddings auto-download to target/models on first run.
#
# If no PDF path is given, the Alice in Wonderland PDF is lazily downloaded to a
# temp cache ($PDF_CACHE_DIR) on first run and reused thereafter.
#
# Usage:
#   demo/run_alice_demo.sh [PATH_TO_PDF] [--fresh] [--clean]
#
#   PATH_TO_PDF   PDF to ingest (default: lazy-download $PDF_URL to a temp cache)
#   --fresh       Wipe the work directory before running (re-ingest + re-cognify)
#   --clean       Delete the work directory and exit (no pipeline run)
#
# Env overrides: PDF_URL, PDF_CACHE_DIR, PDF_PATH, DATASET_NAME, WORK_DIR, BIN, TOP_K

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ── Config (override via env) ────────────────────────────────────────────────
# PDF_PATH empty => lazy-download $PDF_URL into $PDF_CACHE_DIR (cached across runs).
PDF_URL="${PDF_URL:-https://www.adobe.com/be_en/active-use/pdf/Alice_in_Wonderland.pdf}"
PDF_CACHE_DIR="${PDF_CACHE_DIR:-${TMPDIR:-/tmp}/cognee-alice-demo}"
PDF_PATH="${PDF_PATH:-}"
DATASET_NAME="${DATASET_NAME:-alice_in_wonderland}"
# Work dir lives in the system temp dir (not the repo tree), so nothing here
# needs to be gitignored.
WORK_DIR="${WORK_DIR:-${TMPDIR:-/tmp}/cognee-alice-demo/work}"
BIN="${BIN:-$PROJECT_ROOT/target/release/cognee-cli}"
TOP_K="${TOP_K:-10}"
FRESH=0
CLEAN=0

# ── Parse args ───────────────────────────────────────────────────────────────
for arg in "$@"; do
  case "$arg" in
    --fresh) FRESH=1 ;;
    --clean) CLEAN=1 ;;
    -h|--help) sed -n '3,23p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) PDF_PATH="$arg" ;;
  esac
done

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
step() { printf '\n\033[1;34m━━ %s\033[0m\n' "$*"; }

# ── Timing helpers ───────────────────────────────────────────────────────────
# `now` returns a high-resolution epoch timestamp (seconds.fraction). macOS's
# default bash 3.2 has neither `date +%s%N` nor `$EPOCHREALTIME`, so we prefer
# bash 5's $EPOCHREALTIME and fall back to perl's Time::HiRes (ships with macOS).
now() {
  if [[ -n "${EPOCHREALTIME:-}" ]]; then
    printf '%s\n' "${EPOCHREALTIME/,/.}"   # some locales use a comma decimal
  else
    perl -MTime::HiRes=time -e 'printf "%.3f\n", time' 2>/dev/null || date +%s
  fi
}

# Parallel arrays recording each measured phase (bash 3.2 has no assoc arrays).
TIMING_LABELS=()
TIMING_SECS=()

# record <label> <start_ts> — append the elapsed time since <start_ts>.
record() {
  local label="$1" start="$2" end
  end="$(now)"
  TIMING_SECS+=("$(awk -v e="$end" -v s="$start" 'BEGIN{printf "%.3f", e - s}')")
  TIMING_LABELS+=("$label")
}

# fmt_dur <seconds> — human-friendly duration ("4.21s" or "1m 04.2s").
fmt_dur() {
  awk -v s="$1" 'BEGIN{
    if (s < 60) { printf "%.2fs", s }
    else { printf "%dm %04.1fs", int(s/60), s - 60*int(s/60) }
  }'
}

# ── Cleanup-only mode ────────────────────────────────────────────────────────
# `--clean` removes all generated state and exits. It needs neither the PDF nor
# the built binary, so it runs before the preconditions below.
if [[ "$CLEAN" -eq 1 ]]; then
  if [[ -d "$WORK_DIR" ]]; then
    step "Cleaning work directory: $WORK_DIR"
    rm -rf "$WORK_DIR"
    echo "Removed all generated data (graph, vector store, relational DB, ingested files, visualization)."
  else
    echo "Nothing to clean — work directory does not exist: $WORK_DIR"
  fi
  exit 0
fi

# ── Lazy-load the PDF ─────────────────────────────────────────────────────────
# No explicit path => fetch the demo PDF into the temp cache once and reuse it.
if [[ -z "$PDF_PATH" ]]; then
  mkdir -p "$PDF_CACHE_DIR"
  PDF_PATH="$PDF_CACHE_DIR/$(basename "$PDF_URL")"
  if [[ -f "$PDF_PATH" ]]; then
    echo "Using cached PDF: $PDF_PATH"
  else
    step "Downloading PDF: $PDF_URL"
    # Download to a temp file first, then move into place, so an interrupted
    # download never leaves a corrupt file in the cache.
    tmp_dl="$(mktemp "$PDF_CACHE_DIR/.download.XXXXXX")"
    if command -v curl >/dev/null 2>&1; then
      curl -fSL --retry 3 -o "$tmp_dl" "$PDF_URL" \
        || { echo "ERROR: download failed: $PDF_URL" >&2; rm -f "$tmp_dl"; exit 1; }
    elif command -v wget >/dev/null 2>&1; then
      wget -q -O "$tmp_dl" "$PDF_URL" \
        || { echo "ERROR: download failed: $PDF_URL" >&2; rm -f "$tmp_dl"; exit 1; }
    else
      rm -f "$tmp_dl"
      echo "ERROR: need curl or wget to download the PDF (or pass a local PATH_TO_PDF)" >&2
      exit 1
    fi
    mv "$tmp_dl" "$PDF_PATH"
    echo "Saved to: $PDF_PATH ($(wc -c < "$PDF_PATH" | tr -d ' ') bytes)"
  fi
fi

# ── Preconditions ────────────────────────────────────────────────────────────
if [[ ! -f "$PDF_PATH" ]]; then
  echo "ERROR: PDF not found: $PDF_PATH" >&2
  echo "       Pass a readable path as the first argument, or leave it unset to" >&2
  echo "       lazy-download the demo PDF from \$PDF_URL." >&2
  exit 1
fi
PDF_PATH="$(cd "$(dirname "$PDF_PATH")" && pwd)/$(basename "$PDF_PATH")"  # absolutize

if [[ ! -x "$BIN" ]]; then
  step "Building cognee-cli (release) — first build can take a few minutes"
  (cd "$PROJECT_ROOT" && cargo build --release -p cognee-cli)
fi

if [[ ! -f "$PROJECT_ROOT/.env" ]]; then
  echo "WARNING: no .env at $PROJECT_ROOT — cognify/search need OPENAI_URL/OPENAI_TOKEN/OPENAI_MODEL" >&2
fi

# ── Isolate all backend state under WORK_DIR ─────────────────────────────────
if [[ "$FRESH" -eq 1 ]]; then
  step "Wiping work directory: $WORK_DIR"
  rm -rf "$WORK_DIR"
fi
mkdir -p "$WORK_DIR"

export COGNEE_SYSTEM_ROOT_DIRECTORY="$WORK_DIR/system"   # graph + vector DB land here
export COGNEE_DATA_ROOT_DIRECTORY="$WORK_DIR/data"       # ingested file copies
export DATABASE_URL="sqlite:$WORK_DIR/cognee.db?mode=rwc" # relational metadata

# Run from PROJECT_ROOT so `.env` is loaded and the target/models embedding
# cache is reused across runs.
cd "$PROJECT_ROOT"

bold "Alice in Wonderland — cognee-rust demo"
echo "  PDF:        $PDF_PATH"
echo "  Dataset:    $DATASET_NAME"
echo "  Work dir:   $WORK_DIR"
echo "  Binary:     $BIN"

# ── 1. Add (ingest) ──────────────────────────────────────────────────────────
step "1/4  add — ingesting the PDF"
T0="$(now)"
"$BIN" add "$PDF_PATH" --dataset-name "$DATASET_NAME"
record "add" "$T0"

# ── 2. Cognify (build knowledge graph) ───────────────────────────────────────
step "2/4  cognify — extracting the knowledge graph (LLM-backed, may take a while)"
T0="$(now)"
"$BIN" cognify --datasets "$DATASET_NAME"
record "cognify" "$T0"

# ── 3. Searches ──────────────────────────────────────────────────────────────
step "3/4  search — querying the knowledge graph"

run_search() {
  local qtype="$1"; local query="$2" t0
  printf '\n\033[1;32m▸ [%s]\033[0m %s\n' "$qtype" "$query"
  # The CLI emits search results through the tracing layer (tagged with the
  # search command's module), interleaved with backend log noise. Keep only
  # those result lines and strip the timestamp/level/module prefix so the demo
  # prints clean answers.
  t0="$(now)"
  "$BIN" search "$query" \
    --query-type "$qtype" \
    --datasets "$DATASET_NAME" \
    --top-k "$TOP_K" \
    --output-format pretty 2>&1 \
  | grep -F '[cognee_cli::commands::search]' \
  | sed -E 's/^[0-9T:.-]+ \[[A-Z ]+\] //; s/ \[cognee_cli::commands::search\]$//'
  record "search ($qtype)" "$t0"
}

run_search GRAPH_COMPLETION "Who is Alice and what happens to her in the story?"
run_search GRAPH_COMPLETION "What is the relationship between the Queen of Hearts and the other characters?"
run_search RAG_COMPLETION   "Describe the tea party scene with the Mad Hatter."
run_search SUMMARIES        "Summarize the main events of Alice's adventures."
run_search CHUNKS           "the White Rabbit"

# ── 4. Visualize (interactive HTML knowledge graph) ──────────────────────────
step "4/4  visualize — rendering the knowledge graph to HTML"
GRAPH_HTML="$WORK_DIR/alice_graph.html"
# `visualize` reads the same graph DB (selected via COGNEE_SYSTEM_ROOT_DIRECTORY)
# and prints the written path on stdout; suppress the interleaved backend logs.
T0="$(now)"
"$BIN" visualize --output "$GRAPH_HTML" 2>/dev/null
record "visualize" "$T0"
if [[ -f "$GRAPH_HTML" ]]; then
  echo "Wrote self-contained d3.js visualization: $GRAPH_HTML ($(wc -c < "$GRAPH_HTML" | tr -d ' ') bytes)"
else
  echo "WARNING: visualization file was not created at $GRAPH_HTML" >&2
fi

# ── Timing summary ───────────────────────────────────────────────────────────
step "Timing summary"
total=0
printf '  %-28s %12s\n' "Phase" "Elapsed"
printf '  %-28s %12s\n' "----------------------------" "------------"
for i in "${!TIMING_LABELS[@]}"; do
  secs="${TIMING_SECS[$i]}"
  total="$(awk -v t="$total" -v s="$secs" 'BEGIN{printf "%.3f", t + s}')"
  printf '  %-28s %12s\n' "${TIMING_LABELS[$i]}" "$(fmt_dur "$secs")"
done
printf '  %-28s %12s\n' "----------------------------" "------------"
printf '  \033[1m%-28s %12s\033[0m\n' "TOTAL (add+cognify+search+viz)" "$(fmt_dur "$total")"

step "Done"
echo "Knowledge graph + vector store persisted under: $WORK_DIR"
echo "Open the graph visualization:   open $GRAPH_HTML"
echo "Re-run searches instantly (skip add/cognify) by querying the same dataset, e.g.:"
echo "  COGNEE_SYSTEM_ROOT_DIRECTORY=$WORK_DIR/system COGNEE_DATA_ROOT_DIRECTORY=$WORK_DIR/data \\"
echo "  DATABASE_URL='sqlite:$WORK_DIR/cognee.db?mode=rwc' \\"
echo "  $BIN search \"your question\" --datasets $DATASET_NAME"
