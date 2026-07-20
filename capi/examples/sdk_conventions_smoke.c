/*
 * sdk_conventions_smoke.c — Phase 2 deferred-delivery assertion (R1).
 *
 * Verifies that the callback passed to any cg_sdk_* async op is NEVER
 * invoked synchronously from the initiating call — it always fires
 * asynchronously from a tokio worker thread.
 *
 * Tests:
 *   1. Call cg_sdk_warm; assert flag is NOT set immediately after the call
 *      returns (before the callback fires).
 *   2. Wait for the callback via a pthread condvar; assert flag IS set after
 *      the condvar wait unblocks (callback completed).
 *   3. Repeat for cg_sdk_owner_id.
 *
 * Threading model:
 *   The flag is written by the tokio worker thread (inside the callback)
 *   and read by the main thread.  A pthread mutex + condvar provide the
 *   memory barrier so that the write is visible after the condvar wait.
 *
 *   Key invariant (R1): if the callback were called synchronously, the flag
 *   would already be set by the time the initiating cg_sdk_* call returns.
 *   We assert it is 0 at that point, then wait and assert it becomes 1.
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
    /* volatile: also visible to the main thread outside the mutex in the
     * pre-wait assertion.  The mutex/cond provides the barrier for the
     * post-wait read. */
    volatile int    fired;   /* 0 = not yet, 1 = callback fired */
    CgErrorCode     code;
} OpSync;

static void op_sync_init(OpSync* s)
{
    pthread_mutex_init(&s->mu, NULL);
    pthread_cond_init(&s->cv, NULL);
    s->fired = 0;
    s->code  = CG_OK;
}

static void op_sync_destroy(OpSync* s)
{
    pthread_mutex_destroy(&s->mu);
    pthread_cond_destroy(&s->cv);
}

/**
 * Custom callback: sets the flag and signals the condvar so the main
 * thread can unblock.  Fires on a tokio worker thread (R1).
 */
static void deferred_callback(CgErrorCode code, const char* result_json,
                              const char* error_message, void* user_data)
{
    (void)result_json;
    (void)error_message;
    OpSync* s = (OpSync*)user_data;
    pthread_mutex_lock(&s->mu);
    s->code  = code;
    s->fired = 1;
    pthread_cond_signal(&s->cv);
    pthread_mutex_unlock(&s->mu);
}

/* ── Helpers ──────────────────────────────────────────────────────────────── */

typedef void (*SdkOpFn)(const CgSdk*, CgSdkResultCallback, void*);

/**
 * Run one async op and verify the deferred-delivery guarantee (R1):
 *   - The flag must be 0 immediately after the op call returns.
 *   - The flag must be 1 after the condvar wait unblocks (callback fired).
 */
static void check_deferred(const CgSdk* sdk, SdkOpFn op, const char* op_name)
{
    OpSync s;
    op_sync_init(&s);

    /* Invoke the op.  R1: the callback must NOT fire synchronously from
     * inside this call. */
    op(sdk, deferred_callback, &s);

    /* ── Assert: flag is still 0 immediately after the initiating call ── */
    /*
     * No sleep is needed: we are checking the state right after the call
     * returns on this thread.  If the callback were invoked synchronously
     * (R1 violation), s.fired would already be 1 here.
     */
    if (s.fired != 0) {
        fprintf(stderr,
                "FAIL [R1]: callback for %s was fired SYNCHRONOUSLY "
                "(fired=%d, want 0 before condvar wait)\n",
                op_name, s.fired);
        g_failures++;
    } else {
        printf("  R1 pre-wait check OK for %s (fired=0 after initiating call)\n",
               op_name);
    }

    /* ── Wait for the callback to fire ─────────────────────────────────── */
    pthread_mutex_lock(&s.mu);
    while (s.fired == 0) {
        pthread_cond_wait(&s.cv, &s.mu);
    }
    int fired_val = s.fired;
    CgErrorCode code_val = s.code;
    pthread_mutex_unlock(&s.mu);

    /* ── Assert: flag is now 1 ──────────────────────────────────────────── */
    if (fired_val != 1) {
        fprintf(stderr,
                "FAIL [R1]: callback for %s was NOT fired after condvar wait "
                "(fired=%d, want 1)\n",
                op_name, fired_val);
        g_failures++;
    } else {
        printf("  R1 post-wait check OK for %s (fired=1 after condvar wait)\n",
               op_name);
    }

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
