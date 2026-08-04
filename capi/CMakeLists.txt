cmake_minimum_required(VERSION 3.14)
project(cognee-capi C)

# After workspace extraction (D10), the capi/ subtree has its own cargo
# workspace and defaults its target/ to capi/target/. We pin CARGO_TARGET_DIR
# to a known path and forward it to the cargo invocation below so that the
# IMPORTED_LOCATION for the static library is always correct.
# Override with -DCARGO_TARGET_DIR=<path> if you want to reuse an existing
# target directory (e.g. the root workspace target/ to avoid a cold rebuild).
set(CARGO_TARGET_DIR "${CMAKE_SOURCE_DIR}/target" CACHE PATH
    "Cargo target directory for the cognee-capi build")

# Optional cargo features forwarded to the cognee-capi Rust build.
# Used by `capi/scripts/check.sh` to enable `testing-panic` when
# building the gap-07 panic-hook smoke binary.
set(COGNEE_CAPI_CARGO_FEATURES "" CACHE STRING
    "Extra cargo features to enable when building cognee-capi (comma-separated)")

# When ON, passes --no-default-features to the cargo invocation.
# Used for the slim build (--no-default-features --features sqlite,testing)
# verified in check.sh's build-slim stage (Phase 7, D6).
option(COGNEE_CAPI_NO_DEFAULT_FEATURES
    "Pass --no-default-features to cargo when building cognee-capi"
    OFF)

if(COGNEE_CAPI_NO_DEFAULT_FEATURES)
    set(CARGO_NO_DEFAULT_ARGS --no-default-features)
else()
    set(CARGO_NO_DEFAULT_ARGS "")
endif()

if(COGNEE_CAPI_CARGO_FEATURES)
    set(CARGO_FEATURE_ARGS --features "${COGNEE_CAPI_CARGO_FEATURES}")
else()
    set(CARGO_FEATURE_ARGS "")
endif()

# Re-enable lbug's internal consistency checks (KU_ASSERT / RUNTIME_CHECK).
#
# capi/Cargo.toml's [profile.dev] deliberately builds lbug's bundled Ladybug
# tree as a Release C++ build (2.9 GiB -> 208 MiB), and Release defines NDEBUG.
# lbug gates those checks on `defined(LBUG_RUNTIME_CHECKS) || !defined(NDEBUG)`
# (lbug-src/src/include/common/assert.h), so they are off by default here.
#
# Defining LBUG_RUNTIME_CHECKS satisfies the first half of that condition
# independently of NDEBUG, which re-arms the asserts while keeping the small
# Release tree — cheaper than dropping to a Debug C++ build for the same
# effect. Turning an assert failure into a thrown InternalException is what
# makes a bad handle or index crossing the cg_sdk_* boundary stop at a precise
# file and line instead of running into undefined behaviour.
#
# CXXFLAGS is the injection point because the `cmake` crate builds lbug from
# lbug's own build.rs — this project never invokes that CMake run directly and
# so cannot pass -DENABLE_RUNTIME_CHECKS=ON to it. Appending keeps any CXXFLAGS
# the caller already set.
#
# Caveat: cargo fingerprints the lbug build-script unit on its build-dependency
# closure, not on CXXFLAGS, so toggling this does NOT by itself force the C++
# tree to rebuild. Run `cargo clean -p lbug --manifest-path capi/Cargo.toml`
# (or a full clean) after flipping it.
option(COGNEE_CAPI_LBUG_ASSERTS
    "Define LBUG_RUNTIME_CHECKS so lbug's KU_ASSERT/RUNTIME_CHECK stay compiled in"
    OFF)

if(COGNEE_CAPI_LBUG_ASSERTS)
    # string(STRIP) so an unset caller CXXFLAGS does not leave a leading space
    # in the value; `cmake -E env` would pass it through verbatim.
    string(STRIP "$ENV{CXXFLAGS} -DLBUG_RUNTIME_CHECKS" CARGO_CXXFLAGS)
    set(CARGO_ASSERT_ENV "CXXFLAGS=${CARGO_CXXFLAGS}")
    message(STATUS "cognee-capi: lbug runtime checks ENABLED (CXXFLAGS=${CARGO_CXXFLAGS})")
else()
    # `env` treats an empty list element as nothing to set, so this is a no-op
    # and the caller's own CXXFLAGS pass through untouched.
    set(CARGO_ASSERT_ENV "")
endif()

# Build Rust library via cargo.
# CARGO_TARGET_DIR is forwarded explicitly so cargo writes artifacts into
# capi/target/ (pinned above) regardless of the calling directory. Without
# this, cargo would derive target/ from the workspace root, which changed
# after the capi workspace was extracted (D10).
add_custom_target(cognee_capi_cargo ALL
    COMMAND ${CMAKE_COMMAND} -E env
        CARGO_TARGET_DIR=${CARGO_TARGET_DIR}
        ${CARGO_ASSERT_ENV}
        cargo build --manifest-path "${CMAKE_SOURCE_DIR}/cognee-capi/Cargo.toml"
        ${CARGO_NO_DEFAULT_ARGS}
        ${CARGO_FEATURE_ARGS}
    WORKING_DIRECTORY "${CMAKE_SOURCE_DIR}"
    COMMENT "Building cognee-capi Rust library (no-default=${COGNEE_CAPI_NO_DEFAULT_FEATURES} features: ${COGNEE_CAPI_CARGO_FEATURES})"
)

