/*
 * sdk_config_smoke.c — Phase 3 Tier-A smoke test for the config surface.
 *
 * Tests (no network, no LLM required):
 *
 *   1. API version bump: cg_api_version() now returns (1 << 16) | 2 (minor=2).
 *
 *   2. cg_sdk_config_set_str: set a key, verify cg_sdk_config_get reflects it.
 *
 *   3. cg_sdk_config_set with JSON value: set a numeric key, read it back.
 *
 *   4. cg_sdk_config_set_llm_config bulk setter: set multiple LLM keys at once.
 *
 *   5. cg_sdk_config_set_embedding_config bulk setter: set embedding keys.
 *
 *   6. cg_sdk_config_set_vector_db_config bulk setter.
 *
 *   7. cg_sdk_config_set_graph_db_config bulk setter.
 *
 *   8. Unknown key → CG_ERR_UNKNOWN_CONFIG_KEY (17).
 *
 *   9. Type mismatch → CG_ERR_CONFIG_TYPE_MISMATCH (18).
 *
 *  10. Malformed JSON → CG_ERR_SDK_VALIDATION (14).
 *
 *  11. Rebuild-on-change: version bumps after set; the next cg_sdk_warm
 *      rebuilds services (observable because services() checks version).
 *
 *  12. Secret fields are redacted in cg_sdk_config_get output.
 *
 * Environment:
 *   MOCK_EMBEDDING=true / embedding_provider = "mock" (set in test).
 *
 * Exit codes: 0 = all assertions passed, 1 = at least one failure.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cognee_sdk.h"

/* ── Helpers ──────────────────────────────────────────────────────────────── */

static int g_failures = 0;

#define ASSERT(cond, msg)                                             \
    do {                                                              \
        if (!(cond)) {                                                \
            fprintf(stderr, "FAIL [%s:%d]: %s\n",                    \
                    __FILE__, __LINE__, (msg));                       \
            g_failures++;                                             \
        }                                                             \
    } while (0)

#define ASSERT_EQ(a, b, msg)                                          \
    do {                                                              \
        if ((a) != (b)) {                                             \
            fprintf(stderr, "FAIL [%s:%d]: %s (got %d, want %d)\n",  \
                    __FILE__, __LINE__, (msg), (int)(a), (int)(b));   \
            g_failures++;                                             \
        }                                                             \
    } while (0)

/** Check that a JSON string contains the expected substring. */
static void assert_json_contains(const char* json,
                                 const char* needle,
                                 const char* label)
{
    if (!json) {
        fprintf(stderr, "FAIL: %s — out_json is NULL\n", label);
        g_failures++;
        return;
    }
    if (!strstr(json, needle)) {
        fprintf(stderr, "FAIL: %s — expected \"%s\" in JSON:\n  %s\n",
                label, needle, json);
        g_failures++;
    }
}

/** Check that a JSON string does NOT contain a substring. */
static void assert_json_not_contains(const char* json,
                                     const char* needle,
                                     const char* label)
{
    if (!json) {
        fprintf(stderr, "FAIL: %s — out_json is NULL\n", label);
        g_failures++;
        return;
    }
    if (strstr(json, needle)) {
        fprintf(stderr, "FAIL: %s — did not expect \"%s\" in JSON:\n  %.200s\n",
                label, needle, json);
        g_failures++;
    }
}

/** Run cg_sdk_warm through the waiter, return the error code. */
static CgErrorCode warm_sdk(CgSdk* sdk)
{
    CgSdkWaiter* w = cg_sdk_waiter_new();
    if (!w) return CG_ERR_RUNTIME;
    cg_sdk_warm(sdk, cg_sdk_waiter_callback, (void*)w);
    CgErrorCode code = cg_sdk_waiter_wait(w, NULL);
    cg_sdk_waiter_destroy(w);
    return code;
}

/* ── Main ─────────────────────────────────────────────────────────────────── */

