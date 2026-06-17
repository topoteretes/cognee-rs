# Cross-SDK E2E test image — builds both Python and Rust cognee CLIs
# and packages them with a pytest harness.
#
# Build context must be the cognee monorepo root (parent of cognee-rust/).
#
#   docker build -f cognee-rust/e2e-cross-sdk/Dockerfile .

# ─── Stage 1: Rust CLI builder ──────────────────────────────────────────────
# Use Debian Trixie for glibc 2.41 (ort-sys/ONNX Runtime requires glibc ≥ 2.38)
FROM debian:trixie-slim AS rust-builder

# Install Rust toolchain + system deps for qdrant/ort native builds
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl ca-certificates gcc g++ make ccache \
    cmake clang libssl-dev pkg-config protobuf-compiler libprotobuf-dev \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

# ccache for lbug's bundled Ladybug C++ build (~1000 compiler calls per fresh
# OUT_DIR; cargo relocates the OUT_DIR whenever lbug's build-dep closure
# shifts in the resolved graph — see docs/build/lbug-rebuilds.md). The repo's
# .cargo/config.toml + scripts/ccache-launcher.sh are not copied into this
# image, so wire the CMake launcher directly; ccache is installed above.
#   CCACHE_NOHASHDIR  — the release profile has debug=true; without it ccache
#                       hashes the cwd (the shifting OUT_DIR) for -g builds.
#   CCACHE_BASEDIR    — normalizes -I<OUT_DIR> paths for generated headers.
#   COMPILERCHECK     — apt layer rebuilds change compiler mtimes; hash
#                       compiler content instead.
ENV CCACHE_DIR=/ccache \
    CCACHE_BASEDIR=/build/cognee-rust \
    CCACHE_NOHASHDIR=true \
    CCACHE_COMPILERCHECK=content \
    CMAKE_C_COMPILER_LAUNCHER=ccache \
    CMAKE_CXX_COMPILER_LAUNCHER=ccache

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain 1.93.0 --profile minimal

WORKDIR /build/cognee-rust

# Copy workspace
COPY cognee-rust/Cargo.toml cognee-rust/Cargo.lock ./
COPY cognee-rust/crates/ crates/
COPY cognee-rust/examples/ examples/
COPY cognee-rust/capi/ capi/
COPY cognee-rust/python/ python/
# Harness-only crate that drives cognee_lib::telemetry::send_telemetry
# directly. Source lives under e2e-cross-sdk/bin/telemetry_emit.rs;
# manifest lives under e2e-cross-sdk/telemetry-emit/Cargo.toml. Both
# are needed at workspace resolution time because the workspace root
# Cargo.toml lists `e2e-cross-sdk/telemetry-emit` as a member.
COPY cognee-rust/e2e-cross-sdk/telemetry-emit/ e2e-cross-sdk/telemetry-emit/
COPY cognee-rust/e2e-cross-sdk/bin/telemetry_emit.rs e2e-cross-sdk/bin/telemetry_emit.rs

# Build the CLI in release mode; cache cargo registry and target dir
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/cognee-rust/target \
    --mount=type=cache,target=/ccache \
    cargo build --release --package cognee-cli \
    && cp target/release/cognee-cli /usr/local/bin/cognee-cli-rust

# Build the HTTP server binary in release mode (feature-gated `bin`)
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/cognee-rust/target \
    --mount=type=cache,target=/ccache \
    cargo build --release --package cognee-http-server --features bin \
    && cp target/release/cognee-http-server /usr/local/bin/cognee-http-server

# Build the harness-only telemetry emit binary used by the cross-SDK
# parity test (docs/telemetry/02/10-cross-sdk-parity.md).
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/cognee-rust/target \
    --mount=type=cache,target=/ccache \
    cargo build --release --package cognee-telemetry-emit \
    && cp target/release/cognee-telemetry-emit /usr/local/bin/cognee-telemetry-emit

# Download ONNX embedding models (needed by Rust CLI for cognify)
RUN mkdir -p /opt/models \
    && curl -fL "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/onnx/model_quantized.onnx" \
       -o /opt/models/BGE-Small-v1.5-model_quantized.onnx \
    && curl -fL "https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main/tokenizer.json" \
       -o /opt/models/bge-small-tokenizer.json


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

# Install deps without the project itself
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --frozen --no-install-project --no-dev --no-editable

# Copy project source and install. cognee declares
# `[tool.hatch.build.targets.wheel] packages = ["cognee", "distributed"]`
# in pyproject.toml.
COPY cognee/cognee /build/cognee
COPY cognee/distributed /build/distributed
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --frozen --no-dev --no-editable

# kuzu is an optional cognee backend not pulled in by the default `uv sync`,
# but cognee's alembic migration `b9274c27a25a_kuzu_11_migration.py` does a
# top-level `import kuzu`. alembic loads every version module to build the
# revision graph (even for `stamp head`), so a missing kuzu aborts the harness
# startup with ModuleNotFoundError. Install it so the module is importable; the
# migration body never runs under `stamp`, so any importable version suffices.
RUN --mount=type=cache,target=/root/.cache/uv \
    uv pip install --python /build/.venv kuzu


# ─── Stage 3: Test harness ──────────────────────────────────────────────────
# Must use Trixie to match glibc of the Rust binary (2.41)
FROM python:3.12-slim-trixie AS harness

RUN apt-get update && apt-get install -y --no-install-recommends \
    sqlite3 \
    && rm -rf /var/lib/apt/lists/*

# Copy Rust CLI binary
COPY --from=rust-builder /usr/local/bin/cognee-cli-rust /usr/local/bin/cognee-cli-rust

# Copy Rust HTTP server binary
COPY --from=rust-builder /usr/local/bin/cognee-http-server /usr/local/bin/cognee-http-server

# Copy harness-only telemetry emit binary (cross-SDK parity test)
COPY --from=rust-builder /usr/local/bin/cognee-telemetry-emit /usr/local/bin/cognee-telemetry-emit

# Copy ONNX models
COPY --from=rust-builder /opt/models/ /opt/models/

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

# Copy dual-server entrypoint scripts
COPY cognee-rust/e2e-cross-sdk/bin/ /usr/local/bin/

# Make scripts executable
RUN chmod +x /usr/local/bin/start_servers.sh /harness/wait_for_health.sh

WORKDIR /workspace

CMD ["pytest", "-xvs", "/harness/"]
