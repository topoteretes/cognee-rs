/*
 * Regression test: async execution runs the task list, and a pipeline
 * shared with an in-flight run can be touched without aborting (SDK-131).
 *
 * Originally cg_pipeline_execute_in_background / cg_pipeline_execute_async
 * ran an empty pipeline because clone_pipeline() dropped the task list.
 * The fix shares the pipeline through an Arc — which made every
 * cg_pipeline_set_* / cg_pipeline_add_task call panic (and, under
 * panic = "abort", kill the host process) whenever a run still held that
 * Arc. This example covers both halves:
 *
 *   1. cg_pipeline_execute_async runs the task (output == 2 * input).
 *   2. A setter called while a run is in flight is refused with an error
 *      message instead of aborting.
 *   3. By the time the completion callback runs, the pipeline is free
 *      again: it can be mutated (even from the callback) and re-run.
 *   4. The same refusal applies during a blocking run.
 */
#include "common.h"
#include <stdatomic.h>

static atomic_int g_release; /* gates the "gated" task */
static atomic_int g_entered; /* set once the gated task is running */

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

static CgErrorCode add_one(const CgValue* input, const CgTaskContext* ctx,
                           void* user_data, CgValue** out) {
    (void)ctx;
    (void)user_data;
    int64_t val;
    CgErrorCode rc = cg_value_as_i64(input, &val);
    if (rc != CG_OK) return rc;
    *out = cg_value_from_i64(val + 1);
    return CG_OK;
}

/* Doubles its input, but only after the main thread releases it. */
static CgErrorCode gated_double(const CgValue* input, const CgTaskContext* ctx,
                                void* user_data, CgValue** out) {
    atomic_store(&g_entered, 1);
    while (!atomic_load(&g_release)) {
        /* busy-spin — acceptable in a short example/test */
    }
    return double_value(input, ctx, user_data, out);
}

static int g_blocking_refused; /* set by mutate_own_pipeline */

/* Doubles its input after trying to append a task to the pipeline running
 * it (passed as user_data) — which must be refused mid-run. */
static CgErrorCode mutate_own_pipeline(const CgValue* input, const CgTaskContext* ctx,
                                       void* user_data, CgValue** out) {
    cg_last_error_clear();
    cg_pipeline_add_task((CgPipeline*)user_data,
                         cg_task_info_new(cg_task_sync(add_one, NULL, NULL)));
    const char* msg = cg_last_error_message();
    g_blocking_refused = msg != NULL && strstr(msg, "in use") != NULL;
    return double_value(input, ctx, NULL, out);
}

typedef struct {
    int64_t     result_value;
    size_t      output_count;
    CgErrorCode status;
    CgPipeline* extend;         /* if set, the callback appends add_one to it */
    int         extend_refused; /* that append was refused as "in use" */
    atomic_int  done;
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
    if (state->extend != NULL) {
        /* Chaining from the completion callback: the run must already have
         * released the pipeline, or this append is refused. */
        cg_last_error_clear();
        cg_pipeline_add_task(state->extend,
                             cg_task_info_new(cg_task_sync(add_one, NULL, NULL)));
        /* Evaluate here: the message lives in this worker's thread-local
         * storage and is gone once the worker makes its next cg_* call. */
        const char* msg = cg_last_error_message();
        state->extend_refused = msg != NULL && strstr(msg, "in use") != NULL;
        state->extend = NULL;
    }
    atomic_store(&state->done, 1);
}

static void run_async(CgPipeline* pipeline, int64_t in, const CgTaskContext* ctx,
                      CallbackState* state) {
    CgValue* input = cg_value_from_i64(in);
    const CgValue* inputs[] = { input };
    state->result_value = 0;
    state->output_count = 0;
    state->status = CG_OK;
    atomic_store(&state->done, 0);
    cg_pipeline_execute_async(pipeline, inputs, 1, ctx, NULL, on_done, state);
    cg_value_destroy(input);
}

static void wait_done(CallbackState* state) {
    while (!atomic_load(&state->done)) {
        /* busy-spin */
    }
}

static void expect_result(const CallbackState* state, int64_t expected, const char* what) {
    if (state->status != CG_OK) {
        fprintf(stderr, "FAIL (%s): status %d\n", what, (int)state->status);
        exit(1);
    }
    if (state->output_count != 1) {
        fprintf(stderr, "FAIL (%s): output_count == %zu, expected 1 "
                        "(task list not executed?)\n", what, state->output_count);
        exit(1);
    }
    if (state->result_value != expected) {
        fprintf(stderr, "FAIL (%s): expected %ld, got %ld\n", what,
                (long)expected, (long)state->result_value);
        exit(1);
    }
    printf("%s: %ld (expected %ld)\n", what, (long)state->result_value, (long)expected);
}

