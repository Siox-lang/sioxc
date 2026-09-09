#ifndef SIOX_PROCESS_RUNTIME_H
#define SIOX_PROCESS_RUNTIME_H

#include <stdint.h>

/* Run one immutable Process IR test descriptor. Zero means success. */
int sx_runtime_run_test(uint32_t test);

/* Human-readable explanation for the most recent nonzero result. */
const char *sx_runtime_error(void);

#endif
