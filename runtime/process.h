#ifndef SIOX_PROCESS_RUNTIME_H
#define SIOX_PROCESS_RUNTIME_H

#include <stdint.h>

/* Run one immutable Process IR test descriptor. Zero means success. */
int sx_runtime_run_test(uint32_t test);

/* Human-readable explanation for the most recent nonzero result. */
const char *sx_runtime_error(void);

/* Current simulation time in femtoseconds. */
uint64_t sx_runtime_now(void);

/* Capture a narrow value, optional packed companion words and physical target
 * offset. The driver/root family plus each physical scalar lane identifies
 * the inertial waveform. Descriptor arrays are immutable object constants. */
void sx_runtime_schedule(uint32_t site, uint64_t delay, const uint64_t *words,
                         uint32_t word_count, uint32_t waveform,
                         uint32_t offset, uint32_t width, uint8_t reverse,
                         const uint32_t *lane_offsets,
                         const uint32_t *lane_widths, uint32_t lane_count);

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
/* Attach a source location to an error raised while evaluating a declaration
 * initializer. A successful initializer leaves the runtime untouched. */
void sx_runtime_note_location(uint32_t file, uint32_t offset);
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
/* How the next number is written: form 0 decimal, 1/2 scientific (e/E),
 * 3/4 hex (x/X), 5 binary, 6 octal; precision -1 for none; flags bit 0 `+`,
 * bit 1 `#` (radix prefix), bit 2 `?` (a real keeps its point). Reset once a
 * number consumes it. */
void sx_runtime_format_notation(uint32_t form, int32_t precision, uint32_t flags);
/* Pad everything written until the matching close to `width` characters
 * with `fill` (a Unicode scalar), align 0 left, 1 center, 2 right; `zero`
 * pads a number with zeros after its sign and radix prefix instead. */
void sx_runtime_format_open(uint32_t fill, uint32_t align, uint32_t width, uint32_t zero);
void sx_runtime_format_close(void);
const char *sx_runtime_format_end(void);
uint32_t sx_runtime_warning_count(void);

/* Reproducible testbench randomization. Values cross the fixed ABI as raw
 * words; `uniform` returns the IEEE-754 bit representation of an f64. */
void sx_runtime_seed(uint64_t seed);
uint64_t sx_runtime_rand(void);
uint64_t sx_runtime_randint(uint64_t left, uint64_t right);
uint64_t sx_runtime_uniform(void);

/* Runtime-owned UTF-8 strings and raw file values. Handles are nonzero and remain valid until
 * the current test finishes; the scheduler releases every allocation between
 * tests. Fixed values use the same low-word-first, low-byte-first packing on
 * every host. Paths have already been resolved against the source directory
 * by Process lowering. */
uint64_t sx_runtime_read_utf8(const char *path);
uint8_t sx_runtime_read_utf8_fixed(const char *path, uint64_t *words,
                                   uint32_t word_count,
                                   uint32_t character_capacity);
uint8_t sx_runtime_read_binary(const char *path, uint64_t *words,
                               uint32_t word_count, uint32_t byte_capacity);
uint64_t sx_runtime_file_exists(const char *path);
uint64_t sx_runtime_string_length(uint64_t handle);
uint64_t sx_runtime_string_index(uint64_t handle, uint64_t index);
uint64_t sx_runtime_string_index_at(uint64_t handle, uint64_t index,
                                    uint32_t file, uint32_t offset);
uint64_t sx_runtime_string_equals_utf8(uint64_t handle, const char *text);

#endif
