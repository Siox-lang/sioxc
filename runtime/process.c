#include "process.h"
#include "wave.h"

#include <math.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum {
    SX_PROCESS_COMPLETED = 0,
    SX_PROCESS_SUSPENDED = 1,
    SX_PROCESS_STOPPED = 2,
    SX_PROCESS_FINISHED = 3,
    SX_PROCESS_SETTLING = 4,
    SX_PROCESS_UNSUPPORTED = 255,
    SX_PROCESS_ABI = 15,
    SX_EVENT_WRITE = 0,
    SX_EVENT_RESUME = 1,
    SX_SUSPENSION_NONE = 0,
    SX_SUSPENSION_TIME = 1,
    SX_SUSPENSION_SETTLE = 2,
    SX_SUSPENSION_CONDITION = 3
};

typedef uint8_t (*sx_process_entry)(uint32_t resume_block);

extern const uint32_t sx_process_abi_version;
extern const uint32_t sx_test_count;
extern const uint32_t sx_test_roots[];
extern const uint32_t sx_test_process_offsets[];
extern const uint32_t sx_test_process_ids[];
extern const uint32_t sx_process_count;
extern sx_process_entry const sx_process_entries[];
extern const uint32_t sx_process_roots[];
extern const uint32_t sx_process_owners[];
extern const uint32_t sx_process_initial_blocks[];
extern const uint8_t sx_process_activations[];
extern const uint32_t sx_process_sensitivity_offsets[];
extern const uint8_t sx_process_sensitivity_kinds[];
extern const uint32_t sx_process_sensitivity_ids[];
extern const uint32_t sx_source_location_count;
extern const uint32_t sx_source_location_files[];
extern const uint32_t sx_source_location_offsets[];
extern const char *const sx_source_location_texts[];
extern const uint32_t sx_index_site_count;
extern const int64_t sx_index_site_left[];
extern const int64_t sx_index_site_right[];
extern const char *const sx_index_site_locations[];
extern const uint32_t sx_range_site_count;
extern const char *const sx_range_site_locations[];
extern const uint32_t sx_range_signal_count;
extern const char *const sx_range_signal_names[];
extern const int64_t sx_range_signal_left[];
extern const int64_t sx_range_signal_right[];
extern const char *const sx_range_signal_locations[];

extern void sx_reset_test(uint32_t root);
extern uint8_t sx_process_commit(void);
extern uint8_t sx_process_changed(uint32_t signal);
extern uint8_t sx_process_storage_changed(uint32_t storage);
extern uint32_t sx_index_error(void);
extern int64_t sx_index_value(void);
extern uint32_t sx_range_error(void);
extern int64_t sx_range_value(void);
extern uint32_t sx_range_site(void);
extern uint8_t sx_process_apply_scheduled(uint32_t site, uint32_t offset,
                                          const uint64_t *words,
                                          const uint64_t *masks,
                                          uint32_t word_count);

typedef struct sx_event {
    struct sx_event *next;
    uint64_t due;
    uint64_t sequence;
    uint32_t target;
    uint32_t process;
    uint32_t resume_block;
    uint32_t word_count;
    uint32_t lane_count;
    uint32_t waveform;
    uint32_t offset;
    uint32_t width;
    uint8_t reverse;
    const uint32_t *lane_offsets;
    const uint32_t *lane_widths;
    uint8_t kind;
    /* Primary words, optional companion words, then word_count mask words.
     * The selected offset is owned by this event, never a live selector. */
    uint64_t words[];
} sx_event;

static char *sx_error;
static char sx_error_fallback[] = "cannot allocate runtime error message";
static sx_event *sx_events;
static uint64_t sx_now;
static uint64_t sx_sequence;
static int sx_running;
static uint32_t sx_current_process;
static uint8_t sx_suspension_kind;
static uint32_t sx_settle_resume_block;
static uint32_t sx_condition_recheck_block;
static uint32_t sx_warnings;
static char *sx_format;
static size_t sx_format_len;
static size_t sx_format_cap;
static uint64_t sx_random_state = UINT64_C(0x9E3779B97F4A7C15);

typedef struct {
    uint32_t *values;
    size_t length;
    char *utf8;
    size_t byte_length;
} sx_runtime_string;

static sx_runtime_string *sx_strings;
static size_t sx_string_count;
static size_t sx_string_capacity;

static void sx_set_error(const char *format, ...);
static sx_runtime_string *sx_runtime_string_for(uint64_t handle);

const char *sx_runtime_error(void) { return sx_error; }
uint64_t sx_runtime_now(void) { return sx_now; }
uint32_t sx_runtime_warning_count(void) { return sx_warnings; }

void sx_runtime_seed(uint64_t seed) { sx_random_state = seed ? seed : 1; }

uint64_t sx_runtime_rand(void) {
    sx_random_state ^= sx_random_state >> 12;
    sx_random_state ^= sx_random_state << 25;
    sx_random_state ^= sx_random_state >> 27;
    return sx_random_state * UINT64_C(0x2545F4914F6CDD1D);
}

uint64_t sx_runtime_randint(uint64_t left, uint64_t right) {
    if (right < left) {
        uint64_t swap = left;
        left = right;
        right = swap;
    }
    uint64_t span = right - left;
    if (span == UINT64_MAX) return left + sx_runtime_rand();
    uint64_t range = span + 1;
    uint64_t threshold = (UINT64_C(0) - range) % range;
    uint64_t draw;
    do {
        draw = sx_runtime_rand();
    } while (draw < threshold);
    return left + draw % range;
}

uint64_t sx_runtime_uniform(void) {
    double value = (double)(sx_runtime_rand() >> 11) /
                   (double)(UINT64_C(1) << 53);
    uint64_t bits;
    memcpy(&bits, &value, sizeof(bits));
    return bits;
}

static void sx_clear_strings(void) {
    for (size_t index = 0; index < sx_string_count; ++index) {
        free(sx_strings[index].values);
        free(sx_strings[index].utf8);
    }
    free(sx_strings);
    sx_strings = 0;
    sx_string_count = 0;
    sx_string_capacity = 0;
}

static int sx_utf8_next(const unsigned char *data, size_t length,
                        size_t *cursor, uint32_t *value) {
    size_t at = *cursor;
    if (at >= length) return 0;
    unsigned b0 = data[at++];
    if (b0 <= 0x7f) *value = b0;
    else if (b0 >= 0xc2 && b0 <= 0xdf && at < length
             && (data[at] & 0xc0) == 0x80) {
        *value = ((b0 & 0x1f) << 6) | (data[at++] & 0x3f);
    } else if (b0 >= 0xe0 && b0 <= 0xef && at + 1 < length
               && (data[at] & 0xc0) == 0x80 && (data[at + 1] & 0xc0) == 0x80
               && !(b0 == 0xe0 && data[at] < 0xa0)
               && !(b0 == 0xed && data[at] >= 0xa0)) {
        *value = ((b0 & 0x0f) << 12) | ((data[at] & 0x3f) << 6)
                 | (data[at + 1] & 0x3f);
        at += 2;
    } else if (b0 >= 0xf0 && b0 <= 0xf4 && at + 2 < length
               && (data[at] & 0xc0) == 0x80 && (data[at + 1] & 0xc0) == 0x80
               && (data[at + 2] & 0xc0) == 0x80
               && !(b0 == 0xf0 && data[at] < 0x90)
               && !(b0 == 0xf4 && data[at] >= 0x90)) {
        *value = ((b0 & 0x07) << 18) | ((data[at] & 0x3f) << 12)
                 | ((data[at + 1] & 0x3f) << 6) | (data[at + 2] & 0x3f);
        at += 3;
    } else return -1;
    *cursor = at;
    return 1;
}

