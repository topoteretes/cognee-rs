# Modern aarch64 cross image for building the TS Neon addon with cross-rs.
#
# cross-rs's stock image is Ubuntu 20.04 (gcc 9/10) — too old for the bundled
# lbug C++ engine, which needs C++20 (<span>) AND libstdc++ 11's heterogeneous
# unordered_map lookup. Ubuntu 22.04 ships a gcc-11 aarch64 cross toolchain
# (glibc 2.35) that compiles lbug. We reuse the cross-rs image only for its
# CMake toolchain file (cross points CMAKE_TOOLCHAIN_FILE at /opt/toolchain.cmake).
#
# Note: binaries produced here require glibc >= 2.35 on the target (Ubuntu
# 22.04 / Debian 12 era).
FROM ghcr.io/cross-rs/aarch64-unknown-linux-gnu:main AS crossbase

FROM ubuntu:22.04
COPY --from=crossbase /opt/toolchain.cmake /opt/toolchain.cmake
ENV DEBIAN_FRONTEND=noninteractive
# Cross toolchain env that cross-rs's stock images normally bake in. Without
# these, Rust target links and cc-rs C builds fall back to the host gcc/ld and
# fail with "Relocations in generic ELF (EM: 183) / file in wrong format".
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++ \
    AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar \
    CROSS_TOOLCHAIN_PREFIX=aarch64-linux-gnu- \
    CROSS_SYSROOT=/usr/aarch64-linux-gnu
RUN apt-get update && \
    apt-get install --assume-yes --no-install-recommends \
        build-essential crossbuild-essential-arm64 cmake pkg-config libssl-dev \
        curl unzip ca-certificates && \
    curl -fsSL https://github.com/protocolbuffers/protobuf/releases/download/v25.1/protoc-25.1-linux-x86_64.zip -o /tmp/protoc.zip && \
    unzip -o /tmp/protoc.zip -d /usr/local bin/protoc 'include/*' && \
    chmod +x /usr/local/bin/protoc && \
    protoc --version && \
    aarch64-linux-gnu-g++ --version | head -1 && \
    rm -rf /var/lib/apt/lists/* /tmp/protoc.zip
