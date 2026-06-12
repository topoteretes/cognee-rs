/*
 * example_sdk_add_cognify.c — Phase 4 Tier-B live add + cognify test.
 *
 * SKIP GUARD (D12): if OPENAI_URL or OPENAI_TOKEN are absent, this example
 * prints "SKIP: ..." to stdout and exits 0.  The capi/scripts/check.sh
 * gated section runs it only when both env vars are present.
 *
 * When run with valid credentials it exercises the full pipeline:
 *   1. Create SDK handle with live LLM + mock embedding (to keep the test
 *      fast and deterministic without a local ONNX model).
 *   2. Add two text inputs to a fresh dataset.
 *   3. Run cg_sdk_add_and_cognify on the same dataset to verify the combined
 *      op works end-to-end.
 *
 * Assertions use strstr() on stable JSON keys — no JSON library dependency.
 *
 * Exit codes: 0 = all assertions passed (or SKIP), 1 = at least one failure.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cognee_sdk.h"

/* ── SKIP guard (D12) ─────────────────────────────────────────────────────── */

static void check_credentials(void)
{
    const char *url   = getenv("OPENAI_URL");
    const char *token = getenv("OPENAI_TOKEN");
    if (!url || !url[0] || !token || !token[0]) {
        printf("SKIP: OPENAI_URL or OPENAI_TOKEN not set — skipping Tier-B live test\n");
        exit(0);
    }
}

/* ── Helpers ──────────────────────────────────────────────────────────────── */

static int g_failures = 0;

#define ASSERT(cond, msg)                                                   \
    do {                                                                    \
        if (!(cond)) {                                                      \
            fprintf(stderr, "FAIL [%s:%d]: %s\n", __FILE__, __LINE__, (msg)); \
            g_failures++;                                                   \
        }                                                                   \
    } while (0)

#define ASSERT_EQ(a, b, msg)                                                \
    do {                                                                    \
        if ((a) != (b)) {                                                   \
            fprintf(stderr, "FAIL [%s:%d]: %s (got %d, want %d)\n",        \
                    __FILE__, __LINE__, (msg), (int)(a), (int)(b));         \
            g_failures++;                                                   \
        }                                                                   \
    } while (0)

static void assert_json_contains(const char *json, const char *needle,
                                 const char *label)
{
    if (!json) {
        fprintf(stderr, "FAIL: %s — result_json is NULL\n", label);
        g_failures++;
        return;
    }
    if (!strstr(json, needle)) {
        fprintf(stderr, "FAIL: %s — expected \"%s\" in:\n  %.400s\n",
                label, needle, json);
        g_failures++;
    }
}

/** Run a no-arg SDK op (warm, owner_id) through the waiter. */
typedef void (*SdkNoArgOp)(const CgSdk *, CgSdkResultCallback, void *);

static CgErrorCode run_no_arg_op(const CgSdk *sdk, SdkNoArgOp op, char **out)
{
    CgSdkWaiter *w = cg_sdk_waiter_new();
    if (!w) { g_failures++; return CG_ERR_RUNTIME; }
    op(sdk, cg_sdk_waiter_callback, (void *)w);
    char *result = NULL;
    CgErrorCode code = cg_sdk_waiter_wait(w, &result);
    cg_sdk_waiter_destroy(w);
    if (out) *out = result; else cg_string_destroy(result);
    return code;
}

/** Run cg_sdk_add through the waiter. */
static char *run_add(const CgSdk *sdk, const char *inputs_json,
                     const char *dataset_name, const char *opts_json)
{
    CgSdkWaiter *w = cg_sdk_waiter_new();
    if (!w) { g_failures++; return NULL; }
    cg_sdk_add(sdk, inputs_json, dataset_name, opts_json,
               cg_sdk_waiter_callback, (void *)w);
    char *result = NULL;
    CgErrorCode code = cg_sdk_waiter_wait(w, &result);
    cg_sdk_waiter_destroy(w);
    if (code != CG_OK) {
        fprintf(stderr, "cg_sdk_add failed: code=%d  last_error=%s\n",
                (int)code,
                cg_last_error_message() ? cg_last_error_message() : "(none)");
        g_failures++;
        cg_string_destroy(result);
        return NULL;
    }
    return result;
}

