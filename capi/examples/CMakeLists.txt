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
