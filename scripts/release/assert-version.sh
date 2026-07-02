#!/usr/bin/env bash
# Assert that EVERY version location is exactly <X.Y.Z>.
#
# Guards against a `set-version.sh` run that missed a manifest — if that slipped
# through, the crates.io publish loop would hit "already uploaded" for the
# un-bumped crate and silently SKIP it (code-review finding #7), shipping a
# release where one crate lags the tag. This script is the gate: it runs on the
# release PR (release-verify.yml) and can be run locally after set-version.sh.
#
# Checks the PACKAGE version of every crate in all three Cargo workspaces (root,
# capi, ts-neon) and every npm package.json (+ its @cognee/neon-*
# optionalDependencies pins). This is the one thing cargo cannot catch for us: a
# stale intra-workspace *dependency requirement* already makes `cargo update` /
# `cargo publish --dry-run` fail to resolve (a bumped crate no longer satisfies
# the old `^x.y.z`), so ci.yml + publish-dry-run.yml cover that — this script
# owns the "a package was left at the old version" case that would otherwise be
# silently SKIPped by the publish loop.
#
# Usage: scripts/release/assert-version.sh 0.1.3
set -euo pipefail

VERSION="${1:-}"
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: expected a bare X.Y.Z version argument, got '${VERSION:-}'" >&2
  exit 2
fi

cd "$(dirname "$0")/../.."
ROOT="$(pwd)"
fails=0
fail() { echo "  MISMATCH: $*" >&2; fails=$((fails + 1)); }

# --- Cargo manifests: package version of every workspace member ----------------
# Uses cargo metadata so inherited (version.workspace) members resolve correctly.
# Covers all three workspaces: root (all cognee-* crates), capi, and ts-neon.
python3 - "$VERSION" <<'PY' || fails=$((fails + 1))
import json, subprocess, sys
version = sys.argv[1]
bad = []
for manifest in ("Cargo.toml", "capi/Cargo.toml", "ts/cognee-ts-neon/Cargo.toml"):
    meta = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--format-version", "1",
         "--manifest-path", manifest]))
    for p in meta["packages"]:
        if p["version"] != version:
            bad.append(f'{p["name"]} ({manifest}) = {p["version"]}')
if bad:
    sys.stderr.write("  MISMATCH (cargo package versions):\n")
    for b in sorted(set(bad)):
        sys.stderr.write(f"    - {b}\n")
    sys.exit(1)
print("  cargo packages: all at", version)
PY

# --- npm package.json files ----------------------------------------------------
node - "$VERSION" "$ROOT" <<'NODE' || fails=$((fails + 1))
const fs = require("fs"), path = require("path");
const [version, root] = process.argv.slice(2);
const bad = [];
function check(rel) {
  const p = JSON.parse(fs.readFileSync(path.join(root, rel), "utf8"));
  if (p.version !== version) bad.push(`${rel} version=${p.version}`);
  for (const [name, ver] of Object.entries(p.optionalDependencies || {})) {
    if (name.startsWith("@cognee/neon-") && ver !== version)
      bad.push(`${rel} optionalDependencies[${name}]=${ver}`);
  }
}
check("ts/package.json");
const platDir = path.join(root, "ts/platform-packages");
for (const e of fs.readdirSync(platDir, { withFileTypes: true })) {
  const rel = path.join("ts/platform-packages", e.name, "package.json");
  if (e.isDirectory() && fs.existsSync(path.join(root, rel))) check(rel);
}
if (bad.length) {
  process.stderr.write("  MISMATCH (npm):\n");
  bad.forEach((b) => process.stderr.write(`    - ${b}\n`));
  process.exit(1);
}
console.log("  npm packages: all at", version);
NODE

if [ "$fails" -gt 0 ]; then
  echo "assert-version: FAILED — ${fails} location group(s) not at ${VERSION}" >&2
  exit 1
fi
echo "assert-version: OK — every version location is ${VERSION}"
