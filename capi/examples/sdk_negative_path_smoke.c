/*
 * sdk_negative_path_smoke.c — Phase 2 negative-path error delivery tests.
 *
 * Tests (no network required):
 *
 *   1. Sync bad JSON to cg_sdk_new:
 *      - Pass malformed JSON as settings_json.
 *      - Assert return value is NULL.
 *      - Assert cg_last_error_message() is non-null.
 *
 *   2. Waiter single-use guard (CG_ERR_SDK_VALIDATION on second wait):
 *      - Create a waiter, run an op, call wait once (succeeds).
 *      - Call wait again on the same waiter.
 *      - Assert second wait returns CG_ERR_SDK_VALIDATION (14).
 *
 *   3. error_message forwarded to calling-thread last-error slot:
 *      - TODO: once spawn_sdk_op is used by an op that can fail with a
 *        deliberate bad input, verify that cg_sdk_waiter_wait sets
 *        cg_last_error_message() on the calling thread when the op fails.
 *        For now, the single-use guard (test 2) exercises the same
 *        cg_sdk_waiter_wait last-error path (it calls set_last_error on
 *        the calling thread with the single-use message).
 *
 * Environment:
 *   MOCK_EMBEDDING=true set via JSON settings overlay (no network needed).
 *
 * Exit codes: 0 = all assertions passed, 1 = at least one failure.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cognee_sdk.h"

/* ── Helpers ──────────────────────────────────────────────────────────────── */

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

typedef void (*SdkOpFn)(const CgSdk*, CgSdkResultCallback, void*);

static CgErrorCode run_via_waiter(const CgSdk* sdk, SdkOpFn op,
                                  char** out_result)
{
    CgSdkWaiter* w = cg_sdk_waiter_new();
    if (!w) {
        fprintf(stderr, "cg_sdk_waiter_new() returned NULL\n");
        g_failures++;
        return CG_ERR_RUNTIME;
    }
    op(sdk, cg_sdk_waiter_callback, w);
    char* result = NULL;
    CgErrorCode code = cg_sdk_waiter_wait(w, &result);
    cg_sdk_waiter_destroy(w);
    if (out_result) {
        *out_result = result;
    } else {
        cg_string_destroy(result);
    }
    return code;
}

/* ── Main ─────────────────────────────────────────────────────────────────── */

int main(void)
{
    /* ── Runtime init ────────────────────────────────────────────────────── */
    CgErrorCode rc = cg_init();
    ASSERT_EQ(rc, CG_OK, "cg_init() must succeed");
    if (rc != CG_OK) return 1;

    /* ── Test 1: bad JSON to cg_sdk_new → NULL + last-error set ─────────── */
    printf("=== Test 1: cg_sdk_new with malformed JSON ===\n");

    cg_last_error_clear();
    CgSdk* bad_sdk = cg_sdk_new("{ this is not valid json !!!");
    ASSERT(bad_sdk == NULL,
           "cg_sdk_new with malformed JSON must return NULL");
    ASSERT(cg_last_error_message() != NULL,
           "cg_sdk_new with malformed JSON must set cg_last_error_message()");
    if (bad_sdk == NULL && cg_last_error_message() != NULL) {
        printf("  cg_sdk_new(bad JSON) = NULL, last_error = \"%s\"  OK\n",
               cg_last_error_message());
    }
    if (bad_sdk != NULL) {
        cg_sdk_destroy(bad_sdk);
    }

    /* ── Test 2: non-object JSON to cg_sdk_new ───────────────────────────── */
    printf("=== Test 2: cg_sdk_new with non-object JSON (array) ===\n");

    cg_last_error_clear();
    CgSdk* array_sdk = cg_sdk_new("[1, 2, 3]");
    ASSERT(array_sdk == NULL,
           "cg_sdk_new with non-object JSON must return NULL");
    ASSERT(cg_last_error_message() != NULL,
           "cg_sdk_new with non-object JSON must set cg_last_error_message()");
    if (array_sdk == NULL && cg_last_error_message() != NULL) {
        printf("  cg_sdk_new([1,2,3]) = NULL, last_error = \"%s\"  OK\n",
               cg_last_error_message());
    }
    if (array_sdk != NULL) {
        cg_sdk_destroy(array_sdk);
    }

    /* ── Test 3: waiter single-use guard → CG_ERR_SDK_VALIDATION ─────────── */
    printf("=== Test 3: waiter single-use guard (CG_ERR_SDK_VALIDATION) ===\n");

    const char* settings_json =
        "{"
        "  \"embeddingProvider\": \"mock\","
        "  \"llmApiKey\": \"dummy-key-for-smoke-test\""
        "}";

    CgSdk* sdk = cg_sdk_new(settings_json);
    ASSERT(sdk != NULL, "cg_sdk_new must return non-NULL for valid settings");
    if (!sdk) {
        fprintf(stderr, "  last error: %s\n",
                cg_last_error_message() ? cg_last_error_message() : "(none)");
        cg_shutdown();
        return 1;
    }

    /* First warm — should succeed. */
    rc = run_via_waiter(sdk, cg_sdk_warm, NULL);
    ASSERT_EQ(rc, CG_OK, "first cg_sdk_warm must succeed");

    /* Second wait on the same waiter object is not possible (the waiter
     * was destroyed in run_via_waiter).  Instead, test the guard directly:
     * create a waiter, use it, then attempt a second wait. */
    CgSdkWaiter* w = cg_sdk_waiter_new();
    ASSERT(w != NULL, "cg_sdk_waiter_new must return non-NULL");
    if (!w) { goto cleanup; }

    cg_sdk_warm(sdk, cg_sdk_waiter_callback, w);
    char* first_result = NULL;
    CgErrorCode first_code = cg_sdk_waiter_wait(w, &first_result);
    cg_string_destroy(first_result);
    ASSERT_EQ(first_code, CG_OK, "first wait on fresh waiter must return CG_OK");

    /* Second wait on the already-consumed waiter: must return
     * CG_ERR_SDK_VALIDATION (14) and set cg_last_error_message(). */
    cg_last_error_clear();
    char* second_result = NULL;
    CgErrorCode second_code = cg_sdk_waiter_wait(w, &second_result);
    cg_string_destroy(second_result);
    ASSERT_EQ(second_code, (CgErrorCode)CG_ERR_SDK_VALIDATION,
              "second wait on consumed waiter must return CG_ERR_SDK_VALIDATION (14)");
    ASSERT(cg_last_error_message() != NULL,
           "second wait on consumed waiter must set cg_last_error_message()");
    if (second_code == (CgErrorCode)CG_ERR_SDK_VALIDATION &&
        cg_last_error_message() != NULL) {
        printf("  second waiter wait: code=%d, last_error=\"%s\"  OK\n",
               (int)second_code, cg_last_error_message());
    }
    cg_sdk_waiter_destroy(w);

cleanup:
    /* ── Cleanup ─────────────────────────────────────────────────────────── */
    cg_sdk_destroy(sdk);
    cg_shutdown();

    /* ── Result ──────────────────────────────────────────────────────────── */
    if (g_failures == 0) {
        printf("\nPASSED (sdk_negative_path_smoke)\n");
        return 0;
    } else {
        fprintf(stderr, "\nFAILED: %d assertion(s) failed\n", g_failures);
        return 1;
    }
}