uint64_t sx_runtime_read_utf8(const char *path) {
    if (!path) {
        sx_set_error("read<string>: invalid path");
        return 0;
    }
    FILE *file = fopen(path, "rb");
    if (!file) {
        sx_set_error("read<string>(\"%s\"): cannot open file", path);
        return 0;
    }
    if (fseek(file, 0, SEEK_END) || ftell(file) < 0) {
        fclose(file);
        sx_set_error("read<string>(\"%s\"): cannot determine file length", path);
        return 0;
    }
    long end = ftell(file);
    if (end < 0 || fseek(file, 0, SEEK_SET)) {
        fclose(file);
        sx_set_error("read<string>(\"%s\"): cannot seek file", path);
        return 0;
    }
    size_t length = (size_t)end;
    if (length == SIZE_MAX || length > SIZE_MAX / sizeof(uint32_t)) {
        fclose(file);
        sx_set_error("read<string>(\"%s\"): file is too large", path);
        return 0;
    }
    char *bytes = malloc(length + 1);
    uint32_t *values = calloc(length ? length : 1, sizeof(uint32_t));
    if (!bytes || !values) {
        free(bytes);
        free(values);
        fclose(file);
        sx_set_error("read<string>(\"%s\"): out of memory", path);
        return 0;
    }
    size_t read = fread(bytes, 1, length, file);
    fclose(file);
    if (read != length) {
        free(bytes);
        free(values);
        sx_set_error("read<string>(\"%s\"): short read", path);
        return 0;
    }
    bytes[length] = 0;
    size_t cursor = 0, count = 0;
    while (cursor < length) {
        if (sx_utf8_next((const unsigned char *)bytes, length, &cursor,
                         &values[count]) < 0) {
            free(bytes);
            free(values);
            sx_set_error("read<string>(\"%s\"): file is not valid UTF-8", path);
            return 0;
        }
        ++count;
    }
    if (sx_string_count == sx_string_capacity) {
        size_t capacity = sx_string_capacity ? sx_string_capacity * 2 : 4;
        if (capacity < sx_string_capacity
            || capacity > SIZE_MAX / sizeof(sx_runtime_string)) {
            free(bytes);
            free(values);
            sx_set_error("read<string>(\"%s\"): too many runtime strings", path);
            return 0;
        }
        sx_runtime_string *grown = realloc(
            sx_strings, capacity * sizeof(sx_runtime_string));
        if (!grown) {
            free(bytes);
            free(values);
            sx_set_error("read<string>(\"%s\"): out of memory", path);
            return 0;
        }
        sx_strings = grown;
        sx_string_capacity = capacity;
    }
    sx_strings[sx_string_count] = (sx_runtime_string){
        values, count, bytes, length
    };
    return (uint64_t)++sx_string_count;
}

uint8_t sx_runtime_read_utf8_fixed(const char *path, uint64_t *words,
                                   uint32_t word_count,
                                   uint32_t character_capacity) {
    if (!words || (uint64_t)word_count * sizeof(uint64_t) <
                      (uint64_t)character_capacity * sizeof(uint32_t)) {
        sx_set_error("read<string>: invalid fixed-string destination");
        return 0;
    }
    uint64_t handle = sx_runtime_read_utf8(path);
    if (!handle) return 0;
    sx_runtime_string *string = sx_runtime_string_for(handle);
    if (!string) return 0;
    if (string->length > character_capacity) {
        sx_set_error(
            "read<string>(\"%s\"): %llu characters do not fit a %u-element string",
            path ? path : "", (unsigned long long)string->length,
            (unsigned)character_capacity);
        return 0;
    }
    memset(words, 0, (size_t)word_count * sizeof(uint64_t));
    for (size_t index = 0; index < string->length; ++index) {
        uint64_t bit_offset = (uint64_t)index * 32;
        words[bit_offset / 64] |=
            (uint64_t)string->values[index] << (bit_offset % 64);
    }
    return 1;
}

uint8_t sx_runtime_read_binary(const char *path, uint64_t *words,
                               uint32_t word_count, uint32_t byte_capacity) {
    if (!path || !words || !word_count) {
        sx_set_error("read: invalid binary destination");
        return 0;
    }
    if ((uint64_t)word_count * sizeof(uint64_t) < byte_capacity) {
        sx_set_error("read: invalid binary destination capacity");
        return 0;
    }
    FILE *file = fopen(path, "rb");
    if (!file) {
        sx_set_error("read(\"%s\"): cannot open file", path);
        return 0;
    }
    if (fseek(file, 0, SEEK_END) || ftell(file) < 0) {
        fclose(file);
        sx_set_error("read(\"%s\"): cannot determine file length", path);
        return 0;
    }
    long end = ftell(file);
    if (end < 0 || fseek(file, 0, SEEK_SET)) {
        fclose(file);
        sx_set_error("read(\"%s\"): cannot seek file", path);
        return 0;
    }
    if ((uint64_t)end > byte_capacity) {
        fclose(file);
        sx_set_error("read(\"%s\"): %llu bytes do not fit in %u bytes", path,
                     (unsigned long long)end, (unsigned)byte_capacity);
        return 0;
    }
    memset(words, 0, (size_t)word_count * sizeof(uint64_t));
    size_t length = (size_t)end;
    unsigned char bytes[4096];
    size_t offset = 0;
    while (offset < length) {
        size_t remaining = length - offset;
        size_t count = remaining < sizeof(bytes) ? remaining : sizeof(bytes);
        size_t read = fread(bytes, 1, count, file);
        if (read != count) {
            fclose(file);
            sx_set_error("read(\"%s\"): short read", path);
            return 0;
        }
        for (size_t index = 0; index < count; ++index) {
            size_t destination = offset + index;
            words[destination / 8] |=
                (uint64_t)bytes[index] << ((destination % 8) * 8);
        }
        offset += count;
    }
    fclose(file);
    return 1;
}

uint64_t sx_runtime_file_exists(const char *path) {
    if (!path) return 0;
    FILE *file = fopen(path, "rb");
    if (!file) return 0;
    fclose(file);
    return 1;
}

static sx_runtime_string *sx_runtime_string_for(uint64_t handle) {
    if (!handle || handle > sx_string_count) {
        sx_set_error("invalid runtime string handle");
        return 0;
    }
    return &sx_strings[handle - 1];
}

uint64_t sx_runtime_string_length(uint64_t handle) {
    sx_runtime_string *string = sx_runtime_string_for(handle);
    return string ? (uint64_t)string->length : 0;
}

uint64_t sx_runtime_string_index(uint64_t handle, uint64_t index) {
    sx_runtime_string *string = sx_runtime_string_for(handle);
    if (!string) return 0;
    if (index >= string->length) {
        sx_set_error("runtime string index %llu is out of bounds for length %llu",
                     (unsigned long long)index,
                     (unsigned long long)string->length);
        return 0;
    }
    return string->values[index];
}

uint64_t sx_runtime_string_equals_utf8(uint64_t handle, const char *text) {
    sx_runtime_string *string = sx_runtime_string_for(handle);
    if (!string || !text) return 0;
    size_t length = strlen(text);
    return length == string->byte_length
        && memcmp(string->utf8, text, length) == 0;
}

static void sx_clear_error(void) {
    if (sx_error != sx_error_fallback) free(sx_error);
    sx_error = 0;
}

static void sx_set_error(const char *format, ...) {
    if (sx_error) return;
    va_list arguments;
    va_start(arguments, format);
    va_list measured;
    va_copy(measured, arguments);
    int length = vsnprintf(0, 0, format, measured);
    va_end(measured);
    if (length >= 0) {
        sx_error = malloc((size_t)length + 1);
        if (sx_error)
            vsnprintf(sx_error, (size_t)length + 1, format, arguments);
    }
    va_end(arguments);
    if (!sx_error) sx_error = sx_error_fallback;
}

static void sx_append_error_location(const char *location) {
    if (!location || !*location || !sx_error || sx_error == sx_error_fallback)
        return;
    size_t message_length = strlen(sx_error);
    size_t location_length = strlen(location);
    static const char prefix[] = "\n  --> ";
    if (message_length > SIZE_MAX - sizeof(prefix) - location_length) return;
    size_t length = message_length + sizeof(prefix) - 1 + location_length + 1;
    char *grown = realloc(sx_error, length);
    if (!grown) return;
    sx_error = grown;
    memcpy(sx_error + message_length, prefix, sizeof(prefix) - 1);
    memcpy(sx_error + message_length + sizeof(prefix) - 1, location,
           location_length + 1);
}

