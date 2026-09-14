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

/* Suspend the current process and enqueue its resume block after `delay`. */
void sx_runtime_suspend_time(uint32_t process, uint32_t resume_block,
                             uint64_t delay);

/* Suspend until a design/storage change lets the emitted trigger block
 * re-evaluate its normalized condition. */
void sx_runtime_suspend_condition(uint32_t process, uint32_t recheck_block);

/* Resume the current foreground process after reactive delta cycles settle. */
void sx_runtime_settle(uint32_t process, uint32_t resume_block);

/* Report source-level runtime operations emitted from Process IR. Assertions
 * return nonzero when they fail so the generated process entry can stop before
 * executing any later statement in the same basic block. */
uint8_t sx_runtime_assert(uint8_t condition, const char *message,
                          uint32_t file, uint32_t offset);
void sx_runtime_warn(uint8_t condition, const char *message,
                     uint32_t file, uint32_t offset);
void sx_runtime_print(const char *message);
/* Build a typed runtime message without design-specific generated source.
 * Numeric words are little-endian and `width` is the exact logical width. */
void sx_runtime_format_begin(void);
void sx_runtime_format_text(const char *text);
void sx_runtime_format_unsigned(const uint64_t *words, uint32_t word_count,
                                uint32_t width);
void sx_runtime_format_signed(const uint64_t *words, uint32_t word_count,
                              uint32_t width);
void sx_runtime_format_real(uint64_t bits);
void sx_runtime_format_char(uint32_t value);
const char *sx_runtime_format_end(void);
uint32_t sx_runtime_warning_count(void);

#endif
