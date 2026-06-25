/*
 * sdk_retrieval_smoke.c — Phase 5 Tier-A smoke test for cg_sdk_search and
 * cg_sdk_recall.
 *
 * Tests (no network, no LLM required — MOCK_EMBEDDING=true):
 *
 *   1. String-mapping check — all 15 valid SearchType strings are accepted
 *      (not rejected as CG_ERR_SDK_VALIDATION); an invalid string IS rejected.
 *      This sub-test does NOT execute live searches; it just verifies that
 *      parse_search_type accepts all 15 known values and rejects unknowns.
 *      (Mirrors the TS locked SearchType ↔ string test.)
 *
 *   2. Valid cg_sdk_search with a non-LLM search type (CHUNKS_LEXICAL) against
 *      an empty mock store → CG_OK, well-formed JSON array.
 *
 *   3. cg_sdk_recall with each scope variant that does NOT invoke an LLM on an
 *      empty store ("session", "trace", "graph_context") → CG_OK, result
 *      object with "items", "searchTypeUsed", "autoRouted", "searchResponse"
 *      keys.
 *
 *   4. cg_sdk_recall scope as a string array → CG_OK.
 *
 *   5. cg_sdk_recall with null opts (default auto scope) — verifies that the
 *      call round-trips without crashing; the specific LLM-dispatching path
 *      ("auto", "graph") is covered by Tier-B only.
 *
 * Isolation: each run creates a fresh temporary directory per PID.
 *
 * Environment:
 *   MOCK_EMBEDDING=true   — also redundantly set in the JSON settings overlay.
 *
 * Exit codes: 0 = all assertions passed, 1 = at least one failure.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
#  include <process.h>
#  define getpid _getpid
#else
#  include <unistd.h>
#endif

#include "cognee_sdk.h"

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

#define ASSERT_NEQ(a, b, msg)                                               \
    do {                                                                    \
        if ((a) == (b)) {                                                   \
            fprintf(stderr, "FAIL [%s:%d]: %s (both = %d)\n",              \
                    __FILE__, __LINE__, (msg), (int)(a));                   \
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

/** Run cg_sdk_search through the waiter; always returns result+code. */
static char *run_search(const CgSdk *sdk, const char *query,
                        const char *opts_json, CgErrorCode *out_code)
{
    CgSdkWaiter *w = cg_sdk_waiter_new();
    if (!w) { g_failures++; return NULL; }
    cg_sdk_search(sdk, query, opts_json, cg_sdk_waiter_callback, (void *)w);
    char *result = NULL;
    CgErrorCode code = cg_sdk_waiter_wait(w, &result);
    cg_sdk_waiter_destroy(w);
    if (out_code) *out_code = code;
    return result;
}

/** Run cg_sdk_recall through the waiter; always returns result+code. */
static char *run_recall(const CgSdk *sdk, const char *query,
                        const char *opts_json, CgErrorCode *out_code)
{
    CgSdkWaiter *w = cg_sdk_waiter_new();
    if (!w) { g_failures++; return NULL; }
    cg_sdk_recall(sdk, query, opts_json, cg_sdk_waiter_callback, (void *)w);
    char *result = NULL;
    CgErrorCode code = cg_sdk_waiter_wait(w, &result);
    cg_sdk_waiter_destroy(w);
    if (out_code) *out_code = code;
    return result;
}

/* ── Main ─────────────────────────────────────────────────────────────────── */