static int sx_fail(const char *message) {
    sx_set_error("%s", message);
    return 1;
}

static int sx_format_reserve(size_t extra) {
    if (extra > SIZE_MAX - sx_format_len - 1) {
        sx_fail("runtime message is too large");
        return 0;
    }
    size_t needed = sx_format_len + extra + 1;
    if (needed <= sx_format_cap) return 1;
    size_t capacity = sx_format_cap ? sx_format_cap : 64;
    while (capacity < needed) {
        if (capacity > SIZE_MAX / 2) {
            capacity = needed;
            break;
        }
        capacity *= 2;
    }
    char *grown = realloc(sx_format, capacity);
    if (!grown) {
        sx_fail("cannot allocate runtime message");
        return 0;
    }
    sx_format = grown;
    sx_format_cap = capacity;
    return 1;
}

/* The notation the next number is written in (`{:.3e}`, `{:#x}`), set by
 * sx_runtime_format_notation and reset once a number consumes it. */
enum {
    SX_FORM_DISPLAY = 0,
    SX_FORM_LOWER_EXP = 1,
    SX_FORM_UPPER_EXP = 2,
    SX_FORM_LOWER_HEX = 3,
    SX_FORM_UPPER_HEX = 4,
    SX_FORM_BINARY = 5,
    SX_FORM_OCTAL = 6,
};
/* `+`, `#`, and `?`: a Debug real keeps its point (`2.0`). */
enum { SX_NOTATION_PLUS = 1u, SX_NOTATION_ALTERNATE = 2u, SX_NOTATION_DEBUG = 4u };
static uint32_t sx_notation_form;
static int32_t sx_notation_precision = -1;
static uint32_t sx_notation_flags;

static void sx_notation_reset(void) {
    sx_notation_form = SX_FORM_DISPLAY;
    sx_notation_precision = -1;
    sx_notation_flags = 0;
}

/* Padded regions: each `{:width}` placeholder's whole output, measured and
 * padded when it closes. Deeper nesting than this is written unpadded. */
#define SX_FORMAT_FRAMES 32u
static struct {
    size_t start;
    uint32_t fill;
    uint32_t align;
    uint32_t width;
    uint32_t zero;
} sx_format_frames[SX_FORMAT_FRAMES];
static uint32_t sx_format_depth;

void sx_runtime_format_begin(void) {
    sx_format_len = 0;
    sx_format_depth = 0;
    sx_notation_reset();
    if (sx_format_reserve(0)) sx_format[0] = 0;
}

void sx_runtime_format_text(const char *text) {
    if (!text || sx_error) return;
    size_t length = strlen(text);
    if (!sx_format_reserve(length)) return;
    memcpy(sx_format + sx_format_len, text, length + 1);
    sx_format_len += length;
}

void sx_runtime_format_notation(uint32_t form, int32_t precision, uint32_t flags) {
    sx_notation_form = form;
    sx_notation_precision = precision;
    sx_notation_flags = flags;
}

void sx_runtime_format_open(uint32_t fill, uint32_t align, uint32_t width,
                            uint32_t zero) {
    if (sx_format_depth < SX_FORMAT_FRAMES) {
        sx_format_frames[sx_format_depth].start = sx_format_len;
        sx_format_frames[sx_format_depth].fill = fill;
        sx_format_frames[sx_format_depth].align = align;
        sx_format_frames[sx_format_depth].width = width;
        sx_format_frames[sx_format_depth].zero = zero;
    }
    ++sx_format_depth;
}

/* Insert `count` copies of the UTF-8 encoding of `fill` at `at`. */
static void sx_format_insert(size_t at, uint32_t fill, size_t count) {
    char encoded[5] = {0};
    size_t size;
    if (fill <= 0x7fu) {
        encoded[0] = (char)fill;
        size = 1;
    } else if (fill <= 0x7ffu) {
        encoded[0] = (char)(0xc0u | (fill >> 6u));
        encoded[1] = (char)(0x80u | (fill & 0x3fu));
        size = 2;
    } else if (fill <= 0xffffu) {
        encoded[0] = (char)(0xe0u | (fill >> 12u));
        encoded[1] = (char)(0x80u | ((fill >> 6u) & 0x3fu));
        encoded[2] = (char)(0x80u | (fill & 0x3fu));
        size = 3;
    } else {
        encoded[0] = (char)(0xf0u | (fill >> 18u));
        encoded[1] = (char)(0x80u | ((fill >> 12u) & 0x3fu));
        encoded[2] = (char)(0x80u | ((fill >> 6u) & 0x3fu));
        encoded[3] = (char)(0x80u | (fill & 0x3fu));
        size = 4;
    }
    if (!count || count > SIZE_MAX / size || !sx_format_reserve(count * size)) return;
    memmove(sx_format + at + count * size, sx_format + at, sx_format_len - at + 1);
    for (size_t index = 0; index < count; ++index)
        memcpy(sx_format + at + index * size, encoded, size);
    sx_format_len += count * size;
}

void sx_runtime_format_close(void) {
    if (!sx_format_depth) return;
    uint32_t depth = --sx_format_depth;
    if (depth >= SX_FORMAT_FRAMES || sx_error) return;
    size_t start = sx_format_frames[depth].start;
    size_t characters = 0;
    for (size_t at = start; at < sx_format_len; ++at)
        characters += ((unsigned char)sx_format[at] & 0xc0u) != 0x80u;
    uint32_t width = sx_format_frames[depth].width;
    if (characters >= width) return;
    size_t pad = width - characters;
    if (sx_format_frames[depth].zero) {
        /* Zeros go after the sign and any radix prefix: -0042, 0x002a. */
        size_t at = start;
        if (at < sx_format_len && (sx_format[at] == '-' || sx_format[at] == '+')) ++at;
        if (at + 1 < sx_format_len && sx_format[at] == '0' &&
            (sx_format[at + 1] == 'x' || sx_format[at + 1] == 'b' ||
             sx_format[at + 1] == 'o'))
            at += 2;
        sx_format_insert(at, '0', pad);
        return;
    }
    uint32_t fill = sx_format_frames[depth].fill;
    switch (sx_format_frames[depth].align) {
    case 0: /* left */
        sx_format_insert(sx_format_len, fill, pad);
        break;
    case 1: /* center: the odd character goes right, as in Rust */
        sx_format_insert(sx_format_len, fill, pad - pad / 2u);
        sx_format_insert(start, fill, pad / 2u);
        break;
    default: /* right */
        sx_format_insert(start, fill, pad);
        break;
    }
}

/* Append `length` decimal digits (most significant first) in scientific
 * form, Rust's `1.2345e4`: `precision` digits after the point (rounded half
 * up), or as many as the value has when negative. */
static void sx_format_scientific(const char *digits, size_t length, int negative,
                                 int32_t precision, int upper) {
    char *mantissa = malloc(length + 2u);
    if (!mantissa) {
        sx_fail("cannot allocate runtime scientific value");
        return;
    }
    memcpy(mantissa, digits, length);
    size_t exponent = length - 1u;
    size_t keep = length;
    if (precision >= 0 && (size_t)precision + 1u < length) {
        keep = (size_t)precision + 1u;
        if (mantissa[keep] >= '5') {
            size_t at = keep;
            while (at > 0) {
                if (mantissa[at - 1] != '9') {
                    ++mantissa[at - 1];
                    break;
                }
                mantissa[at - 1] = '0';
                --at;
            }
            if (at == 0) {
                /* 9.99 rounded up: one more digit before the point. */
                memmove(mantissa + 1, mantissa, keep);
                mantissa[0] = '1';
                ++exponent;
            }
        }
    } else if (precision < 0) {
        while (keep > 1 && mantissa[keep - 1] == '0') --keep;
    }
    size_t after = precision >= 0 ? (size_t)precision : keep - 1u;
    char exponent_text[32];
    snprintf(exponent_text, sizeof exponent_text, "%c%zu", upper ? 'E' : 'e', exponent);
    size_t total = (size_t)negative + 1u + (after ? after + 1u : 0u) + strlen(exponent_text);
    if (!sx_format_reserve(total)) {
        free(mantissa);
        return;
    }
    if (negative) sx_format[sx_format_len++] = '-';
    sx_format[sx_format_len++] = mantissa[0];
    if (after) {
        sx_format[sx_format_len++] = '.';
        for (size_t index = 1; index <= after; ++index)
            sx_format[sx_format_len++] = index < keep ? mantissa[index] : '0';
    }
    sx_format[sx_format_len] = 0;
    sx_runtime_format_text(exponent_text);
    free(mantissa);
}

