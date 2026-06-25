/*
 * example_sdk_add.c — Phase 4 Tier-A smoke test for cg_sdk_add.
 *
 * Tests (no network, no LLM required — MOCK_EMBEDDING=true):
 *
 *   1. Add two distinct text inputs → addedCount == 2, deduplicatedCount == 0.
 *
 *   2. Re-add the same two texts (duplicates) → addedCount == 0,
 *      deduplicatedCount == 2.
 *
 *   3. Add one new + one duplicate → addedCount == 1, deduplicatedCount == 1.
 *
 * Result validation uses strstr() substring checks on stable JSON keys
 * ("addedCount", "deduplicatedCount") — no JSON library dependency in examples.
 *
 * Isolation: each run creates a fresh temporary directory under /tmp (using
 * the process PID) and configures system_root_directory / data_root_directory
 * inside that tmpdir so the test never touches the shared ~/.cognee store.
 *
 * Environment:
 *   MOCK_EMBEDDING=true   — also set in the JSON settings overlay (redundantly)
 *                           so this example is self-contained.
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

/** Assert that a JSON string contains the given substring. */
static void assert_json_contains(const char *json, const char *needle,
                                 const char *label)
{
    if (!json) {
        fprintf(stderr, "FAIL: %s — result_json is NULL\n", label);
        g_failures++;
        return;
    }
    if (!strstr(json, needle)) {
        fprintf(stderr, "FAIL: %s — expected \"%s\" in:\n  %.300s\n",
                label, needle, json);
        g_failures++;
    }
}

/** Run cg_sdk_add through the waiter and return the JSON result.
 *  Returns NULL on error (failures counted).  Caller must cg_string_destroy. */
