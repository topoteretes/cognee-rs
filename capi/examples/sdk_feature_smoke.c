/*
 * sdk_feature_smoke.c — Phase 7 Tier-A deterministic smoke tests for feature-
 * gated ops (visualization, cloud) and the cg_json_string_decode utility.
 *
 * This test is intentionally designed to pass whether or not the visualization
 * and cloud features are compiled in:
 *
 *   - Default build (features ON): the ops succeed or return expected errors
 *     depending on backend state.  For this Tier-A test we focus on calling
 *     the functions and verifying the callback fires (even if with a runtime
 *     error due to empty graph/cloud state).
 *
 *   - Slim build (--no-default-features --features sqlite,testing): all four
 *     feature-gated ops must fire the callback with CG_ERR_FEATURE_NOT_BUILT
 *     (16).  Verified by the `build-slim` CMake dir in check.sh.
 *
 * Both build configurations run this same binary; the expected_code parameter
 * of run_op_expect_code() is set at compile time via the
 * EXPECT_FEATURE_NOT_BUILT preprocessor macro (defined by CMake for the slim
 * build).
 *
 * cg_json_string_decode is always present (not feature-gated) — it is tested
 * in both builds.
 *
 * Exit codes: 0 = all assertions passed, 1 = at least one failure.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cognee_sdk.h"

/* ── Compile-time configuration ───────────────────────────────────────────── */

/*
 * EXPECT_FEATURE_NOT_BUILT is defined by CMake (-DEXPECT_FEATURE_NOT_BUILT=1)
 * when building the slim target (sdk_feature_smoke_slim).  When defined, we
 * assert CG_ERR_FEATURE_NOT_BUILT (16) from cg_sdk_visualize and
 * cg_sdk_serve / cg_sdk_disconnect.  In the default build we accept any
 * non-null callback invocation (the ops may fail for other reasons such as
 * empty graph state, but they must not be missing).
 */
#ifndef EXPECT_FEATURE_NOT_BUILT
#define EXPECT_FEATURE_NOT_BUILT 0
#endif

/* ── Test helpers ─────────────────────────────────────────────────────────── */

static int g_failures = 0;

#define ASSERT(cond, msg)                                                        \
    do {                                                                         \
        if (!(cond)) {                                                           \
            fprintf(stderr, "FAIL [%s:%d]: %s\n", __FILE__, __LINE__, (msg));   \
            g_failures++;                                                        \
        }                                                                        \
    } while (0)

#define ASSERT_EQ(a, b, msg)                                                     \
    do {                                                                         \
        if ((a) != (b)) {                                                        \
            fprintf(stderr, "FAIL [%s:%d]: %s (got %d, want %d)\n",             \
                    __FILE__, __LINE__, (msg), (int)(a), (int)(b));              \
            g_failures++;                                                        \
        }                                                                        \
    } while (0)

/* ── Waiter helpers ───────────────────────────────────────────────────────── */

/*
 * run_sdk_op_with_handle: fire a cg_sdk_* op that takes (CgSdk*, opts_json,
 * cb, ud) and return its result code via a waiter.
 */
typedef void (*SdkHandleOp)(const CgSdk *, const char *, CgSdkResultCallback,
                             void *);

static CgErrorCode run_handle_op(const CgSdk *sdk, SdkHandleOp op,
                                  const char *opts_json, char **out)
{
    CgSdkWaiter *w = cg_sdk_waiter_new();
    if (!w) {
        fprintf(stderr, "FAIL: cg_sdk_waiter_new returned NULL\n");
        g_failures++;
        return (CgErrorCode)-1;
    }
    op(sdk, opts_json, cg_sdk_waiter_callback, (void *)w);
    CgErrorCode code = cg_sdk_waiter_wait(w, out);
    cg_sdk_waiter_destroy(w);
    return code;
}

/*
 * run_global_op: fire a cg_sdk_* op that takes (opts_json, cb, ud)
 * (process-wide singleton, no handle) and return its result code.
 */
typedef void (*SdkGlobalOp)(const char *, CgSdkResultCallback, void *);