static void sx_runtime_format_integer(const uint64_t *words,
                                      uint32_t word_count, uint32_t width,
                                      uint8_t is_signed) {
    uint32_t form = sx_notation_form;
    int32_t precision = sx_notation_precision;
    uint32_t flags = sx_notation_flags;
    sx_notation_reset();
    if (sx_error) return;
    if (!words || !word_count || !width ||
        word_count != (width - 1u) / 64u + 1u) {
        sx_fail("invalid runtime integer format value");
        return;
    }
    if ((size_t)word_count > SIZE_MAX / sizeof(uint64_t)) {
        sx_fail("runtime integer is too large");
        return;
    }
    uint64_t *magnitude = malloc((size_t)word_count * sizeof(uint64_t));
    if (!magnitude) {
        sx_fail("cannot allocate runtime integer format value");
        return;
    }
    memcpy(magnitude, words, (size_t)word_count * sizeof(uint64_t));
    uint32_t top_bits = width % 64u;
    uint64_t top_mask = top_bits ? (UINT64_MAX >> (64u - top_bits)) : UINT64_MAX;
    magnitude[word_count - 1] &= top_mask;
    uint32_t base = 10;
    if (form == SX_FORM_LOWER_HEX || form == SX_FORM_UPPER_HEX) base = 16;
    if (form == SX_FORM_BINARY) base = 2;
    if (form == SX_FORM_OCTAL) base = 8;
    /* A radix form writes the bits, two's complement included, as Rust's
     * `{:x}` of a negative number does; decimal writes the signed value. */
    uint8_t negative = base == 10 && is_signed &&
        ((magnitude[(width - 1u) / 64u] >> ((width - 1u) % 64u)) & 1u);
    if (negative) {
        for (uint32_t word = 0; word < word_count; ++word)
            magnitude[word] = ~magnitude[word];
        magnitude[word_count - 1] &= top_mask;
        uint64_t carry = 1;
        for (uint32_t word = 0; word < word_count && carry; ++word) {
            uint64_t before = magnitude[word];
            magnitude[word] += carry;
            carry = magnitude[word] < before;
        }
        magnitude[word_count - 1] &= top_mask;
    }

    size_t digit_cap = (size_t)width + 3u;
    char *digits = malloc(digit_cap);
    if (!digits) {
        free(magnitude);
        sx_fail("cannot allocate runtime decimal value");
        return;
    }
    const char *alphabet = form == SX_FORM_UPPER_HEX ? "0123456789ABCDEF" : "0123456789abcdef";
    size_t length = 0;
    for (;;) {
        uint8_t nonzero = 0;
        for (uint32_t word = 0; word < word_count; ++word)
            nonzero |= magnitude[word] != 0;
        if (!nonzero) break;
        uint64_t remainder = 0;
        for (uint32_t word = word_count; word-- > 0;) {
            __uint128_t dividend = ((__uint128_t)remainder << 64u) | magnitude[word];
            magnitude[word] = (uint64_t)(dividend / base);
            remainder = (uint64_t)(dividend % base);
        }
        digits[length++] = alphabet[remainder];
    }
    if (!length) digits[length++] = '0';
    /* Most significant digit first from here on. */
    for (size_t low = 0, high = length - 1; low < high; ++low, --high) {
        char swap = digits[low];
        digits[low] = digits[high];
        digits[high] = swap;
    }
    if ((flags & SX_NOTATION_PLUS) && !negative) sx_runtime_format_text("+");
    if (form == SX_FORM_LOWER_EXP || form == SX_FORM_UPPER_EXP) {
        sx_format_scientific(digits, length, negative, precision,
                             form == SX_FORM_UPPER_EXP);
    } else {
        const char *prefix = "";
        if (flags & SX_NOTATION_ALTERNATE)
            prefix = base == 16 ? "0x" : base == 2 ? "0b" : base == 8 ? "0o" : "";
        size_t prefix_length = strlen(prefix);
        if (sx_format_reserve(length + negative + prefix_length)) {
            if (negative) sx_format[sx_format_len++] = '-';
            memcpy(sx_format + sx_format_len, prefix, prefix_length);
            sx_format_len += prefix_length;
            memcpy(sx_format + sx_format_len, digits, length);
            sx_format_len += length;
            sx_format[sx_format_len] = 0;
        }
    }
    free(digits);
    free(magnitude);
}

void sx_runtime_format_unsigned(const uint64_t *words, uint32_t word_count,
                                uint32_t width) {
    sx_runtime_format_integer(words, word_count, width, 0);
}

void sx_runtime_format_signed(const uint64_t *words, uint32_t word_count,
                              uint32_t width) {
    sx_runtime_format_integer(words, word_count, width, 1);
}

void sx_runtime_format_real(uint64_t bits) {
    uint32_t form = sx_notation_form;
    int32_t precision = sx_notation_precision;
    uint32_t flags = sx_notation_flags;
    sx_notation_reset();
    union { uint64_t bits; double value; } real;
    char rendered[512];
    real.bits = bits;
    if ((flags & SX_NOTATION_PLUS) && !signbit(real.value) && !isnan(real.value))
        sx_runtime_format_text("+");
    if (!isfinite(real.value) ||
        (form != SX_FORM_LOWER_EXP && form != SX_FORM_UPPER_EXP)) {
        if (precision >= 0 && isfinite(real.value))
            snprintf(rendered, sizeof rendered, "%.*f", (int)(precision > 300 ? 300 : precision),
                     real.value);
        else
            snprintf(rendered, sizeof rendered, "%g", real.value);
        sx_runtime_format_text(rendered);
        if ((flags & SX_NOTATION_DEBUG) && isfinite(real.value) && !strpbrk(rendered, ".e"))
            sx_runtime_format_text(".0");
        return;
    }
    /* Scientific: the requested digits, or the fewest that read back as the
     * same double, then Rust's exponent form (`e4`, `e-5`, no padding). */
    int digits = precision >= 0 ? (precision > 300 ? 300 : precision) : 0;
    if (precision < 0) {
        for (; digits < 17; ++digits) {
            snprintf(rendered, sizeof rendered, "%.*e", digits, real.value);
            if (strtod(rendered, NULL) == real.value) break;
        }
    }
    snprintf(rendered, sizeof rendered, "%.*e", digits, real.value);
    char *exponent = strchr(rendered, 'e');
    if (!exponent) {
        sx_runtime_format_text(rendered);
        return;
    }
    long power = strtol(exponent + 1, NULL, 10);
    *exponent = 0;
    char tail[32];
    snprintf(tail, sizeof tail, "%c%ld", form == SX_FORM_UPPER_EXP ? 'E' : 'e', power);
    sx_runtime_format_text(rendered);
    sx_runtime_format_text(tail);
}

void sx_runtime_format_char(uint32_t value) {
    char encoded[5] = {0};
    if (value > 0x10ffffu || (value >= 0xd800u && value <= 0xdfffu)) value = 0xfffdu;
    if (value <= 0x7fu) {
        encoded[0] = (char)value;
    } else if (value <= 0x7ffu) {
        encoded[0] = (char)(0xc0u | (value >> 6u));
        encoded[1] = (char)(0x80u | (value & 0x3fu));
    } else if (value <= 0xffffu) {
        encoded[0] = (char)(0xe0u | (value >> 12u));
        encoded[1] = (char)(0x80u | ((value >> 6u) & 0x3fu));
        encoded[2] = (char)(0x80u | (value & 0x3fu));
    } else {
        encoded[0] = (char)(0xf0u | (value >> 18u));
        encoded[1] = (char)(0x80u | ((value >> 12u) & 0x3fu));
        encoded[2] = (char)(0x80u | ((value >> 6u) & 0x3fu));
        encoded[3] = (char)(0x80u | (value & 0x3fu));
    }
    sx_runtime_format_text(encoded);
}

