#ifndef SIOX_WAVE_RUNTIME_H
#define SIOX_WAVE_RUNTIME_H

#include <stdint.h>

/* Open one VCD stream described entirely by immutable tables in the design
 * object. Zero reports an already-rendered host error. */
int sx_wave_open_vcd(const char *path);

/* Start a test on the stream's monotonic multi-test timeline. */
void sx_wave_begin_test(void);

/* Sample the settled design state at one simulation timestamp. */
void sx_wave_sample(uint64_t now);

/* Flush, close, and release all waveform state. */
void sx_wave_close(void);

#endif
