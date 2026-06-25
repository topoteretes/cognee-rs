#!/usr/bin/env bash
#
# Run the cognee-rust demo for one or more HowTo100M videos (Android device).
#
# For each video ID the script runs:
#   1. <video_id>.json       — add + cognify commands (with timestamp offsets)
#   2. <video_id>_search.json — search commands
#
# Usage:
#   ./demo/run_video_demo_android.sh [--skip-build] <video_id> [video_id ...]
#
# Example:
#   ./demo/run_video_demo_android.sh --skip-build GIxObShU_UE
#
# Available video IDs (in demo/how_to_videos/):
#   GIxObShU_UE  ieVqbc0VjEI  JRo71AhOWK8  LMR7xw8xIiM  rGHhLCU_Sks  _vvCaZE3g5c

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

EXTRA_ARGS=()
VIDEO_IDS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-build)
      EXTRA_ARGS+=("--skip-build")
      shift
      ;;
    *)
      VIDEO_IDS+=("$1")
      shift
      ;;
  esac
done

if [[ ${#VIDEO_IDS[@]} -eq 0 ]]; then
  echo "Usage: $0 [--skip-build] <video_id> [video_id ...]" >&2
  echo "" >&2
  echo "Available video IDs:" >&2
  for f in "${SCRIPT_DIR}"/how_to_videos/*_search.json; do
    basename "$f" | sed 's/_search\.json$/  /' | tr -d '\n'
  done >&2
  echo "" >&2
  exit 1
fi

exec "${SCRIPT_DIR}/run_cognee_rust_demo_android.sh" "${EXTRA_ARGS[@]}" --video-ids "${VIDEO_IDS[@]}"
