/*
 * Example: AsyncFn task — adds 100 to an integer via callback.
 *
 * Demonstrates:
 *   - Creating an async task with callback completion model
 *   - The callback can be invoked immediately (synchronously)
 */
#include "common.h"

static void adder(const CgValue* input, const CgTaskContext* ctx,
                  void* user_data, CgAsyncResultCallback callback,
                  void* callback_data) {
    (void)ctx;
    (void)user_data;

    int64_t val;
    CgErrorCode rc = cg_value_as_i64(input, &val);
    if (rc != CG_OK) {
        callback(rc, NULL, callback_data);
        return;
    }

    CgValue* result = cg_value_from_i64(val + 100);
    callback(CG_OK, result, callback_data);
}

int main(void) {
    CHECK(cg_init());

    CgCancellationHandle* handle = NULL;
    CgTaskContext* ctx = NULL;
    CHECK(cg_task_context_mock(&handle, &ctx));

    CgTask* task = cg_task_async(adder, NULL, NULL);
    CgTaskInfo* info = cg_task_info_new(task);
    cg_task_info_set_name(info, "adder");

    CgPipeline* pipeline = cg_pipeline_new("async adder pipeline");
    cg_pipeline_add_task(pipeline, info);

    CgValue* input = cg_value_from_i64(42);
    const CgValue* inputs[] = { input };
    CgPipelineRunResult* result = NULL;
    CHECK(cg_pipeline_execute_blocking(pipeline, inputs, 1, ctx, NULL, &result));

    size_t count = cg_run_result_output_count(result);
    printf("Output count: %zu\n", count);

    if (count > 0) {
        CgValue* out = cg_run_result_output_at(result, 0);
        int64_t val;
        CHECK(cg_value_as_i64(out, &val));
        printf("Result: %ld (expected 142)\n", (long)val);
        cg_value_destroy(out);
    }

    cg_run_result_destroy(result);
    cg_value_destroy(input);
    cg_pipeline_destroy(pipeline);
    cg_task_context_destroy(ctx);
    cg_cancellation_handle_destroy(handle);
    cg_shutdown();

    printf("PASSED\n");
    return 0;
}
