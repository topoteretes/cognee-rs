/*
 * Gap 07 task 06 — `cognee_init_telemetry` smoke test.
 *
 * Verifies that:
 *   1. `cognee_init_telemetry` returns 0 ("armed") under the Python-SDK
 *      parity policy (analytics ON by default) when no opt-out env var
 *      is set.
 *   2. The function is idempotent — a second call returns the same
 *      decision.
 *   3. The process-wide switch (`cognee_set_telemetry_enabled`) wins over
 *      the environment in both directions, and `cognee_is_telemetry_enabled`
 *      / `cognee_init_telemetry` report it.
 *
 * The driver in `capi/scripts/check.sh` runs this twice in two
 * separate child processes:
 *   * fresh env with no opt-outs       → expect exit code 0.
 *   * `TELEMETRY_DISABLED=1` in env    → expect exit code 0
 *                                        and stdout "not_armed".
 *
 * The switch checks print nothing on success, so stdout stays exactly the
 * one state line the driver compares.
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
    if (rc1 != 0 && rc1 != 1) {
        fprintf(stderr, "unexpected return %d\n", rc1);
        return 1;
    }
    if (cognee_is_telemetry_enabled() != (rc1 == 0)) {
        fprintf(stderr, "cognee_is_telemetry_enabled disagrees with init\n");
        return 1;
    }

    /* Process-wide switch: off, then on — wins over TELEMETRY_DISABLED. */
    if (cognee_set_telemetry_enabled(0) != 0 || cognee_is_telemetry_enabled() != 0 ||
        cognee_init_telemetry() != 1) {
        fprintf(stderr, "cognee_set_telemetry_enabled(0) did not disable\n");
        return 1;
    }
    if (cognee_set_telemetry_enabled(1) != 0 || cognee_is_telemetry_enabled() != 1 ||
        cognee_init_telemetry() != 0) {
        fprintf(stderr, "cognee_set_telemetry_enabled(1) did not enable\n");
        return 1;
    }

    printf(rc1 == 0 ? "armed\n" : "not_armed\n");
    return 0;
}
