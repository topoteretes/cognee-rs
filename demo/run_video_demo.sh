#!/usr/bin/env bash
#
# Run the cognee-rust demo for one or more HowTo100M videos (host).
#
# For each video ID the script runs:
#   1. <video_id>.json       — add + cognify commands (with timestamp offsets)
#   2. <video_id>_search.json — search commands
#
# Usage:
#   ./demo/run_video_demo.sh <video_id> [video_id ...]
#
# Example:
#   ./demo/run_video_demo.sh GIxObShU_UE ieVqbc0VjEI
#
# Available video IDs (in demo/how_to_videos/):
#   GIxObShU_UE  ieVqbc0VjEI  JRo71AhOWK8  LMR7xw8xIiM  rGHhLCU_Sks  _vvCaZE3g5c

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [[ $# -eq 0 ]]; then
  echo "Usage: $0 <video_id> [video_id ...]" >&2
  echo "" >&2
  echo "Available video IDs:" >&2
  for f in "${SCRIPT_DIR}"/how_to_videos/*_search.json; do
    basename "$f" | sed 's/_search\.json$/  /' | tr -d '\n'
  done >&2
  echo "" >&2
  exit 1
fi

exec "${SCRIPT_DIR}/run_cognee_rust_demo.sh" --video-ids "$@"