# Import the built static library
add_library(cognee_capi STATIC IMPORTED GLOBAL)
set_target_properties(cognee_capi PROPERTIES
    IMPORTED_LOCATION "${CARGO_TARGET_DIR}/debug/libcognee_capi.a"
)
add_dependencies(cognee_capi cognee_capi_cargo)

# …and the shared library, which the same `cargo build` already produced.
#
# cognee-capi is `crate-type = ["cdylib", "staticlib"]`, so every build emits
# both a ~805 MB .a and a ~185 MB .dylib. Linking the examples against the .a
# is enormously wasteful: lbug's build.rs emits `static:+whole-archive` for
# liblbug and all 16 bundled third-party C++ archives, so each example embeds
# the entire Ladybug engine. Measured, linking one example:
#
#   against the .a                   276.3 MB   0.94 s
#   against the .a + -dead_strip      15.7 MB   0.23 s
#   against the .dylib                  35 KB   0.02 s
#
# 19 examples x ~264 MB is the 4.9 GiB `capi/build` tree. Linking them against
# the .dylib instead makes that tree a rounding error, and the smoke tests
# still exercise the real engine — the code just lives in the dylib rather
# than being copied into each binary.
#
# How the examples find it at run time differs by platform, and neither route
# is one we control from here:
#   macOS - cargo links the dylib with an absolute install_name, though it
#           points at the deps/ copy (`otool -D` shows
#           .../target/debug/deps/libcognee_capi.dylib), not the path below.
#   Linux - no absolute SONAME; the examples resolve it only because CMake
#           injects a build-tree RPATH for a full-path imported library.
# Both are fine for a local check tree and neither survives copying a binary
# out of capi/build — which is fine, these are never distributed.
add_library(cognee_capi_shared SHARED IMPORTED GLOBAL)
set_target_properties(cognee_capi_shared PROPERTIES
    IMPORTED_LOCATION "${CARGO_TARGET_DIR}/debug/${CMAKE_SHARED_LIBRARY_PREFIX}cognee_capi${CMAKE_SHARED_LIBRARY_SUFFIX}"
)
if(WIN32)
    # On DLL platforms CMake needs the import library to build a link line at
    # all; IMPORTED_LOCATION alone fails at generate time. cargo emits
    # cognee_capi.dll.lib beside the DLL. Untested — nothing CMake-configures
    # the capi tree on Windows today (COGNEE_SYSTEM_LIBS below is POSIX-only),
    # so treat this as a starting point rather than a working configuration.
    set_target_properties(cognee_capi_shared PROPERTIES
        IMPORTED_IMPLIB "${CARGO_TARGET_DIR}/debug/cognee_capi.dll.lib"
    )
endif()
add_dependencies(cognee_capi_shared cognee_capi_cargo)

# Which library the examples link by default. ON restores the old all-static
# behaviour for anyone who needs to reproduce a static-link problem.
# capi/examples/CMakeLists.txt keeps one example statically linked either way,
# so the .a path and the Apple-framework list below never go untested.
option(COGNEE_CAPI_STATIC_EXAMPLES
    "Link every C example against libcognee_capi.a instead of the .dylib"
    OFF)

if(COGNEE_CAPI_STATIC_EXAMPLES)
    set(COGNEE_EXAMPLE_LIB cognee_capi)
else()
    set(COGNEE_EXAMPLE_LIB cognee_capi_shared)
endif()

# System libraries needed by the Rust static lib
# stdc++ is required because cognee-graph depends on lbug (C++ graph engine)
set(COGNEE_SYSTEM_LIBS pthread dl m stdc++)

# macOS: a Rust *staticlib* does not carry the `#[link]` / `cargo:rustc-link-lib`
# directives its dependencies declare — rustc only acts on those when it drives
# the final link itself (cdylib, bin). Because CMake links libcognee_capi.a with
# clang instead, every Apple framework and system library the Rust side needs
# must be named here, or the examples fail with undefined symbols such as
# `_kSecValueData` / `_kSecClass` coming out of the security_framework objects
# inside the archive.
#
# This list is the deduplicated output of
#   cargo rustc --manifest-path capi/cognee-capi/Cargo.toml \
#       --crate-type staticlib -- --print native-static-libs
# minus the entries clang already links by default (-lSystem, -lc, -lm,
# -lclang_rt.osx) and -lc++ (covered by `stdc++` above, which clang maps to
# libc++ on Apple). Re-run that command and refresh this list whenever a
# dependency that binds an Apple API is added, removed, or swapped out.
#
#   Security            — security-framework-sys, reached via rustls-native-certs
#                         (reqwest -> hyper-rustls) reading the system trust store
#   CoreFoundation      — core-foundation-sys: iana-time-zone (chrono) plus the
#                         CF types the Security bindings pass around
#   SystemConfiguration — whoami's SCDynamicStoreCopyComputerName, used for the
#                         host name reported in telemetry
#   Foundation, CoreML  — ort-sys (ONNX Runtime) and its CoreML execution provider
#   iconv               — libc's Apple bindings declare `#[link(name = "iconv")]`
if(APPLE)
    list(APPEND COGNEE_SYSTEM_LIBS
        "-framework Security"
        "-framework CoreFoundation"
        "-framework SystemConfiguration"
        "-framework Foundation"
        "-framework CoreML"
        iconv
    )
endif()

# Header include path
set(COGNEE_INCLUDE_DIR "${CMAKE_SOURCE_DIR}/include")

add_subdirectory(examples)
