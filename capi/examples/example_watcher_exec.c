/*
 * Regression test: every execution mode reports to the caller's watcher.
 *
 * cg_pipeline_execute_in_background and cg_pipeline_execute_async used to
 * ignore their `watcher` argument and run with a no-op watcher, so a C
 * caller silently got no progress events in exactly the modes where it
 * cannot observe the run any other way.
 *
 * For each of blocking / background / async this example:
 *   1. Runs a one-task pipeline with a counting watcher.
 *   2. For background/async, destroys the watcher handle right after the
 *      execute call returns — the run must keep the watcher alive.
 *   3. Asserts the pipeline-level Started/Succeeded events, the run-completed
 *      event, and that the vtable's destroy fired exactly once, before the
 *      completion callback.
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

typedef struct {
    atomic_int pipeline_started;
    atomic_int pipeline_succeeded;
    atomic_int run_completed;
    atomic_int task_succeeded;
    atomic_int destroyed;
} Counters;

static void on_pipeline(void* state, const char* pipeline_id, int status_tag,
                        size_t count_or_index, const char* detail) {
    (void)pipeline_id;
    (void)count_or_index;
    (void)detail;
    Counters* c = (Counters*)state;
    if (status_tag == 0) atomic_fetch_add(&c->pipeline_started, 1);
    if (status_tag == 1) atomic_fetch_add(&c->pipeline_succeeded, 1);
}

static void on_task(void* state, const char* pipeline_id, size_t task_index,
                    const char* task_name, size_t total_tasks, int status_tag,
                    uint32_t attempts, const char* detail) {
    (void)pipeline_id;
    (void)task_index;
    (void)task_name;
    (void)total_tasks;
    (void)attempts;
    (void)detail;
    if (status_tag == 2) atomic_fetch_add(&((Counters*)state)->task_succeeded, 1);
}

static void on_run_completed(void* state, const char* run_id, size_t output_count) {
    (void)run_id;
    (void)output_count;
    atomic_fetch_add(&((Counters*)state)->run_completed, 1);
}

static void on_destroy(void* state) {
    atomic_fetch_add(&((Counters*)state)->destroyed, 1);
}

static CgPipelineWatcher* counting_watcher(Counters* c) {
    CgPipelineWatcherVtable vt;
    memset(&vt, 0, sizeof vt);
    vt.on_pipeline = on_pipeline;
    vt.on_task = on_task;
    vt.on_run_completed = on_run_completed;
    vt.destroy = on_destroy;
    return cg_pipeline_watcher_new(c, vt);
}

typedef struct {
    Counters*   counters;
    CgErrorCode status;
    size_t      output_count;
    int         destroyed_at_callback;
    atomic_int  done;
} CallbackState;

static void on_done(CgErrorCode status, CgPipelineRunResult* result, void* data) {
    CallbackState* s = (CallbackState*)data;
    s->status = status;
    s->destroyed_at_callback = atomic_load(&s->counters->destroyed);
    if (result != NULL) {
        s->output_count = cg_run_result_output_count(result);
        cg_run_result_destroy(result);
    }
    atomic_store(&s->done, 1);
}

static void check_counters(const char* mode, Counters* c) {
    int ok = atomic_load(&c->pipeline_started) == 1 &&
             atomic_load(&c->pipeline_succeeded) == 1 &&
             atomic_load(&c->task_succeeded) == 1 &&
             atomic_load(&c->run_completed) == 1 &&
             atomic_load(&c->destroyed) == 1;
    printf("%s: started=%d succeeded=%d task_succeeded=%d run_completed=%d destroyed=%d\n",
           mode, atomic_load(&c->pipeline_started), atomic_load(&c->pipeline_succeeded),
           atomic_load(&c->task_succeeded), atomic_load(&c->run_completed),
           atomic_load(&c->destroyed));
    if (!ok) {
        fprintf(stderr, "FAIL (%s): watcher did not see the run "
                        "(expected 1 of each event and one destroy)\n", mode);
        exit(1);
    }
}

static void check_callback(const char* mode, const CallbackState* s) {
    if (s->status != CG_OK || s->output_count != 1) {
        fprintf(stderr, "FAIL (%s): status %d, output_count %zu\n", mode,
                (int)s->status, s->output_count);
        exit(1);
    }
    if (s->destroyed_at_callback != 1) {
        fprintf(stderr, "FAIL (%s): watcher destroy had not fired by the "
                        "completion callback\n", mode);
        exit(1);
    }
}

static void wait_done(CallbackState* s) {
    while (!atomic_load(&s->done)) {
        /* busy-spin — acceptable in a short example/test */
    }
}

int main(void) {
    (void)cognee_setup_logging();
    CHECK(cg_init());

    CgCancellationHandle* handle = NULL;
    CgTaskContext* ctx = NULL;
    CHECK(cg_task_context_mock(&handle, &ctx));

    CgPipeline* pipeline = cg_pipeline_new("watched doubler");
    cg_pipeline_add_task(pipeline, cg_task_info_new(cg_task_sync(double_value, NULL, NULL)));

    CgValue* input = cg_value_from_i64(21);
    const CgValue* inputs[] = { input };

    /* Blocking — the baseline that always worked. */
    {
        Counters c = { 0 };
        CgPipelineWatcher* w = counting_watcher(&c);
        CgPipelineRunResult* result = NULL;
        CHECK(cg_pipeline_execute_blocking(pipeline, inputs, 1, ctx, w, &result));
        cg_run_result_destroy(result);
        cg_pipeline_watcher_destroy(w);
        check_counters("blocking", &c);
    }

    /* Background — watcher handle destroyed while the run may be in flight. */
    {
        Counters c = { 0 };
        CallbackState s = { .counters = &c };
        atomic_init(&s.done, 0);
        CgPipelineWatcher* w = counting_watcher(&c);
        CgPipelineRunHandle* rh = cg_pipeline_execute_in_background(pipeline, inputs, 1, ctx, w);
        cg_pipeline_watcher_destroy(w);
        if (rh == NULL) {
            fprintf(stderr, "FAIL (background): %s\n", cg_last_error_message());
            exit(1);
        }
        cg_run_handle_wait(rh, on_done, &s);
        wait_done(&s);
        check_callback("background", &s);
        check_counters("background", &c);
    }

    /* Async — same, with the completion callback. */
    {
        Counters c = { 0 };
        CallbackState s = { .counters = &c };
        atomic_init(&s.done, 0);
        CgPipelineWatcher* w = counting_watcher(&c);
        cg_pipeline_execute_async(pipeline, inputs, 1, ctx, w, on_done, &s);
        cg_pipeline_watcher_destroy(w);
        wait_done(&s);
        check_callback("async", &s);
        check_counters("async", &c);
    }

    /* NULL watcher still works in the non-blocking modes. */
    {
        Counters c = { 0 };
        CallbackState s = { .counters = &c };
        atomic_init(&s.done, 0);
        atomic_store(&c.destroyed, 1); /* no watcher to destroy */
        cg_pipeline_execute_async(pipeline, inputs, 1, ctx, NULL, on_done, &s);
        wait_done(&s);
        check_callback("async, NULL watcher", &s);
        printf("async, NULL watcher: ok\n");
    }

    cg_value_destroy(input);
    cg_pipeline_destroy(pipeline);
    cg_task_context_destroy(ctx);
    cg_cancellation_handle_destroy(handle);
    cg_shutdown();

    printf("PASSED\n");
    return 0;
}
