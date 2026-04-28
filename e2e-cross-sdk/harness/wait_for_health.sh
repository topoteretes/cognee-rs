#!/usr/bin/env bash
# wait_for_health.sh — poll a URL until it returns HTTP 200 or times out.
#
# Usage:  wait_for_health.sh <url>
# Exits 0 on success, 1 after 30 s (60 attempts × 0.5 s).

set -euo pipefail

URL="${1:?Usage: wait_for_health.sh <url>}"

for i in $(seq 1 60); do
    if curl -fsS --max-time 1 "$URL" > /dev/null 2>&1; then
        echo "[wait_for_health] $URL is healthy (attempt $i)"
        exit 0
    fi
    sleep 0.5
done

echo "[wait_for_health] TIMEOUT: $URL did not respond within 30 seconds" >&2
exit 1
