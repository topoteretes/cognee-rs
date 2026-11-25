#!/usr/bin/env bash
# Cognify a document list with the Rust CLI and print its entity-type
# distribution (SDK-1694 test 11).
#
# Usage:
#   run_rust.sh <docs_dir> <file_list> <cmp_dir> [engine_name] [limit]
#
#   docs_dir     directory holding the documents named in file_list
#   file_list    one file name per line (e.g. the 81-document list)
#   cmp_dir      comparison directory; the export lands in
#                <cmp_dir>/<engine_name>/db/graph_{node,edge}.csv.gz, next to
#                any earlier engine (python, rust, rust-local) for type_distribution.py
#   engine_name  default rust-o7
#   limit        only the first N files of the list (smoke runs)
#
# Environment: the LLM comes from the caller (LLM_PROVIDER, LLM_MODEL,
# LLM_API_KEY / LLM_ENDPOINT, or OPENAI_URL / OPENAI_TOKEN / OPENAI_MODEL;
# Bedrock reads the AWS credential chain, e.g. AWS_PROFILE). COGNEE_CLI picks
# the binary (default: target/release/cognee-cli). Everything else is pinned
# below to the settings of the original comparison: 512-token chunks counted
# with tiktoken's legacy per-word count, 32 parallel calls, RunToEnd with
# failed_items, and **no ontology**. Embeddings are mocked (deterministic) by
# default — the type analysis reads only the graph — unless KEEP_EMBEDDINGS=1.
set -euo pipefail

if [ $# -lt 3 ]; then
    sed -n '2,24p' "$0"
    exit 2
fi
docs_dir=$1
file_list=$2
cmp_dir=$3
engine=${4:-rust-o7}
limit=${5:-}

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
cli=${COGNEE_CLI:-$repo/target/release/cognee-cli}
[ -x "$cli" ] || { echo "error: $cli not found; build it or set COGNEE_CLI" >&2; exit 1; }

work="$cmp_dir/$engine/work"
rm -rf "$work"
mkdir -p "$work/input" "$work/config/cognee-rust" "$work/data" "$work/system"

if [ -n "$limit" ]; then
    files=$(head -n "$limit" "$file_list")
else
    files=$(cat "$file_list")
fi
for f in $files; do cp "$docs_dir/$f" "$work/input/"; done
echo "── $(ls "$work/input" | wc -l) documents, $(cat "$work"/input/* | wc -c) bytes"

cat > "$work/config/cognee-rust/config.json" <<EOF
{"version": 1, "settings": {
  "relational_db_url": "sqlite:$work/cognee.db?mode=rwc",
  "data_root_directory": "$work/data",
  "system_root_directory": "$work/system"
}}
EOF

export XDG_CONFIG_HOME="$work/config"
export COGNEE_CHUNK_SIZE=512
export COGNEE_TOKEN_COUNTER=tiktoken
export COGNEE_LEGACY_PER_WORD_TOKEN_COUNT=true
export LLM_MAX_PARALLEL_REQUESTS=${LLM_MAX_PARALLEL_REQUESTS:-32}
export RAISE_INCREMENTAL_LOADING_ERRORS=false
export COGNEE_COGNIFY_ROLLBACK_SCOPE=failed_items
export TELEMETRY_DISABLED=1
unset ONTOLOGY_FILE_PATH ONTOLOGY_RESOLVER COGNEE_ONTOLOGY_FILE_PATH
if [ "${KEEP_EMBEDDINGS:-0}" != 1 ]; then
    export MOCK_EMBEDDING=deterministic
fi

cd "$work"
start=$(date +%s)
"$cli" add "$work"/input/* -d type_distribution
"$cli" cognify -d type_distribution || echo "warning: cognify exited $? (failed_items keeps the completed documents)"
echo "── add + cognify took $(( $(date +%s) - start )) s"

"$cli" export -o "$work/export"
python3 "$here/export_to_csv.py" "$work/export" "$cmp_dir/$engine"
python3 "$here/type_distribution.py" "$cmp_dir" "$engine"
