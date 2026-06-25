/*
 * Regression test: background execution runs the task list (CR-2).
 *
 * Before the fix, cg_pipeline_execute_in_background and
 * cg_pipeline_execute_async ran an empty pipeline (task list was
 * dropped by clone_pipeline), so they silently produced no outputs.
 *
 * This example:
 *   1. Builds a single-task pipeline (doubles an i64 input).
 *   2. Runs it via cg_pipeline_execute_in_background + cg_run_handle_wait.
 *   3. Asserts the task actually ran (output == 2 * input).
 *
 * If the bug were still present, cg_run_result_output_count() would
 * return 0 and the test would fail with an assertion error.
 */
#include "common.h"
#include <stdatomic.h>

static CgErrorCode double_value(const CgValue* input, const CgTaskContext* ctx,
                                void* user_data, CgValue** out) {
    (void)ctx;
    (void)user_data;
    int64_t val;
    CgErrorCode rc = cg_value_as_i64(input, &val);
    if (rc != CG_OK) return rc;
    *out = cg_value_from_i64(val * 2);
    return CG_OK;
}

/* Callback state shared between the spawned task and the main thread. */
typedef struct {
    int64_t result_value;
    size_t  output_count;
    CgErrorCode status;
    atomic_int done; /* set to 1 when callback fires */
} CallbackState;

static void on_done(CgErrorCode status, CgPipelineRunResult* result, void* data) {
    CallbackState* state = (CallbackState*)data;
    state->status = status;

    if (status == CG_OK && result != NULL) {
        state->output_count = cg_run_result_output_count(result);
        if (state->output_count > 0) {
            CgValue* out = cg_run_result_output_at(result, 0);
            cg_value_as_i64(out, &state->result_value);
            cg_value_destroy(out);
        }
        cg_run_result_destroy(result);
    }

    atomic_store(&state->done, 1);
}

int main(void) {
    (void)cognee_setup_logging();
    CHECK(cg_init());

    CgCancellationHandle* handle = NULL;
    CgTaskContext* ctx = NULL;
    CHECK(cg_task_context_mock(&handle, &ctx));

    /* Build pipeline: one sync task that doubles the input. */
    CgTaskInfo* info = cg_task_info_new(cg_task_sync(double_value, NULL, NULL));
    cg_task_info_set_name(info, "doubler");

    CgPipeline* pipeline = cg_pipeline_new("background doubler");
    cg_pipeline_add_task(pipeline, info);

    /* Execute in background */
    CgValue* input = cg_value_from_i64(21);
    const CgValue* inputs[] = { input };
    CgPipelineRunHandle* run_handle =
        cg_pipeline_execute_in_background(pipeline, inputs, 1, ctx, NULL);

    if (run_handle == NULL) {
        const char* msg = cg_last_error_message();
        fprintf(stderr, "cg_pipeline_execute_in_background failed: %s\n",
                msg ? msg : "(no message)");
        exit(1);
    }

    /* Wait for completion via callback */
    CallbackState state = { 0, 0, CG_OK, 0 };
    atomic_store(&state.done, 0);
    cg_run_handle_wait(run_handle, on_done, &state);

    /* Poll until callback fires (the runtime is multi-threaded). */
    while (!atomic_load(&state.done)) {
        /* busy-spin — acceptable in a short example/test */
    }

    /* Assertions */
    if (state.status != CG_OK) {
        fprintf(stderr, "Pipeline failed with status %d\n", (int)state.status);
        exit(1);
    }

    if (state.output_count == 0) {
        fprintf(stderr,
                "FAIL: output_count == 0 — task list was not executed "
                "(regression: empty-task background pipeline bug)\n");
        exit(1);
    }

    if (state.result_value != 42) {
        fprintf(stderr,
                "FAIL: expected result 42, got %ld\n",
                (long)state.result_value);
        exit(1);
    }

    printf("Output count: %zu\n", state.output_count);
    printf("Result: %ld (expected 42)\n", (long)state.result_value);

    cg_value_destroy(input);
    cg_pipeline_destroy(pipeline);
    cg_task_context_destroy(ctx);
    cg_cancellation_handle_destroy(handle);
    cg_shutdown();

    printf("PASSED\n");
    return 0;
}
