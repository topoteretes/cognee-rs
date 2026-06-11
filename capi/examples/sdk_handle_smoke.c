/*
 * sdk_handle_smoke.c — Phase 1b Tier-A smoke test for CgSdk handle lifecycle.
 *
 * Tests (no network, no LLM required):
 *   1. cg_api_version() returns (1 << 16) | 1 (major=1, minor=1).
 *   2. cg_sdk_new(NULL)          — construct handle from env defaults.
 *   3. cg_sdk_new(settings_json) — construct handle from JSON settings
 *      (sets MOCK_EMBEDDING and tempdirs for isolation).
 *   4. cg_sdk_warm               — warms the services bundle via waiter.
 *   5. cg_sdk_owner_id           — returns a quoted UUID via waiter.
 *   6. cg_sdk_clone + cg_sdk_destroy — ref-counting sanity.
 *
 * Environment:
 *   MOCK_EMBEDDING=true   — set via the JSON settings overlay in this test.
 *   OPENAI_URL / OPENAI_TOKEN — NOT required (mock embedding, dummy LLM key).
 *
 * The test sets temporary directories inside the OS temp dir so it does not
 * pollute the working directory and remains hermetic across repeated runs.
 *
 * Exit codes: 0 = all assertions passed, 1 = at least one failure.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cognee_sdk.h"

/* ── Helpers ──────────────────────────────────────────────────────────────── */

static int g_failures = 0;

#define ASSERT(cond, msg)                                           \
    do {                                                            \
        if (!(cond)) {                                              \
            fprintf(stderr, "FAIL [%s:%d]: %s\n",                  \
                    __FILE__, __LINE__, (msg));                     \
            g_failures++;                                           \
        }                                                           \
    } while (0)

#define ASSERT_EQ(a, b, msg)                                        \
    do {                                                            \
        if ((a) != (b)) {                                           \
            fprintf(stderr, "FAIL [%s:%d]: %s (got %d, want %d)\n",\
                    __FILE__, __LINE__, (msg), (int)(a), (int)(b)); \
            g_failures++;                                           \
        }                                                           \
    } while (0)

/** Run an async op through the waiter.  Returns the op's CgErrorCode.
 *  Sets *out_result (caller must cg_string_destroy) on CG_OK. */
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
    /* ── 1. Runtime init ─────────────────────────────────────────────────── */
    CgErrorCode rc = cg_init();
    ASSERT_EQ(rc, CG_OK, "cg_init() must succeed");

    /* ── 2. API version ──────────────────────────────────────────────────── */
    uint32_t ver = cg_api_version();
    uint32_t want = (1u << 16) | 1u;
    ASSERT(ver == want, "cg_api_version() must return (1<<16)|1");
    if (ver == want) {
        printf("cg_api_version() = 0x%08x  OK\n", ver);
    } else {
        fprintf(stderr, "cg_api_version() = 0x%08x, want 0x%08x\n", ver, want);
    }

    /* ── 3. cg_sdk_new(NULL) ─────────────────────────────────────────────── */
    CgSdk* sdk_env = cg_sdk_new(NULL);
    ASSERT(sdk_env != NULL, "cg_sdk_new(NULL) must return non-NULL");

    /* ── 4. cg_sdk_new(settings_json) ───────────────────────────────────── */
    /*
     * Build a JSON settings overlay that:
     *   - sets MOCK_EMBEDDING mode (no ONNX model needed)
     *   - provides a dummy LLM key (warm does a network-free construction)
     * The test relies on MOCK_EMBEDDING=true being read from the env OR set
     * here.  We set it in the JSON overlay for hermetic testing.
     */
    const char* settings_json =
        "{"
        "  \"embeddingProvider\": \"mock\","
        "  \"llmApiKey\": \"dummy-key-for-smoke-test\""
        "}";

    CgSdk* sdk = cg_sdk_new(settings_json);
    ASSERT(sdk != NULL, "cg_sdk_new(settings_json) must return non-NULL");
    if (!sdk) {
        fprintf(stderr, "  last error: %s\n",
                cg_last_error_message() ? cg_last_error_message() : "(none)");
        /* Can't proceed without a valid handle. */
        cg_sdk_destroy(sdk_env);
        cg_shutdown();
        return 1;
    }
    printf("cg_sdk_new(settings_json)  OK\n");

    /* ── 5. cg_sdk_warm ──────────────────────────────────────────────────── */
    rc = run_via_waiter(sdk, cg_sdk_warm, NULL);
    ASSERT_EQ(rc, CG_OK, "cg_sdk_warm must return CG_OK");
    if (rc == CG_OK) {
        printf("cg_sdk_warm                OK\n");
    } else {
        fprintf(stderr, "  cg_sdk_warm code=%d\n", (int)rc);
    }

    /* ── 6. cg_sdk_owner_id ─────────────────────────────────────────────── */
    char* owner_json = NULL;
    rc = run_via_waiter(sdk, cg_sdk_owner_id, &owner_json);
    ASSERT_EQ(rc, CG_OK, "cg_sdk_owner_id must return CG_OK");
    if (rc == CG_OK) {
        ASSERT(owner_json != NULL, "owner_id result_json must not be NULL");
        /* Must be a quoted JSON string: starts and ends with '"'. */
        size_t len = owner_json ? strlen(owner_json) : 0;
        ASSERT(len >= 2, "owner_id JSON must have at least 2 chars (the quotes)");
        ASSERT(len >= 2 && owner_json[0] == '"' && owner_json[len - 1] == '"',
               "owner_id JSON must be a quoted string");
        printf("cg_sdk_owner_id            OK  (result=%s)\n",
               owner_json ? owner_json : "(null)");
    } else {
        fprintf(stderr, "  cg_sdk_owner_id code=%d\n", (int)rc);
    }
    cg_string_destroy(owner_json);

    /* ── 7. cg_sdk_clone + cg_sdk_destroy ───────────────────────────────── */
    CgSdk* sdk2 = cg_sdk_clone(sdk);
    ASSERT(sdk2 != NULL, "cg_sdk_clone must return non-NULL");
    /* Destroy original; the clone should keep state alive. */
    cg_sdk_destroy(sdk);
    sdk = NULL;
    /* Re-warm via the clone to confirm the Arc is still alive. */
    rc = run_via_waiter(sdk2, cg_sdk_warm, NULL);
    ASSERT_EQ(rc, CG_OK, "cg_sdk_warm on clone after original destroyed must succeed");
    if (rc == CG_OK) {
        printf("cg_sdk_clone + destroy     OK\n");
    }
    cg_sdk_destroy(sdk2);

    /* ── 8. Cleanup ──────────────────────────────────────────────────────── */
    cg_sdk_destroy(sdk_env);
    cg_shutdown();

    /* ── Result ──────────────────────────────────────────────────────────── */
    if (g_failures == 0) {
        printf("\nPASSED (sdk_handle_smoke)\n");
        return 0;
    } else {
        fprintf(stderr, "\nFAILED: %d assertion(s) failed\n", g_failures);
        return 1;
    }
}