int main(void)
{
    /* ── Runtime init ─────────────────────────────────────────────────────── */
    CgErrorCode rc = cg_init();
    ASSERT_EQ(rc, CG_OK, "cg_init() must succeed");
    if (rc != CG_OK) return 1;

    /* Build a handle with mock embedding and process-unique temp dirs. */
    char sys_dir[256];
    char data_dir[256];
    char db_url[512];
    snprintf(sys_dir,  sizeof(sys_dir),  "/tmp/cg_smoke_retrieval_%d_sys",  (int)getpid());
    snprintf(data_dir, sizeof(data_dir), "/tmp/cg_smoke_retrieval_%d_data", (int)getpid());
    snprintf(db_url, sizeof(db_url),
             "sqlite:/tmp/cg_smoke_retrieval_%d_sys/cognee.db?mode=rwc", (int)getpid());

    char settings[2048];
    snprintf(settings, sizeof(settings),
             "{"
             "  \"embedding_provider\": \"mock\","
             "  \"llm_api_key\": \"dummy-key-retrieval-smoke\","
             "  \"vector_db_provider\": \"brute-force\","
             "  \"system_root_directory\": \"%s\","
             "  \"data_root_directory\": \"%s\","
             "  \"relational_db_url\": \"%s\""
             "}",
             sys_dir, data_dir, db_url);

    CgSdk *sdk = cg_sdk_new(settings);
    ASSERT(sdk != NULL, "cg_sdk_new must return non-NULL");
    if (!sdk) {
        fprintf(stderr, "  last_error: %s\n",
                cg_last_error_message() ? cg_last_error_message() : "(none)");
        cg_shutdown();
        return 1;
    }

    /* Warm the handle. */
    {
        CgSdkWaiter *w = cg_sdk_waiter_new();
        cg_sdk_warm(sdk, cg_sdk_waiter_callback, (void *)w);
        rc = cg_sdk_waiter_wait(w, NULL);
        cg_sdk_waiter_destroy(w);
        ASSERT_EQ(rc, CG_OK, "cg_sdk_warm must succeed");
        if (rc != CG_OK) {
            fprintf(stderr, "  warm last_error: %s\n",
                    cg_last_error_message() ? cg_last_error_message() : "(none)");
            cg_sdk_destroy(sdk);
            cg_shutdown();
            return 1;
        }
    }
    printf("cg_sdk_warm  OK\n");

    /* ── Test 1: SearchType string-mapping check ─────────────────────────── */
    /*
     * This is a string-mapping sub-test only: verify that all 15 valid
     * SearchType strings are NOT rejected as CG_ERR_SDK_VALIDATION (14), and
     * that an invalid string IS rejected as CG_ERR_SDK_VALIDATION.
     *
     * Note: some search types call the LLM, so the result code for those will
     * be CG_ERR_RUNTIME (3 — LLM auth error) against a dummy-key handle.
     * That is expected and acceptable; the only failure we are guarding against
     * here is code 14 (unknown string).  The point of this test is that the
     * string-to-SearchType mapping is correct and complete.
     */
    printf("=== Test 1: SearchType string-mapping check ===\n");
    {
        /* 1a: invalid string must yield CG_ERR_SDK_VALIDATION. */
        CgErrorCode code = CG_OK;
        char *result = run_search(sdk, "test",
                                  "{\"searchType\":\"NOT_A_VALID_TYPE\"}", &code);
        ASSERT_EQ(code, CG_ERR_SDK_VALIDATION,
                  "invalid searchType must yield CG_ERR_SDK_VALIDATION");
        ASSERT(result == NULL, "result_json must be NULL on validation error");
        cg_string_destroy(result);
        printf("  invalid searchType → CG_ERR_SDK_VALIDATION  OK\n");

        /* 1b: all 15 valid strings must NOT yield CG_ERR_SDK_VALIDATION. */
        const char *search_types[] = {
            "SUMMARIES",
            "CHUNKS",
            "RAG_COMPLETION",
            "TRIPLET_COMPLETION",
            "GRAPH_COMPLETION",
            "GRAPH_SUMMARY_COMPLETION",
            "CYPHER",
            "NATURAL_LANGUAGE",
            "GRAPH_COMPLETION_COT",
            "GRAPH_COMPLETION_CONTEXT_EXTENSION",
            "FEELING_LUCKY",
            "FEEDBACK",
            "TEMPORAL",
            "CODING_RULES",
            "CHUNKS_LEXICAL",
        };
        int n_types = (int)(sizeof(search_types) / sizeof(search_types[0]));
        char opts_buf[128];

        for (int i = 0; i < n_types; i++) {
            snprintf(opts_buf, sizeof(opts_buf),
                     "{\"searchType\":\"%s\"}", search_types[i]);
            CgErrorCode st_code = CG_OK;
            char *st_result = run_search(sdk, "test", opts_buf, &st_code);
            /* Must NOT be rejected with validation error (code 14). */
            ASSERT_NEQ(st_code, CG_ERR_SDK_VALIDATION,
                       "valid searchType must NOT yield CG_ERR_SDK_VALIDATION");
            printf("  %-40s accepted (code=%d%s)\n",
                   search_types[i], (int)st_code,
                   st_code == CG_OK ? ", CG_OK" : "");
            cg_string_destroy(st_result);
        }
        printf("  all 15 searchType strings accepted (not CG_ERR_SDK_VALIDATION)  OK\n");
    }

    /* ── Test 2: cg_sdk_search non-LLM type → CG_OK, well-formed JSON ──── */
    /*
     * CHUNKS_LEXICAL is a pure text/vector retriever that does NOT call the LLM.
     * Against an empty store it returns CG_OK with an empty JSON array.
     */
    printf("=== Test 2: cg_sdk_search CHUNKS_LEXICAL (non-LLM) → CG_OK ===\n");
    {
        CgErrorCode code = CG_OK;
        char *result = run_search(sdk, "what is cognee?",
                                  "{\"searchType\":\"CHUNKS_LEXICAL\"}", &code);
        ASSERT_EQ(code, CG_OK, "CHUNKS_LEXICAL search must succeed against empty store");
        if (result) {
            /* SearchResponse is an array or object — must start with [ or {. */
            ASSERT(result[0] == '[' || result[0] == '{',
                   "search result must be a JSON array or object");
            printf("  CHUNKS_LEXICAL result: %.200s\n", result);
            cg_string_destroy(result);
        } else {
            fprintf(stderr, "  last_error: %s\n",
                    cg_last_error_message() ? cg_last_error_message() : "(none)");
        }
    }

    /* ── Test 3: cg_sdk_recall with non-LLM scope variants ──────────────── */
    /*
     * "session", "trace", and "graph_context" scopes query the session store
     * and graph context respectively — they do NOT call the LLM on an empty
     * store and return CG_OK with empty items.
     *
     * "auto" and "graph" dispatch to graph search which calls the LLM;
     * those are covered by Tier-B only.
     */
    printf("=== Test 3: cg_sdk_recall with non-LLM scope variants ===\n");
    {
        const char *non_llm_scopes[] = { "session", "trace", "graph_context" };
        int n_scopes = (int)(sizeof(non_llm_scopes) / sizeof(non_llm_scopes[0]));
        char opts_buf[128];

        for (int i = 0; i < n_scopes; i++) {
            snprintf(opts_buf, sizeof(opts_buf),
                     "{\"scope\":\"%s\"}", non_llm_scopes[i]);
            CgErrorCode code = CG_OK;
            char *result = run_recall(sdk, "what is cognee?", opts_buf, &code);
            ASSERT_EQ(code, CG_OK, "non-LLM scope recall must succeed");
            if (result) {
                assert_json_contains(result, "\"items\"",
                                     "recall result must contain items key");
                assert_json_contains(result, "\"searchTypeUsed\"",
                                     "recall result must contain searchTypeUsed key");
                assert_json_contains(result, "\"autoRouted\"",
                                     "recall result must contain autoRouted key");
                assert_json_contains(result, "\"searchResponse\"",
                                     "recall result must contain searchResponse key");
                printf("  scope=%-14s  OK  %.150s\n", non_llm_scopes[i], result);
                cg_string_destroy(result);
            } else {
                fprintf(stderr, "  scope=%s last_error: %s\n", non_llm_scopes[i],
                        cg_last_error_message() ? cg_last_error_message() : "(none)");
            }
        }
        printf("  non-LLM scope variants (session/trace/graph_context) OK\n");
    }

    /* ── Test 4: cg_sdk_recall scope as string array ─────────────────────── */
    printf("=== Test 4: cg_sdk_recall scope as string array [session,trace] ===\n");
    {
        CgErrorCode code = CG_OK;
        char *result = run_recall(sdk, "what is cognee?",
                                  "{\"scope\":[\"session\",\"trace\"]}", &code);
        ASSERT_EQ(code, CG_OK, "cg_sdk_recall scope array must succeed");
        if (result) {
            assert_json_contains(result, "\"items\"",
                                 "recall scope-array result must contain items");
            printf("  scope array  OK  %.150s\n", result);
            cg_string_destroy(result);
        }
    }

    /* ── Test 5: cg_sdk_recall null opts (call completes, no crash) ──────── */
    /*
     * null opts → auto scope → dispatches to graph search (LLM call).
     * With a dummy API key this yields CG_ERR_RUNTIME (LLM auth error).
     * The point of this sub-test is that the call round-trips correctly without
     * crashing and delivers a non-zero error code through the callback (D4/R1).
     * CG_OK would mean the search somehow succeeded; any code != crash is fine.
     */
    printf("=== Test 5: cg_sdk_recall null opts (round-trip, no crash) ===\n");
    {
        CgErrorCode code = CG_OK;
        char *result = run_recall(sdk, "what is cognee?", NULL, &code);
        /* Either CG_OK (unlikely with dummy key) or a non-crash error code. */
        ASSERT(code == CG_OK || code == CG_ERR_RUNTIME,
               "null-opts recall must return CG_OK or CG_ERR_RUNTIME (not crash)");
        printf("  null opts recall round-trip  code=%d  OK\n", (int)code);
        cg_string_destroy(result);
    }

    /* ── Cleanup ──────────────────────────────────────────────────────────── */
    cg_sdk_destroy(sdk);
    cg_shutdown();

    /* ── Result ───────────────────────────────────────────────────────────── */
    if (g_failures == 0) {
        printf("\nPASSED (sdk_retrieval_smoke)\n");
        return 0;
    } else {
        fprintf(stderr, "\nFAILED: %d assertion(s) failed\n", g_failures);
        return 1;
    }
}