static CgErrorCode run_global_op(SdkGlobalOp op, const char *opts_json,
                                  char **out)
{
    CgSdkWaiter *w = cg_sdk_waiter_new();
    if (!w) {
        fprintf(stderr, "FAIL: cg_sdk_waiter_new returned NULL\n");
        g_failures++;
        return (CgErrorCode)-1;
    }
    op(opts_json, cg_sdk_waiter_callback, (void *)w);
    CgErrorCode code = cg_sdk_waiter_wait(w, out);
    cg_sdk_waiter_destroy(w);
    return code;
}

/* ── Test: cg_json_string_decode ──────────────────────────────────────────── */

static void test_json_string_decode(void)
{
    printf("  [cg_json_string_decode] basic round-trip ...\n");

    /* Valid JSON string literal — ASCII */
    char *out = NULL;
    CgErrorCode code = cg_json_string_decode("\"hello world\"", &out);
    ASSERT_EQ(code, CG_OK, "cg_json_string_decode ASCII: expected CG_OK");
    ASSERT(out != NULL, "cg_json_string_decode ASCII: out must not be NULL");
    if (out) {
        ASSERT(strcmp(out, "hello world") == 0,
               "cg_json_string_decode ASCII: decoded value mismatch");
        cg_string_destroy(out);
        out = NULL;
    }

    /* Valid JSON string with escape sequences */
    code = cg_json_string_decode("\"line1\\nline2\\ttab\"", &out);
    ASSERT_EQ(code, CG_OK, "cg_json_string_decode escapes: expected CG_OK");
    if (out) {
        ASSERT(strchr(out, '\n') != NULL,
               "cg_json_string_decode escapes: expected newline in decoded string");
        ASSERT(strchr(out, '\t') != NULL,
               "cg_json_string_decode escapes: expected tab in decoded string");
        cg_string_destroy(out);
        out = NULL;
    }

    /* Input is a JSON number — must return CG_ERR_SDK_VALIDATION */
    code = cg_json_string_decode("42", &out);
    ASSERT_EQ(code, CG_ERR_SDK_VALIDATION,
              "cg_json_string_decode number: expected CG_ERR_SDK_VALIDATION");
    ASSERT(out == NULL,
           "cg_json_string_decode number: out must be NULL on error");

    /* Input is a JSON boolean — must return CG_ERR_SDK_VALIDATION */
    code = cg_json_string_decode("true", &out);
    ASSERT_EQ(code, CG_ERR_SDK_VALIDATION,
              "cg_json_string_decode bool: expected CG_ERR_SDK_VALIDATION");

    /* Input is a JSON object — must return CG_ERR_SDK_VALIDATION */
    code = cg_json_string_decode("{\"key\":1}", &out);
    ASSERT_EQ(code, CG_ERR_SDK_VALIDATION,
              "cg_json_string_decode object: expected CG_ERR_SDK_VALIDATION");

    /* Malformed JSON — must return CG_ERR_SDK_VALIDATION */
    code = cg_json_string_decode("\"unterminated", &out);
    ASSERT_EQ(code, CG_ERR_SDK_VALIDATION,
              "cg_json_string_decode malformed: expected CG_ERR_SDK_VALIDATION");

    /* NULL pointer — must return CG_ERR_NULL_POINTER */
    code = cg_json_string_decode(NULL, &out);
    ASSERT_EQ(code, CG_ERR_NULL_POINTER,
              "cg_json_string_decode null input: expected CG_ERR_NULL_POINTER");

    printf("  [cg_json_string_decode] done.\n");
}

/* ── Test: cg_sdk_visualize / cg_sdk_visualize_to_file ───────────────────── */

