/*
 * sdk_conventions_smoke.c — Phase 2 deferred-delivery assertion (R1).
 *
 * Verifies that the callback passed to any cg_sdk_* async op is NEVER
 * invoked synchronously from the initiating call — it always fires
 * asynchronously from a tokio worker thread.
 *
 * What R1 actually says (cognee_sdk.h, "Deferred-callback rule (R1)"):
 *
 *   "The callback ... is **always** invoked asynchronously — never from
 *    inside the initiating `cg_sdk_*` call itself.  This matches the
 *    libuv / gRPC / ORT convention and avoids re-entrancy surprises in
 *    event loops."
 *
 *   R1 is therefore a statement about *where* the callback runs: it must
 *   never be invoked re-entrantly, on the caller's own thread, while the
 *   initiating call is still on that thread's stack.  R1 is NOT a promise
 *   that the operation is still incomplete when the initiating call
 *   returns — a genuinely asynchronous op may well have been picked up by
 *   a tokio worker and run to completion on another core before the
 *   caller's next instruction retires.  Asserting "the flag is still 0
 *   right after the call" tests that stronger, *unguaranteed* property and
 *   is a race: it cannot distinguish a correct fast async completion from
 *   an actual R1 violation.
 *
 * How we test it deterministically:
 *   The caller records its own thread id and raises an `in_call` sentinel
 *   around the initiating call.  The callback compares pthread_self()
 *   against that thread id:
 *     - different thread  → deferred delivery, R1 satisfied (this is the
 *       only outcome a tokio-spawned callback can produce, regardless of
 *       how fast it runs);
 *     - same thread while `in_call` is set → re-entrant synchronous
 *       invocation, a hard R1 violation;
 *     - same thread after the call returned → also a violation (the C API
 *       has no run loop to legitimately dispatch onto the caller).
 *   No sleeps, no timing assumptions, no unsynchronized reads.
 *
 * Tests:
 *   1. cg_sdk_warm    — assert R1 (no re-entrant/caller-thread delivery),
 *      then wait on a condvar and assert the callback fired exactly once
 *      with CG_OK.
 *   2. cg_sdk_owner_id — same.
 *
 * Threading model:
 *   All mutable state shared with the callback is guarded by `mu`; the
 *   mutex/condvar pair provides the memory barrier so the callback's
 *   writes are visible to the main thread after the wait.  The only
 *   unguarded field is `caller_tid`/`in_call`, which are written by the
 *   caller thread and read by the callback *only* when the callback is
 *   running on that same thread — i.e. never concurrently.
 *
 * Environment:
 *   MOCK_EMBEDDING=true set via JSON settings overlay (no network needed).
 *
 * Exit codes: 0 = all assertions passed, 1 = at least one failure.
 */

#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cognee_sdk.h"

/* ── Shared state for custom callback ────────────────────────────────────── */

static int g_failures = 0;

#define ASSERT(cond, msg)                                          \
    do {                                                           \
        if (!(cond)) {                                             \
            fprintf(stderr, "FAIL [%s:%d]: %s\n",                 \
                    __FILE__, __LINE__, (msg));                    \
            g_failures++;                                          \
        }                                                          \
    } while (0)

#define ASSERT_EQ(a, b, msg)                                              \
    do {                                                                   \
        if ((a) != (b)) {                                                  \
            fprintf(stderr, "FAIL [%s:%d]: %s (got %d, want %d)\n",       \
                    __FILE__, __LINE__, (msg), (int)(a), (int)(b));        \
            g_failures++;                                                  \
        }                                                                  \
    } while (0)

/* Per-op sync state passed as user_data to the custom callback. */
typedef struct {
    pthread_mutex_t mu;
    pthread_cond_t  cv;

    /* ── Guarded by mu ───────────────────────────────────────────────── */
    int         fired;       /* 0 = not yet, 1 = callback fired */
    int         fire_count;  /* callback must fire exactly once */
    CgErrorCode code;
    int         on_caller_thread;  /* R1 violation: ran on the caller's thread */
    int         reentrant;         /* R1 violation: ... inside the initiating call */

    /* ── R1 sentinel: written by the caller thread only ──────────────── */
    pthread_t    caller_tid;  /* set before the initiating call */
    volatile int in_call;     /* 1 while the initiating call is on our stack */
} OpSync;

static void op_sync_init(OpSync* s)
{
    pthread_mutex_init(&s->mu, NULL);
    pthread_cond_init(&s->cv, NULL);
    s->fired            = 0;
    s->fire_count       = 0;
    s->code             = CG_OK;
    s->on_caller_thread = 0;
    s->reentrant        = 0;
    s->caller_tid       = pthread_self();
    s->in_call          = 0;
}

static void op_sync_destroy(OpSync* s)
{
    pthread_mutex_destroy(&s->mu);
    pthread_cond_destroy(&s->cv);
}

/**
 * Custom callback: records where it ran (R1 evidence), sets the flag and
 * signals the condvar so the main thread can unblock.  Must fire on a
 * tokio worker thread, never on the caller's thread (R1).
 */
