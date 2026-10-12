#!/usr/bin/env bash
# check_env_boundary.sh — ratchet for the configuration-boundary rule.
#
# The root clippy.toml bans direct process-env access (std::env::var & co.,
# dotenv::*, cognee_config_core::ProcessEnv::new). Clippy can be silenced with an
# `allow`, and an `allow` on a function silences every read inside it, so this
# script counts the direct-access *call sites* in production code per file and
# compares them with scripts/env_boundary_baseline.txt:
#
#   * a file with more call sites than its baseline (or a new file with any)
#     fails: new code must not read the process env directly — add a field to a
#     config struct and read it at the config boundary instead;
#   * a file with fewer call sites than its baseline also fails, asking you to
#     run `scripts/check_env_boundary.sh --update` so the lower count is locked in.
#
# Production code = *.rs under crates/ capi/ python/ ts/ java/ e2e-cross-sdk/,
# excluding tests/, examples/, benches/, crates/test-utils/, target/ and the
# bodies of `#[cfg(test)]` modules. Comment text is ignored.
#
# A genuinely new config boundary (or env-only-by-design read) is added by
# running `--update` in the same PR, which makes it an explicit, reviewable
# baseline change. See docs/configuration.md § "Configuration model".
#
# Usage: scripts/check_env_boundary.sh [--update]
set -euo pipefail

cd "$(dirname "$0")/.."

exec python3 - "$@" <<'PY'
import os
import re
import sys

ROOTS = ["crates", "capi", "python", "ts", "java", "e2e-cross-sdk"]
BASELINE = "scripts/env_boundary_baseline.txt"
SKIP_PARTS = {"tests", "examples", "benches", "target", "build", "node_modules"}
SKIP_PREFIXES = ("crates/test-utils/",)
SKIP_FILES = ("crates/vector/src/pgvector_index_tests.rs",)  # a #[cfg(test)] module file

ACCESS = re.compile(
    r"\benv::(?:var|var_os|vars|vars_os|set_var|remove_var)\s*\("
    r"|\bdotenv::\w+\s*\("
    r"|\bProcessEnv::new\s*\("
)
MOD = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{")


def code_of(line):
    # Drop string literals, then line comments, so neither can match.
    line = re.sub(r'"(?:\\.|[^"\\])*"', '""', line)
    return line.split("//", 1)[0]


def count_sites(path):
    lines = open(path, encoding="utf-8").read().split("\n")
    n = 0
    i = 0
    pending_cfg_test = False
    while i < len(lines):
        stripped = lines[i].strip()
        if re.search(r"cfg\((?:all\(|any\()?test\b", stripped):
            pending_cfg_test = True
        elif MOD.match(lines[i]) and pending_cfg_test:
            # Skip the whole #[cfg(test)] module body by brace depth.
            depth = 0
            while i < len(lines):
                c = code_of(lines[i])
                depth += c.count("{") - c.count("}")
                i += 1
                if depth <= 0:
                    break
            pending_cfg_test = False
            continue
        elif stripped.endswith((";", "}")) or (stripped.endswith("{") and not MOD.match(lines[i])):
            # Real code between `#[cfg(test)]` and a `mod` means the cfg applied
            # to something else. Attribute continuation lines (`clippy::x,`,
            # `reason = "...",`, `)]`) do not end like this, so a multi-line
            # attribute between the cfg and the module keeps it pending.
            pending_cfg_test = False
        n += len(ACCESS.findall(code_of(lines[i])))
        i += 1
    return n


def scan():
    counts = {}
    for root in ROOTS:
        if not os.path.isdir(root):
            continue
        for dirpath, dirnames, filenames in os.walk(root):
            dirnames[:] = [d for d in dirnames if d not in SKIP_PARTS and not d.startswith(".")]
            for name in filenames:
                if not name.endswith(".rs"):
                    continue
                path = os.path.join(dirpath, name)
                if path.startswith(SKIP_PREFIXES) or path in SKIP_FILES:
                    continue
                n = count_sites(path)
                if n:
                    counts[path] = n
    return counts


def load_baseline():
    base = {}
    if os.path.exists(BASELINE):
        for raw in open(BASELINE, encoding="utf-8"):
            raw = raw.strip()
            if not raw or raw.startswith("#"):
                continue
            n, path = raw.split(None, 1)
            base[path] = int(n)
    return base


def write_baseline(counts):
    with open(BASELINE, "w", encoding="utf-8") as f:
        f.write("# Direct process-env access sites in production code, per file.\n")
        f.write("# Maintained by scripts/check_env_boundary.sh --update; may only go down\n")
        f.write("# except for a deliberate, reviewed new config boundary.\n")
        for path in sorted(counts):
            f.write(f"{counts[path]} {path}\n")


counts = scan()
if sys.argv[1:] == ["--update"]:
    write_baseline(counts)
    print(f"wrote {BASELINE}: {sum(counts.values())} sites in {len(counts)} files")
    sys.exit(0)

base = load_baseline()
grew = sorted(p for p in counts if counts[p] > base.get(p, 0))
shrank = sorted(p for p in base if counts.get(p, 0) < base[p])
total, base_total = sum(counts.values()), sum(base.values())

if grew:
    print(f"error: direct process-env access grew ({total} sites, baseline {base_total}):", file=sys.stderr)
    for p in grew:
        print(f"  {p}: {counts[p]} (baseline {base.get(p, 0)})", file=sys.stderr)
    print("New code must not read the process environment directly. Add a field to a\n"
          "config struct and read it at the config boundary instead:\n"
          "  docs/configuration.md#adding-a-setting\n"
          "If this is a deliberate new config boundary, run\n"
          "  scripts/check_env_boundary.sh --update\n"
          "and commit the baseline change for review.", file=sys.stderr)
    sys.exit(1)
if shrank:
    print(f"error: direct process-env access shrank ({total} sites, baseline {base_total}):", file=sys.stderr)
    for p in shrank:
        print(f"  {p}: {counts.get(p, 0)} (baseline {base[p]})", file=sys.stderr)
    print("Nice. Lock it in: run scripts/check_env_boundary.sh --update and commit the baseline.",
          file=sys.stderr)
    sys.exit(1)
print(f"env boundary ok ({total} direct-access sites in {len(counts)} production files, matches baseline)")
PY
