#!/usr/bin/env bash
# Set EVERY cognee-rust version location to a single X.Y.Z.
#
# cognee-rust ships from four separate manifests plus a set of npm package.json
# files, and they must all carry the same version at release time. This script
# is the single source of truth for that bump — the release-open workflow
# (.github/workflows/release-open.yml) calls it, and it is safe to run by
# hand for a local dry run.
#
# Locations bumped:
#   1. Root workspace  — Cargo.toml [workspace.package] version, and every
#      internal `cognee-* = { path = ..., version = "X" }` requirement across
#      crates/*/Cargo.toml. `cargo set-version --workspace` does both. Members
#      inherit via `version.workspace = true`; `python/` (cognee-python) rides
#      along the same way and its pyproject.toml is `dynamic = ["version"]`, so
#      the wheel version follows automatically — no separate Python bump.
#   2. capi workspace  — capi/Cargo.toml [workspace.package] version.
#   3. ts-neon crate   — ts/cognee-ts-neon/Cargo.toml [package] version.
#   4. TS npm packages — ts/package.json version, its @cognee/neon-*
#      optionalDependencies pins, and each ts/platform-packages/*/package.json.
#
# Requires cargo-edit (`cargo install cargo-edit`) for `cargo set-version` and
# node for the JSON edits.
#
# Usage:
#   scripts/release/set-version.sh 0.1.3
#   scripts/release/set-version.sh --help
set -euo pipefail

usage() {
  # Print the header comment block (portable — no GNU-only `head -n -1`):
  # from line 2, strip a leading "# ", stop before the `set -euo pipefail` line.
  awk 'NR>=2 { if ($0 == "set -euo pipefail") exit; sub(/^# ?/, ""); print }' "$0"
}

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  usage
  exit 0
fi

VERSION="${1:-}"
if [[ -z "$VERSION" ]]; then
  echo "error: version argument required (e.g. 0.1.3)" >&2
  usage >&2
  exit 2
fi
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: '$VERSION' is not a bare X.Y.Z semver (no leading 'v', no pre-release suffix)" >&2
  exit 2
fi

if ! cargo set-version --help >/dev/null 2>&1; then
  echo "error: 'cargo set-version' not found — install cargo-edit: cargo install cargo-edit" >&2
  exit 3
fi
if ! command -v node >/dev/null 2>&1; then
  echo "error: node not found — required to bump the TS package.json files" >&2
  exit 3
fi

cd "$(dirname "$0")/../.."
ROOT="$(pwd)"

echo "== Setting cognee-rust version to ${VERSION} =="

# 1. Root workspace (+ internal cognee-* dep requirements).
echo "-- root workspace (Cargo.toml + crates/*/Cargo.toml dep reqs)"
cargo set-version --workspace "$VERSION"

# 2. capi standalone workspace.
echo "-- capi/Cargo.toml"
cargo set-version --manifest-path "$ROOT/capi/Cargo.toml" "$VERSION"

# 3. ts-neon standalone crate.
echo "-- ts/cognee-ts-neon/Cargo.toml"
cargo set-version --manifest-path "$ROOT/ts/cognee-ts-neon/Cargo.toml" "$VERSION"

# 4. TS npm packages (main package version + @cognee/neon-* optionalDependencies
#    pins + each platform sub-package version).
echo "-- ts/package.json + ts/platform-packages/*/package.json"
node - "$VERSION" "$ROOT" <<'NODE'
const fs = require("fs");
const path = require("path");
const [version, root] = process.argv.slice(2);

function edit(file, mutate) {
  const abs = path.join(root, file);
  const json = JSON.parse(fs.readFileSync(abs, "utf8"));
  mutate(json);
  fs.writeFileSync(abs, JSON.stringify(json, null, 2) + "\n");
  console.log(`   ${file} -> ${version}`);
}

// Main package: own version + pin every @cognee/neon-* optional dep.
edit("ts/package.json", (pkg) => {
  pkg.version = version;
  const opt = pkg.optionalDependencies || {};
  for (const name of Object.keys(opt)) {
    if (name.startsWith("@cognee/neon-")) opt[name] = version;
  }
});

// Each platform package's own version.
const platDir = path.join(root, "ts/platform-packages");
for (const entry of fs.readdirSync(platDir, { withFileTypes: true })) {
  if (!entry.isDirectory()) continue;
  const rel = path.join("ts/platform-packages", entry.name, "package.json");
  if (!fs.existsSync(path.join(root, rel))) continue;
  edit(rel, (pkg) => { pkg.version = version; });
}
NODE

echo "== Done. Review with: git diff --stat =="