static void deferred_callback(CgErrorCode code, const char* result_json,
                              const char* error_message, void* user_data)
{
    (void)result_json;
    (void)error_message;
    OpSync* s = (OpSync*)user_data;

    /* R1 evidence.  Reading caller_tid/in_call here is race-free: they are
     * written only by the caller thread, and `in_call` is only consulted
     * when we have already established that *we are* that thread, in which
     * case the access is sequential, not concurrent. */
    int same_thread = pthread_equal(pthread_self(), s->caller_tid) != 0;
    int reentrant   = same_thread && s->in_call != 0;

    /* No deadlock even on a synchronous (R1-violating) invocation: the
     * caller does not hold `mu` while the initiating call is in flight. */
    pthread_mutex_lock(&s->mu);
    if (same_thread) {
        s->on_caller_thread = 1;
        if (reentrant) {
            s->reentrant = 1;
        }
    }
    s->code = code;
    s->fired = 1;
    s->fire_count++;
    pthread_cond_signal(&s->cv);
    pthread_mutex_unlock(&s->mu);
}

/* ── Helpers ──────────────────────────────────────────────────────────────── */

typedef void (*SdkOpFn)(const CgSdk*, CgSdkResultCallback, void*);

/**
 * Run one async op and verify the deferred-delivery guarantee (R1):
 *   - The callback must not be invoked on the caller's thread — and in
 *     particular must not be invoked re-entrantly from inside the
 *     initiating call.
 *   - The callback must fire exactly once, with CG_OK.
 *
 * Deliberately NOT asserted: that the callback has not yet *completed*
 * when the initiating call returns.  That is not what R1 guarantees, and
 * a correct async implementation may finish first on another core — see
 * the file header.
 */
static void check_deferred(const CgSdk* sdk, SdkOpFn op, const char* op_name)
{
    OpSync s;
    op_sync_init(&s);

    /* Invoke the op.  R1: the callback must NOT fire synchronously from
     * inside this call.  The sentinel lets the callback detect exactly
     * that, on whichever thread it happens to run. */
    s.caller_tid = pthread_self();
    s.in_call    = 1;
    op(sdk, deferred_callback, &s);
    s.in_call    = 0;

    /* ── Wait for the callback to fire ─────────────────────────────────── */
    pthread_mutex_lock(&s.mu);
    while (s.fired == 0) {
        pthread_cond_wait(&s.cv, &s.mu);
    }
    int reentrant_val = s.reentrant;
    int same_thread_val = s.on_caller_thread;
    int fire_count_val = s.fire_count;
    CgErrorCode code_val = s.code;
    pthread_mutex_unlock(&s.mu);

    /* ── Assert: delivery was deferred (R1) ─────────────────────────────── */
    if (reentrant_val) {
        fprintf(stderr,
                "FAIL [R1]: callback for %s was invoked SYNCHRONOUSLY — "
                "re-entrantly on the caller's thread from inside the "
                "initiating call\n",
                op_name);
        g_failures++;
    } else if (same_thread_val) {
        fprintf(stderr,
                "FAIL [R1]: callback for %s ran on the CALLER'S thread "
                "(delivery must happen on a tokio worker thread)\n",
                op_name);
        g_failures++;
    } else {
        printf("  R1 deferred-delivery OK for %s "
               "(callback ran off the caller's thread, not re-entrantly)\n",
               op_name);
    }

    ASSERT_EQ(fire_count_val, 1, "callback must fire exactly once");
    ASSERT_EQ(code_val, CG_OK, "op must complete with CG_OK");

    op_sync_destroy(&s);
}

/* ── Main ─────────────────────────────────────────────────────────────────── */

int main(void)
{
    /* ── Runtime init ────────────────────────────────────────────────────── */
    CgErrorCode rc = cg_init();
    ASSERT_EQ(rc, CG_OK, "cg_init() must succeed");
    if (rc != CG_OK) return 1;

    /* ── Create SDK handle (mock embedding, no network) ──────────────────── */
    /* snake_case to match cognee ConfigManager dispatch keys.
     * vector_db_provider=mock selects MockVectorDB (testing feature)
     * since T4 moved the Qdrant adapter to the closed cognee-vector-qdrant
     * crate. T5 will introduce a brute-force default. */
    const char* settings_json =
        "{"
        "  \"embedding_provider\": \"mock\","
        "  \"llm_api_key\": \"dummy-key-for-smoke-test\","
        "  \"vector_db_provider\": \"mock\""
        "}";

    CgSdk* sdk = cg_sdk_new(settings_json);
    ASSERT(sdk != NULL, "cg_sdk_new must return non-NULL");
    if (!sdk) {
        fprintf(stderr, "  last error: %s\n",
                cg_last_error_message() ? cg_last_error_message() : "(none)");
        cg_shutdown();
        return 1;
    }

    /* ── Test 1: cg_sdk_warm deferred delivery (R1) ──────────────────────── */
    printf("=== Test 1: cg_sdk_warm deferred-delivery (R1) ===\n");
    check_deferred(sdk, cg_sdk_warm, "cg_sdk_warm");

    /* ── Test 2: cg_sdk_owner_id deferred delivery (R1) ─────────────────── */
    printf("=== Test 2: cg_sdk_owner_id deferred-delivery (R1) ===\n");
    check_deferred(sdk, cg_sdk_owner_id, "cg_sdk_owner_id");

    /* ── Cleanup ─────────────────────────────────────────────────────────── */
    cg_sdk_destroy(sdk);
    cg_shutdown();

    /* ── Result ──────────────────────────────────────────────────────────── */
    if (g_failures == 0) {
        printf("\nPASSED (sdk_conventions_smoke)\n");
        return 0;
    } else {
        fprintf(stderr, "\nFAILED: %d assertion(s) failed\n", g_failures);
        return 1;
    }
}
