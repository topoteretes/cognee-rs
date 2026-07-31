#!/usr/bin/env bash
# clean_all.sh — reclaim disk space by cleaning EVERY Cargo workspace in the repo.
#
# Why this exists: `cargo clean` at the repo root only empties the root
# workspace's target/. The bindings live in *separate* Cargo workspaces with
# their own target/ dirs, so they survive a root clean — and they are usually
# the biggest offenders (the Neon target/ alone regularly passes 20 GB):
#
#   Cargo.toml                      root workspace (crates/, examples/, python/)
#   capi/Cargo.toml                 C API workspace          (decision D10)
#   ts/cognee-ts-neon/Cargo.toml    Neon cdylib, standalone crate
#   java/cognee-java-jni/Cargo.toml JNI cdylib, standalone crate
#
# Usage:
#   bash scripts/clean_all.sh                   # cargo clean every workspace above
#   bash scripts/clean_all.sh --all             # ...plus non-Cargo build outputs
#   bash scripts/clean_all.sh --dry-run --all   # report sizes, delete nothing
#
# Flags:
#   --all              also remove non-Cargo build outputs: ts/node_modules,
#                      ts/lib, the Neon .node files (incl. platform-packages/),
#                      java/target, capi/build*, ios/.build, python/dist,
#                      *.egg-info and __pycache__ dirs.
#   --include-models   also delete target/models. Kept by default: it holds
#                      downloaded ONNX/tokenizer files (the default
#                      EMBEDDING_MODEL_PATH) that no check script re-downloads.
#   --xcframework      also delete capi/CogneeSDK.xcframework. Not in --all:
#                      rebuilding it costs 20–30 min via
#                      capi/scripts/build_xcframework.sh, and ios/Package.swift
#                      cannot resolve without it.
#   --docker           also remove the Docker images this repo builds (see
#                      DOCKER_IMAGES below). Not in --all: rebuilding the e2e
#                      image compiles both SDKs from scratch. A no-op when
#                      docker is absent or the daemon is down.
#   -n, --dry-run      report what would be freed, delete nothing.
#
# Not touched: local databases and data stores (cognee.db, .data_storage/,
# .cognee_system/) — they can hold a developer's own knowledge graph.
#
# Docker: --docker removes only the images THIS repo builds. The daemon is
# shared, and a developer machine typically also holds unrelated images (kind
# node images, other services' local tags) — so `docker system prune` is never
# run from here, and neither are dangling images, which usually belong to
# whichever other build produced them. The overall reclaimable total is printed
# for information at the end of every run; acting on it is your call.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Absolute path to this file: the header block below is re-read for --help,
# which must keep working after the `cd "$REPO_ROOT"` a few lines down (a
# relative $BASH_SOURCE would no longer resolve from the new cwd).
SELF="$SCRIPT_DIR/$(basename "${BASH_SOURCE[0]}")"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$REPO_ROOT"

# Cargo workspaces / standalone crates, each with its own target/ dir.
CARGO_MANIFESTS=(
    "Cargo.toml"
    "capi/Cargo.toml"
    "ts/cognee-ts-neon/Cargo.toml"
    "java/cognee-java-jni/Cargo.toml"
)

# Non-Cargo build outputs, removed only with --all. Every entry is gitignored
# and reproducible from a check script or an npm/maven/maturin build. Globs are
# expanded at array-definition time; unmatched ones stay literal and are
# filtered by the `-e` test in the removal loop.
ALL_ARTIFACTS=(
    "ts/node_modules"                             # npm deps (ts/scripts/check.sh reinstalls)
    "ts/lib"                                      # tsc output
    "ts/cognee_ts_neon.node"                      # copied Neon cdylib (~470 MB)
    "ts/platform-packages/"*"/cognee_ts_neon.node" # prebuilt per-platform cdylibs (~240 MB each)
    "java/target"                                 # Maven output
    "capi/build"                                  # cmake/example build dir
    "capi/build-"*                                # per-platform variants
    "ios/.build"                                  # SwiftPM (re-resolves on next build)
    "python/dist"                                 # maturin/pip wheels
    "python/"*.egg-info
)

