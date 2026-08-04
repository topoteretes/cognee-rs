# example_sync_task is deliberately absent here — it is the static-link canary
# built separately below. Everything in this list links whichever library
# COGNEE_EXAMPLE_LIB selects (the .dylib by default; see ../CMakeLists.txt).
set(EXAMPLES
    example_async_task
    example_iter_task
    example_batch_task
    example_pipeline
    example_cancellation
    example_background_task
)

foreach(example ${EXAMPLES})
    add_executable(${example} ${example}.c)
    target_include_directories(${example} PRIVATE ${COGNEE_INCLUDE_DIR})
    target_link_libraries(${example} ${COGNEE_EXAMPLE_LIB} ${COGNEE_SYSTEM_LIBS})
endforeach()

# Static-link canary — always the .a, never COGNEE_EXAMPLE_LIB.
#
# With every other example on the dylib, nothing would exercise the staticlib
# link path or the hand-maintained Apple-framework list in ../CMakeLists.txt.
# A missing entry there is what broke the build in #116: rustc only honours
# `#[link]` directives when it drives the link itself, so a `.a` consumer must
# name every framework. Keeping one example static means that still fails here.
#
# Deliberately NOT dead-stripped, even though that would take it from ~276 MB
# to ~16 MB. ld64 dead-strips before it checks for undefined symbols, so
# -Wl,-dead_strip silently drops exactly the unresolved framework references
# this canary exists to surface — verified: a force-loaded archive member with
# a missing symbol fails the link normally, and links clean with -dead_strip.
# The ~260 MB is the price of the check actually working; the other 18
# examples are ~50 KB each, so capi/build is still ~280 MB rather than 4.9 GiB.
add_executable(example_sync_task example_sync_task.c)
target_include_directories(example_sync_task PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(example_sync_task cognee_capi ${COGNEE_SYSTEM_LIBS})

# Phase 1b SDK handle smoke test (Tier-A: mock embedding, no network).
add_executable(sdk_handle_smoke sdk_handle_smoke.c)
target_include_directories(sdk_handle_smoke PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(sdk_handle_smoke ${COGNEE_EXAMPLE_LIB} ${COGNEE_SYSTEM_LIBS})

# Phase 2 conventions smoke tests (Tier-A: mock embedding, no network).
add_executable(sdk_conventions_smoke sdk_conventions_smoke.c)
target_include_directories(sdk_conventions_smoke PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(sdk_conventions_smoke ${COGNEE_EXAMPLE_LIB} ${COGNEE_SYSTEM_LIBS})

add_executable(sdk_negative_path_smoke sdk_negative_path_smoke.c)
target_include_directories(sdk_negative_path_smoke PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(sdk_negative_path_smoke ${COGNEE_EXAMPLE_LIB} ${COGNEE_SYSTEM_LIBS})

# Phase 3 config surface smoke test (Tier-A: mock embedding, no network).
add_executable(sdk_config_smoke sdk_config_smoke.c)
target_include_directories(sdk_config_smoke PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(sdk_config_smoke ${COGNEE_EXAMPLE_LIB} ${COGNEE_SYSTEM_LIBS})

# Phase 4 core ops smoke test (Tier-A: mock embedding, no LLM).
add_executable(example_sdk_add example_sdk_add.c)
target_include_directories(example_sdk_add PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(example_sdk_add ${COGNEE_EXAMPLE_LIB} ${COGNEE_SYSTEM_LIBS})

# Phase 4 live add+cognify example (Tier-B: requires OPENAI_URL + OPENAI_TOKEN;
# the binary has a built-in skip guard so it exits 0 when credentials are absent).
add_executable(example_sdk_add_cognify example_sdk_add_cognify.c)
target_include_directories(example_sdk_add_cognify PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(example_sdk_add_cognify ${COGNEE_EXAMPLE_LIB} ${COGNEE_SYSTEM_LIBS})

# Phase 5 retrieval smoke test (Tier-A: mock embedding, no network).
add_executable(sdk_retrieval_smoke sdk_retrieval_smoke.c)
target_include_directories(sdk_retrieval_smoke PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(sdk_retrieval_smoke ${COGNEE_EXAMPLE_LIB} ${COGNEE_SYSTEM_LIBS})

# Phase 5 live add+cognify+search example (Tier-B: requires OPENAI_URL + OPENAI_TOKEN;
# the binary has a built-in skip guard so it exits 0 when credentials are absent).
add_executable(example_sdk_add_cognify_search example_sdk_add_cognify_search.c)
target_include_directories(example_sdk_add_cognify_search PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(example_sdk_add_cognify_search ${COGNEE_EXAMPLE_LIB} ${COGNEE_SYSTEM_LIBS})

# Phase 6 data-ops smoke test (Tier-A: mock embedding, no LLM).
add_executable(sdk_data_smoke sdk_data_smoke.c)
target_include_directories(sdk_data_smoke PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(sdk_data_smoke ${COGNEE_EXAMPLE_LIB} ${COGNEE_SYSTEM_LIBS})

# Gap 07 smoke binaries (driven by capi/scripts/check.sh).
#
# `init_otlp_smoke` and `init_telemetry_smoke` exercise the public C
# entrypoints and need no special cargo feature. The panic-hook smoke
# binary links `cg_test_force_panic`, exposed under the `testing-panic`
# cargo feature; capi/scripts/check.sh enables that feature and
# `COGNEE_BUILD_PANIC_SMOKE=ON` on the single default configure, so this
# target is built in the main tree rather than in a second build dir.
set(GAP07_SMOKE_EXAMPLES
    init_otlp_smoke
    init_telemetry_smoke
)

foreach(example ${GAP07_SMOKE_EXAMPLES})
    add_executable(${example} ${example}.c)
    target_include_directories(${example} PRIVATE ${COGNEE_INCLUDE_DIR})
    target_link_libraries(${example} ${COGNEE_EXAMPLE_LIB} ${COGNEE_SYSTEM_LIBS})
endforeach()

if(COGNEE_BUILD_PANIC_SMOKE)
    add_executable(panic_hook_smoke panic_hook_smoke.c)
    target_include_directories(panic_hook_smoke PRIVATE ${COGNEE_INCLUDE_DIR})
    target_link_libraries(panic_hook_smoke ${COGNEE_EXAMPLE_LIB} ${COGNEE_SYSTEM_LIBS})
endif()

# Phase 7 feature-gated smoke test.
#
# Default build: sdk_feature_smoke links the full feature set and verifies
#   that cg_sdk_visualize / cg_sdk_visualize_to_file fire their callbacks,
#   and that cg_json_string_decode round-trips correctly. Cloud ops
#   (cg_sdk_serve / cg_sdk_disconnect) moved to closed `cognee-c-cloud` in
#   T15a and have their own smoke coverage in the closed repo.
#
# Slim build (built in a separate CMake dir by check.sh with
#   -DCOGNEE_CAPI_NO_DEFAULT_FEATURES=ON -DCOGNEE_CAPI_CARGO_FEATURES=sqlite,testing):
#   sdk_feature_smoke_slim compiles the same source with
#   -DEXPECT_FEATURE_NOT_BUILT=1 so that it asserts CG_ERR_FEATURE_NOT_BUILT
#   from the remaining feature-gated ops (visualization).
add_executable(sdk_feature_smoke sdk_feature_smoke.c)
target_include_directories(sdk_feature_smoke PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(sdk_feature_smoke ${COGNEE_EXAMPLE_LIB} ${COGNEE_SYSTEM_LIBS})

# The slim variant is only built in the slim CMake configuration
# (COGNEE_CAPI_NO_DEFAULT_FEATURES=ON).  We gate it on that flag so the
# default build does not try to link an incompatible symbol set.
if(COGNEE_CAPI_NO_DEFAULT_FEATURES)
    add_executable(sdk_feature_smoke_slim sdk_feature_smoke.c)
    target_include_directories(sdk_feature_smoke_slim PRIVATE ${COGNEE_INCLUDE_DIR})
    target_link_libraries(sdk_feature_smoke_slim ${COGNEE_EXAMPLE_LIB} ${COGNEE_SYSTEM_LIBS})
    target_compile_definitions(sdk_feature_smoke_slim PRIVATE EXPECT_FEATURE_NOT_BUILT=1)
endif()
