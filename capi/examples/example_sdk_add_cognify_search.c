/*
 * example_sdk_add_cognify_search.c — Phase 5 Tier-B flagship C example.
 *
 * Mirrors js/examples/add-cognify-search.ts:
 *   add → cognify → cg_sdk_search (GRAPH_COMPLETION) → cg_sdk_recall
 *
 * SKIP GUARD (D12): if OPENAI_URL or OPENAI_TOKEN are absent, this example
 * prints "SKIP: ..." to stdout and exits 0.  The capi/scripts/check.sh gated
 * section runs it only when both env vars are present.
 *
 * When run with valid credentials it exercises the full round-trip:
 *   1. Create SDK handle with live LLM + mock embedding (fast, no ONNX model).
 *   2. Add two text inputs via cg_sdk_add.
 *   3. Cognify with cg_sdk_add_and_cognify.
 *   4. Search with cg_sdk_search (GRAPH_COMPLETION) → verify JSON result.
 *   5. Recall with cg_sdk_recall → verify result has required keys.
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

/** Run cg_sdk_add through the waiter; returns JSON or NULL on error. */
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

/** Run cg_sdk_add_and_cognify through the waiter; returns JSON or NULL. */
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
        fprintf(stderr,
                "cg_sdk_add_and_cognify failed: code=%d  last_error=%s\n",
                (int)code,
                cg_last_error_message() ? cg_last_error_message() : "(none)");
        g_failures++;
        cg_string_destroy(result);
        return NULL;
    }
    return result;
}

/** Run cg_sdk_search through the waiter; returns JSON or NULL on error. */
static char *run_search(const CgSdk *sdk, const char *query,
                        const char *opts_json)
{
    CgSdkWaiter *w = cg_sdk_waiter_new();
    if (!w) { g_failures++; return NULL; }
    cg_sdk_search(sdk, query, opts_json, cg_sdk_waiter_callback, (void *)w);
    char *result = NULL;
    CgErrorCode code = cg_sdk_waiter_wait(w, &result);
    cg_sdk_waiter_destroy(w);
    if (code != CG_OK) {
        fprintf(stderr, "cg_sdk_search failed: code=%d  last_error=%s\n",
                (int)code,
                cg_last_error_message() ? cg_last_error_message() : "(none)");
        g_failures++;
        cg_string_destroy(result);
        return NULL;
    }
    return result;
}

