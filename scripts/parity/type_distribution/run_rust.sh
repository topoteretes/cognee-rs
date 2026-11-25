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
#                any earlier engine (python, rust, rust-local) for
#                type_distribution.py
#   engine_name  default rust-o7
#   limit        only the first N files of the list (smoke runs)
#
# Environment: the LLM comes from the caller (LLM_PROVIDER, LLM_MODEL,
# LLM_API_KEY / LLM_ENDPOINT, or OPENAI_URL / OPENAI_TOKEN / OPENAI_MODEL;
# Bedrock reads the AWS credential chain). No `.env` is read. COGNEE_CLI picks
# the binary (default: target/release/cognee-cli). Everything else is pinned
# below to the settings of the original comparison: 512-token chunks counted
# with tiktoken's legacy per-word count, 32 parallel calls, RunToEnd with
# failed_items, and **no ontology**. Embeddings are mocked (deterministic) by
# default — the type analysis reads only the graph — unless KEEP_EMBEDDINGS=1.
# `cognify.cross_chunk_endpoints` stays at its default (true), so edge counts
# are not like-for-like with Python's chunk-local endpoints; types are not
# affected.
#
# The run exits non-zero if cognify fails or the export holds fewer documents
# than were listed; the export and the analysis are still written first.
set -euo pipefail

if [ $# -lt 3 ]; then
    sed -n '2,30p' "$0"
    exit 2
fi
docs_dir=$1
file_list=$2
cmp_dir=$3
engine=${4:-rust-o7}
limit=${5:-}
for arg in "$docs_dir" "$file_list" "$cmp_dir" "$engine"; do
    [ -n "$arg" ] || { echo "error: empty argument" >&2; exit 2; }
done
case "$engine" in */*|.|..) echo "error: engine_name must be a plain name" >&2; exit 2;; esac

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
cli=${COGNEE_CLI:-$repo/target/release/cognee-cli}
[ -x "$cli" ] || { echo "error: $cli not found; build it or set COGNEE_CLI" >&2; exit 1; }

mkdir -p "$cmp_dir"
cmp_dir="$(cd "$cmp_dir" && pwd)"
work="$cmp_dir/$engine/work"
rm -rf -- "$work"
mkdir -p "$work/input" "$work/config/cognee-rust" "$work/data" "$work/system"

expected=0
while IFS= read -r f || [ -n "$f" ]; do
    [ -n "$f" ] || continue
    if [ -n "$limit" ] && [ "$expected" -ge "$limit" ]; then break; fi
    cp -- "$docs_dir/$f" "$work/input/"
    expected=$((expected + 1))
done < "$file_list"
echo "── $expected documents, $(cat "$work"/input/* | wc -c) bytes"

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
# The CLI otherwise merges a `.env` from its working directory or any
# ancestor, which could set an ontology or another model.
export COGNEE_DISABLE_DOTENV=1
unset ONTOLOGY_FILE_PATH ONTOLOGY_RESOLVER ONTOLOGY_MATCHING_STRATEGY
if [ "${KEEP_EMBEDDINGS:-0}" != 1 ]; then
    export MOCK_EMBEDDING=deterministic
fi

cd "$work"
start=$(date +%s)
"$cli" add "$work"/input/* -d type_distribution
cognify_exit=0
"$cli" cognify -d type_distribution || cognify_exit=$?
echo "── add + cognify took $(( $(date +%s) - start )) s"

"$cli" export -o "$work/export"
python3 "$here/export_to_csv.py" "$work/export" "$cmp_dir/$engine"
python3 "$here/type_distribution.py" "$cmp_dir" "$engine"

saved=$(python3 - "$cmp_dir/$engine/db/graph_node.csv.gz" <<'PY'
import csv, gzip, sys
csv.field_size_limit(10**9)
with gzip.open(sys.argv[1], "rt") as f:
    print(sum(1 for r in csv.DictReader(f) if r["type"].endswith("Document")))
PY
)
result=0
if [ "$cognify_exit" -ne 0 ]; then
    echo "error: cognify exited $cognify_exit (see its failure report above)" >&2
    result=1
fi
if [ "$saved" -lt "$expected" ]; then
    echo "error: the export holds $saved of $expected documents" >&2
    result=1
fi
exit "$result"