# target/models holds downloaded embedding models: the default
# EMBEDDING_MODEL_PATH / EMBEDDING_TOKENIZER_PATH (docs/configuration.md) and
# the staging source for scripts/android-build-and-deploy.sh. `cargo clean`
# removes ./target wholesale and nothing re-downloads them, so stash the dir
# across the root clean unless --include-models is given.
MODELS_DIR="target/models"
MODELS_STASH=".tmp/clean_all-models"   # .tmp/ is gitignored and on the same fs

# Docker images built by this repo, removed only with --docker. Matched by
# repository name, so every tag of each is removed. Keep in sync with the
# `image:` keys of services that also carry a `build:` section in
# e2e-cross-sdk/docker-compose.yml — upstream base images pulled by that file
# (python:3.11-slim and friends) are deliberately absent: they are shared with
# everything else on the machine and cost a re-pull, not a rebuild.
DOCKER_IMAGES=(
    "cognee-e2e-cross-sdk"   # 3-stage Rust+Python e2e harness (~5 GB)
)

ALL=0
DRY_RUN=0
INCLUDE_MODELS=0
XCFRAMEWORK=0
DOCKER=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --all) ALL=1 ;;
        --include-models) INCLUDE_MODELS=1 ;;
        --xcframework) XCFRAMEWORK=1 ;;
        --docker) DOCKER=1 ;;
        -n | --dry-run) DRY_RUN=1 ;;
        -h | --help)
            # Print the header comment block (everything after the shebang up to
            # the first line of code).
            awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$SELF"
            exit 0
            ;;
        *)
            echo "error: unknown argument '$1' (try --help)" >&2
            exit 2
            ;;
    esac
    shift
done

# size_kb PATH — apparent disk usage in KiB; 0 when the path is missing or
# unmeasurable. `du` exits non-zero when any entry underneath is unreadable or
# vanishes mid-walk (a concurrent cargo build is enough); under `set -e` with
# `pipefail` that status would propagate out of the command substitution and
# abort the script before anything is cleaned, so it is swallowed here.
size_kb() {
    [[ -e "$1" ]] || {
        echo 0
        return
    }
    local kb
    kb=$(du -sk "$1" 2>/dev/null | awk 'NR == 1 { print $1 }') || kb=""
    echo "${kb:-0}"
}

# human KIB — format a KiB count as KiB/MiB/GiB/TiB. Negative inputs (possible
# when a `du` failure makes one measurement read as 0) clamp to zero rather than
# printing a nonsensical "-2052 KiB".
human() {
    awk -v k="$1" 'BEGIN {
        if (k < 0) k = 0
        split("KiB MiB GiB TiB", u, " ")
        i = 1
        while (k >= 1024 && i < 4) { k /= 1024; i++ }
        printf (i == 1 ? "%d %s\n" : "%.1f %s\n"), k, u[i]
    }'
}

# docker_ok — true when docker is installed AND its daemon answers. Probed once
# and memoized: `docker` blocks for several seconds before failing when the
# daemon is down, and this is consulted from more than one place. Every docker
# use in this script is gated on it, so a machine without docker (or with it
# stopped) runs exactly as before.
DOCKER_OK=""
# si_to_kb — parse one or more docker size strings ("5.26GB", "621.4MB",
# "25.03GB (41%)") from stdin and print their sum in KiB. Docker prints SI
# units (1 GB = 10^9 B) while `human` and every other figure here is binary, so
# this converts through bytes rather than treating GB as GiB (~7 % overstated).
#
# Used for image sizes as well as the daemon-wide total: `docker image inspect`
# reports the *compressed* content size (1.16 GiB where `docker images` says
# 5.26 GB), and the on-disk figure is the one worth reporting.
si_to_kb() {
    awk '{
        v = $1
        sub(/[A-Za-z]+$/, "", v)
        unit = substr($1, length(v) + 1)
        mult = (unit == "TB") ? 1e12 : \
               (unit == "GB") ? 1e9 : \
               (unit == "MB") ? 1e6 : \
               (unit == "kB" || unit == "KB") ? 1e3 : \
               (unit == "B") ? 1 : 0
        total += v * mult
    } END { printf "%d", total / 1024 }'
}

docker_ok() {
    if [[ -z $DOCKER_OK ]]; then
        if command -v docker > /dev/null 2>&1 && docker system df > /dev/null 2>&1; then
            DOCKER_OK=yes
        else
            DOCKER_OK=no
        fi
    fi
    [[ $DOCKER_OK == yes ]]
}

