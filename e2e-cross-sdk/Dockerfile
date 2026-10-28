# Cross-SDK E2E test image — builds both Python and Rust cognee CLIs
# and packages them with a pytest harness.
#
# Build context must be the cognee monorepo root (parent of cognee-rust/).
#
#   docker build -f cognee-rust/e2e-cross-sdk/Dockerfile .

# The harness runtime image. It is also the base of the `rust-build-env` stage
# the Rust binaries are compiled in, which is what makes them run here: they
# are linked against this exact glibc and libstdc++, by construction. Change it
# in this one place; .github/workflows/http-parity.yml reads it from this line.
ARG HARNESS_BASE=python:3.12-slim-trixie

# ─── Stage 1: Rust build environment (binaries are built OUTSIDE the image) ─
#
# The Rust CLI and telemetry binaries used to be compiled by a builder stage in
# this file. That stage recompiled ~741 crates on every run (~34 min of a
# ~39 min job): its cargo target dir lived in a `--mount=type=cache`, which
# never survives a fresh runner, and BuildKit layer caching was measured
# producing ZERO reuse here even at an identical commit. So #133 moved the
# build onto the runner, where Swatinem/rust-cache persists the target dir.
#
# That made the binaries' glibc whatever the runner image ships, and on
# 2026-10-22 ubuntu-latest became 26.04 (glibc 2.43): the binaries then failed
# in this Trixie (glibc 2.41) image with `GLIBC_2.43 not found`.
#
# This stage keeps both properties. It is only the *toolchain environment* —
# HARNESS_BASE plus the C/C++ build deps — and holds no sources and no Rust
# toolchain. e2e-cross-sdk/build-rust-binaries.sh (run by CI and by hand alike)
# builds just this target, then runs `cargo build` in it with the checkout,
# rustup toolchain and cargo home bind-mounted at their host paths. The target dir therefore stays on the runner (rust-cache still works,
# and the absolute paths in its fingerprints match run to run), while the
# compile and link happen against the harness's own glibc and libstdc++.
#
# What this relies on in HARNESS_BASE: Debian-based (apt), and glibc >= 2.38,
# which ort-sys's prebuilt x86_64 libonnxruntime.a needs (__isoc23_strtol &
# co.).
#
# There is no OpenSSL to satisfy at runtime: reqwest is
# `default-features = false` with `rustls-tls`, and the binaries link no
# openssl. openssl-sys IS in the build graph, though — only as a
# build-dependency of ort-sys (its download script uses ureq + native-tls) —
# so the build env needs libssl-dev, which the ubuntu runner had preinstalled.
#
# To build the image outside CI, produce the binaries first, the same way CI
# does — from the cognee-rust repo root:
#   bash e2e-cross-sdk/build-rust-binaries.sh
# It leaves them in cognee-rust/dist/, where the COPYs below expect them (the
# image's build context is the monorepo root above), and checks they load in
# HARNESS_BASE. Do not substitute a host `cargo build --release`: it links
# against the host's glibc, and on any host newer than HARNESS_BASE the
# binaries fail to start in this image ("GLIBC_2.xx not found").
FROM ${HARNESS_BASE} AS rust-build-env

# build-essential: gcc/g++ + libc6-dev, i.e. the glibc and libstdc++ the
# binaries link against. cmake: lbug's bundled C++ tree. protobuf-compiler +
# libprotobuf-dev: prost (lance-encoding imports google/protobuf/*.proto,
# which only libprotobuf-dev ships). pkg-config + libssl-dev: ort-sys's
# build script (see the OpenSSL note above). git: cargo git dependencies.
# No mold: since Rust 1.90, x86_64-unknown-linux-gnu links with the
# toolchain's own rust-lld, so the system linker is not on the Rust link path.
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential cmake protobuf-compiler libprotobuf-dev pkg-config git \
    libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*


# ─── Stage 2: Python CLI builder ────────────────────────────────────────────
FROM ghcr.io/astral-sh/uv:python3.12-bookworm-slim AS python-builder

WORKDIR /build

