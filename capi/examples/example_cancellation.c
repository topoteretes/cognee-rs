/*
 * Example: Cancellation — create a handle/token pair and use it.
 *
 * Demonstrates:
 *   - Creating a cancellation pair
 *   - Checking cancellation status
 *   - Signaling cancellation
 */
#include "common.h"

int main(void) {
    CHECK(cg_init());

    /* Create cancellation pair */
    CgCancellationHandle* handle = NULL;
    CgCancellationToken* token = NULL;
    CHECK(cg_cancellation_pair(&handle, &token));

    /* Initially not cancelled */
    if (cg_cancellation_handle_is_cancelled(handle)) {
        fprintf(stderr, "ERROR: should not be cancelled initially\n");
        return 1;
    }
    if (cg_cancellation_token_is_cancelled(token)) {
        fprintf(stderr, "ERROR: token should not be cancelled initially\n");
        return 1;
    }
    printf("Before cancel: handle=%d, token=%d\n",
           cg_cancellation_handle_is_cancelled(handle),
           cg_cancellation_token_is_cancelled(token));

    /* Cancel */
    cg_cancellation_handle_cancel(handle);

    if (!cg_cancellation_handle_is_cancelled(handle)) {
        fprintf(stderr, "ERROR: handle should be cancelled\n");
        return 1;
    }
    if (!cg_cancellation_token_is_cancelled(token)) {
        fprintf(stderr, "ERROR: token should be cancelled\n");
        return 1;
    }
    printf("After cancel:  handle=%d, token=%d\n",
           cg_cancellation_handle_is_cancelled(handle),
           cg_cancellation_token_is_cancelled(token));

    /* Clone token and verify */
    CgCancellationToken* token2 = cg_cancellation_token_clone(token);
    if (!cg_cancellation_token_is_cancelled(token2)) {
        fprintf(stderr, "ERROR: cloned token should also be cancelled\n");
        return 1;
    }
    printf("Cloned token:  cancelled=%d\n",
           cg_cancellation_token_is_cancelled(token2));

    /* Cleanup */
    cg_cancellation_token_destroy(token2);
    cg_cancellation_token_destroy(token);
    cg_cancellation_handle_destroy(handle);
    cg_shutdown();

    printf("PASSED\n");
    return 0;
}
