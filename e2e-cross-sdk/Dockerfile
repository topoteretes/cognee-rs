# Cross-SDK E2E test image — builds both Python and Rust cognee CLIs
# and packages them with a pytest harness.
#
# Build context must be the cognee monorepo root (parent of cognee-rust/).
#
#   docker build -f cognee-rust/e2e-cross-sdk/Dockerfile .

# ─── Stage 1: prebuilt Rust binaries (built on the runner, not here) ────────
#
# The Rust CLI/server/telemetry binaries used to be compiled by a builder stage
# in this file. They are now built on the CI runner and copied in from
# cognee-rust/dist/ — see .github/workflows/http-parity.yml.
#
# Why: the builder stage recompiled ~741 crates on every run (~34 min of a
# ~39 min job). Its cargo target dir lived in a `--mount=type=cache`, which
# never survives a fresh runner, and BuildKit layer caching was measured
# producing ZERO reuse here even at an identical commit — so nothing carried
# over between runs. Building on the runner instead lets Swatinem/rust-cache
# persist the target dir, which is machinery this repo has proven works.
#
# Portability: the runner builds on ubuntu-24.04 (glibc 2.39, ort-sys needs
# >= 2.38) and the harness below runs Debian Trixie (glibc 2.41). Old -> new is
# the compatible direction for both glibc and libstdc++ (lbug's bundled C++).
# There is no OpenSSL to satisfy: reqwest is `default-features = false` with
# `rustls-tls`, and the resolved graph for these three binaries contains no
# openssl-sys or native-tls.
#
# To build the image outside CI, produce the binaries first — run these from
# the cognee-rust repo root, so they land in cognee-rust/dist/ as the COPYs
# below expect (the build context is the monorepo root above it):
#   cargo build --release -p cognee-cli -p cognee-http-server \
#     -p cognee-telemetry-emit \
#     --features cognee-http-server/bin,cognee-http-server/dev-mock
#   mkdir -p dist && cp target/release/cognee-cli \
#     target/release/cognee-http-server \
#     target/release/cognee-telemetry-emit dist/


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

# Copy project source and install. At the pinned SHA cognee declares
# `[tool.hatch.build.targets.wheel] packages = ["cognee", "cognee_db_workers", "kuzu"]`
# in pyproject.toml. Every package that is *imported at runtime* has to be
# copied here, or `uv sync` builds a wheel without it.
#
# `cognee_db_workers` is not optional: the Ladybug adapter does a top-level
# `import cognee_db_workers`, and cognee/__init__ reaches it through
# add -> ingest_data -> ... -> graph/config, so omitting it makes `import
# cognee` itself fail and `start_servers.sh` aborts on create_database().
# (`kuzu` is listed too but is only a ladybug shim for `import kuzu`, which
# the PyPI install below already satisfies; `distributed` is copied because
# older pins packaged it and the directory is harmless when they do not.)
COPY cognee/cognee /build/cognee
COPY cognee/cognee_db_workers /build/cognee_db_workers
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
# Trixie (glibc 2.41) runs the runner-built binaries, which link against
# ubuntu-24.04's glibc 2.39 — see the stage-1 note.
FROM python:3.12-slim-trixie AS harness

# curl/ca-certificates are for the ONNX model fetch below; they used to come
# from the deleted rust-builder stage.
RUN apt-get update && apt-get install -y --no-install-recommends \
    sqlite3 curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Prebuilt Rust binaries. `cognee-cli` is renamed on the way in so it does not
# collide with the Python CLI symlinked below as cognee-cli-python.
COPY cognee-rust/dist/cognee-cli /usr/local/bin/cognee-cli-rust
COPY cognee-rust/dist/cognee-http-server /usr/local/bin/cognee-http-server
COPY cognee-rust/dist/cognee-telemetry-emit /usr/local/bin/cognee-telemetry-emit

# ONNX embedding models (needed by the Rust CLI for cognify). Fetched here
# rather than in the deleted builder stage; ~30 MB, a few seconds.
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

# Copy dual-server entrypoint scripts
COPY cognee-rust/e2e-cross-sdk/bin/ /usr/local/bin/

# Make scripts executable
RUN chmod +x /usr/local/bin/start_servers.sh /harness/wait_for_health.sh

WORKDIR /workspace

CMD ["pytest", "-xvs", "/harness/"]
