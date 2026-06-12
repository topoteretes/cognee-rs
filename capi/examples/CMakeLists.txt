set(EXAMPLES
    example_sync_task
    example_async_task
    example_iter_task
    example_batch_task
    example_pipeline
    example_cancellation
)

foreach(example ${EXAMPLES})
    add_executable(${example} ${example}.c)
    target_include_directories(${example} PRIVATE ${COGNEE_INCLUDE_DIR})
    target_link_libraries(${example} cognee_capi ${COGNEE_SYSTEM_LIBS})
endforeach()

# Phase 1b SDK handle smoke test (Tier-A: mock embedding, no network).
add_executable(sdk_handle_smoke sdk_handle_smoke.c)
target_include_directories(sdk_handle_smoke PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(sdk_handle_smoke cognee_capi ${COGNEE_SYSTEM_LIBS})

# Phase 2 conventions smoke tests (Tier-A: mock embedding, no network).
add_executable(sdk_conventions_smoke sdk_conventions_smoke.c)
target_include_directories(sdk_conventions_smoke PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(sdk_conventions_smoke cognee_capi ${COGNEE_SYSTEM_LIBS})

add_executable(sdk_negative_path_smoke sdk_negative_path_smoke.c)
target_include_directories(sdk_negative_path_smoke PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(sdk_negative_path_smoke cognee_capi ${COGNEE_SYSTEM_LIBS})

# Phase 3 config surface smoke test (Tier-A: mock embedding, no network).
add_executable(sdk_config_smoke sdk_config_smoke.c)
target_include_directories(sdk_config_smoke PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(sdk_config_smoke cognee_capi ${COGNEE_SYSTEM_LIBS})

# Phase 4 core ops smoke test (Tier-A: mock embedding, no LLM).
add_executable(example_sdk_add example_sdk_add.c)
target_include_directories(example_sdk_add PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(example_sdk_add cognee_capi ${COGNEE_SYSTEM_LIBS})

# Phase 4 live add+cognify example (Tier-B: requires OPENAI_URL + OPENAI_TOKEN;
# the binary has a built-in skip guard so it exits 0 when credentials are absent).
add_executable(example_sdk_add_cognify example_sdk_add_cognify.c)
target_include_directories(example_sdk_add_cognify PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(example_sdk_add_cognify cognee_capi ${COGNEE_SYSTEM_LIBS})

# Phase 5 retrieval smoke test (Tier-A: mock embedding, no network).
add_executable(sdk_retrieval_smoke sdk_retrieval_smoke.c)
target_include_directories(sdk_retrieval_smoke PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(sdk_retrieval_smoke cognee_capi ${COGNEE_SYSTEM_LIBS})

# Phase 5 live add+cognify+search example (Tier-B: requires OPENAI_URL + OPENAI_TOKEN;
# the binary has a built-in skip guard so it exits 0 when credentials are absent).
add_executable(example_sdk_add_cognify_search example_sdk_add_cognify_search.c)
target_include_directories(example_sdk_add_cognify_search PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(example_sdk_add_cognify_search cognee_capi ${COGNEE_SYSTEM_LIBS})

# Phase 6 data-ops smoke test (Tier-A: mock embedding, no LLM).
add_executable(sdk_data_smoke sdk_data_smoke.c)
target_include_directories(sdk_data_smoke PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(sdk_data_smoke cognee_capi ${COGNEE_SYSTEM_LIBS})

# Gap 07 smoke binaries (driven by capi/scripts/check.sh).
#
# `init_otlp_smoke` and `init_telemetry_smoke` exercise the public C
# entrypoints and need no special cargo feature. The panic-hook smoke
# binary links the `cg_test_force_panic` symbol exposed under the
# `testing-panic` cargo feature; the driver script rebuilds
# cognee-capi with that feature enabled and toggles
# `COGNEE_BUILD_PANIC_SMOKE=ON` to opt this target in.
set(GAP07_SMOKE_EXAMPLES
    init_otlp_smoke
    init_telemetry_smoke
)

foreach(example ${GAP07_SMOKE_EXAMPLES})
    add_executable(${example} ${example}.c)
    target_include_directories(${example} PRIVATE ${COGNEE_INCLUDE_DIR})
    target_link_libraries(${example} cognee_capi ${COGNEE_SYSTEM_LIBS})
endforeach()

if(COGNEE_BUILD_PANIC_SMOKE)
    add_executable(panic_hook_smoke panic_hook_smoke.c)
    target_include_directories(panic_hook_smoke PRIVATE ${COGNEE_INCLUDE_DIR})
    target_link_libraries(panic_hook_smoke cognee_capi ${COGNEE_SYSTEM_LIBS})
endif()

# Phase 7 feature-gated smoke test.
#
# Default build: sdk_feature_smoke links the full feature set and verifies
#   that cg_sdk_visualize / cg_sdk_serve / cg_sdk_disconnect fire their
#   callbacks, and that cg_json_string_decode round-trips correctly.
#
# Slim build (built in a separate CMake dir by check.sh with
#   -DCOGNEE_CAPI_NO_DEFAULT_FEATURES=ON -DCOGNEE_CAPI_CARGO_FEATURES=sqlite,testing):
#   sdk_feature_smoke_slim compiles the same source with
#   -DEXPECT_FEATURE_NOT_BUILT=1 so that it asserts CG_ERR_FEATURE_NOT_BUILT
#   from all four feature-gated ops.
add_executable(sdk_feature_smoke sdk_feature_smoke.c)
target_include_directories(sdk_feature_smoke PRIVATE ${COGNEE_INCLUDE_DIR})
target_link_libraries(sdk_feature_smoke cognee_capi ${COGNEE_SYSTEM_LIBS})

# The slim variant is only built in the slim CMake configuration
# (COGNEE_CAPI_NO_DEFAULT_FEATURES=ON).  We gate it on that flag so the
# default build does not try to link an incompatible symbol set.
if(COGNEE_CAPI_NO_DEFAULT_FEATURES)
    add_executable(sdk_feature_smoke_slim sdk_feature_smoke.c)
    target_include_directories(sdk_feature_smoke_slim PRIVATE ${COGNEE_INCLUDE_DIR})
    target_link_libraries(sdk_feature_smoke_slim cognee_capi ${COGNEE_SYSTEM_LIBS})
    target_compile_definitions(sdk_feature_smoke_slim PRIVATE EXPECT_FEATURE_NOT_BUILT=1)
endif()
