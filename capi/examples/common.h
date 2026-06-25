#ifndef COGNEE_EXAMPLES_COMMON_H
#define COGNEE_EXAMPLES_COMMON_H

#include "cognee.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define CHECK(code)                                                        \
    do {                                                                   \
        CgErrorCode _rc = (code);                                          \
        if (_rc != CG_OK) {                                                \
            const char* _msg = cg_last_error_message();                    \
            fprintf(stderr, "ERROR %d at %s:%d: %s\n",                    \
                    _rc, __FILE__, __LINE__, _msg ? _msg : "(no message)");\
            exit(1);                                                       \
        }                                                                  \
    } while (0)

#endif /* COGNEE_EXAMPLES_COMMON_H */