# restore_models — move a stashed target/models back into place. Runs from the
# EXIT trap so an interrupted clean cannot lose the download.
restore_models() {
    [[ -d $MODELS_STASH ]] || return 0
    mkdir -p target
    rm -rf -- "$MODELS_DIR"
    mv -- "$MODELS_STASH" "$MODELS_DIR"
}
trap restore_models EXIT
# Recover a stash left behind by a previous run that was killed outright
# (SIGKILL skips the trap above).
restore_models

freed_kb=0

echo "================================================================"
echo "=== Cargo workspaces ==="
echo "================================================================"

models_kb=$(size_kb "$MODELS_DIR")
if [[ $INCLUDE_MODELS -eq 0 && $models_kb -gt 0 ]]; then
    printf 'KEEP   %-34s %s  (--include-models to drop)\n' \
        "$MODELS_DIR" "$(human "$models_kb")"
fi

for manifest in "${CARGO_MANIFESTS[@]}"; do
    if [[ ! -f "$manifest" ]]; then
        echo "SKIP  $manifest (not found)"
        continue
    fi
    # Report the conventional target/ next to the manifest. A caller-set
    # CARGO_TARGET_DIR redirects the real output elsewhere; `cargo clean`
    # still honors it, only the size below reads as 0.
    target_dir="$(dirname "$manifest")/target"
    before=$(size_kb "$target_dir")

    # Only the root workspace's target/ contains the model cache.
    keep_models=0
    if [[ "$manifest" == "Cargo.toml" && $INCLUDE_MODELS -eq 0 && -d $MODELS_DIR ]]; then
        keep_models=1
    fi

    if [[ $DRY_RUN -eq 1 ]]; then
        would=$before
        if [[ $keep_models -eq 1 ]]; then
            # Clamped: an unmeasurable target/ reads as 0, which would otherwise
            # make the subtraction negative.
            would=$((before > models_kb ? before - models_kb : 0))
        fi
        printf 'WOULD CLEAN  %-34s %s\n' "$manifest" "$(human "$would")"
        freed_kb=$((freed_kb + would))
        continue
    fi

    printf 'CLEAN  %-34s %s' "$manifest" "$(human "$before")"
    if [[ $keep_models -eq 1 ]]; then
        mkdir -p "$(dirname "$MODELS_STASH")"
        rm -rf -- "$MODELS_STASH"
        mv -- "$MODELS_DIR" "$MODELS_STASH"
    fi
    cargo clean --manifest-path "$manifest" --quiet
    restore_models
    after=$(size_kb "$target_dir")
    freed_kb=$((freed_kb + (before > after ? before - after : 0)))
    echo " -> $(human "$after")"
done

# Assemble the non-Cargo removal list from the flags that were passed.
# Written as `if` blocks, not `[[ … ]] && …`: the latter evaluates to 1 when the
# flag is off, and a false `&&` list as a top-level command aborts under `set -e`.
EXTRAS=()
if [[ $ALL -eq 1 ]]; then
    EXTRAS+=("${ALL_ARTIFACTS[@]}")
fi
if [[ $XCFRAMEWORK -eq 1 ]]; then
    EXTRAS+=("capi/CogneeSDK.xcframework")
fi

