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
    curl ca-certificates gcc g++ make \
    cmake clang libssl-dev pkg-config protobuf-compiler libprotobuf-dev \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain 1.93.0 --profile minimal

WORKDIR /build/cognee-rust

# Copy workspace
COPY cognee-rust/Cargo.toml cognee-rust/Cargo.lock ./
COPY cognee-rust/crates/ crates/
COPY cognee-rust/examples/ examples/
COPY cognee-rust/capi/ capi/
COPY cognee-rust/python/ python/

# Build the CLI in release mode; cache cargo registry and target dir
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/cognee-rust/target \
    cargo build --release --package cognee-cli \
    && cp target/release/cognee-cli /usr/local/bin/cognee-cli-rust

# Build the HTTP server binary in release mode (feature-gated `bin`)
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/cognee-rust/target \
    cargo build --release --package cognee-http-server --features bin \
    && cp target/release/cognee-http-server /usr/local/bin/cognee-http-server

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

# Copy project source and install. cognee dev declares
# `[tool.hatch.build.targets.wheel] packages = ["cognee", "distributed", "kuzu"]`
# in pyproject.toml; the `kuzu/` directory is a compat shim re-exporting
# `ladybug` so legacy `import kuzu` keeps working after the engine swap.
# Without it the wheel uv builds is missing the kuzu package and Alembic's
# top-level `import kuzu` in b9274c27a25a_kuzu_11_migration.py fails.
COPY cognee/cognee /build/cognee
COPY cognee/distributed /build/distributed
COPY cognee/kuzu /build/kuzu
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --frozen --no-dev --no-editable


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
