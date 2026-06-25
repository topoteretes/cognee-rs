#!/usr/bin/env bash
# Emit the names of publishable OSS workspace crates in dependency-topological
# order (dependencies first, dependents last) — the order `cargo publish`
# requires so each crate's `cognee-*` deps are already on crates.io by the
# time it is pushed.
#
# Consumers:
#   * T10c local dry-run sweep (this directory)
#   * release-plz config (release-plz.toml `dependencies_update = true`)
#   * T13 CI publish-dry-run job
#
# Scope: ROOT workspace only. The `capi/` sub-workspace has its own
# Cargo.toml; its sole member (`cognee-capi`) is `publish = false` (T10a) so
# it never reaches crates.io and is excluded by design. If we ever publish a
# capi member, extend this script to cover that sub-workspace.
#
# Crates with `publish = false` (cli, examples, python, bench, test-utils,
# bindings-common, telemetry-emit) are filtered out via the cargo-metadata
# `publish: []` representation.
#
# Usage:
#   scripts/split/publish-order.sh           # one crate name per line on stdout
#   scripts/split/publish-order.sh --help    # this message
set -euo pipefail

usage() {
  sed -n '2,/^set -euo pipefail/p' "$0" | sed 's/^# \{0,1\}//' | head -n -1
}

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  usage
  exit 0
fi

cd "$(dirname "$0")/../.."

cargo metadata --no-deps --format-version 1 --manifest-path Cargo.toml \
  | python3 -c '
import json
import sys
from collections import defaultdict

meta = json.load(sys.stdin)
pkgs = meta["packages"]

# Filter to publishable workspace members.
# cargo represents `publish = false` as "publish": [].
publishable = {}
for p in pkgs:
    if p.get("publish") == []:
        continue
    publishable[p["name"]] = p

# Build dependency edges restricted to intra-workspace publishable deps.
# Skip `dev` and `build` dependency kinds — cargo publish drops those
# without versions when packaging, so they never gate publish order. Keeping
# them would also introduce false cycles (e.g. cognify <-> delete via tests).
deps = defaultdict(set)
rdeps = defaultdict(set)
for name, p in publishable.items():
    for d in p.get("dependencies", []):
        if d.get("kind") in ("dev", "build"):
            continue
        dn = d["name"]
        if dn in publishable and dn != name:
            deps[name].add(dn)
            rdeps[dn].add(name)

# Kahn'\''s algorithm with deterministic ordering.
indeg = {n: len(deps[n]) for n in publishable}
ready = sorted([n for n, d in indeg.items() if d == 0])
out = []
while ready:
    n = ready.pop(0)
    out.append(n)
    for m in sorted(rdeps[n]):
        indeg[m] -= 1
        if indeg[m] == 0:
            ready.append(m)
    ready.sort()

if len(out) != len(publishable):
    leftover = [n for n in publishable if n not in out]
    sys.stderr.write(
        "publish-order.sh: cyclic dependency detected among: "
        + ", ".join(sorted(leftover)) + "\n"
    )
    sys.exit(2)

for n in out:
    print(n)
'
