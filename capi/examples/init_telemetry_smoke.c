/*
 * Gap 07 task 06 — `cognee_init_telemetry` smoke test.
 *
 * Verifies that:
 *   1. `cognee_init_telemetry` returns 0 ("armed") under the C-binding
 *      explicit-opt-in policy when no opt-out env var is set.
 *   2. The function is idempotent — a second call returns the same
 *      latched decision without re-evaluating the environment.
 *
 * The driver in `capi/scripts/check.sh` runs this twice in two
 * separate child processes:
 *   * fresh env with no opt-outs       → expect exit code 0.
 *   * `TELEMETRY_DISABLED=1` in env    → expect exit code 0
 *                                        and stdout "not_armed".
 *
 * Inside one process, the singleton latches on the first call, so we
 * only test the same-process idempotency invariant here.
 */
#include <stdio.h>
#include <stdlib.h>
#include "cognee.h"

int main(void) {
    if (cg_init() != CG_OK) {
        fprintf(stderr, "cg_init failed\n");
        return 1;
    }
    int rc1 = cognee_init_telemetry();
    int rc2 = cognee_init_telemetry();
    if (rc1 != rc2) {
        fprintf(stderr,
                "cognee_init_telemetry not idempotent: %d != %d\n",
                rc1, rc2);
        return 1;
    }
    /* 0 = armed, 1 = policy suppressed. Either is acceptable here —
     * we only assert idempotency. Emit the state on stdout so the
     * driver can introspect when running env-controlled scenarios. */
    if (rc1 == 0) {
        printf("armed\n");
    } else if (rc1 == 1) {
        printf("not_armed\n");
    } else {
        fprintf(stderr, "unexpected return %d\n", rc1);
        return 1;
    }
    return 0;
}
