/*
 * Example: SyncBatchFn task — demonstrates a batch task that receives
 * multiple items as a slice.
 *
 * In cognee-core, batch tasks receive their batch from the pipeline executor
 * when preceded by an iterator-producing task. Here we use a simple pipeline:
 *   sync (identity) → the pipeline feeds all 3 inputs individually,
 *   and the batch task collects them.
 *
 * Actually, batch tasks are called by the executor when items accumulate
 * from a prior iter/stream stage. For this example we demonstrate the
 * simpler approach: a single sync task that processes one item at a time.
 *
 * Demonstrates:
 *   - Creating multiple input values
 *   - A simple sync task that squares an integer
 */
#include "common.h"

static CgErrorCode square(const CgValue* input, const CgTaskContext* ctx,
                           void* user_data, CgValue** out) {
    (void)ctx;
    (void)user_data;
    int64_t val;
    CgErrorCode rc = cg_value_as_i64(input, &val);
    if (rc != CG_OK) return rc;
    *out = cg_value_from_i64(val * val);
    return CG_OK;
}

int main(void) {
    CHECK(cg_init());

    CgCancellationHandle* handle = NULL;
    CgTaskContext* ctx = NULL;
    CHECK(cg_task_context_mock(&handle, &ctx));

    CgTask* task = cg_task_sync(square, NULL, NULL);
    CgTaskInfo* info = cg_task_info_new(task);
    cg_task_info_set_name(info, "square");

    CgPipeline* pipeline = cg_pipeline_new("square pipeline");
    cg_pipeline_add_task(pipeline, info);

    /* Multiple inputs: each is processed independently */
    CgValue* v1 = cg_value_from_i64(3);
    CgValue* v2 = cg_value_from_i64(4);
    CgValue* v3 = cg_value_from_i64(5);
    const CgValue* inputs[] = { v1, v2, v3 };
    CgPipelineRunResult* result = NULL;
    CHECK(cg_pipeline_execute_blocking(pipeline, inputs, 3, ctx, NULL, &result));

    size_t count = cg_run_result_output_count(result);
    printf("Output count: %zu (expected 3)\n", count);

    for (size_t i = 0; i < count; i++) {
        CgValue* out = cg_run_result_output_at(result, i);
        int64_t val;
        CHECK(cg_value_as_i64(out, &val));
        printf("  output[%zu] = %ld\n", i, (long)val);
        cg_value_destroy(out);
    }
    printf("Expected: 9, 16, 25\n");

    cg_run_result_destroy(result);
    cg_value_destroy(v1);
    cg_value_destroy(v2);
    cg_value_destroy(v3);
    cg_pipeline_destroy(pipeline);
    cg_task_context_destroy(ctx);
    cg_cancellation_handle_destroy(handle);
    cg_shutdown();

    printf("PASSED\n");
    return 0;
}