const char *sx_runtime_format_end(void) {
    return sx_format ? sx_format : "";
}

static int sx_fail_id(const char *message, uint32_t id) {
    sx_set_error("%s %u", message, (unsigned)id);
    return 1;
}

static int sx_fail_process_block(const char *message, uint32_t process,
                                 uint32_t block) {
    sx_set_error("%s %u block %u", message, (unsigned)process,
                 (unsigned)block);
    return 1;
}

static const char *sx_source_location(uint32_t file, uint32_t offset) {
    for (uint32_t location = 0; location < sx_source_location_count;
         ++location)
        if (sx_source_location_files[location] == file &&
            sx_source_location_offsets[location] == offset)
            return sx_source_location_texts[location];
    return 0;
}

void sx_runtime_note_location(uint32_t file, uint32_t offset) {
    if (!sx_error) return;
    sx_append_error_location(sx_source_location(file, offset));
}

uint64_t sx_runtime_string_index_at(uint64_t handle, uint64_t index,
                                    uint32_t file, uint32_t offset) {
    sx_runtime_string *string = sx_runtime_string_for(handle);
    if (!string) {
        sx_append_error_location(sx_source_location(file, offset));
        return 0;
    }
    if (index >= string->length) {
        int64_t right = string->length ? (int64_t)string->length - 1 : -1;
        sx_set_error("index %llu is outside declared range 0..%lld",
                     (unsigned long long)index, (long long)right);
        sx_append_error_location(sx_source_location(file, offset));
        return 0;
    }
    return string->values[index];
}

uint8_t sx_runtime_assert(uint8_t condition, const char *message,
                          uint32_t file, uint32_t offset) {
    if (condition) return 0;
    if (!message || !*message) message = "assertion failed";
    const char *location = sx_source_location(file, offset);
    if (location)
        sx_set_error("%s\n  --> %s", message, location);
    else
        sx_set_error("%s (source %u:%u)", message, (unsigned)file,
                     (unsigned)offset);
    return 1;
}

void sx_runtime_warn(uint8_t condition, const char *message,
                     uint32_t file, uint32_t offset) {
    if (condition) return;
    if (!message || !*message) message = "warning";
    const char *location = sx_source_location(file, offset);
    fprintf(stderr, "warning: %s\n", message);
    if (location)
        fprintf(stderr, "  --> %s\n", location);
    else
        fprintf(stderr, "  --> source %u:%u\n", (unsigned)file,
                (unsigned)offset);
    sx_warnings++;
}

void sx_runtime_print(const char *message) {
    puts(message ? message : "");
}

static void sx_clear_events(void) {
    while (sx_events) {
        sx_event *event = sx_events;
        sx_events = event->next;
        free(event);
    }
}

static void sx_insert_event(sx_event *event) {
    sx_event **position = &sx_events;
    while (*position && ((*position)->due < event->due ||
                         ((*position)->due == event->due &&
                          (*position)->sequence < event->sequence)))
        position = &(*position)->next;
    event->next = *position;
    *position = event;
}

static sx_event *sx_allocate_event(uint64_t delay, uint32_t word_count) {
    size_t word_bytes = (size_t)word_count * sizeof(uint64_t);
    if (word_count && word_bytes / sizeof(uint64_t) != word_count) {
        sx_fail("invalid Process IR event value");
        return 0;
    }
    if (word_bytes > (SIZE_MAX - sizeof(sx_event)) / 2) {
        sx_fail("invalid Process IR event value");
        return 0;
    }
    sx_event *event = malloc(sizeof(sx_event) + word_bytes * 2);
    if (!event) {
        sx_fail("cannot allocate Process IR event");
        return 0;
    }
    event->next = 0;
    event->due = UINT64_MAX - sx_now < delay ? UINT64_MAX : sx_now + delay;
    event->sequence = sx_sequence++;
    event->target = 0;
    event->process = UINT32_MAX;
    event->resume_block = 0;
    event->word_count = word_count;
    event->lane_count = 0;
    event->waveform = 0;
    event->offset = 0;
    event->width = 0;
    event->reverse = 0;
    event->lane_offsets = 0;
    event->lane_widths = 0;
    event->kind = SX_EVENT_WRITE;
    return event;
}

static uint64_t *sx_event_masks(sx_event *event) {
    return event->words + event->word_count;
}

static const uint64_t *sx_event_const_masks(const sx_event *event) {
    return event->words + event->word_count;
}

static int sx_bit(const uint64_t *words, uint32_t bit) {
    return (int)((words[bit / 64] >> (bit % 64)) & UINT64_C(1));
}

static void sx_set_bit(uint64_t *words, uint32_t bit) {
    words[bit / 64] |= UINT64_C(1) << (bit % 64);
}

static void sx_clear_lane(sx_event *event, uint32_t lane) {
    uint32_t offset = event->lane_offsets[lane];
    uint32_t width = event->lane_widths[lane];
    uint64_t *masks = sx_event_masks(event);
    for (uint32_t bit = 0; bit < width; ++bit)
        masks[(offset + bit) / 64] &=
            ~(UINT64_C(1) << ((offset + bit) % 64));
    uint32_t primary_words = event->width / 64 + (event->width % 64 != 0);
    if (event->word_count > primary_words) {
        uint32_t begin = primary_words * 64 + offset * 4;
        for (uint32_t bit = 0; bit < width * 4; ++bit)
            masks[(begin + bit) / 64] &=
                ~(UINT64_C(1) << ((begin + bit) % 64));
    }
}

static int sx_lane_active(const sx_event *event, uint32_t lane) {
    uint32_t offset = event->lane_offsets[lane];
    uint32_t width = event->lane_widths[lane];
    const uint64_t *masks = sx_event_const_masks(event);
    for (uint32_t bit = 0; bit < width; ++bit)
        if (!sx_bit(masks, offset + bit)) return 0;
    return 1;
}

static uint32_t sx_lane_physical_offset(const sx_event *event, uint32_t lane) {
    uint32_t relative = event->lane_offsets[lane];
    if (event->reverse)
        relative = event->width - relative - event->lane_widths[lane];
    return event->offset + relative;
}

static int sx_find_lane(const sx_event *event, const sx_event *incoming,
                        uint32_t incoming_lane,
                        uint32_t *lane) {
    if (event->waveform != incoming->waveform) return 0;
    uint32_t offset = sx_lane_physical_offset(incoming, incoming_lane);
    for (uint32_t index = 0; index < event->lane_count; ++index) {
        if (sx_lane_physical_offset(event, index) == offset &&
            event->lane_widths[index] == incoming->lane_widths[incoming_lane] &&
            sx_lane_active(event, index)) {
            *lane = index;
            return 1;
        }
    }
    return 0;
}

static int sx_lane_value_equals(const sx_event *event, uint32_t old_lane,
                                const sx_event *incoming, uint32_t lane) {
    uint32_t offset = incoming->lane_offsets[lane];
    uint32_t width = incoming->lane_widths[lane];
    if (event->lane_widths[old_lane] != width) return 0;
    uint32_t old_offset = event->lane_offsets[old_lane];
    for (uint32_t bit = 0; bit < width; ++bit)
        if (sx_bit(event->words, old_offset + bit) !=
            sx_bit(incoming->words, offset + bit))
            return 0;
    uint32_t old_primary = event->width / 64 + (event->width % 64 != 0);
    uint32_t new_primary = incoming->width / 64 + (incoming->width % 64 != 0);
    int old_meta = event->word_count > old_primary;
    int new_meta = incoming->word_count > new_primary;
    if (old_meta != new_meta) return 0;
    if (old_meta) {
        for (uint32_t bit = 0; bit < width * 4; ++bit)
            if (sx_bit(event->words, old_primary * 64 + old_offset * 4 + bit) !=
                sx_bit(incoming->words, new_primary * 64 + offset * 4 + bit))
                return 0;
    }
    return 1;
}

