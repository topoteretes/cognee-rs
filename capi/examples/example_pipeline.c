/*
 * Example: Multi-task pipeline — chain of sync → sync → sync tasks.
 *
 * Pipeline: double → add_ten → negate
 * Input: 5  →  10  →  20  →  -20
 *
 * Demonstrates building a pipeline with multiple tasks of different types.
 */
#include "common.h"

static CgErrorCode doubler(const CgValue* input, const CgTaskContext* ctx,
                            void* user_data, CgValue** out) {
    (void)ctx; (void)user_data;
    int64_t val;
    CgErrorCode rc = cg_value_as_i64(input, &val);
    if (rc != CG_OK) return rc;
    *out = cg_value_from_i64(val * 2);
    return CG_OK;
}

static CgErrorCode add_ten(const CgValue* input, const CgTaskContext* ctx,
                            void* user_data, CgValue** out) {
    (void)ctx; (void)user_data;
    int64_t val;
    CgErrorCode rc = cg_value_as_i64(input, &val);
    if (rc != CG_OK) return rc;
    *out = cg_value_from_i64(val + 10);
    return CG_OK;
}

static CgErrorCode negate(const CgValue* input, const CgTaskContext* ctx,
                           void* user_data, CgValue** out) {
    (void)ctx; (void)user_data;
    int64_t val;
    CgErrorCode rc = cg_value_as_i64(input, &val);
    if (rc != CG_OK) return rc;
    *out = cg_value_from_i64(-val);
    return CG_OK;
}

int main(void) {
    /* Smoke-check the gap-06 logging entrypoint exports correctly.
     * 0 = success (or idempotent re-call). Non-zero on env-var config
     * error. Either way we proceed — the example does not require
     * logging to be functional. */
    (void)cognee_setup_logging();
    CHECK(cg_init());

    CgCancellationHandle* handle = NULL;
    CgTaskContext* ctx = NULL;
    CHECK(cg_task_context_mock(&handle, &ctx));

    /* Task 1: doubler */
    CgTaskInfo* info1 = cg_task_info_new(cg_task_sync(doubler, NULL, NULL));
    cg_task_info_set_name(info1, "doubler");

    /* Task 2: add_ten */
    CgTaskInfo* info2 = cg_task_info_new(cg_task_sync(add_ten, NULL, NULL));
    cg_task_info_set_name(info2, "add_ten");

    /* Task 3: negate */
    CgTaskInfo* info3 = cg_task_info_new(cg_task_sync(negate, NULL, NULL));
    cg_task_info_set_name(info3, "negate");

    /* Build pipeline */
    CgPipeline* pipeline = cg_pipeline_new("multi-step pipeline");
    cg_pipeline_set_name(pipeline, "compute");
    cg_pipeline_add_task(pipeline, info1);
    cg_pipeline_add_task(pipeline, info2);
    cg_pipeline_add_task(pipeline, info3);

    /* Execute with input = 5 */
    CgValue* input = cg_value_from_i64(5);
    const CgValue* inputs[] = { input };
    CgPipelineRunResult* result = NULL;
    CHECK(cg_pipeline_execute_blocking(pipeline, inputs, 1, ctx, NULL, &result));

    /* Read result: 5 → 10 → 20 → -20 */
    size_t count = cg_run_result_output_count(result);
    printf("Output count: %zu\n", count);

    if (count > 0) {
        CgValue* out = cg_run_result_output_at(result, 0);
        int64_t val;
        CHECK(cg_value_as_i64(out, &val));
        printf("Result: %ld (expected -20)\n", (long)val);
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
