/*
 * Example: SyncFn task — doubles an integer.
 *
 * Demonstrates:
 *   - Creating a CgValue from C
 *   - Creating a sync task from a C function pointer
 *   - Building and executing a pipeline (blocking)
 *   - Reading the result
 */
#include "common.h"

static CgErrorCode doubler(const CgValue* input, const CgTaskContext* ctx,
                            void* user_data, CgValue** out) {
    (void)ctx;
    (void)user_data;

    int64_t val;
    CgErrorCode rc = cg_value_as_i64(input, &val);
    if (rc != CG_OK) return rc;

    *out = cg_value_from_i64(val * 2);
    return CG_OK;
}

int main(void) {
    CHECK(cg_init());

    /* Create mock context */
    CgCancellationHandle* handle = NULL;
    CgTaskContext* ctx = NULL;
    CHECK(cg_task_context_mock(&handle, &ctx));

    /* Build task */
    CgTask* task = cg_task_sync(doubler, NULL, NULL);
    CgTaskInfo* info = cg_task_info_new(task);
    cg_task_info_set_name(info, "doubler");

    /* Build pipeline */
    CgPipeline* pipeline = cg_pipeline_new("doubler pipeline");
    cg_pipeline_add_task(pipeline, info);  /* info consumed */

    /* Execute */
    CgValue* input = cg_value_from_i64(21);
    const CgValue* inputs[] = { input };
    CgPipelineRunResult* result = NULL;
    CHECK(cg_pipeline_execute_blocking(pipeline, inputs, 1, ctx, NULL, &result));

    /* Read result */
    size_t count = cg_run_result_output_count(result);
    printf("Output count: %zu\n", count);

    if (count > 0) {
        CgValue* out = cg_run_result_output_at(result, 0);
        int64_t val;
        CHECK(cg_value_as_i64(out, &val));
        printf("Result: %ld (expected 42)\n", (long)val);
        cg_value_destroy(out);
    }

    /* Cleanup */
    cg_run_result_destroy(result);
    cg_value_destroy(input);
    cg_pipeline_destroy(pipeline);
    cg_task_context_destroy(ctx);
    cg_cancellation_handle_destroy(handle);
    cg_shutdown();

    printf("PASSED\n");
    return 0;
}