static int sx_event_has_value(const sx_event *event) {
    const uint64_t *masks = sx_event_const_masks(event);
    for (uint32_t word = 0; word < event->word_count; ++word)
        if (masks[word]) return 1;
    return 0;
}

/* Edit one driver's projected output waveform using the VHDL default
 * inertial rule. The rejection limit equals the delay, hence every still
 * pending transaction before the new due time lies in the rejection window.
 * Retain only each scalar subelement's equal-valued suffix and remove every
 * later transaction for that driver/subelement. A composite event carries a
 * mask so independently rejected lanes can still expire together. */
static void sx_reject_inertial_transactions(sx_event *incoming) {
    for (uint32_t lane = 0; lane < incoming->lane_count; ++lane) {
        sx_event *last_different = 0;
        for (sx_event *event = sx_events;
             event && event->due < incoming->due; event = event->next) {
            uint32_t old_lane;
            if (event->kind == SX_EVENT_WRITE &&
                sx_find_lane(event, incoming, lane, &old_lane) &&
                !sx_lane_value_equals(event, old_lane, incoming, lane))
                last_different = event;
        }

        int reject_prefix = last_different != 0;
        for (sx_event *event = sx_events; event; event = event->next) {
            uint32_t old_lane;
            int same_waveform = event->kind == SX_EVENT_WRITE &&
                                sx_find_lane(event, incoming, lane, &old_lane);
            if (same_waveform &&
                (event->due >= incoming->due || reject_prefix))
                sx_clear_lane(event, old_lane);
            if (event == last_different) reject_prefix = 0;
        }
    }

    sx_event **position = &sx_events;
    while (*position) {
        sx_event *event = *position;
        if (event->kind != SX_EVENT_WRITE || sx_event_has_value(event)) {
            position = &event->next;
            continue;
        }
        *position = event->next;
        free(event);
    }
}

void sx_runtime_schedule(uint32_t site, uint64_t delay, const uint64_t *words,
                         uint32_t word_count, uint32_t waveform,
                         uint32_t offset, uint32_t width, uint8_t reverse,
                         const uint32_t *lane_offsets,
                         const uint32_t *lane_widths, uint32_t lane_count) {
    if (sx_error) return;
    if (!sx_running) {
        sx_fail("delayed write scheduled outside a running test");
        return;
    }
    uint64_t primary_words = ((uint64_t)width + 63) / 64;
    uint64_t meta_words = ((uint64_t)width * 4 + 63) / 64;
    if (!words || !width || !word_count || !lane_offsets ||
        !lane_widths || !lane_count || reverse > 1 ||
        (word_count != primary_words && word_count != primary_words + meta_words) ||
        (uint64_t)word_count * 64 > UINT32_MAX ||
        (uint64_t)offset + width > UINT32_MAX) {
        sx_fail("invalid delayed Process IR value");
        return;
    }
    sx_event *event = sx_allocate_event(delay, word_count);
    if (!event) return;
    event->target = site;
    event->process = sx_current_process;
    event->lane_count = lane_count;
    event->waveform = waveform;
    event->offset = offset;
    event->width = width;
    event->reverse = reverse;
    event->lane_offsets = lane_offsets;
    event->lane_widths = lane_widths;
    for (uint32_t word = 0; word < word_count; ++word)
        event->words[word] = words[word];
    memset(sx_event_masks(event), 0, (size_t)word_count * sizeof(uint64_t));
    for (uint32_t lane = 0; lane < lane_count; ++lane) {
        uint64_t end = (uint64_t)lane_offsets[lane] + lane_widths[lane];
        if (!lane_widths[lane] || end > width) {
            free(event);
            sx_fail("invalid delayed Process IR lane");
            return;
        }
        for (uint32_t bit = 0; bit < lane_widths[lane]; ++bit) {
            uint32_t at = lane_offsets[lane] + bit;
            if (sx_bit(sx_event_masks(event), at)) {
                free(event);
                sx_fail("overlapping delayed Process IR lanes");
                return;
            }
            sx_set_bit(sx_event_masks(event), at);
            if (word_count > primary_words) {
                uint32_t meta_at = (uint32_t)primary_words * 64 + at * 4;
                for (uint32_t bit = 0; bit < 4; ++bit)
                    sx_set_bit(sx_event_masks(event), meta_at + bit);
            }
        }
    }
    sx_reject_inertial_transactions(event);
    sx_insert_event(event);
}

void sx_runtime_suspend_time(uint32_t process, uint32_t resume_block,
                             uint64_t delay) {
    if (sx_error) return;
    if (!sx_running || sx_current_process == UINT32_MAX) {
        sx_fail("process suspension registered outside a running process");
        return;
    }
    if (process != sx_current_process || process >= sx_process_count) {
        sx_fail_id("invalid suspending Process IR process", process);
        return;
    }
    if (sx_suspension_kind != SX_SUSPENSION_NONE) {
        sx_fail_id("process registered more than one suspension", process);
        return;
    }
    sx_event *event = sx_allocate_event(delay, 0);
    if (!event) return;
    event->kind = SX_EVENT_RESUME;
    event->target = process;
    event->process = process;
    event->resume_block = resume_block;
    sx_insert_event(event);
    sx_suspension_kind = SX_SUSPENSION_TIME;
}

void sx_runtime_settle(uint32_t process, uint32_t resume_block) {
    if (sx_error) return;
    if (!sx_running || sx_current_process == UINT32_MAX) {
        sx_fail("process settle registered outside a running process");
        return;
    }
    if (process != sx_current_process || process >= sx_process_count) {
        sx_fail_id("invalid settling Process IR process", process);
        return;
    }
    if (sx_suspension_kind != SX_SUSPENSION_NONE) {
        sx_fail_id("process registered more than one suspension", process);
        return;
    }
    sx_settle_resume_block = resume_block;
    sx_suspension_kind = SX_SUSPENSION_SETTLE;
}

void sx_runtime_suspend_condition(uint32_t process, uint32_t recheck_block) {
    if (sx_error) return;
    if (!sx_running || sx_current_process == UINT32_MAX) {
        sx_fail("process suspension registered outside a running process");
        return;
    }
    if (process != sx_current_process || process >= sx_process_count) {
        sx_fail_id("invalid suspending Process IR process", process);
        return;
    }
    if (sx_suspension_kind != SX_SUSPENSION_NONE) {
        sx_fail_id("process registered more than one suspension", process);
        return;
    }
    sx_condition_recheck_block = recheck_block;
    sx_suspension_kind = SX_SUSPENSION_CONDITION;
}

static int sx_design_failed(void) {
    uint32_t index = sx_index_error();
    if (index) {
        uint32_t site = index - 1;
        if (site < sx_index_site_count) {
            sx_set_error("index %lld is outside declared range %lld..%lld",
                         (long long)sx_index_value(),
                         (long long)sx_index_site_left[site],
                         (long long)sx_index_site_right[site]);
            sx_append_error_location(sx_index_site_locations[site]);
        } else {
            sx_set_error("runtime index failure at site %u: index %lld",
                         (unsigned)site, (long long)sx_index_value());
        }
        return 1;
    }
    uint32_t signal = sx_range_error();
    if (signal) {
        uint32_t site = sx_range_site();
        uint32_t id = signal - 1;
        if (id >= sx_range_signal_count) {
            sx_set_error(
                "runtime range failure for invalid signal %u at site %u: value %lld",
                (unsigned)id, (unsigned)(site ? site - 1 : 0),
                (long long)sx_range_value());
            return 1;
        }
        sx_set_error("`%s` left its range %lld..%lld (it was %lld)",
                     sx_range_signal_names[id],
                     (long long)sx_range_signal_left[id],
                     (long long)sx_range_signal_right[id],
                     (long long)sx_range_value());
        const char *location = sx_range_signal_locations[id];
        if (site && site <= sx_range_site_count &&
            sx_range_site_locations[site - 1][0])
            location = sx_range_site_locations[site - 1];
        sx_append_error_location(location);
        return 1;
    }
    return 0;
}

