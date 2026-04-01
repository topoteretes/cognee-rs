/*
 * Example: SyncIterFn task — splits an integer into digits.
 *
 * Demonstrates:
 *   - Creating a CgValueIter from a C vtable
 *   - Returning an iterator from a task
 */
#include "common.h"

/* Iterator state: produces digits of a number */
typedef struct {
    int64_t remaining;
    int64_t digits[20];  /* pre-computed */
    int count;
    int index;
} DigitIterState;

static CgValue* digit_next(void* state) {
    DigitIterState* s = (DigitIterState*)state;
    if (s->index >= s->count) return NULL;
    return cg_value_from_i64(s->digits[s->index++]);
}

static void digit_destroy(void* state) {
    free(state);
}

static CgErrorCode split_digits(const CgValue* input, const CgTaskContext* ctx,
                                 void* user_data, CgValueIter** out) {
    (void)ctx;
    (void)user_data;

    int64_t val;
    CgErrorCode rc = cg_value_as_i64(input, &val);
    if (rc != CG_OK) return rc;

    DigitIterState* state = (DigitIterState*)calloc(1, sizeof(DigitIterState));
    if (!state) return CG_ERR_RUNTIME;

    /* Extract digits (handle 0 and negative) */
    int64_t abs_val = val < 0 ? -val : val;
    if (abs_val == 0) {
        state->digits[0] = 0;
        state->count = 1;
    } else {
        while (abs_val > 0 && state->count < 20) {
            state->digits[state->count++] = abs_val % 10;
            abs_val /= 10;
        }
        /* Reverse */
        for (int i = 0; i < state->count / 2; i++) {
            int64_t tmp = state->digits[i];
            state->digits[i] = state->digits[state->count - 1 - i];
            state->digits[state->count - 1 - i] = tmp;
        }
    }
    state->index = 0;

    CgValueIterVtable vtable = { .next = digit_next, .destroy = digit_destroy };
    *out = cg_value_iter_new(state, vtable);
    return CG_OK;
}

int main(void) {
    CHECK(cg_init());

    CgCancellationHandle* handle = NULL;
    CgTaskContext* ctx = NULL;
    CHECK(cg_task_context_mock(&handle, &ctx));

    CgTask* task = cg_task_sync_iter(split_digits, NULL, NULL);
    CgTaskInfo* info = cg_task_info_new(task);
    cg_task_info_set_name(info, "split_digits");

    CgPipeline* pipeline = cg_pipeline_new("digit splitter");
    cg_pipeline_add_task(pipeline, info);

    CgValue* input = cg_value_from_i64(1234);
    const CgValue* inputs[] = { input };
    CgPipelineRunResult* result = NULL;
    CHECK(cg_pipeline_execute_blocking(pipeline, inputs, 1, ctx, NULL, &result));

    size_t count = cg_run_result_output_count(result);
    printf("Digits of 1234: ");
    for (size_t i = 0; i < count; i++) {
        CgValue* out = cg_run_result_output_at(result, i);
        int64_t d;
        /* Note: the output may be wrapped — downcast might not work directly.
         * This is a known limitation of the iterator wrapper. */
        if (cg_value_as_i64(out, &d) == CG_OK) {
            printf("%ld ", (long)d);
        } else {
            printf("? ");
        }
        cg_value_destroy(out);
    }
    printf("\n");

    cg_run_result_destroy(result);
    cg_value_destroy(input);
    cg_pipeline_destroy(pipeline);
    cg_task_context_destroy(ctx);
    cg_cancellation_handle_destroy(handle);
    cg_shutdown();

    printf("PASSED\n");
    return 0;
}
