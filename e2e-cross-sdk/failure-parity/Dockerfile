# Python half of the cognify-failure differential harness.
#
# This image carries only the Python SDK, so the probe can run against a chosen
# cognee without rebuilding the full e2e-cross-sdk image.
#
# The default is the exact Python pin of `.github/workflows/http-parity.yml`
# (topoteretes/cognee @ 1d3ed15b), installed from source. That is a later `dev`
# commit whose pyproject still says 1.6.5, and no PyPI release matches it: the
# PyPI 1.6.5 release (tag `v1.6.5`) predates COG-6089 (`a88402500`, which drops
# `run_info.data`) and still writes that key, while 1.6.6 already carries LLM-free
# summaries by default (SDK-1312, `d84a42fec`) and the separate
# `indexing_pipeline` (SDK-397, `137c5f08b`), which Rust has not ported. Bump
# COGNEE_REF with the CI pin. A release works too:
# `--build-arg COGNEE_SPEC=cognee==1.6.6`.
#
# Recorded runs before SDK-946 used cognee 1.5.3, the last release whose
# `cognify()` returned a value on a failed tolerant run (`raise_on_error`
# landed after it).
FROM python:3.12-slim

ARG COGNEE_REF=1d3ed15b1d7faf93af3ae7efff8167d164ee0847
ARG COGNEE_SPEC="cognee @ git+https://github.com/topoteretes/cognee@${COGNEE_REF}"

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential git curl \
    && rm -rf /var/lib/apt/lists/*

RUN pip install --no-cache-dir "${COGNEE_SPEC}"

WORKDIR /work