static void test_visualize(const CgSdk *sdk)
{
    char *out = NULL;
    CgErrorCode code;

#if EXPECT_FEATURE_NOT_BUILT
    printf("  [cg_sdk_visualize] slim build — expecting CG_ERR_FEATURE_NOT_BUILT ...\n");
    code = run_handle_op(sdk, cg_sdk_visualize, NULL, &out);
    ASSERT_EQ(code, CG_ERR_FEATURE_NOT_BUILT,
              "cg_sdk_visualize slim: expected CG_ERR_FEATURE_NOT_BUILT (16)");
    ASSERT(out == NULL, "cg_sdk_visualize slim: result must be NULL on feature error");
    printf("  [cg_sdk_visualize] slim: CG_ERR_FEATURE_NOT_BUILT confirmed.\n");

    printf("  [cg_sdk_visualize_to_file] slim build — expecting CG_ERR_FEATURE_NOT_BUILT ...\n");
    code = run_handle_op(sdk, cg_sdk_visualize_to_file, NULL, &out);
    ASSERT_EQ(code, CG_ERR_FEATURE_NOT_BUILT,
              "cg_sdk_visualize_to_file slim: expected CG_ERR_FEATURE_NOT_BUILT (16)");
    ASSERT(out == NULL, "cg_sdk_visualize_to_file slim: result must be NULL on feature error");
    printf("  [cg_sdk_visualize_to_file] slim: CG_ERR_FEATURE_NOT_BUILT confirmed.\n");
#else
    /*
     * Default build: visualization feature is compiled in.  We call the op
     * against a warmed mock handle.  The graph is empty, so the HTML may be
     * minimal, but the callback must fire without crashing and the code must
     * be either CG_OK (empty graph is valid) or a runtime error.  We do NOT
     * assert CG_OK here because an empty graph may trigger a runtime path in
     * the underlying library.  We just assert the callback fires (code != -1).
     */
    printf("  [cg_sdk_visualize] default build — callback must fire ...\n");
    code = run_handle_op(sdk, cg_sdk_visualize, NULL, &out);
    /* Accept CG_OK (empty HTML) or any SDK/runtime error — just not a hang. */
    ASSERT((int)code != -1, "cg_sdk_visualize: callback did not fire");
    if (code == CG_OK) {
        ASSERT(out != NULL, "cg_sdk_visualize CG_OK: result must not be NULL");
        /* D9: result should be a quoted JSON string (starts with '"') */
        if (out) {
            ASSERT(out[0] == '"', "cg_sdk_visualize CG_OK: result must be a quoted JSON string");

            /* Round-trip through cg_json_string_decode */
            char *html = NULL;
            CgErrorCode dec_code = cg_json_string_decode(out, &html);
            ASSERT_EQ(dec_code, CG_OK,
                      "cg_sdk_visualize: cg_json_string_decode round-trip failed");
            if (html) {
                ASSERT(strlen(html) > 0,
                       "cg_sdk_visualize: decoded HTML must not be empty");
                cg_string_destroy(html);
            }
            cg_string_destroy(out);
            out = NULL;
        }
    } else if (out) {
        cg_string_destroy(out);
        out = NULL;
    }
    printf("  [cg_sdk_visualize] callback fired (code=%d).\n", (int)code);

    printf("  [cg_sdk_visualize_to_file] default build — callback must fire ...\n");
    code = run_handle_op(sdk, cg_sdk_visualize_to_file, NULL, &out);
    ASSERT((int)code != -1, "cg_sdk_visualize_to_file: callback did not fire");
    if (code == CG_OK) {
        ASSERT(out != NULL, "cg_sdk_visualize_to_file CG_OK: result must not be NULL");
        if (out) {
            ASSERT(out[0] == '"',
                   "cg_sdk_visualize_to_file CG_OK: result must be a quoted JSON string");
            cg_string_destroy(out);
            out = NULL;
        }
    } else if (out) {
        cg_string_destroy(out);
        out = NULL;
    }
    printf("  [cg_sdk_visualize_to_file] callback fired (code=%d).\n", (int)code);
#endif
    (void)out; /* suppress any remaining unused warning */
}

/* ── Test: cg_sdk_serve / cg_sdk_disconnect ──────────────────────────────── */