/** Run cg_sdk_add_and_cognify through the waiter. */
static char *run_add_and_cognify(const CgSdk *sdk, const char *inputs_json,
                                  const char *dataset_name, const char *opts_json)
{
    CgSdkWaiter *w = cg_sdk_waiter_new();
    if (!w) { g_failures++; return NULL; }
    cg_sdk_add_and_cognify(sdk, inputs_json, dataset_name, opts_json,
                            cg_sdk_waiter_callback, (void *)w);
    char *result = NULL;
    CgErrorCode code = cg_sdk_waiter_wait(w, &result);
    cg_sdk_waiter_destroy(w);
    if (code != CG_OK) {
        fprintf(stderr, "cg_sdk_add_and_cognify failed: code=%d  last_error=%s\n",
                (int)code,
                cg_last_error_message() ? cg_last_error_message() : "(none)");
        g_failures++;
        cg_string_destroy(result);
        return NULL;
    }
    return result;
}

/* ── Main ─────────────────────────────────────────────────────────────────── */

int main(void)
{
    /* Skip immediately if credentials are absent (D12). */
    check_credentials();

    const char *openai_url   = getenv("OPENAI_URL");
    const char *openai_token = getenv("OPENAI_TOKEN");

    printf("Using OPENAI_URL=%s\n", openai_url);

    /* ── Runtime init ─────────────────────────────────────────────────────── */
    CgErrorCode rc = cg_init();
    ASSERT_EQ(rc, CG_OK, "cg_init() must succeed");
    if (rc != CG_OK) return 1;

    /*
     * Build settings with:
     *   - live LLM endpoint from env
     *   - mock embedding (no ONNX model download)
     */
    char settings[1024];
    snprintf(settings, sizeof(settings),
             "{"
             "  \"embedding_provider\": \"mock\","
             "  \"llm_endpoint\": \"%s\","
             "  \"llm_api_key\": \"%s\","
             "  \"llm_model\": \"gpt-4o-mini\""
             "}",
             openai_url, openai_token);

    CgSdk *sdk = cg_sdk_new(settings);
    ASSERT(sdk != NULL, "cg_sdk_new must return non-NULL");
    if (!sdk) {
        fprintf(stderr, "  last_error: %s\n",
                cg_last_error_message() ? cg_last_error_message() : "(none)");
        cg_shutdown();
        return 1;
    }

    /* Warm. */
    rc = run_no_arg_op(sdk, cg_sdk_warm, NULL);
    ASSERT_EQ(rc, CG_OK, "cg_sdk_warm must succeed");
    if (rc != CG_OK) {
        cg_sdk_destroy(sdk);
        cg_shutdown();
        return 1;
    }
    printf("cg_sdk_warm  OK\n");

    /* ── Test 1: cg_sdk_add ──────────────────────────────────────────────── */
    printf("=== Test 1: cg_sdk_add two texts ===\n");
    {
        const char *inputs =
            "[{\"type\":\"text\",\"text\":\"Cognee is a knowledge graph AI memory library.\"},"
            " {\"type\":\"text\",\"text\":\"It supports add, cognify, and search operations.\"}]";

        char *result = run_add(sdk, inputs, "live-smoke-dataset", NULL);
        ASSERT(result != NULL, "cg_sdk_add must succeed");
        if (result) {
            assert_json_contains(result, "\"addedCount\"",
                                 "result must contain addedCount key");
            assert_json_contains(result, "\"datasetName\"",
                                 "result must contain datasetName key");
            printf("  cg_sdk_add result: %.200s\n", result);
            cg_string_destroy(result);
        }
    }

    /* ── Test 2: cg_sdk_add_and_cognify ─────────────────────────────────── */
    printf("=== Test 2: cg_sdk_add_and_cognify new text ===\n");
    {
        const char *inputs =
            "[{\"type\":\"text\","
            "  \"text\":\"Knowledge graphs enable structured memory retrieval.\"}]";

        char *result = run_add_and_cognify(sdk, inputs, "live-smoke-dataset", NULL);
        ASSERT(result != NULL, "cg_sdk_add_and_cognify must succeed");
        if (result) {
            /* Combined result has both "add" and "cognify" keys. */
            assert_json_contains(result, "\"add\"",
                                 "combined result must contain add key");
            assert_json_contains(result, "\"cognify\"",
                                 "combined result must contain cognify key");
            assert_json_contains(result, "\"chunks\"",
                                 "cognify sub-result must contain chunks key");
            printf("  cg_sdk_add_and_cognify result: %.300s\n", result);
            cg_string_destroy(result);
        }
    }

    /* ── Cleanup ──────────────────────────────────────────────────────────── */
    cg_sdk_destroy(sdk);
    cg_shutdown();

    /* ── Result ───────────────────────────────────────────────────────────── */
    if (g_failures == 0) {
        printf("\nPASSED (example_sdk_add_cognify)\n");
        return 0;
    } else {
        fprintf(stderr, "\nFAILED: %d assertion(s) failed\n", g_failures);
        return 1;
    }
}
