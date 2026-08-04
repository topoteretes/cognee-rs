#!/usr/bin/env bash
# assert-bindings-layout.sh — guard the path contract between the bindings/
# workspace and the two prebuild workflows.
#
# Why this exists, and why it is assertions rather than a build:
#
# ts-prebuild.yml and java-prebuild.yml trigger only on `v*` tags and
# workflow_dispatch — never on pull requests or pushes to main. So the paths
# they depend on are exercised for the first time during a release. When the
# ts and java cdylibs moved into the bindings/ workspace (#125) every one of
# those paths changed, and nothing in normal CI would have noticed if one had
# been missed.
#
# Running the workflows on PRs is not an option: each leg is up to 150 minutes,
# the aarch64 leg builds an emulated container, and their workflow_dispatch
# path publishes to npm and Maven.
#
# But everything #125 changed is *static*. The cross container, its toolchain
# and its flags are untouched; only where cross looks for its config moved.
# Path drift is the failure mode, and path drift is checkable in milliseconds.
# That is what this asserts. It deliberately does NOT prove the cross build
# compiles — only a real dispatch does that, and it is unchanged by the move.
#
# Runs in well under a second, so it is wired into the existing lint job and
# scripts/check_all.sh rather than a job of its own.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$REPO_ROOT"

WORKFLOWS=(.github/workflows/ts-prebuild.yml .github/workflows/java-prebuild.yml)
fails=0
note() { printf '  %-6s %s\n' "$1" "$2"; }
fail() {
    note "FAIL" "$1"
    fails=$((fails + 1))
}

echo "================================================================"
echo "=== bindings/ layout contract (prebuild workflows) ==="
echo "================================================================"

# 1. Cargo's own view. cross-rs resolves Cross.toml as
#    metadata.workspace_root.join("Cross.toml"), so the workspace root is the
#    anchor everything else hangs off — take it from cargo, not from a literal.
WS_ROOT=$(cargo metadata --manifest-path bindings/Cargo.toml --no-deps --format-version 1 \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["workspace_root"])')
if [[ "$WS_ROOT" == "$REPO_ROOT/bindings" ]]; then
    note "ok" "workspace root is bindings/"
else
    fail "workspace root is '$WS_ROOT', expected '$REPO_ROOT/bindings'"
fi

# 2. Cross.toml must sit at that root. A copy left in a member directory is
#    silently ignored by cross — the build then runs without the custom
#    Dockerfile and fails deep inside the container on a missing toolchain.
if [[ -f "$WS_ROOT/Cross.toml" ]]; then
    note "ok" "Cross.toml at the workspace root"
else
    fail "no Cross.toml at $WS_ROOT (cross reads workspace_root/Cross.toml)"
fi
for stale in ts/cognee-ts-neon/Cross.toml java/cognee-java-jni/Cross.toml; do
    [[ -f "$stale" ]] && fail "stale $stale would be ignored by cross — delete it"
done

# 3. Every dockerfile= in Cross.toml resolves *relative to Cross.toml*. This is
#    the entry most likely to rot: the file moved one level up in #125, so the
#    "../" depth changed with it.
while IFS= read -r df; do
    if [[ -f "$WS_ROOT/$df" ]]; then
        note "ok" "dockerfile resolves: $df"
    else
        fail "dockerfile '$df' in Cross.toml does not resolve from $WS_ROOT"
    fi
done < <(grep -oE '^[[:space:]]*dockerfile[[:space:]]*=[[:space:]]*"[^"]+"' "$WS_ROOT/Cross.toml" \
    | sed 's/.*"\(.*\)"/\1/')

# 4. Cross.toml asks for the whole repo to be mounted via COGNEE_REPO_ROOT
#    (the members' path-deps live outside the workspace root). If a workflow
#    stops exporting it, cross mounts only the root and the build dies with
#    "failed to find a workspace root".
# The variable name is read out of Cross.toml rather than hardcoded, so
# renaming the volume there keeps the check honest. Matching the YAML
# assignment (`NAME:` at the start of a line) rather than any occurrence
# matters twice over: a passing mention in a comment must not satisfy it, and
# a substring match would accept a renamed `COGNEE_REPO_ROOT_SOMETHING` —
# which is exactly how an earlier version of this check silently passed a
# mutation test.
while IFS= read -r volvar; do
    for wf in "${WORKFLOWS[@]}"; do
        if grep -qE "^[[:space:]]*${volvar}:[[:space:]]" "$wf"; then
            note "ok" "$(basename "$wf") assigns $volvar"
        else
            fail "$(basename "$wf") does not assign $volvar, required by Cross.toml volumes"
        fi
    done
done < <(grep -oE '^[[:space:]]*volumes[[:space:]]*=[[:space:]]*\[[^]]*\]' "$WS_ROOT/Cross.toml" \
    | grep -oE '"[A-Z_][A-Z0-9_]*"' | tr -d '"' | sort -u)

# 5. Each `cargo|cross ... -p <name>` in the workflows must name a real member,
#    and must run from the workspace root — cross treats the cwd as the project
#    and will not find the workspace from inside a member directory.
MEMBERS=$(cargo metadata --manifest-path bindings/Cargo.toml --no-deps --format-version 1 \
    | python3 -c 'import json,sys; print(" ".join(p["name"] for p in json.load(sys.stdin)["packages"]))')
for wf in "${WORKFLOWS[@]}"; do
    while IFS= read -r pkg; do
        if [[ " $MEMBERS " == *" $pkg "* ]]; then
            note "ok" "$(basename "$wf") -p $pkg is a member"
        else
            fail "$(basename "$wf") builds -p '$pkg', not a member of bindings/ ($MEMBERS)"
        fi
    done < <(grep -oE '(cargo|cross) build [^|]*-p [a-z0-9-]+' "$wf" | grep -oE '\-p [a-z0-9-]+$' | awk '{print $2}' | sort -u)

    if grep -qE '^[[:space:]]*working-directory:[[:space:]]*bindings[[:space:]]*$' "$wf"; then
        note "ok" "$(basename "$wf") runs from bindings/"
    else
        fail "$(basename "$wf") has no 'working-directory: bindings' step; cross needs the workspace root as cwd"
    fi
done

# 6. No workflow may still point at a pre-#125 target dir. These would not
#    error — they would silently cache or copy nothing, so the build "succeeds"
#    and publishes a stale or missing artifact.
for wf in "${WORKFLOWS[@]}"; do
    while IFS= read -r hit; do
        fail "$(basename "$wf") still references a pre-merge path: $hit"
    done < <(grep -oE '(ts/cognee-ts-neon|java/cognee-java-jni)/target[^ "]*' "$wf" | sort -u)
done

echo ""
if [[ $fails -eq 0 ]]; then
    echo "=== bindings layout OK ==="
else
    echo "=== $fails assertion(s) failed ==="
    echo "The prebuild workflows would break at release time. They do not run on"
    echo "PRs, so this script is the only thing between a bad path and a failed"
    echo "release — fix the paths rather than relaxing the check."
    exit 1
fi