RUN apt-get update && apt-get install -y --no-install-recommends \
    gcc g++ libpq-dev git curl cmake clang build-essential \
    && rm -rf /var/lib/apt/lists/*

ENV UV_LINK_MODE=copy

# Create a relocatable venv up-front so script shebangs use a sh-shim that
# resolves at runtime instead of the absolute /build/.venv/bin/python path.
# This lets the harness stage COPY the venv to /opt/python-venv with no
# shebang rewriting required (the previous sed-based fix only matched
# `python3` shebangs, missing `python` entries like uvicorn).
RUN uv venv --relocatable /build/.venv

# Copy dependency files first for layer caching
COPY cognee/pyproject.toml cognee/uv.lock cognee/README.md ./

# Install deps without the project itself.
#
# `--extra dlt`: older pins (up to 1.5.x) made dlt optional, and cognee
# registers `dlt_csv_loader` only when dlt is importable. Without it, Python
# falls back to the same `csv_loader` Rust uses, the two sides agree, and a
# real routing divergence — Python routes CSV through the DLT_SOURCE cognify
# route, Rust has no route concept at all — is masked by the harness's own
# environment rather than by the code under test. From the current pin on,
# dlt is a core dependency and `dlt` is an empty extra kept for
# compatibility, so the flag is a no-op there but stays valid.
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --frozen --no-install-project --no-dev --no-editable --extra dlt

# Copy project source and install. At the pinned SHA cognee declares
# `[tool.hatch.build.targets.wheel] packages = ["cognee", "cognee_db_workers", "kuzu"]`
# in pyproject.toml. Every package that is *imported at runtime* has to be
# copied here, or `uv sync` builds a wheel without it.
#
# `cognee_db_workers` is not optional: the Ladybug adapter does a top-level
# `import cognee_db_workers`, and cognee/__init__ reaches it through
# add -> ingest_data -> ... -> graph/config, so omitting it makes `import
# cognee` itself fail — which means every Python-CLI invocation in the harness
# dies before it does anything.
# (`kuzu` is listed too but is only a ladybug shim for `import kuzu`, which
# the PyPI install below already satisfies; `distributed` is copied because
# older pins packaged it and the directory is harmless when they do not.)
COPY cognee/cognee /build/cognee
COPY cognee/cognee_db_workers /build/cognee_db_workers
COPY cognee/distributed /build/distributed
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --frozen --no-dev --no-editable --extra dlt

# kuzu is an optional cognee backend not pulled in by the default `uv sync`,
# but cognee's alembic migration `b9274c27a25a_kuzu_11_migration.py` does a
# top-level `import kuzu`. alembic loads every version module to build the
# revision graph, so a missing kuzu aborts Python-side DB setup with
# ModuleNotFoundError. Install it so the module is importable; the migration
# body never runs here, so any importable version suffices.
RUN --mount=type=cache,target=/root/.cache/uv \
    uv pip install --python /build/.venv kuzu


# ─── Stage 3: Test harness ──────────────────────────────────────────────────
# Same base as `rust-build-env`, so the prebuilt binaries COPYd below were
# linked against this image's glibc and libstdc++ — see the stage-1 note.
FROM ${HARNESS_BASE} AS harness

# curl/ca-certificates are for the ONNX model fetch below.
RUN apt-get update && apt-get install -y --no-install-recommends \
    sqlite3 curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Prebuilt Rust binaries. `cognee-cli` is renamed on the way in so it does not
# collide with the Python CLI symlinked below as cognee-cli-python.
COPY cognee-rust/dist/cognee-cli /usr/local/bin/cognee-cli-rust
COPY cognee-rust/dist/cognee-telemetry-emit /usr/local/bin/cognee-telemetry-emit

# ONNX embedding models (needed by the Rust CLI for cognify). ~30 MB, a few
# seconds.
# --retry: these are build-time network fetches, so a transient DNS hiccup or
# 5xx from the CDN would otherwise fail the whole image build.
RUN mkdir -p /opt/models \
    && curl -fL --retry 3 --retry-delay 2 --retry-connrefused \
       "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/onnx/model_quantized.onnx" \
       -o /opt/models/BGE-Small-v1.5-model_quantized.onnx \
    && curl -fL --retry 3 --retry-delay 2 --retry-connrefused \
       "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/tokenizer.json" \
       -o /opt/models/bge-small-tokenizer.json

# Copy Python virtualenv. The venv was created with `uv venv --relocatable`
# in stage 2, so script shebangs already use a sh-shim that resolves the
# python interpreter relative to the script's own location — no shebang
# rewriting needed.
COPY --from=python-builder /build/.venv /opt/python-venv
ENV PATH="/opt/python-venv/bin:$PATH"
RUN ln -sf /opt/python-venv/bin/cognee-cli /usr/local/bin/cognee-cli-python

# Install harness dependencies
COPY cognee-rust/e2e-cross-sdk/harness/requirements.txt /tmp/requirements.txt
RUN pip install --no-cache-dir -r /tmp/requirements.txt && rm /tmp/requirements.txt

# Copy test data
COPY cognee-rust/e2e-cross-sdk/test_data/ /test_data/

# Copy harness code
COPY cognee-rust/e2e-cross-sdk/harness/ /harness/

# NOTE: e2e-cross-sdk/bin/ is deliberately NOT copied. It held the dual-server
# entrypoint (start_servers.sh) for the HTTP parity lane, which moved out of
# this repository along with cognee-http-server. What is left in that directory
# is telemetry_emit.rs (a crate source file) and telemetry_proxy.py, which the
# telemetry-proxy compose service bind-mounts from the host into its own
# python:3.11-slim image — neither belongs in this image.

WORKDIR /workspace

CMD ["pytest", "-xvs", "/harness/"]
