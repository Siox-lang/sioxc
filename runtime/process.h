#ifndef SIOX_PROCESS_RUNTIME_H
#define SIOX_PROCESS_RUNTIME_H

#include <stdint.h>

/* Run one immutable Process IR test descriptor. Zero means success. */
int sx_runtime_run_test(uint32_t test);

/* Human-readable explanation for the most recent nonzero result. */
const char *sx_runtime_error(void);

/* Current simulation time in femtoseconds. */
uint64_t sx_runtime_now(void);

/* Copy one exact-width value into the time-ordered delayed-write queue. */
void sx_runtime_schedule(uint32_t site, uint64_t delay, const uint64_t *words,
                         uint32_t word_count);

#endif