static char *run_add(const CgSdk *sdk, const char *inputs_json,
                     const char *dataset_name, const char *opts_json)
{
    CgSdkWaiter *w = cg_sdk_waiter_new();
    if (!w) {
        fprintf(stderr, "cg_sdk_waiter_new() returned NULL\n");
        g_failures++;
        return NULL;
    }
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

/* ── Main ─────────────────────────────────────────────────────────────────── */

int main(void)
{
    /* ── Runtime init ─────────────────────────────────────────────────────── */
    CgErrorCode rc = cg_init();
    ASSERT_EQ(rc, CG_OK, "cg_init() must succeed");
    if (rc != CG_OK) return 1;

    /*
     * Build a handle with mock embedding and process-unique temp directories
     * so each run starts with a clean slate (no cross-run data pollution).
     *
     * system_root_directory holds the SQLite DB + graph DB.
     * data_root_directory   holds the ingested file storage.
     */
    char sys_dir[256];
    char data_dir[256];
    char db_url[512];
    snprintf(sys_dir,  sizeof(sys_dir),  "/tmp/cg_smoke_add_%d_sys",  (int)getpid());
    snprintf(data_dir, sizeof(data_dir), "/tmp/cg_smoke_add_%d_data", (int)getpid());
    /* SQLite URL must be absolute so it doesn't depend on CWD. */
    snprintf(db_url, sizeof(db_url),
             "sqlite:/tmp/cg_smoke_add_%d_sys/cognee.db?mode=rwc", (int)getpid());

    char settings[2048];
    snprintf(settings, sizeof(settings),
             "{"
             "  \"embedding_provider\": \"mock\","
             "  \"llm_api_key\": \"dummy-key-sdk-add-smoke\","
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

    /*
     * Use a unique dataset name per invocation (PID-based) to avoid cross-run
     * data pollution.  The content-addressed UUIDs are stable, so if two runs
     * share a dataset name the second run sees the first run's data as
     * "pre-existing" and reports addedCount=0 instead of 2.
     */
    char dataset_name[128];
    snprintf(dataset_name, sizeof(dataset_name), "smoke-add-dataset-%d", (int)getpid());

    /* Warm the handle before running ops. */
    {
        CgSdkWaiter *w = cg_sdk_waiter_new();
        cg_sdk_warm(sdk, cg_sdk_waiter_callback, (void *)w);
        rc = cg_sdk_waiter_wait(w, NULL);
        cg_sdk_waiter_destroy(w);
        ASSERT_EQ(rc, CG_OK, "cg_sdk_warm must succeed");
        if (rc != CG_OK) {
            fprintf(stderr, "  cg_sdk_warm last_error: %s\n",
                    cg_last_error_message() ? cg_last_error_message() : "(none)");
            cg_sdk_destroy(sdk);
            cg_shutdown();
            return 1;
        }
    }
    printf("cg_sdk_warm  OK  (dataset=%s)\n", dataset_name);

    /* ── Test 1: add two distinct texts ──────────────────────────────────── */
    printf("=== Test 1: add two distinct texts (addedCount=2) ===\n");
    {
        const char *inputs =
            "[{\"type\":\"text\",\"text\":\"Hello, cognee!\"},"
            " {\"type\":\"text\",\"text\":\"A second distinct piece of text.\"}]";

        char *result = run_add(sdk, inputs, dataset_name, NULL);
        ASSERT(result != NULL, "cg_sdk_add must succeed for two new texts");
        if (result) {
            assert_json_contains(result, "\"addedCount\":2",
                                 "addedCount must be 2 for two new texts");
            assert_json_contains(result, "\"deduplicatedCount\":0",
                                 "deduplicatedCount must be 0 for new texts");
            assert_json_contains(result, "\"datasetName\"",
                                 "result must contain datasetName key");
            printf("  addedCount=2, deduplicatedCount=0  OK\n");
            cg_string_destroy(result);
        }
    }

    /* ── Test 2: re-add the same texts (duplicates) ──────────────────────── */
    printf("=== Test 2: re-add same texts (deduplicatedCount=2) ===\n");
    {
        const char *inputs =
            "[{\"type\":\"text\",\"text\":\"Hello, cognee!\"},"
            " {\"type\":\"text\",\"text\":\"A second distinct piece of text.\"}]";

        char *result = run_add(sdk, inputs, dataset_name, NULL);
        ASSERT(result != NULL, "cg_sdk_add must succeed for duplicate texts");
        if (result) {
            assert_json_contains(result, "\"addedCount\":0",
                                 "addedCount must be 0 for all-duplicate add");
            assert_json_contains(result, "\"deduplicatedCount\":2",
                                 "deduplicatedCount must be 2 for all-duplicate add");
            printf("  addedCount=0, deduplicatedCount=2  OK\n");
            cg_string_destroy(result);
        }
    }

    /* ── Test 3: one new + one duplicate ─────────────────────────────────── */
    printf("=== Test 3: one new + one duplicate (addedCount=1, deduplicatedCount=1) ===\n");
    {
        const char *inputs =
            "[{\"type\":\"text\",\"text\":\"Hello, cognee!\"},"
            " {\"type\":\"text\",\"text\":\"Brand new text not seen before.\"}]";

        char *result = run_add(sdk, inputs, dataset_name, NULL);
        ASSERT(result != NULL, "cg_sdk_add must succeed for mixed add");
        if (result) {
            assert_json_contains(result, "\"addedCount\":1",
                                 "addedCount must be 1 for one new + one dup");
            assert_json_contains(result, "\"deduplicatedCount\":1",
                                 "deduplicatedCount must be 1 for one new + one dup");
            printf("  addedCount=1, deduplicatedCount=1  OK\n");
            cg_string_destroy(result);
        }
    }

    /* ── Cleanup ──────────────────────────────────────────────────────────── */
    cg_sdk_destroy(sdk);
    cg_shutdown();

    /* ── Result ───────────────────────────────────────────────────────────── */
    if (g_failures == 0) {
        printf("\nPASSED (example_sdk_add)\n");
        return 0;
    } else {
        fprintf(stderr, "\nFAILED: %d assertion(s) failed\n", g_failures);
        return 1;
    }
}