int main(void) {
    (void)cognee_setup_logging();
    CHECK(cg_init());

    CgCancellationHandle* handle = NULL;
    CgTaskContext* ctx = NULL;
    CHECK(cg_task_context_mock(&handle, &ctx));

    CallbackState state = { 0 };
    atomic_init(&state.done, 0);

    /* 1. cg_pipeline_execute_async runs the task list. */
    CgPipeline* pipeline = cg_pipeline_new("async doubler");
    CgTaskInfo* info = cg_task_info_new(cg_task_sync(double_value, NULL, NULL));
    cg_task_info_set_name(info, "doubler");
    cg_pipeline_add_task(pipeline, info);

    /* 3. The completion callback itself appends a task. Before the fix the
     *    run's Arc was still alive while the callback ran, so this aborted
     *    the process (and, with the abort removed, would be refused). */
    state.extend = pipeline;
    run_async(pipeline, 21, ctx, &state);
    wait_done(&state);
    expect_result(&state, 42, "execute_async");
    if (state.extend_refused) {
        fprintf(stderr, "FAIL: pipeline still in use inside the completion callback\n");
        exit(1);
    }

    /* ...and from the caller's thread once the callback has fired. */
    cg_pipeline_set_name(pipeline, "async doubler + 1");

    run_async(pipeline, 21, ctx, &state);
    wait_done(&state);
    expect_result(&state, 43, "re-run after mutation");
    cg_pipeline_destroy(pipeline);

    /* 2. Mutating a pipeline that an in-flight run still holds is refused,
     *    not a process abort. */
    pipeline = cg_pipeline_new("gated doubler");
    info = cg_task_info_new(cg_task_sync(gated_double, NULL, NULL));
    cg_pipeline_add_task(pipeline, info);

    atomic_store(&g_release, 0);
    atomic_store(&g_entered, 0);
    run_async(pipeline, 21, ctx, &state);
    while (!atomic_load(&g_entered)) {
        /* busy-spin until the run holds the pipeline */
    }

    cg_last_error_clear();
    cg_pipeline_set_name(pipeline, "renamed mid-run");
    const char* msg = cg_last_error_message();
    if (msg == NULL || strstr(msg, "in use") == NULL) {
        fprintf(stderr, "FAIL: mid-run cg_pipeline_set_name did not report an error (got: %s)\n",
                msg ? msg : "(null)");
        exit(1);
    }
    printf("Mid-run mutation refused: %s\n", msg);

    /* add_task takes ownership of info even when refused — no leak, no abort. */
    cg_pipeline_add_task(pipeline, cg_task_info_new(cg_task_sync(add_one, NULL, NULL)));

    atomic_store(&g_release, 1);
    wait_done(&state);
    /* The refused add_one must not have leaked into the in-flight run. */
    expect_result(&state, 42, "gated run");

    cg_pipeline_destroy(pipeline);

    /* 4. Blocking runs hold the pipeline too: a mutation made during the run
     *    (here, by the running task itself) is refused rather than racing
     *    the executor's walk over the task list. */
    pipeline = cg_pipeline_new("self-mutating doubler");
    cg_pipeline_add_task(pipeline,
                         cg_task_info_new(cg_task_sync(mutate_own_pipeline, pipeline, NULL)));
    {
        CgValue* input = cg_value_from_i64(21);
        const CgValue* inputs[] = { input };
        CgPipelineRunResult* result = NULL;
        CHECK(cg_pipeline_execute_blocking(pipeline, inputs, 1, ctx, NULL, &result));
        cg_value_destroy(input);
        CgValue* out = cg_run_result_output_at(result, 0);
        int64_t val = 0;
        CHECK(cg_value_as_i64(out, &val));
        cg_value_destroy(out);
        cg_run_result_destroy(result);
        if (!g_blocking_refused || val != 42) {
            fprintf(stderr, "FAIL: blocking mid-run mutation refused=%d, result %ld "
                            "(expected refused=1, 42)\n", g_blocking_refused, (long)val);
            exit(1);
        }
        printf("Blocking mid-run mutation refused, result: %ld\n", (long)val);
    }
    /* ...and mutable again once the blocking call has returned. */
    cg_last_error_clear();
    cg_pipeline_set_name(pipeline, "after blocking run");
    if (cg_last_error_message() != NULL) {
        fprintf(stderr, "FAIL: pipeline still in use after the blocking run returned\n");
        exit(1);
    }
    cg_pipeline_destroy(pipeline);

    cg_task_context_destroy(ctx);
    cg_cancellation_handle_destroy(handle);
    cg_shutdown();

    printf("PASSED\n");
    return 0;
}
