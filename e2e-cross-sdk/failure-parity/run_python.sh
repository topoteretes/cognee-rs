#!/usr/bin/env bash
# Drive the Python half of the matrix, one fresh process per cell (cognee
# caches its engines process-globally, so cells must not share one) and one
# fresh workspace per process.
#
# REPS defaults to 3 because Python's default failure path is not
# deterministic: `run_tasks` gathers the per-item chains without
# `return_exceptions`, so the first failure propagates while the siblings are
# still running and the rollback races them. A single sample would report one
# arbitrary draw from that race as if it were the behaviour.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="${1:-$HERE/observations_python.jsonl}"
REPS="${REPS:-3}"
: > "$OUT"
for rep in $(seq 1 "$REPS"); do
  for scenario in clean unreadable_file extraction_failure summarization_failure second_run_after_success; do
    for config in raise_true raise_false; do
      echo "=== python rep$rep $scenario / $config ===" >&2
      docker run --rm -e HOME=/tmp -v "$HERE":/work cognee-failure-parity-py:1.5.3 \
        python /work/py_probe.py "$scenario" "$config" \
        2>"$HERE/.py_${scenario}_${config}.log" \
        | grep '^@@OBS@@' | sed 's/^@@OBS@@//' \
        | python3 -c "import sys,json
for line in sys.stdin:
    obs = json.loads(line)
    obs['repeat'] = $rep
    print(json.dumps(obs, sort_keys=True))" >> "$OUT"
    done
  done
done
wc -l "$OUT" >&2