int main(void)
{
    /* ── Runtime init ────────────────────────────────────────────────────── */
    CgErrorCode rc = cg_init();
    ASSERT_EQ(rc, CG_OK, "cg_init() must succeed");
    if (rc != CG_OK) return 1;

    /* Build a handle with mock embedding so no network/models are needed.
     * vector_db_provider=mock selects MockVectorDB (testing feature)
     * since T4 moved the Qdrant adapter to the closed
     * cognee-vector-qdrant crate. T5 will introduce a brute-force default. */
    const char* base_settings =
        "{"
        "  \"embedding_provider\": \"mock\","
        "  \"llm_api_key\": \"dummy-key-smoke-test\","
        "  \"vector_db_provider\": \"mock\""
        "}";
    CgSdk* sdk = cg_sdk_new(base_settings);
    ASSERT(sdk != NULL, "cg_sdk_new must return non-NULL");
    if (!sdk) {
        fprintf(stderr, "  last_error: %s\n",
                cg_last_error_message() ? cg_last_error_message() : "(none)");
        cg_shutdown();
        return 1;
    }

    /* ── Test 1: API version check (major=1, minor>=2 after Phase 3) ───────
     * Accept any minor >= 2 so later phases (Phase 4+) that bump the minor
     * do not break this Phase-3 test.                                        */
    printf("=== Test 1: cg_api_version() major=1 minor>=2 ===\n");
    uint32_t ver = cg_api_version();
    uint32_t ver_major = ver >> 16;
    uint32_t ver_minor = ver & 0xffffu;
    ASSERT(ver_major == 1u && ver_minor >= 2u,
           "cg_api_version must return major=1, minor>=2 after Phase 3");
    printf("  cg_api_version() = 0x%08x (major=%u minor=%u)  %s\n", ver,
           ver_major, ver_minor,
           (ver_major == 1u && ver_minor >= 2u) ? "OK" : "FAIL");

    /* ── Test 2: cg_sdk_config_set_str ──────────────────────────────────── */
    printf("=== Test 2: cg_sdk_config_set_str / cg_sdk_config_get round-trip ===\n");

    rc = cg_sdk_config_set_str(sdk, "llm_model", "smoke-test-model");
    ASSERT_EQ(rc, CG_OK, "cg_sdk_config_set_str(llm_model) must return CG_OK");

    char* cfg_json = NULL;
    rc = cg_sdk_config_get(sdk, &cfg_json);
    ASSERT_EQ(rc, CG_OK, "cg_sdk_config_get must return CG_OK");
    assert_json_contains(cfg_json, "smoke-test-model",
                         "config JSON must contain the new llm_model value");
    printf("  set_str + get round-trip  OK\n");
    cg_string_destroy(cfg_json);
    cfg_json = NULL;

    /* ── Test 3: cg_sdk_config_set with JSON numeric value ───────────────── */
    printf("=== Test 3: cg_sdk_config_set (JSON numeric) ===\n");

    rc = cg_sdk_config_set(sdk, "chunk_size", "2048");
    ASSERT_EQ(rc, CG_OK, "cg_sdk_config_set(chunk_size, 2048) must return CG_OK");

    rc = cg_sdk_config_get(sdk, &cfg_json);
    ASSERT_EQ(rc, CG_OK, "cg_sdk_config_get must return CG_OK");
    assert_json_contains(cfg_json, "2048",
                         "config JSON must reflect the new chunk_size");
    printf("  set JSON numeric + get round-trip  OK\n");
    cg_string_destroy(cfg_json);
    cfg_json = NULL;

    /* ── Test 4: cg_sdk_config_set_llm_config bulk setter ───────────────── */
    printf("=== Test 4: cg_sdk_config_set_llm_config bulk setter ===\n");

    const char* llm_cfg =
        "{"
        "  \"llm_provider\": \"openai\","
        "  \"llm_model\": \"gpt-4o-mini\","
        "  \"llm_temperature\": 0.5"
        "}";
    rc = cg_sdk_config_set_llm_config(sdk, llm_cfg);
    ASSERT_EQ(rc, CG_OK, "cg_sdk_config_set_llm_config must return CG_OK");

    rc = cg_sdk_config_get(sdk, &cfg_json);
    ASSERT_EQ(rc, CG_OK, "cg_sdk_config_get must return CG_OK after bulk LLM set");
    assert_json_contains(cfg_json, "gpt-4o-mini",
                         "config JSON must contain the bulk-set llm_model");
    assert_json_contains(cfg_json, "openai",
                         "config JSON must contain the bulk-set llm_provider");
    printf("  bulk LLM config set + get  OK\n");
    cg_string_destroy(cfg_json);
    cfg_json = NULL;

    /* ── Test 5: cg_sdk_config_set_embedding_config bulk setter ──────────── */
    printf("=== Test 5: cg_sdk_config_set_embedding_config bulk setter ===\n");

    const char* emb_cfg =
        "{"
        "  \"embedding_provider\": \"mock\","
        "  \"embedding_dimensions\": 768"
        "}";
    rc = cg_sdk_config_set_embedding_config(sdk, emb_cfg);
    ASSERT_EQ(rc, CG_OK, "cg_sdk_config_set_embedding_config must return CG_OK");

    rc = cg_sdk_config_get(sdk, &cfg_json);
    ASSERT_EQ(rc, CG_OK, "cg_sdk_config_get must return CG_OK after bulk embedding set");
    assert_json_contains(cfg_json, "768",
                         "config JSON must reflect bulk-set embedding_dimensions");
    printf("  bulk embedding config set + get  OK\n");
    cg_string_destroy(cfg_json);
    cfg_json = NULL;

    /* ── Test 6: cg_sdk_config_set_vector_db_config bulk setter ──────────── */
    printf("=== Test 6: cg_sdk_config_set_vector_db_config bulk setter ===\n");

    /* Use `mock` instead of `qdrant` post-T4 (Qdrant moved closed).
     * The bulk-set + get round-trip is what's under test here, not the
     * provider value itself. */
    const char* vec_cfg =
        "{"
        "  \"vector_db_provider\": \"mock\","
        "  \"vector_db_host\": \"127.0.0.1\""
        "}";
    rc = cg_sdk_config_set_vector_db_config(sdk, vec_cfg);
    ASSERT_EQ(rc, CG_OK, "cg_sdk_config_set_vector_db_config must return CG_OK");

    rc = cg_sdk_config_get(sdk, &cfg_json);
    ASSERT_EQ(rc, CG_OK, "cg_sdk_config_get must return CG_OK after bulk vector set");
    assert_json_contains(cfg_json, "mock",
                         "config JSON must reflect bulk-set vector_db_provider");
    printf("  bulk vector DB config set + get  OK\n");
    cg_string_destroy(cfg_json);
    cfg_json = NULL;

    /* ── Test 7: cg_sdk_config_set_graph_db_config bulk setter ──────────── */
    printf("=== Test 7: cg_sdk_config_set_graph_db_config bulk setter ===\n");

    const char* graph_cfg =
        "{"
        "  \"graph_database_provider\": \"ladybug\","
        "  \"graph_file_path\": \"/tmp/smoke-graph\""
        "}";
    rc = cg_sdk_config_set_graph_db_config(sdk, graph_cfg);
    ASSERT_EQ(rc, CG_OK, "cg_sdk_config_set_graph_db_config must return CG_OK");

    rc = cg_sdk_config_get(sdk, &cfg_json);
    ASSERT_EQ(rc, CG_OK, "cg_sdk_config_get must return CG_OK after bulk graph set");
    assert_json_contains(cfg_json, "ladybug",
                         "config JSON must reflect bulk-set graph_database_provider");
    printf("  bulk graph DB config set + get  OK\n");
    cg_string_destroy(cfg_json);
    cfg_json = NULL;

    /* ── Test 8: unknown key → CG_ERR_UNKNOWN_CONFIG_KEY (17) ───────────── */
    printf("=== Test 8: unknown key → CG_ERR_UNKNOWN_CONFIG_KEY ===\n");

    rc = cg_sdk_config_set_str(sdk, "nonexistent_key_xyz", "value");
    ASSERT_EQ(rc, (CgErrorCode)CG_ERR_UNKNOWN_CONFIG_KEY,
              "unknown key must return CG_ERR_UNKNOWN_CONFIG_KEY (17)");
    ASSERT(cg_last_error_message() != NULL,
           "unknown key must set cg_last_error_message()");
    if (rc == (CgErrorCode)CG_ERR_UNKNOWN_CONFIG_KEY) {
        printf("  unknown key: code=%d, msg=\"%s\"  OK\n",
               (int)rc,
               cg_last_error_message() ? cg_last_error_message() : "(none)");
    }

    /* Also test via the generic set */
    rc = cg_sdk_config_set(sdk, "another_bad_key", "\"value\"");
    ASSERT_EQ(rc, (CgErrorCode)CG_ERR_UNKNOWN_CONFIG_KEY,
              "generic set with unknown key must return CG_ERR_UNKNOWN_CONFIG_KEY");

    /* ── Test 9: type mismatch → CG_ERR_CONFIG_TYPE_MISMATCH (18) ───────── */
    printf("=== Test 9: type mismatch → CG_ERR_CONFIG_TYPE_MISMATCH ===\n");

    /* chunk_size expects a number; give it a string. */
    rc = cg_sdk_config_set(sdk, "chunk_size", "\"not-a-number\"");
    ASSERT_EQ(rc, (CgErrorCode)CG_ERR_CONFIG_TYPE_MISMATCH,
              "type mismatch must return CG_ERR_CONFIG_TYPE_MISMATCH (18)");
    ASSERT(cg_last_error_message() != NULL,
           "type mismatch must set cg_last_error_message()");
    if (rc == (CgErrorCode)CG_ERR_CONFIG_TYPE_MISMATCH) {
        printf("  type mismatch: code=%d, msg=\"%s\"  OK\n",
               (int)rc,
               cg_last_error_message() ? cg_last_error_message() : "(none)");
    }

    /* ── Test 10: malformed JSON → CG_ERR_SDK_VALIDATION (14) ──────────── */
    printf("=== Test 10: malformed JSON → CG_ERR_SDK_VALIDATION ===\n");

    rc = cg_sdk_config_set(sdk, "llm_model", "{ this is not json");
    ASSERT_EQ(rc, (CgErrorCode)CG_ERR_SDK_VALIDATION,
              "malformed JSON in value_json must return CG_ERR_SDK_VALIDATION (14)");
    if (rc == (CgErrorCode)CG_ERR_SDK_VALIDATION) {
        printf("  malformed value_json: code=%d  OK\n", (int)rc);
    }

    /* Malformed JSON in bulk setter. */
    rc = cg_sdk_config_set_llm_config(sdk, "not valid json !!!");
    ASSERT_EQ(rc, (CgErrorCode)CG_ERR_SDK_VALIDATION,
              "malformed JSON in bulk setter must return CG_ERR_SDK_VALIDATION (14)");
    if (rc == (CgErrorCode)CG_ERR_SDK_VALIDATION) {
        printf("  malformed bulk JSON: code=%d  OK\n", (int)rc);
    }

    /* ── Test 11: version bump / rebuild-on-change ───────────────────────── */
    printf("=== Test 11: version bump triggers services rebuild ===\n");

    /*
     * Warm the handle to build the initial service bundle at version V0.
     * The services cache is keyed on the config version, so any set() call
     * that bumps the version will invalidate the cache and force a rebuild
     * on the next services() call (i.e. on the next cg_sdk_warm).
     *
     * We cannot directly read the version counter from C, but we CAN observe
     * the rebuild by:
     *   1. Warm once (V0 cached).
     *   2. Call config_set (bumps to V1 → cache invalidated).
     *   3. Warm again — must succeed (rebuilds at V1).
     *
     * Both warms must return CG_OK, confirming the rebuild path is functional.
     */
    rc = warm_sdk(sdk);
    ASSERT_EQ(rc, CG_OK, "first warm must succeed");
    printf("  first warm (V0)  OK\n");

    /* Change a non-service-critical key to bump the version. */
    rc = cg_sdk_config_set_str(sdk, "monitoring_tool", "none");
    ASSERT_EQ(rc, CG_OK, "set after warm must return CG_OK");

    /* Second warm: must rebuild services (version advanced). */
    rc = warm_sdk(sdk);
    ASSERT_EQ(rc, CG_OK, "second warm after config change must succeed (rebuild path)");
    printf("  second warm after config change (V1 rebuild)  OK\n");

    /* ── Test 12: secret fields are redacted in cg_sdk_config_get ─────────── */
    printf("=== Test 12: secret fields redacted in cg_sdk_config_get ===\n");

    /*
     * Set a known "secret" key to a recognisable sentinel value, then read
     * back the config and verify:
     *   (a) the sentinel is NOT present (redacted),
     *   (b) the redaction placeholder IS present.
     */
    rc = cg_sdk_config_set_str(sdk, "llm_api_key", "super-secret-value-12345");
    ASSERT_EQ(rc, CG_OK, "set llm_api_key must succeed");

    rc = cg_sdk_config_get(sdk, &cfg_json);
    ASSERT_EQ(rc, CG_OK, "cg_sdk_config_get must return CG_OK");
    assert_json_not_contains(cfg_json, "super-secret-value-12345",
                             "secret value must NOT appear in config JSON");
    assert_json_contains(cfg_json, "***REDACTED***",
                         "config JSON must contain redaction placeholder");
    printf("  secret field redacted  OK\n");
    cg_string_destroy(cfg_json);
    cfg_json = NULL;

    /* ── Cleanup ─────────────────────────────────────────────────────────── */
    cg_sdk_destroy(sdk);
    cg_shutdown();

    /* ── Result ──────────────────────────────────────────────────────────── */
    if (g_failures == 0) {
        printf("\nPASSED (sdk_config_smoke)\n");
        return 0;
    } else {
        fprintf(stderr, "\nFAILED: %d assertion(s) failed\n", g_failures);
        return 1;
    }
}