static int sx_has_changed_sensitivity(uint32_t process) {
    uint32_t begin = sx_process_sensitivity_offsets[process];
    uint32_t end = sx_process_sensitivity_offsets[process + 1];
    for (uint32_t item = begin; item < end; ++item) {
        uint32_t id = sx_process_sensitivity_ids[item];
        if (sx_process_sensitivity_kinds[item] == 0) {
            if (sx_process_changed(id)) return 1;
        } else if (sx_process_sensitivity_kinds[item] == 1) {
            if (sx_process_storage_changed(id)) return 1;
        } else {
            sx_fail_id("invalid Process IR sensitivity kind", item);
            return -1;
        }
    }
    return 0;
}

static int sx_release_settling(uint32_t begin, uint32_t end,
                               const uint8_t *stopped, uint8_t *settling,
                               uint8_t *ready) {
    int released = 0;
    for (uint32_t item = begin; item < end; ++item) {
        uint32_t process = sx_test_process_ids[item];
        if (settling[process] && !stopped[process]) {
            settling[process] = 0;
            ready[process] = 1;
            released = 1;
        }
    }
    return released;
}

/* Apply every transaction due in the current simulation time. Delays of zero
 * are therefore staged into the update phase that follows the process batch
 * which scheduled them, before sensitivity-driven processes resume. */
static int sx_apply_due_events(uint8_t *ready, uint8_t *suspended,
                               uint8_t *timed_ready,
                               uint32_t *resume_blocks) {
    while (sx_events && sx_events->due == sx_now) {
        sx_event *event = sx_events;
        sx_events = event->next;
        if (event->kind == SX_EVENT_RESUME) {
            uint32_t process = event->target;
            if (process >= sx_process_count || !suspended[process]) {
                free(event);
                return sx_fail_id("invalid Process IR resume event", process);
            }
            resume_blocks[process] = event->resume_block;
            suspended[process] = 0;
            timed_ready[process] = 1;
            ready[process] = 1;
            free(event);
            continue;
        }
        if (event->kind != SX_EVENT_WRITE) {
            free(event);
            return sx_fail("invalid Process IR event kind");
        }
        uint8_t applied = sx_process_apply_scheduled(
            event->target, event->offset, event->words, sx_event_masks(event),
            event->word_count);
        free(event);
        if (applied == SX_PROCESS_UNSUPPORTED)
            return sx_fail("invalid or unsupported delayed Process IR site");
        if (applied != SX_PROCESS_COMPLETED)
            return sx_fail("failed to apply delayed Process IR write");
        if (sx_design_failed()) return 1;
    }
    return 0;
}

