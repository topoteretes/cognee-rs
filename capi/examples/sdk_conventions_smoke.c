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
 *   No sleeps and no unsynchronized reads.  The R1 verdict itself carries
 *   no timing assumption; the only clock in the file is the
 *   CALLBACK_DEADLINE_SECONDS failsafe below, which exists so an op that
 *   never calls back fails instead of hanging, and is set far above any
 *   legitimate completion so it cannot decide a verdict.
 *
 * Tests:
 *   1. cg_sdk_warm    — assert R1 (no re-entrant/caller-thread delivery),
 *      then wait on a condvar and assert the callback fired with CG_OK.
 *   2. cg_sdk_owner_id — same.
 *
 * Known gap, deliberate: nothing here asserts that the *initiating call*
 * returns before the work completes. An implementation that spawned and
 * then joined would deliver off the caller's thread and still pass. That
 * property is not specified anywhere — every capi mention of "D4" restates
 * the same R1 delivery rule and none of them promises a non-blocking
 * call — and the old pre-wait check only covered it as a side effect of
 * testing a stronger, unguaranteed property, which is exactly what made it
 * flaky. (The likelier regression, `rt.block_on(...)` in place of
 * `spawn`, runs the callback on the caller's thread and IS caught.) If the
 * non-blocking property is wanted, specify it in cognee_sdk.h first, then
 * test it deterministically.
 *
 * Threading model:
 *   All mutable state shared with the callback is guarded by `mu`; the
 *   mutex/condvar pair provides the memory barrier so the callback's
 *   writes are visible to the main thread after the wait.
 *
 *   Two fields are read outside `mu`, for different reasons:
 *     - `in_call` is read by the callback only after it has established it
 *       is running on the caller's own thread (the `&&` short-circuits), so
 *       that access is sequential, never concurrent.
 *     - `caller_tid` IS read unconditionally, on whatever thread the
 *       callback lands on. That read is safe by ordering rather than by
 *       exclusion: it is written before the op is initiated, and the
 *       runtime's own spawn synchronization establishes happens-before
 *       between the initiating call and the callback. It is never written
 *       again while an op is in flight.
 *   Do not add further unguarded fields on the strength of the first rule;
 *   it does not cover the second.
 *
 * Environment:
 *   MOCK_EMBEDDING=true set via JSON settings overlay (no network needed).
 *
 * Exit codes: 0 = all assertions passed, 1 = at least one failure.
 */

#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "cognee_sdk.h"

/* ── Shared state for custom callback ────────────────────────────────────── */

/*
 * Failsafe only. R1 is decided by thread identity with no timing
 * assumption; this bound exists so an op that never calls back fails
 * instead of hanging the whole C API check. Orders of magnitude above any
 * legitimate completion, so it cannot itself flake.
 */
#define CALLBACK_DEADLINE_SECONDS 60

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
    int         fire_count;  /* best-effort duplicate-fire check; see check_deferred */
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

    /* R1 evidence.  `caller_tid` is read on whatever thread we landed on;
     * that is safe by ordering (written before the op was initiated, and
     * the spawn establishes happens-before).  `in_call` is only consulted
     * once we have already established that *we are* that thread, in which
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
 * a correct async implementation may finish first on another core.  Nor
 * that the initiating call is non-blocking — see the "Known gap" note in
 * the file header.
 */
static void check_deferred(const CgSdk* sdk, SdkOpFn op, const char* op_name)
{
    OpSync s;
    op_sync_init(&s);

    /* Invoke the op.  R1: the callback must NOT fire synchronously from
     * inside this call.  The sentinel lets the callback detect exactly
     * that, on whichever thread it happens to run. */
    /* `caller_tid` was recorded by op_sync_init on this same thread. */
    s.in_call = 1;
    op(sdk, deferred_callback, &s);
    s.in_call = 0;

    /* ── Wait for the callback to fire ─────────────────────────────────── */
    /*
     * Bounded, so that an op which never calls back FAILS rather than
     * hanging: capi/scripts/check.sh runs this binary with no `timeout`
     * wrapper, so an unbounded wait would stall the whole C API check (and
     * its CI lane) until the job timeout. That is not hypothetical — both
     * ops have a documented early-return path that never invokes the
     * callback ("NULL -> no-op (null-check returns early)", cognee_sdk.h),
     * so any regression routing into one would convert a clean FAIL into a
     * hang.
     *
     * The deadline is a failsafe, not the correctness mechanism: R1 itself
     * is decided by thread identity, with no timing assumption. 60s is far
     * beyond any legitimate completion (these ops finish in milliseconds),
     * so it cannot produce a flaky failure the way the old pre-wait check
     * produced flaky failures.
     */
    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += CALLBACK_DEADLINE_SECONDS;

    int timed_out = 0;
    pthread_mutex_lock(&s.mu);
    while (s.fired == 0) {
        if (pthread_cond_timedwait(&s.cv, &s.mu, &deadline) == ETIMEDOUT) {
            timed_out = s.fired == 0;
            break;
        }
    }
    int reentrant_val = s.reentrant;
    int same_thread_val = s.on_caller_thread;
    int fire_count_val = s.fire_count;
    CgErrorCode code_val = s.code;
    pthread_mutex_unlock(&s.mu);

    if (timed_out) {
        fprintf(stderr,
                "FAIL: callback for %s never fired within %d seconds — the op "
                "returned without ever invoking it\n",
                op_name, CALLBACK_DEADLINE_SECONDS);
        g_failures++;
        op_sync_destroy(&s);
        return;
    }

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

    /*
     * Best-effort only, and deliberately not advertised as an exactly-once
     * check: `fire_count` is snapshotted the moment the first signal wakes
     * us, so it catches a duplicate that arrived *before* that point but
     * cannot see a later one. A genuinely late second callback would in
     * fact land on this frame after `op_sync_destroy` — undefined
     * behaviour, not an assertion failure. Detecting that properly needs an
     * `OpSync` that outlives the frame, which is not worth the machinery
     * here; the exactly-once guarantee is the implementation's
     * (`CgSdkResultCallback` is documented as "invoked exactly once").
     */
    ASSERT_EQ(fire_count_val, 1, "callback must not have fired more than once");
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
