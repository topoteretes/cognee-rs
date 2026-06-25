#!/usr/bin/env bash
# wait_for_health.sh — poll a URL until it returns HTTP 200 or times out.
#
# Usage:  wait_for_health.sh <url>
# Exits 0 on success, 1 after 30 s (60 attempts × 0.5 s).

set -euo pipefail

URL="${1:?Usage: wait_for_health.sh <url>}"

for i in $(seq 1 60); do
    if python - "$URL" <<'PY'
import sys
import urllib.request

url = sys.argv[1]
try:
    with urllib.request.urlopen(url, timeout=1) as resp:
        raise SystemExit(0 if resp.status == 200 else 1)
except Exception:
    raise SystemExit(1)
PY
    then
        echo "[wait_for_health] $URL is healthy (attempt $i)"
        exit 0
    fi
    sleep 0.5
done

echo "[wait_for_health] TIMEOUT: $URL did not respond within 30 seconds" >&2
exit 1