/** Run cg_sdk_recall through the waiter; returns JSON or NULL on error. */
static char *run_recall(const CgSdk *sdk, const char *query,
                        const char *opts_json)
{
    CgSdkWaiter *w = cg_sdk_waiter_new();
    if (!w) { g_failures++; return NULL; }
    cg_sdk_recall(sdk, query, opts_json, cg_sdk_waiter_callback, (void *)w);
    char *result = NULL;
    CgErrorCode code = cg_sdk_waiter_wait(w, &result);
    cg_sdk_waiter_destroy(w);
    if (code != CG_OK) {
        fprintf(stderr, "cg_sdk_recall failed: code=%d  last_error=%s\n",
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
    const char *openai_model = getenv("OPENAI_MODEL");
    if (!openai_model || !openai_model[0]) openai_model = "gpt-4o-mini";

    printf("Using OPENAI_URL=%s model=%s\n", openai_url, openai_model);

    /* ── Runtime init ─────────────────────────────────────────────────────── */
    CgErrorCode rc = cg_init();
    ASSERT_EQ(rc, CG_OK, "cg_init() must succeed");
    if (rc != CG_OK) return 1;

    /*
     * Build settings with:
     *   - live LLM endpoint from env
     *   - mock embedding (no ONNX model download, keeps test fast)
     */
    char settings[1024];
    snprintf(settings, sizeof(settings),
             "{"
             "  \"embedding_provider\": \"mock\","
             "  \"vector_db_provider\": \"brute-force\","
             "  \"llm_endpoint\": \"%s\","
             "  \"llm_api_key\": \"%s\","
             "  \"llm_model\": \"%s\""
             "}",
             openai_url, openai_token, openai_model);

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
        fprintf(stderr, "  cg_sdk_warm last_error: %s\n",
                cg_last_error_message() ? cg_last_error_message() : "(none)");
        cg_sdk_destroy(sdk);
        cg_shutdown();
        return 1;
    }
    printf("cg_sdk_warm  OK\n");

    /* ── Test 1: cg_sdk_add two text inputs ─────────────────────────────── */
    printf("=== Test 1: cg_sdk_add two texts ===\n");
    {
        const char *inputs =
            "[{\"type\":\"text\","
            "  \"text\":\"Cognee is an AI memory library that builds knowledge graphs.\"},"
            " {\"type\":\"text\","
            "  \"text\":\"Knowledge graphs enable structured, queryable AI memory.\"}]";

        char *result = run_add(sdk, inputs, "acs-dataset", NULL);
        ASSERT(result != NULL, "cg_sdk_add must succeed");
        if (result) {
            assert_json_contains(result, "\"addedCount\"",
                                 "add result must contain addedCount");
            assert_json_contains(result, "\"datasetName\"",
                                 "add result must contain datasetName");
            printf("  cg_sdk_add result: %.200s\n", result);
            cg_string_destroy(result);
        }
    }

    /* ── Test 2: cg_sdk_add_and_cognify a new text ──────────────────────── */
    printf("=== Test 2: cg_sdk_add_and_cognify new text ===\n");
    {
        const char *inputs =
            "[{\"type\":\"text\","
            "  \"text\":\"Cognee supports add, cognify, and search operations end-to-end.\"}]";

        char *result = run_add_and_cognify(sdk, inputs, "acs-dataset", NULL);
        ASSERT(result != NULL, "cg_sdk_add_and_cognify must succeed");
        if (result) {
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

    /* ── Test 3: cg_sdk_search (GRAPH_COMPLETION) ───────────────────────── */
    printf("=== Test 3: cg_sdk_search GRAPH_COMPLETION ===\n");
    {
        char *result = run_search(sdk, "What is cognee?",
                                  "{\"searchType\":\"GRAPH_COMPLETION\"}");
        ASSERT(result != NULL, "cg_sdk_search must succeed");
        if (result) {
            /* SearchResponse is an array or object — must start with [ or {. */
            ASSERT(result[0] == '[' || result[0] == '{',
                   "search result must be a JSON array or object");
            printf("  cg_sdk_search result: %.300s\n", result);
            cg_string_destroy(result);
        }
    }

    /* ── Test 4: cg_sdk_recall ──────────────────────────────────────────── */
    printf("=== Test 4: cg_sdk_recall ===\n");
    {
        char *result = run_recall(sdk, "What is cognee?", NULL);
        ASSERT(result != NULL, "cg_sdk_recall must succeed");
        if (result) {
            assert_json_contains(result, "\"items\"",
                                 "recall result must contain items key");
            assert_json_contains(result, "\"searchTypeUsed\"",
                                 "recall result must contain searchTypeUsed key");
            assert_json_contains(result, "\"autoRouted\"",
                                 "recall result must contain autoRouted key");
            assert_json_contains(result, "\"searchResponse\"",
                                 "recall result must contain searchResponse key");
            printf("  cg_sdk_recall result: %.300s\n", result);
            cg_string_destroy(result);
        }
    }

    /* ── Test 5: cg_sdk_memify ───────────────────────────────────────────── */
    printf("=== Test 5: cg_sdk_memify ===\n");
    {
        CgSdkWaiter *w = cg_sdk_waiter_new();
        if (!w) { g_failures++; goto cleanup; }
        cg_sdk_memify(sdk, NULL, cg_sdk_waiter_callback, (void *)w);
        char *result = NULL;
        CgErrorCode mrc = cg_sdk_waiter_wait(w, &result);
        cg_sdk_waiter_destroy(w);
        ASSERT_EQ(mrc, CG_OK, "cg_sdk_memify must succeed");
        if (result) {
            assert_json_contains(result, "\"tripletCount\"",
                                 "memify result must contain tripletCount key");
            printf("  cg_sdk_memify result: %.300s\n", result);
            cg_string_destroy(result);
        }
    }

    /* ── Test 6: cg_sdk_remember ─────────────────────────────────────────── */
    printf("=== Test 6: cg_sdk_remember ===\n");
    {
        const char *inputs =
            "{\"type\":\"text\","
            " \"text\":\"Cognee is an AI memory library for knowledge graphs.\"}";
        CgSdkWaiter *w = cg_sdk_waiter_new();
        if (!w) { g_failures++; goto cleanup; }
        cg_sdk_remember(sdk, inputs, "acs-dataset", NULL,
                        cg_sdk_waiter_callback, (void *)w);
        char *result = NULL;
        CgErrorCode rrc = cg_sdk_waiter_wait(w, &result);
        cg_sdk_waiter_destroy(w);
        ASSERT_EQ(rrc, CG_OK, "cg_sdk_remember must succeed");
        if (result) {
            printf("  cg_sdk_remember result: %.300s\n", result);
            cg_string_destroy(result);
        }
    }

cleanup:
    /* ── Cleanup ──────────────────────────────────────────────────────────── */
    cg_sdk_destroy(sdk);
    cg_shutdown();

    /* ── Result ───────────────────────────────────────────────────────────── */
    if (g_failures == 0) {
        printf("\nPASSED (example_sdk_add_cognify_search)\n");
        return 0;
    } else {
        fprintf(stderr, "\nFAILED: %d assertion(s) failed\n", g_failures);
        return 1;
    }
}
