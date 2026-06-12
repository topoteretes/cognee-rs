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

# Build Rust library via cargo.
# CARGO_TARGET_DIR is forwarded explicitly so cargo writes artifacts into
# capi/target/ (pinned above) regardless of the calling directory. Without
# this, cargo would derive target/ from the workspace root, which changed
# after the capi workspace was extracted (D10).
add_custom_target(cognee_capi_cargo ALL
    COMMAND ${CMAKE_COMMAND} -E env
        CARGO_TARGET_DIR=${CARGO_TARGET_DIR}
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

# System libraries needed by the Rust static lib
# stdc++ is required because cognee-graph depends on lbug (C++ graph engine)
set(COGNEE_SYSTEM_LIBS pthread dl m stdc++)

# macOS requires CoreFoundation for iana_time_zone (used by chrono)
if(APPLE)
    list(APPEND COGNEE_SYSTEM_LIBS "-framework CoreFoundation")
endif()

# Header include path
set(COGNEE_INCLUDE_DIR "${CMAKE_SOURCE_DIR}/include")

add_subdirectory(examples)
