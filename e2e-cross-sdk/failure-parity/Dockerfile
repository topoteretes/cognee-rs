# Python half of the cognify-failure differential harness.
#
# The existing e2e-cross-sdk image pins cognee 1.0.9 and a Rust binary built in
# May, so it cannot answer what *this* branch does. This image carries only the
# Python SDK at the reference version (1.5.3 == /tmp/cognee-python @ 690c0ec),
# with the reference source tree overlaid at run time via a bind mount so the
# executed code is byte-identical to the tree the Rust port is read against.
FROM python:3.12-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential git curl \
    && rm -rf /var/lib/apt/lists/*

RUN pip install --no-cache-dir "cognee==1.5.3"

WORKDIR /work