int sx_runtime_run_test(uint32_t test) {
    uint8_t *ready = 0, *next = 0, *stopped = 0, *suspended = 0,
            *settling = 0, *timed_ready = 0, *selected = 0;
    uint32_t *resume_blocks = 0;
    uint32_t begin, end;
    uint32_t initialization_cursor = 0, initializer = UINT32_MAX;
    int initializing = 1;
    int foreground_started = 0;
    int result = 0;
    sx_clear_events();
    sx_clear_strings();
    sx_clear_error();
    sx_now = 0;
    sx_sequence = 0;
    sx_current_process = UINT32_MAX;
    sx_suspension_kind = SX_SUSPENSION_NONE;
    sx_settle_resume_block = 0;
    sx_condition_recheck_block = 0;

    if (sx_process_abi_version != SX_PROCESS_ABI)
        return sx_fail("unsupported Process IR runtime ABI");
    if (test >= sx_test_count) return sx_fail_id("invalid test descriptor", test);
    sx_running = 1;

    size_t bytes = sx_process_count ? (size_t)sx_process_count : 1;
    ready = calloc(bytes, 1);
    next = calloc(bytes, 1);
    stopped = calloc(bytes, 1);
    suspended = calloc(bytes, 1);
    settling = calloc(bytes, 1);
    timed_ready = calloc(bytes, 1);
    selected = calloc(bytes, 1);
    resume_blocks = calloc(bytes, sizeof(uint32_t));
    if (!ready || !next || !stopped || !suspended || !settling ||
        !timed_ready || !selected || !resume_blocks) {
        result = sx_fail("cannot allocate Process IR ready queue");
        goto done;
    }

    sx_reset_test(sx_test_roots[test]);
    if (sx_error) {
        result = 1;
        goto done;
    }
    sx_wave_begin_test();
    /* Reset initializes process storage and stages its input bindings. Publish
       those values and settle the initialized design before test stimulus
       starts, so no reactive process observes an uncommitted binding. */
    (void)sx_process_commit();
    if (sx_design_failed()) {
        result = 1;
        goto done;
    }
    begin = sx_test_process_offsets[test];
    end = sx_test_process_offsets[test + 1];
    for (uint32_t item = begin; item < end; ++item) {
        uint32_t process = sx_test_process_ids[item];
        if (process >= sx_process_count) {
            result = sx_fail_id("test references invalid process", process);
            goto done;
        }
        selected[process] = 1;
        resume_blocks[process] = sx_process_initial_blocks[process];
    }
    for (;;) {
        int ran = 0;
        int finish = 0;
        int reactive_ready = 0;
        for (uint32_t process = 0; process < sx_process_count; ++process) {
            if (process < sx_process_count && ready[process] &&
                !stopped[process] && !suspended[process] &&
                !settling[process] && sx_process_activations[process] == 1) {
                reactive_ready = 1;
                break;
            }
        }
        for (uint32_t process = 0; process < sx_process_count; ++process) {
            if (!ready[process] || stopped[process] || suspended[process] ||
                settling[process])
                continue;
            /* A timed foreground resume and a scheduled signal update may
             * expire at the same femtosecond. Reactive hardware must consume
             * and settle that update before foreground code observes it;
             * otherwise process-id order changes the result. Put foreground
             * into the existing settling state: merely carrying its ready bit
             * can starve it forever behind a free-running reactive clock. */
            if (reactive_ready && timed_ready[process] &&
                sx_process_activations[process] == 0) {
                settling[process] = 1;
                timed_ready[process] = 0;
                continue;
            }
            timed_ready[process] = 0;
            ran = 1;
            sx_current_process = process;
            sx_suspension_kind = SX_SUSPENSION_NONE;
            sx_process_entry entry = sx_process_entries[process];
            if (!entry) {
                result = sx_fail_id("missing Process IR entry", process);
                goto done;
            }
            uint8_t status = entry(resume_blocks[process]);
            sx_current_process = UINT32_MAX;
            if (sx_error) {
                result = 1;
                goto done;
            }
            if (sx_design_failed()) {
                result = 1;
                goto done;
            }
            if (status != SX_PROCESS_SUSPENDED && status != SX_PROCESS_SETTLING &&
                sx_suspension_kind != SX_SUSPENSION_NONE) {
                result = sx_fail_id(
                    "process registered a suspension but returned status", process);
                goto done;
            }
            if (status == SX_PROCESS_COMPLETED) {
                if (sx_process_activations[process] != 1) stopped[process] = 1;
            } else if (status == SX_PROCESS_STOPPED) {
                stopped[process] = 1;
            } else if (status == SX_PROCESS_FINISHED) {
                stopped[process] = 1;
                printf("finish at %llu fs\n", (unsigned long long)sx_now);
                finish = 1;
            } else if (status == SX_PROCESS_SUSPENDED) {
                if (sx_suspension_kind != SX_SUSPENSION_TIME &&
                    sx_suspension_kind != SX_SUSPENSION_CONDITION) {
                    result = sx_fail_id(
                        "process suspended without a runtime resume record",
                        process);
                    goto done;
                }
                suspended[process] = sx_suspension_kind;
                if (sx_suspension_kind == SX_SUSPENSION_CONDITION)
                    resume_blocks[process] = sx_condition_recheck_block;
            } else if (status == SX_PROCESS_SETTLING) {
                if (sx_suspension_kind != SX_SUSPENSION_SETTLE) {
                    result = sx_fail_id(
                        "process settling without a settle runtime resume record",
                        process);
                    goto done;
                }
                settling[process] = 1;
                resume_blocks[process] = sx_settle_resume_block;
            } else if (status == SX_PROCESS_UNSUPPORTED) {
                result = sx_fail_process_block(
                    "direct Process IR lowering is incomplete for process",
                    process, resume_blocks[process]);
                goto done;
            } else {
                result = sx_fail_id("invalid Process IR entry status from process", process);
                goto done;
            }
        }
        if (ran) {
            if (sx_apply_due_events(next, suspended, timed_ready, resume_blocks)) {
                result = 1;
                goto done;
            }
            uint8_t changed = sx_process_commit();
            if (sx_design_failed()) {
                result = 1;
                goto done;
            }
            if (finish) {
                sx_wave_sample(sx_now);
                break;
            }
            /* A no-change commit is the fixed point even if a reactive entry
             * was conservatively queued once more from the previous delta.
             * Release foreground observers here so a self-sensitive clock
             * cannot keep them in the settling set forever. */
            if (!changed) {
                if (!initializing) {
                    sx_wave_sample(sx_now);
                    (void)sx_release_settling(begin, end, stopped, settling, next);
                }
            }
            /* Initialization uses the same continuation queue, but hardware
             * cannot observe a partially initialized root. A settle edge is
             * satisfied by publication alone at this earlier boundary. */
            if (initializing && initializer != UINT32_MAX &&
                !stopped[initializer]) {
                if (settling[initializer]) {
                    settling[initializer] = 0;
                    next[initializer] = 1;
                }
                if (changed && suspended[initializer] == SX_SUSPENSION_CONDITION) {
                    suspended[initializer] = SX_SUSPENSION_NONE;
                    next[initializer] = 1;
                }
            }
            if (changed && !initializing) {
                for (uint32_t process = 0; process < sx_process_count; ++process) {
                    int nested_bootstrap = !foreground_started &&
                                           sx_process_owners[process] !=
                                               sx_process_roots[process];
                    if (!selected[process] && !nested_bootstrap) continue;
                    if (stopped[process] || settling[process])
                        continue;
                    if (suspended[process] == SX_SUSPENSION_CONDITION) {
                        suspended[process] = SX_SUSPENSION_NONE;
                        next[process] = 1;
                        continue;
                    }
                    if (suspended[process] != SX_SUSPENSION_NONE ||
                        sx_process_activations[process] != 1)
                        continue;
                    int changed_sensitivity = sx_has_changed_sensitivity(process);
                    if (changed_sensitivity < 0) {
                        result = 1;
                        goto done;
                    }
                    if (changed_sensitivity) next[process] = 1;
                }
            }
            uint8_t *swap = ready;
            ready = next;
            next = swap;
            for (uint32_t process = 0; process < sx_process_count; ++process)
                next[process] = 0;
            continue;
        }

        /* A waveform observes settled change points, never an intermediate
           delta. Changed-value suppression in the fixed writer makes repeated
           quiescent visits free of duplicate records. */
        if (!initializing) sx_wave_sample(sx_now);

        if (initializing && (initializer == UINT32_MAX || stopped[initializer])) {
            // Reset has always initialized all roots in the combined object.
            // Serialize their CFGs in object/declaration order, even under a
            // test filter; ordinary stimulus and clocks remain unselected.
            initializer = UINT32_MAX;
            while (initialization_cursor < sx_process_count) {
                uint32_t process = initialization_cursor++;
                if (sx_process_activations[process] != 2) continue;
                initializer = process;
                resume_blocks[process] = sx_process_initial_blocks[process];
                ready[process] = 1;
                break;
            }
            if (initializer != UINT32_MAX) continue;
            initializing = 0;
            // Bootstrap nested hardware for every reset root as before, but
            // only now, after all source initializer CFGs have completed.
            for (uint32_t process = 0; process < sx_process_count; ++process) {
                int nested_hardware = sx_process_owners[process] != sx_process_roots[process];
                if (sx_process_activations[process] == 1 &&
                    (selected[process] || nested_hardware)) {
                    resume_blocks[process] = sx_process_initial_blocks[process];
                    ready[process] = 1;
                }
            }
            continue;
        }

        /* Reactive hardware starts once at time zero and reaches a fixed point
           before foreground/test processes observe it. Events registered by
           clocks during that bootstrap stay queued; test stimulus begins at
           the same simulation time before the wheel may advance. */
        if (!foreground_started && !initializing) {
            foreground_started = 1;
            for (uint32_t item = begin; item < end; ++item) {
                uint32_t process = sx_test_process_ids[item];
                if (sx_process_activations[process] == 0 && !stopped[process] &&
                    !suspended[process])
                    ready[process] = 1;
            }
            continue;
        }

        /* A foreground drive resumes only after every reactive process it
         * awakened has reached quiescence at this simulation time. Keeping
         * this separate from a zero-delay event prevents the observer and DUT
         * from running in the same pre-commit batch. */
        int released_settling = !initializing &&
            sx_release_settling(begin, end, stopped, settling, ready);
        if (released_settling) continue;

        /* A completed foreground stimulus defines the end of its test after
         * its own queued transactions have drained. Free-running reactive
         * clocks may still have events forever, but they are implementation
         * support rather than a reason to keep the finished test alive. */
        int foreground_live = initializing;
        for (uint32_t item = begin; item < end; ++item) {
            uint32_t process = sx_test_process_ids[item];
            if (sx_process_activations[process] == 0 && !stopped[process]) {
                foreground_live = 1;
                break;
            }
        }
        int foreground_transaction = 0;
        for (sx_event *event = sx_events; event; event = event->next) {
            if (event->kind == SX_EVENT_WRITE && event->process < sx_process_count &&
                sx_process_activations[event->process] != 1) {
                foreground_transaction = 1;
                break;
            }
        }
        if (!foreground_live && !foreground_transaction) break;

        if (!sx_events) {
            if (initializing) {
                result = sx_fail_process_block(
                    "initializer has no future event for process", initializer,
                    resume_blocks[initializer]);
                goto done;
            }
            for (uint32_t item = begin; item < end; ++item) {
                uint32_t process = sx_test_process_ids[item];
                if (suspended[process] == SX_SUSPENSION_CONDITION) {
                    result = sx_fail_process_block(
                        "await condition has no future event for process",
                        process, resume_blocks[process]);
                    goto done;
                }
            }
            break;
        }
        sx_now = sx_events->due;
        if (sx_apply_due_events(ready, suspended, timed_ready, resume_blocks)) {
            result = 1;
            goto done;
        }

        uint8_t changed = sx_process_commit();
        if (sx_design_failed()) {
            result = 1;
            goto done;
        }
        if (changed && initializing && initializer != UINT32_MAX &&
            suspended[initializer] == SX_SUSPENSION_CONDITION) {
            suspended[initializer] = SX_SUSPENSION_NONE;
            ready[initializer] = 1;
        }
        if (changed && !initializing) {
            for (uint32_t item = begin; item < end; ++item) {
                uint32_t process = sx_test_process_ids[item];
                if (stopped[process] || settling[process])
                    continue;
                if (suspended[process] == SX_SUSPENSION_CONDITION) {
                    suspended[process] = SX_SUSPENSION_NONE;
                    ready[process] = 1;
                    continue;
                }
                if (suspended[process] != SX_SUSPENSION_NONE ||
                    sx_process_activations[process] != 1)
                    continue;
                int changed_sensitivity = sx_has_changed_sensitivity(process);
                if (changed_sensitivity < 0) {
                    result = 1;
                    goto done;
                }
                if (changed_sensitivity) ready[process] = 1;
            }
        }
    }

done:
    sx_running = 0;
    sx_current_process = UINT32_MAX;
    sx_clear_events();
    sx_clear_strings();
    free(resume_blocks);
    free(selected);
    free(settling);
    free(timed_ready);
    free(suspended);
    free(stopped);
    free(next);
    free(ready);
    return result;
}
