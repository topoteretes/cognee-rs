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
 *   5. ...and during a background run, which also frees the pipeline by the
 *      time cg_run_handle_wait's callback fires.
 *
 * Every setter is checked for refusal, including set_data_id_fn, which must
 * still free its user_data (exactly once) when refused. All waits are
 * bounded, so a callback that never fires fails the test instead of hanging
 * CI.
 */
#include "common.h"
#include <stdatomic.h>
#include <time.h>

#define WAIT_TIMEOUT_SECS 60

/* Spin until *flag is set; exit(1) after WAIT_TIMEOUT_SECS. */
static void spin_until(atomic_int* flag, const char* what) {
    time_t deadline = time(NULL) + WAIT_TIMEOUT_SECS;
    while (!atomic_load(flag)) {
        if (time(NULL) > deadline) {
            fprintf(stderr, "FAIL: timed out after %ds waiting for %s\n",
                    WAIT_TIMEOUT_SECS, what);
            exit(1);
        }
    }
}

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
    spin_until(&state->done, "the completion callback");
}

static atomic_int g_data_id_ud_destroyed;

static bool data_id_fn(const CgValue* v, char* buf, size_t buf_len, size_t* written,
                       void* user_data) {
    (void)v;
    (void)buf;
    (void)buf_len;
    (void)user_data;
    *written = 0;
    return false;
}

static void data_id_ud_destroy(void* user_data) {
    (void)user_data;
    atomic_fetch_add(&g_data_id_ud_destroyed, 1);
}

/* Fails unless the last cg_* call on this thread was refused as "in use". */
static void expect_refused(const char* what) {
    const char* msg = cg_last_error_message();
    if (msg == NULL || strstr(msg, "in use") == NULL) {
        fprintf(stderr, "FAIL: mid-run %s was not refused (last error: %s)\n", what,
                msg ? msg : "(null)");
        exit(1);
    }
}

/* Every setter must be refused while a run holds `pipeline`. */
static void expect_all_setters_refused(CgPipeline* pipeline) {
    CgRetryDelaySpec delay = { CG_RETRY_DELAY_CONSTANT, 1, 1 };

    cg_last_error_clear();
    cg_pipeline_set_name(pipeline, "renamed mid-run");
    expect_refused("cg_pipeline_set_name");

    /* add_task takes ownership of info even when refused — no leak, no abort. */
    cg_last_error_clear();
    cg_pipeline_add_task(pipeline, cg_task_info_new(cg_task_sync(add_one, NULL, NULL)));
    expect_refused("cg_pipeline_add_task");

    cg_last_error_clear();
    cg_pipeline_set_batch_size(pipeline, 7);
    expect_refused("cg_pipeline_set_batch_size");

    cg_last_error_clear();
    cg_pipeline_set_concurrency(pipeline, 3);
    expect_refused("cg_pipeline_set_concurrency");

    cg_last_error_clear();
    cg_pipeline_set_retry_none(pipeline);
    expect_refused("cg_pipeline_set_retry_none");

    cg_last_error_clear();
    cg_pipeline_set_retry_limited(pipeline, 2, delay);
    expect_refused("cg_pipeline_set_retry_limited");

    /* set_data_id_fn takes ownership of user_data: a refused call must still
     * run destroy_ud, exactly once, before returning. */
    atomic_store(&g_data_id_ud_destroyed, 0);
    cg_last_error_clear();
    cg_pipeline_set_data_id_fn(pipeline, data_id_fn, NULL, data_id_ud_destroy);
    expect_refused("cg_pipeline_set_data_id_fn");
    if (atomic_load(&g_data_id_ud_destroyed) != 1) {
        fprintf(stderr, "FAIL: refused cg_pipeline_set_data_id_fn ran destroy_ud %d times "
                        "(expected 1)\n", atomic_load(&g_data_id_ud_destroyed));
        exit(1);
    }
    printf("All setters refused mid-run (destroy_ud ran once)\n");
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
    spin_until(&g_entered, "the gated task to start (async)");

    expect_all_setters_refused(pipeline);

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

    /* 5. Background runs: refused mid-run, and free again by the time
     *    cg_run_handle_wait's callback fires — the callback appends a task,
     *    and a re-run must see it. */
    pipeline = cg_pipeline_new("gated background doubler");
    cg_pipeline_add_task(pipeline, cg_task_info_new(cg_task_sync(gated_double, NULL, NULL)));

    atomic_store(&g_release, 0);
    atomic_store(&g_entered, 0);
    {
        CgValue* input = cg_value_from_i64(21);
        const CgValue* inputs[] = { input };
        CgPipelineRunHandle* rh =
            cg_pipeline_execute_in_background(pipeline, inputs, 1, ctx, NULL);
        cg_value_destroy(input);
        if (rh == NULL) {
            fprintf(stderr, "FAIL: cg_pipeline_execute_in_background: %s\n",
                    cg_last_error_message());
            exit(1);
        }
        spin_until(&g_entered, "the gated task to start (background)");

        expect_all_setters_refused(pipeline);

        state.result_value = 0;
        state.output_count = 0;
        state.extend = pipeline;
        atomic_store(&state.done, 0);
        cg_run_handle_wait(rh, on_done, &state);
        atomic_store(&g_release, 1);
        wait_done(&state);
        expect_result(&state, 42, "background gated run");
        if (state.extend_refused) {
            fprintf(stderr, "FAIL: pipeline still in use inside cg_run_handle_wait's callback\n");
            exit(1);
        }
    }
    {
        /* The run's gated task plus the add_one appended from the callback. */
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
        if (val != 43) {
            fprintf(stderr, "FAIL: re-run after background wait: expected 43, got %ld\n",
                    (long)val);
            exit(1);
        }
        printf("re-run after background wait: %ld (expected 43)\n", (long)val);
    }
    cg_pipeline_destroy(pipeline);

    cg_task_context_destroy(ctx);
    cg_cancellation_handle_destroy(handle);
    cg_shutdown();

    printf("PASSED\n");
    return 0;
}
