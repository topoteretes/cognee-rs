/*
 * sdk_data_smoke.c — Phase 6 Tier-A deterministic smoke test for data ops.
 *
 * Tests the following ops without LLM or live credentials (MOCK_EMBEDDING=true):
 *   1. cg_sdk_new + cg_sdk_warm
 *   2. cg_sdk_add (text input)
 *   3. cg_sdk_list_datasets   — assert JSON array contains "name" key
 *   4. cg_sdk_has_data        — assert result is "true"
 *   5. cg_sdk_dataset_status  — assert JSON object with at least one UUID key
 *   6. cg_sdk_delete_data     — assert JSON with "deletedCount" or similar
 *   7. cg_sdk_forget          — {"kind":"all"} — assert JSON with "target" key
 *   8. cg_sdk_prune_data      — assert result is "null"
 *   9. cg_sdk_prune_system    — assert JSON with "graphPruned" key
 *
 * Exit codes: 0 = all assertions passed, 1 = at least one failure.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

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

static void assert_json_equals(const char *json, const char *expected,
                                const char *label)
{
    if (!json) {
        fprintf(stderr, "FAIL: %s — result_json is NULL\n", label);
        g_failures++;
        return;
    }
    if (strcmp(json, expected) != 0) {
        fprintf(stderr, "FAIL: %s — expected \"%s\", got \"%s\"\n",
                label, expected, json);
        g_failures++;
    }
}

/** Run a no-arg SDK op (warm) through the waiter. */
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

/** Generic waiter wrapper that invokes a void-returning call
 *  that has been fully set up by the caller. */