if [[ ${#EXTRAS[@]} -gt 0 ]]; then
    echo ""
    echo "================================================================"
    echo "=== Other build artifacts ==="
    echo "================================================================"
    for path in "${EXTRAS[@]}"; do
        # Unmatched globs stay literal in bash; skip those along with
        # artifacts that were simply never built.
        [[ -e "$path" ]] || continue
        sz=$(size_kb "$path")
        if [[ $DRY_RUN -eq 1 ]]; then
            printf 'WOULD REMOVE  %-34s %s\n' "$path" "$(human "$sz")"
        else
            printf 'REMOVE  %-34s %s\n' "$path" "$(human "$sz")"
            rm -rf -- "$path"
        fi
        freed_kb=$((freed_kb + sz))
    done
fi

if [[ $ALL -eq 1 ]]; then
    # Python bytecode caches are scattered; sweep them in one pass.
    pycache_kb=0
    while IFS= read -r d; do
        sz=$(size_kb "$d")
        pycache_kb=$((pycache_kb + ${sz:-0}))
        [[ $DRY_RUN -eq 1 ]] || rm -rf -- "$d"
    done < <(find python e2e-cross-sdk scripts -type d -name __pycache__ -prune 2>/dev/null)
    if [[ $pycache_kb -gt 0 ]]; then
        printf '%s  %-34s %s\n' \
            "$([[ $DRY_RUN -eq 1 ]] && echo 'WOULD REMOVE' || echo 'REMOVE      ')" \
            "__pycache__ dirs" "$(human "$pycache_kb")"
        freed_kb=$((freed_kb + pycache_kb))
    fi
fi

if [[ $DOCKER -eq 1 ]]; then
    echo ""
    echo "================================================================"
    echo "=== Docker images built by this repo ==="
    echo "================================================================"
    if ! docker_ok; then
        echo "SKIP  docker unavailable (not installed, or the daemon is down)"
    else
        # IDs already counted, so N tags of one image don't count N times over:
        # `docker images` reports the full image size against every tag, but the
        # layers are only freed once, when the last reference goes.
        counted_ids=" "
        for repo_name in "${DOCKER_IMAGES[@]}"; do
            # One line per tag: "<id> <repo:tag> <size>". --filter reference=
            # matches the repository across every tag it carries.
            #
            # Removal is by repo:tag, never by image ID. Several tags routinely
            # share one ID (a `docker tag` alias, a kind.local/ mirror, or an
            # unrelated image with identical content), and `docker rmi <id>`
            # deletes *every* tag pointing at it — which would take unrelated
            # projects' images down with ours. Removing the reference untags
            # just this one and frees the layers only when it was the last tag.
            while read -r img_id img_ref img_size; do
                [[ -n "$img_ref" ]] || continue
                img_kb=$(echo "$img_size" | si_to_kb)
                if [[ $DRY_RUN -eq 1 ]]; then
                    printf 'WOULD REMOVE  %-34s %s\n' "$img_ref" "$(human "$img_kb")"
                else
                    printf 'REMOVE  %-34s %s' "$img_ref" "$(human "$img_kb")"
                    # -f only overrides a stopped container still referencing
                    # the image; against a repo:tag it cannot widen the blast
                    # radius beyond that one reference.
                    if docker rmi -f "$img_ref" > /dev/null 2>&1; then
                        echo " -> gone"
                    else
                        echo " -> FAILED (in use by a running container?)"
                        continue
                    fi
                fi
                if [[ $counted_ids != *" $img_id "* ]]; then
                    counted_ids+="$img_id "
                    freed_kb=$((freed_kb + img_kb))
                fi
            done < <(docker images --filter "reference=$repo_name" \
                --format '{{.ID}} {{.Repository}}:{{.Tag}} {{.Size}}' 2> /dev/null)
        done
    fi
fi

echo ""
echo "================================================================"
if [[ $DRY_RUN -eq 1 ]]; then
    echo "=== Dry run: $(human "$freed_kb") would be freed ==="
else
    echo "=== Freed $(human "$freed_kb") ==="
fi
echo "================================================================"

# Whole-daemon total, for information only. Never pruned from here: it counts
# images, build cache and volumes belonging to every project on the machine,
# not just this one. --docker (above) removes this repo's images specifically.
if docker_ok; then
    # Four rows: images, containers, volumes, build cache.
    docker_reclaimable=$(docker system df --format '{{.Reclaimable}}' 2> /dev/null | si_to_kb)
    if [[ ${docker_reclaimable:-0} -gt 0 ]]; then
        echo ""
        printf 'Docker, whole daemon: %s reclaimable across ALL projects.\n' \
            "$(human "$docker_reclaimable")"
        if [[ $DOCKER -eq 0 ]]; then
            echo "  --docker removes just this repo's images; 'docker system prune'"
            echo "  would also drop other projects' images, volumes and build cache."
        else
            echo "  Mostly build cache and other projects' images — 'docker system"
            echo "  prune' reclaims it, but it is not scoped to this repo."
        fi
    fi
fi

if [[ $ALL -eq 0 ]]; then
    echo "Cargo targets only. --all also drops node_modules, the Neon .node"
    echo "files, java/target, capi/build*, ios/.build and python/dist."
    echo "See --help for the full list."
fi
