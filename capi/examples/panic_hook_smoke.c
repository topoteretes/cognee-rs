/*
 * Gap 07 task 04 — panic hook smoke test.
 *
 * Verifies that the panic hook installed by `cg_init` writes a
 * `[cognee-capi panic]` line to stderr when Rust panics cross the
 * FFI boundary. This program intentionally crashes itself: the
 * driver in `capi/scripts/check.sh` captures stderr and greps for
 * the marker, then accepts a non-zero exit code.
 *
 * The trigger symbol `cg_test_force_panic` is exposed only when the
 * `testing-panic` cargo feature is enabled — release builds must NOT
 * enable it.
 */
#include <stdio.h>
#include "cognee.h"

/* Symbol exposed by `capi/cognee-capi/src/lib.rs` under the
 * `testing-panic` feature. Declared here so we do not need to
 * regenerate the public header for a test-only artefact. */
extern void cg_test_force_panic(void);

int main(void) {
    if (cg_init() != CG_OK) {
        fprintf(stderr, "cg_init failed\n");
        return 2;
    }
    /* The panic hook writes the marker to stderr before the process
     * aborts. We never return from this call — the panic unwinds
     * past `main`. */
    cg_test_force_panic();
    /* Unreachable. If we get here, the panic was swallowed and the
     * test should fail explicitly. */
    fprintf(stderr, "ERROR: panic did not propagate\n");
    return 1;
}