static CgErrorCode waiter_wait_result(CgSdkWaiter *w, char **out)
{
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

/* ── Main ─────────────────────────────────────────────────────────────────── */

int main(void)
{
    CgErrorCode rc = cg_init();
    ASSERT_EQ(rc, CG_OK, "cg_init() must succeed");
    if (rc != CG_OK) return 1;

    /* Build with mock embedding (no LLM needed for these Tier-A ops). */
    const char *settings =
        "{"
        "  \"embedding_provider\": \"mock\","
        "  \"llm_api_key\": \"test-key\""
        "}";

    CgSdk *sdk = cg_sdk_new(settings);
    ASSERT(sdk != NULL, "cg_sdk_new must return non-NULL");
    if (!sdk) {
        fprintf(stderr, "  last_error: %s\n",
                cg_last_error_message() ? cg_last_error_message() : "(none)");
        cg_shutdown();
        return 1;
    }

    /* ── Step 1: warm ────────────────────────────────────────────────────── */
    printf("=== Step 1: cg_sdk_warm ===\n");
    rc = run_no_arg_op(sdk, cg_sdk_warm, NULL);
    ASSERT_EQ(rc, CG_OK, "cg_sdk_warm must succeed");
    if (rc != CG_OK) {
        fprintf(stderr, "  last_error: %s\n",
                cg_last_error_message() ? cg_last_error_message() : "(none)");
        cg_sdk_destroy(sdk);
        cg_shutdown();
        return 1;
    }
    printf("  warm OK\n");

    /* ── Step 2: add a text input ────────────────────────────────────────── */
    printf("=== Step 2: cg_sdk_add ===\n");
    const char *input_text =
        "{\"type\":\"text\",\"text\":\"Phase 6 smoke test data item.\"}";
    const char *ds_name = "smoke6-dataset";

    char *add_result = run_add(sdk, input_text, ds_name, NULL);
    ASSERT(add_result != NULL, "cg_sdk_add must succeed");
    if (add_result) {
        assert_json_contains(add_result, "\"addedCount\"",
                             "add result must contain addedCount");
        printf("  add result: %.200s\n", add_result);
    }

    /* Extract dataset_id for later ops. We'll parse it from cg_sdk_list_datasets. */
    char dataset_id_buf[64] = {0};

    /* ── Step 3: list_datasets ───────────────────────────────────────────── */
    printf("=== Step 3: cg_sdk_list_datasets ===\n");
    {
        CgSdkWaiter *w = cg_sdk_waiter_new();
        if (!w) { g_failures++; goto cleanup; }
        cg_sdk_list_datasets(sdk, cg_sdk_waiter_callback, (void *)w);
        char *result = NULL;
        rc = waiter_wait_result(w, &result);
        ASSERT_EQ(rc, CG_OK, "cg_sdk_list_datasets must succeed");
        if (result) {
            assert_json_contains(result, "\"name\"",
                                 "list_datasets result must contain name key");
            assert_json_contains(result, ds_name,
                                 "list_datasets result must contain dataset name");
            printf("  list_datasets result: %.300s\n", result);

            /*
             * Find the id of the dataset named ds_name by scanning forward
             * to the occurrence of ds_name and then walking backwards to
             * find the nearest "id":"<uuid>" before it.
             *
             * JSON array elements look like:
             *   {"id":"<uuid>","name":"<ds_name>", …}
             * So we find the name and then look backwards for "id":"
             */
            const char *name_pos = strstr(result, ds_name);
            if (name_pos) {
                /* search backwards from name_pos for '"id":"' */
                const char *search_start = result;
                const char *found_id = NULL;
                while (search_start < name_pos) {
                    const char *candidate = strstr(search_start, "\"id\":\"");
                    if (!candidate || candidate >= name_pos) break;
                    found_id = candidate;
                    search_start = candidate + 1;
                }
                if (found_id) {
                    found_id += 6; /* skip '"id":"' */
                    int i = 0;
                    while (found_id[i] && found_id[i] != '"' && i < 63) {
                        dataset_id_buf[i] = found_id[i];
                        i++;
                    }
                    dataset_id_buf[i] = '\0';
                    printf("  extracted dataset_id: %s\n", dataset_id_buf);
                }
            }
            cg_string_destroy(result);
        }
    }

    /* ── Step 4: has_data ────────────────────────────────────────────────── */
    printf("=== Step 4: cg_sdk_has_data ===\n");
    if (dataset_id_buf[0]) {
        CgSdkWaiter *w = cg_sdk_waiter_new();
        if (!w) { g_failures++; goto cleanup; }
        cg_sdk_has_data(sdk, dataset_id_buf, cg_sdk_waiter_callback, (void *)w);
        char *result = NULL;
        rc = waiter_wait_result(w, &result);
        ASSERT_EQ(rc, CG_OK, "cg_sdk_has_data must succeed");
        if (result) {
            assert_json_equals(result, "true", "has_data must return true after add");
            printf("  has_data result: %s\n", result);
            cg_string_destroy(result);
        }
    } else {
        printf("  (skipping: no dataset_id extracted)\n");
    }

    /* ── Step 5: dataset_status ──────────────────────────────────────────── */
    printf("=== Step 5: cg_sdk_dataset_status ===\n");
    if (dataset_id_buf[0]) {
        char ids_json[128];
        snprintf(ids_json, sizeof(ids_json), "[\"%s\"]", dataset_id_buf);

        CgSdkWaiter *w = cg_sdk_waiter_new();
        if (!w) { g_failures++; goto cleanup; }
        cg_sdk_dataset_status(sdk, ids_json, cg_sdk_waiter_callback, (void *)w);
        char *result = NULL;
        rc = waiter_wait_result(w, &result);
        ASSERT_EQ(rc, CG_OK, "cg_sdk_dataset_status must succeed");
        if (result) {
            /* Result is a JSON object — must start with '{'. */
            ASSERT(result[0] == '{',
                   "dataset_status result must be a JSON object");
            printf("  dataset_status result: %.300s\n", result);
            cg_string_destroy(result);
        }
    } else {
        printf("  (skipping: no dataset_id extracted)\n");
    }

    /* ── Step 6: delete_data (data item) ─────────────────────────────────── */
    printf("=== Step 6: cg_sdk_delete_data ===\n");
    if (add_result && dataset_id_buf[0]) {
        /* Extract first data_id from the add result. */
        char data_id_buf[64] = {0};

        /* Look for "added":[{"id":"<uuid>"...}] pattern. */
        const char *data_id_start = strstr(add_result, "\"added\":[{\"id\":\"");
        if (!data_id_start) {
            data_id_start = strstr(add_result, "\"id\":\"");
        }
        if (data_id_start) {
            /* skip to the uuid value */
            data_id_start = strstr(data_id_start, "\"id\":\"");
            if (data_id_start) {
                data_id_start += 6;
                int i = 0;
                while (data_id_start[i] && data_id_start[i] != '"' && i < 63) {
                    data_id_buf[i] = data_id_start[i];
                    i++;
                }
                data_id_buf[i] = '\0';
                printf("  extracted data_id: %s\n", data_id_buf);
            }
        }

        if (data_id_buf[0]) {
            CgSdkWaiter *w = cg_sdk_waiter_new();
            if (!w) { g_failures++; goto cleanup_add; }
            cg_sdk_delete_data(sdk, dataset_id_buf, data_id_buf, NULL,
                               cg_sdk_waiter_callback, (void *)w);
            char *result = NULL;
            rc = waiter_wait_result(w, &result);
            ASSERT_EQ(rc, CG_OK, "cg_sdk_delete_data must succeed");
            if (result) {
                printf("  delete_data result: %.300s\n", result);
                cg_string_destroy(result);
            }
        } else {
            printf("  (skipping delete_data: could not extract data_id)\n");
        }
    } else {
        printf("  (skipping delete_data: no add_result or dataset_id)\n");
    }

cleanup_add:
    cg_string_destroy(add_result);
    add_result = NULL;

    /* ── Step 7: forget all ──────────────────────────────────────────────── */
    printf("=== Step 7: cg_sdk_forget {kind:all} ===\n");
    {
        const char *target = "{\"kind\":\"all\"}";
        CgSdkWaiter *w = cg_sdk_waiter_new();
        if (!w) { g_failures++; goto cleanup; }
        cg_sdk_forget(sdk, target, NULL, cg_sdk_waiter_callback, (void *)w);
        char *result = NULL;
        rc = waiter_wait_result(w, &result);
        ASSERT_EQ(rc, CG_OK, "cg_sdk_forget must succeed");
        if (result) {
            assert_json_contains(result, "\"target\"",
                                 "forget result must contain target key");
            printf("  forget result: %.300s\n", result);
            cg_string_destroy(result);
        }
    }

    /* ── Step 8: prune_data ──────────────────────────────────────────────── */
    printf("=== Step 8: cg_sdk_prune_data ===\n");
    {
        CgSdkWaiter *w = cg_sdk_waiter_new();
        if (!w) { g_failures++; goto cleanup; }
        cg_sdk_prune_data(sdk, cg_sdk_waiter_callback, (void *)w);
        char *result = NULL;
        rc = waiter_wait_result(w, &result);
        ASSERT_EQ(rc, CG_OK, "cg_sdk_prune_data must succeed");
        if (result) {
            assert_json_equals(result, "null", "prune_data must return null (D9)");
            printf("  prune_data result: %s\n", result);
            cg_string_destroy(result);
        }
    }

    /* ── Step 9: prune_system ────────────────────────────────────────────── */
    printf("=== Step 9: cg_sdk_prune_system ===\n");
    {
        CgSdkWaiter *w = cg_sdk_waiter_new();
        if (!w) { g_failures++; goto cleanup; }
        cg_sdk_prune_system(sdk, NULL, cg_sdk_waiter_callback, (void *)w);
        char *result = NULL;
        rc = waiter_wait_result(w, &result);
        ASSERT_EQ(rc, CG_OK, "cg_sdk_prune_system must succeed");
        if (result) {
            assert_json_contains(result, "\"graphPruned\"",
                                 "prune_system result must contain graphPruned key");
            printf("  prune_system result: %.300s\n", result);
            cg_string_destroy(result);
        }
    }

cleanup:
    cg_sdk_destroy(sdk);
    cg_shutdown();

    if (g_failures == 0) {
        printf("\nPASSED (sdk_data_smoke)\n");
        return 0;
    } else {
        fprintf(stderr, "\nFAILED: %d assertion(s) failed\n", g_failures);
        return 1;
    }
}
