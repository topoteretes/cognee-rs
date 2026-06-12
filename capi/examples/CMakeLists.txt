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
