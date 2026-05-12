/*
 * Gap 07 task 05 — `cognee_init_otlp` smoke test.
 *
 * Verifies that:
 *   1. `cognee_init_otlp` returns 0 for the no-config case
 *      (no `OTEL_EXPORTER_OTLP_ENDPOINT`, no `COGNEE_TRACING_ENABLED`).
 *   2. The function is idempotent — a second call also returns 0.
 *
 * Driven from `capi/scripts/check.sh`. The smoke binary must exit
 * with status 0.
 */
#include <stdio.h>
#include "cognee.h"

int main(void) {
    if (cg_init() != CG_OK) {
        fprintf(stderr, "cg_init failed\n");
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
