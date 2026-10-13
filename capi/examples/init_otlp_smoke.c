/*
 * Gap 07 task 05 — `cognee_init_otlp` smoke test.
 *
 * Verifies that:
 *   1. `cognee_init_otlp` returns 0 for the no-config case
 *      (no `OTEL_EXPORTER_OTLP_ENDPOINT`, no `COGNEE_TRACING_ENABLED`).
 *   2. The function is idempotent — a second call also returns 0.
 *   3. The `_with_options` variants of `cognee_init_otlp` and
 *      `cognee_setup_logging` reject an unknown key, an invalid value and
 *      malformed JSON with return code 2 and a `cg_last_error_message()`,
 *      without latching (validation runs before installation).
 *
 * Driven from `capi/scripts/check.sh`. The smoke binary must exit
 * with status 0.
 */
#include <stdio.h>
#include <string.h>
#include "cognee.h"

static int expect_rejected(const char* what, int rc, const char* needle) {
    const char* msg = cg_last_error_message();
    if (rc != 2 || msg == NULL || strstr(msg, needle) == NULL) {
        fprintf(stderr, "%s: expected rc 2 with '%s', got rc %d, message '%s'\n",
                what, needle, rc, msg ? msg : "(null)");
        return 1;
    }
    cg_last_error_clear();
    return 0;
}

int main(void) {
    if (cg_init() != CG_OK) {
        fprintf(stderr, "cg_init failed\n");
        return 1;
    }
    if (expect_rejected("otlp unknown key",
                        cognee_init_otlp_with_options("{\"otel_nope\": \"x\"}"),
                        "unknown OpenTelemetry setting") ||
        expect_rejected("otlp malformed json", cognee_init_otlp_with_options("{"),
                        "invalid options JSON") ||
        expect_rejected("logging invalid utf-8",
                        cognee_setup_logging_with_options("{\"log_level\": \"\xff\"}"),
                        "not valid UTF-8") ||
        expect_rejected("logging invalid value",
                        cognee_setup_logging_with_options("{\"log_rotation\": \"weekly\"}"),
                        "log_rotation")) {
        return 1;
    }
    int rc1 = cognee_init_otlp();
    int rc2 = cognee_init_otlp();
    if (rc1 != 0 || rc2 != 0) {
        fprintf(stderr, "cognee_init_otlp returned non-zero: %d %d\n", rc1, rc2);
        return 1;
    }
    printf("PASSED\n");
    return 0;
}
