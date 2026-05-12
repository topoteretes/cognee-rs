cmake_minimum_required(VERSION 3.14)
project(cognee-capi C)

set(CARGO_TARGET_DIR "${CMAKE_SOURCE_DIR}/../target")

# Optional cargo features forwarded to the cognee-capi Rust build.
# Used by `capi/scripts/check.sh` to enable `testing-panic` when
# building the gap-07 panic-hook smoke binary.
set(COGNEE_CAPI_CARGO_FEATURES "" CACHE STRING
    "Extra cargo features to enable when building cognee-capi (comma-separated)")

if(COGNEE_CAPI_CARGO_FEATURES)
    set(CARGO_FEATURE_ARGS --features "${COGNEE_CAPI_CARGO_FEATURES}")
else()
    set(CARGO_FEATURE_ARGS "")
endif()

# Build Rust library via cargo
add_custom_target(cognee_capi_cargo ALL
    COMMAND cargo build --manifest-path "${CMAKE_SOURCE_DIR}/cognee-capi/Cargo.toml" ${CARGO_FEATURE_ARGS}
    WORKING_DIRECTORY "${CMAKE_SOURCE_DIR}/.."
    COMMENT "Building cognee-capi Rust library (features: ${COGNEE_CAPI_CARGO_FEATURES})"
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