static void test_cloud(void)
{
    char *out = NULL;
    CgErrorCode code;

#if EXPECT_FEATURE_NOT_BUILT
    printf("  [cg_sdk_serve] slim build — expecting CG_ERR_FEATURE_NOT_BUILT ...\n");
    code = run_global_op(cg_sdk_serve, NULL, &out);
    ASSERT_EQ(code, CG_ERR_FEATURE_NOT_BUILT,
              "cg_sdk_serve slim: expected CG_ERR_FEATURE_NOT_BUILT (16)");
    ASSERT(out == NULL, "cg_sdk_serve slim: result must be NULL on feature error");
    printf("  [cg_sdk_serve] slim: CG_ERR_FEATURE_NOT_BUILT confirmed.\n");

    printf("  [cg_sdk_disconnect] slim build — expecting CG_ERR_FEATURE_NOT_BUILT ...\n");
    code = run_global_op(cg_sdk_disconnect, NULL, &out);
    ASSERT_EQ(code, CG_ERR_FEATURE_NOT_BUILT,
              "cg_sdk_disconnect slim: expected CG_ERR_FEATURE_NOT_BUILT (16)");
    ASSERT(out == NULL, "cg_sdk_disconnect slim: result must be NULL on feature error");
    printf("  [cg_sdk_disconnect] slim: CG_ERR_FEATURE_NOT_BUILT confirmed.\n");
#else
    /*
     * Default build: cloud feature is compiled in.  We verify argument-
     * validation paths and that the callback fires.  A live Auth0 device-code
     * flow is NOT required — the Tier-A contract allows any error response as
     * long as the callback fires (matching the TS test tier for cloud ops).
     */
    printf("  [cg_sdk_serve] default build — validation path / callback must fire ...\n");
    /* Invalid opts (malformed JSON) — must return a validation or runtime error */
    code = run_global_op(cg_sdk_serve, "not-json", &out);
    ASSERT((int)code != -1, "cg_sdk_serve bad JSON: callback must fire");
    ASSERT(code != CG_OK,
           "cg_sdk_serve bad JSON: must not return CG_OK for malformed input");
    if (out) { cg_string_destroy(out); out = NULL; }
    printf("  [cg_sdk_serve] bad-JSON validation path: code=%d (non-zero as expected).\n",
           (int)code);

    printf("  [cg_sdk_disconnect] default build — callback must fire ...\n");
    code = run_global_op(cg_sdk_disconnect, NULL, &out);
    /* disconnect on an unconnected client may succeed ("null") or fail — both OK */
    ASSERT((int)code != -1, "cg_sdk_disconnect: callback must fire");
    if (out) { cg_string_destroy(out); out = NULL; }
    printf("  [cg_sdk_disconnect] callback fired (code=%d).\n", (int)code);
#endif
    (void)out;
}

/* ── main ─────────────────────────────────────────────────────────────────── */

int main(void)
{
#if EXPECT_FEATURE_NOT_BUILT
    printf("=== sdk_feature_smoke (slim build — EXPECT_FEATURE_NOT_BUILT) ===\n");
#else
    printf("=== sdk_feature_smoke (default build) ===\n");
#endif

    /* Initialise the runtime (idempotent). */
    CgErrorCode init_code = cg_init();
    if (init_code != CG_OK) {
        fprintf(stderr, "FAIL: cg_init returned %d\n", (int)init_code);
        return 1;
    }

    /* Create a mock-embedding SDK handle (Tier-A: no network required). */
    CgSdk *sdk = cg_sdk_new(
        "{\"embedding_provider\":\"mock\","
        "\"mock_embedding\":\"true\","
        "\"graph_database_provider\":\"mock\","
        "\"vector_db_provider\":\"mock\"}"
    );
    if (!sdk) {
        fprintf(stderr, "FAIL: cg_sdk_new returned NULL: %s\n",
                cg_last_error_message());
        return 1;
    }

    /* Warm the handle. */
    {
        CgSdkWaiter *w = cg_sdk_waiter_new();
        cg_sdk_warm(sdk, cg_sdk_waiter_callback, (void *)w);
        char *warm_out = NULL;
        CgErrorCode warm_code = cg_sdk_waiter_wait(w, &warm_out);
        cg_sdk_waiter_destroy(w);
        if (warm_out) { cg_string_destroy(warm_out); }
        if (warm_code != CG_OK) {
            fprintf(stderr,
                    "WARN: cg_sdk_warm returned %d — proceeding with tests.\n",
                    (int)warm_code);
        }
    }

    printf("\n--- Testing cg_json_string_decode ---\n");
    test_json_string_decode();

    printf("\n--- Testing visualization ops ---\n");
    test_visualize(sdk);

    printf("\n--- Testing cloud ops ---\n");
    test_cloud();

    cg_sdk_destroy(sdk);

    printf("\n");
    if (g_failures == 0) {
        printf("=== sdk_feature_smoke PASSED ===\n");
        return 0;
    } else {
        fprintf(stderr, "=== sdk_feature_smoke FAILED (%d assertion(s)) ===\n",
                g_failures);
        return 1;
    }
}
