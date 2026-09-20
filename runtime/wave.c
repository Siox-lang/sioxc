#include "wave.h"
#include "fstapi.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum {
    SX_WAVE_BITS = 0,
    SX_WAVE_REAL = 1,
    SX_WAVE_ENUM = 2,
    SX_WAVE_LOGIC = 3,
    SX_WAVE_LOGIC_VECTOR = 4
};

extern const uint32_t sx_wave_signal_count;
extern const uint32_t sx_wave_signal_ids[];
extern const uint32_t sx_wave_signal_widths[];
extern const uint8_t sx_wave_signal_kinds[];
extern const uint32_t sx_wave_signal_companions[];
extern const uint32_t sx_wave_scope_count;
extern const uint32_t sx_wave_scope_parents[];
extern const char *const sx_wave_scope_names[];
extern const uint32_t sx_wave_signal_scopes[];
extern const char *const sx_wave_signal_names[];
extern const uint32_t sx_wave_symbol_offsets[];
extern const uint64_t sx_wave_symbol_values[];
extern const char *const sx_wave_symbol_texts[];
extern const char sx_wave_vcd_header[];
extern uint64_t sx_read_word(uint32_t signal, uint32_t word);

static FILE *sx_vcd;
static fstWriterContext *sx_fst;
static fstHandle *sx_fst_handles;
static char *sx_fst_value;
static size_t *sx_value_offsets;
static size_t *sx_meta_offsets;
static uint64_t *sx_last_values;
static uint64_t *sx_last_meta;
static uint8_t *sx_seen;
static uint64_t sx_wave_base;
static uint64_t sx_wave_last_time;
static int sx_wave_started;

static size_t sx_wave_words(uint64_t width) {
    return (size_t)(width / 64u + (width % 64u != 0));
}

static int sx_wave_add_words(size_t *total, size_t words) {
    if (*total > SIZE_MAX - words) return 0;
    *total += words;
    return 1;
}

static void sx_wave_close_fst(void) {
    if (sx_fst) fstWriterClose(sx_fst);
    sx_fst = 0;
    free(sx_fst_value);
    free(sx_fst_handles);
    sx_fst_value = 0;
    sx_fst_handles = 0;
}

void sx_wave_close(void) {
    if (sx_vcd) fclose(sx_vcd);
    sx_vcd = 0;
    sx_wave_close_fst();
    free(sx_seen);
    free(sx_last_meta);
    free(sx_last_values);
    free(sx_meta_offsets);
    free(sx_value_offsets);
    sx_seen = 0;
    sx_last_meta = 0;
    sx_last_values = 0;
    sx_meta_offsets = 0;
    sx_value_offsets = 0;
    sx_wave_base = 0;
    sx_wave_last_time = 0;
    sx_wave_started = 0;
}

static int sx_wave_prepare(void) {
    if (sx_value_offsets) return 1;
    size_t count = sx_wave_signal_count;
    if (count == SIZE_MAX) {
        fprintf(stderr, "waveform state is too large\n");
        return 0;
    }
    sx_value_offsets = calloc(count + 1, sizeof(size_t));
    sx_meta_offsets = calloc(count + 1, sizeof(size_t));
    sx_seen = calloc(count ? count : 1, 1);
    if (!sx_value_offsets || !sx_meta_offsets || !sx_seen) {
        fprintf(stderr, "cannot allocate waveform state\n");
        sx_wave_close();
        return 0;
    }

    size_t value_words = 0;
    size_t meta_words = 0;
    for (uint32_t signal = 0; signal < sx_wave_signal_count; ++signal) {
        sx_value_offsets[signal] = value_words;
        sx_meta_offsets[signal] = meta_words;
        if (!sx_wave_add_words(
                &value_words, sx_wave_words(sx_wave_signal_widths[signal]))) {
            fprintf(stderr, "waveform state is too large\n");
            sx_wave_close();
            return 0;
        }
        if (sx_wave_signal_companions[signal] != UINT32_MAX &&
            !sx_wave_add_words(
                &meta_words,
                sx_wave_words((uint64_t)sx_wave_signal_widths[signal] * 4u))) {
            fprintf(stderr, "waveform state is too large\n");
            sx_wave_close();
            return 0;
        }
    }
    sx_value_offsets[count] = value_words;
    sx_meta_offsets[count] = meta_words;
    sx_last_values = calloc(value_words ? value_words : 1, sizeof(uint64_t));
    sx_last_meta = calloc(meta_words ? meta_words : 1, sizeof(uint64_t));
    if (!sx_last_values || !sx_last_meta) {
        fprintf(stderr, "cannot allocate waveform values\n");
        sx_wave_close();
        return 0;
    }
    return 1;
}

int sx_wave_open_vcd(const char *path) {
    if (sx_vcd) {
        fprintf(stderr, "more than one VCD output was requested\n");
        return 0;
    }
    if (!sx_wave_prepare()) return 0;
    sx_vcd = fopen(path, "wb");
    if (!sx_vcd) {
        fprintf(stderr, "cannot open VCD output %s\n", path);
        return 0;
    }
    if (fputs(sx_wave_vcd_header, sx_vcd) < 0) {
        fprintf(stderr, "cannot write VCD output %s\n", path);
        fclose(sx_vcd);
        sx_vcd = 0;
        return 0;
    }
    return 1;
}

static int sx_wave_register_scope(uint32_t scope) {
    fstWriterSetScope(sx_fst, FST_ST_VCD_MODULE, sx_wave_scope_names[scope], 0);
    for (uint32_t descriptor = 0; descriptor < sx_wave_signal_count;
         ++descriptor) {
        if (sx_wave_signal_scopes[descriptor] != scope) continue;
        uint8_t kind = sx_wave_signal_kinds[descriptor];
        enum fstVarType fst_kind = kind == SX_WAVE_REAL
                                       ? FST_VT_VCD_REAL
                                       : (kind == SX_WAVE_ENUM
                                              ? FST_VT_GEN_STRING
                                              : FST_VT_VCD_WIRE);
        uint32_t width = kind == SX_WAVE_REAL || kind == SX_WAVE_LOGIC
                             ? 1
                             : (kind == SX_WAVE_ENUM
                                    ? 0
                                    : sx_wave_signal_widths[descriptor]);
        sx_fst_handles[descriptor] = fstWriterCreateVar(
            sx_fst, fst_kind, FST_VD_IMPLICIT, width,
            sx_wave_signal_names[descriptor], 0);
        if (!sx_fst_handles[descriptor]) return 0;
    }
    for (uint32_t child = 0; child < sx_wave_scope_count; ++child)
        if (sx_wave_scope_parents[child] == scope &&
            !sx_wave_register_scope(child))
            return 0;
    fstWriterSetUpscope(sx_fst);
    return 1;
}

int sx_wave_open_fst(const char *path) {
    if (sx_fst) {
        fprintf(stderr, "more than one FST output was requested\n");
        return 0;
    }
    if (!sx_wave_prepare()) return 0;

    size_t count = sx_wave_signal_count;
    sx_fst_handles = calloc(count ? count : 1, sizeof(fstHandle));
    uint64_t max_width = 32;
    for (uint32_t descriptor = 0; descriptor < sx_wave_signal_count;
         ++descriptor)
        if (sx_wave_signal_widths[descriptor] > max_width)
            max_width = sx_wave_signal_widths[descriptor];
    if (max_width >= SIZE_MAX) {
        fprintf(stderr, "FST waveform value is too wide\n");
        sx_wave_close_fst();
        return 0;
    }
    sx_fst_value = malloc((size_t)max_width + 1);
    if (!sx_fst_handles || !sx_fst_value) {
        fprintf(stderr, "cannot allocate FST waveform state\n");
        sx_wave_close_fst();
        return 0;
    }

    sx_fst = fstWriterCreate(path, 1);
    if (!sx_fst) {
        fprintf(stderr, "cannot open FST output %s\n", path);
        sx_wave_close_fst();
        return 0;
    }
    fstWriterSetPackType(sx_fst, FST_WR_PT_LZ4);
    fstWriterSetTimescale(sx_fst, -15);
    fstWriterSetVersion(sx_fst, "siox native test executable");
    for (uint32_t scope = 0; scope < sx_wave_scope_count; ++scope)
        if (sx_wave_scope_parents[scope] == UINT32_MAX &&
            !sx_wave_register_scope(scope)) {
            fprintf(stderr, "cannot register FST waveform hierarchy\n");
            sx_wave_close_fst();
            return 0;
        }
    return 1;
}

void sx_wave_begin_test(void) {
    if (!sx_vcd && !sx_fst) return;
    sx_wave_base = sx_wave_started && sx_wave_last_time != UINT64_MAX
                       ? sx_wave_last_time + 1
                       : (sx_wave_started ? UINT64_MAX : 0);
    memset(sx_seen, 0, sx_wave_signal_count);
}

static int sx_wave_changed(uint32_t descriptor, uint32_t signal,
                           const size_t *offsets, const uint64_t *last) {
    size_t begin = offsets[descriptor];
    size_t end = offsets[descriptor + 1];
    for (size_t word = 0; word < end - begin; ++word)
        if (sx_read_word(signal, (uint32_t)word) != last[begin + word]) return 1;
    return 0;
}

static void sx_wave_remember(uint32_t descriptor, uint32_t signal,
                             const size_t *offsets, uint64_t *last) {
    size_t begin = offsets[descriptor];
    size_t end = offsets[descriptor + 1];
    for (size_t word = 0; word < end - begin; ++word)
        last[begin + word] = sx_read_word(signal, (uint32_t)word);
}

static unsigned sx_wave_bit(uint32_t signal, uint64_t bit) {
    return (unsigned)((sx_read_word(signal, (uint32_t)(bit / 64u)) >>
                       (unsigned)(bit % 64u)) &
                      1u);
}

static uint64_t sx_wave_discriminant(uint32_t signal, uint32_t element) {
    uint64_t bit = (uint64_t)element * 4u;
    return (sx_read_word(signal, (uint32_t)(bit / 64u)) >>
            (unsigned)(bit % 64u)) &
           15u;
}

static const char *sx_wave_symbol(uint32_t descriptor, uint64_t value) {
    uint32_t begin = sx_wave_symbol_offsets[descriptor];
    uint32_t end = sx_wave_symbol_offsets[descriptor + 1];
    for (uint32_t symbol = begin; symbol < end; ++symbol)
        if (sx_wave_symbol_values[symbol] == value)
            return sx_wave_symbol_texts[symbol];
    return 0;
}

static void sx_wave_timestamp(uint64_t time, int initial, int *wrote) {
    if (*wrote) return;
    if (sx_vcd) {
        if (initial || time != sx_wave_last_time)
            fprintf(sx_vcd, "#%llu\n", (unsigned long long)time);
        if (initial) fputs("$dumpvars\n", sx_vcd);
    }
    if (sx_fst) fstWriterEmitTimeChange(sx_fst, time);
    *wrote = 1;
}

static void sx_wave_render_vcd(uint32_t descriptor) {
    uint32_t signal = sx_wave_signal_ids[descriptor];
    uint32_t width = sx_wave_signal_widths[descriptor];
    uint8_t kind = sx_wave_signal_kinds[descriptor];
    if (kind == SX_WAVE_REAL) {
        uint64_t bits = sx_read_word(signal, 0);
        double value;
        memcpy(&value, &bits, sizeof value);
        fprintf(sx_vcd, "r%.17g v%u\n", value, signal);
    } else if (kind == SX_WAVE_ENUM) {
        uint64_t value = sx_read_word(signal, 0);
        const char *symbol = sx_wave_symbol(descriptor, value);
        if (symbol)
            fprintf(sx_vcd, "s%s v%u\n", symbol, signal);
        else
            fprintf(sx_vcd, "s%llu v%u\n", (unsigned long long)value,
                    signal);
    } else if (kind == SX_WAVE_LOGIC) {
        const char *symbol = sx_wave_symbol(descriptor, sx_read_word(signal, 0));
        fprintf(sx_vcd, "%cv%u\n", symbol && symbol[0] ? symbol[0] : 'x',
                signal);
    } else if (kind == SX_WAVE_LOGIC_VECTOR) {
        uint32_t companion = sx_wave_signal_companions[descriptor];
        fputc('b', sx_vcd);
        for (uint32_t remaining = width; remaining != 0; --remaining) {
            uint32_t bit = remaining - 1;
            const char *symbol =
                sx_wave_symbol(descriptor, sx_wave_discriminant(companion, bit));
            int character = symbol && symbol[0] ? symbol[0] : 'x';
            if (character != 'x' && character != 'z')
                character = sx_wave_bit(signal, bit) ? '1' : '0';
            fputc(character, sx_vcd);
        }
        fprintf(sx_vcd, " v%u\n", signal);
    } else if (width <= 1) {
        fprintf(sx_vcd, "%cv%u\n", sx_wave_bit(signal, 0) ? '1' : '0',
                signal);
    } else {
        fputc('b', sx_vcd);
        for (uint32_t remaining = width; remaining != 0; --remaining)
            fputc(sx_wave_bit(signal, remaining - 1) ? '1' : '0', sx_vcd);
        fprintf(sx_vcd, " v%u\n", signal);
    }
}

static void sx_wave_render_fst(uint32_t descriptor) {
    uint32_t signal = sx_wave_signal_ids[descriptor];
    uint32_t width = sx_wave_signal_widths[descriptor];
    uint8_t kind = sx_wave_signal_kinds[descriptor];
    if (kind == SX_WAVE_REAL) {
        uint64_t bits = sx_read_word(signal, 0);
        double value;
        memcpy(&value, &bits, sizeof value);
        fstWriterEmitValueChange(sx_fst, sx_fst_handles[descriptor], &value);
    } else if (kind == SX_WAVE_ENUM) {
        uint64_t value = sx_read_word(signal, 0);
        const char *symbol = sx_wave_symbol(descriptor, value);
        if (!symbol) {
            snprintf(sx_fst_value, 33, "%llu", (unsigned long long)value);
            symbol = sx_fst_value;
        }
        fstWriterEmitVariableLengthValueChange(
            sx_fst, sx_fst_handles[descriptor], symbol,
            (uint32_t)strlen(symbol));
    } else if (kind == SX_WAVE_LOGIC) {
        const char *symbol = sx_wave_symbol(descriptor, sx_read_word(signal, 0));
        sx_fst_value[0] = symbol && symbol[0] ? symbol[0] : 'x';
        sx_fst_value[1] = 0;
        fstWriterEmitValueChange(sx_fst, sx_fst_handles[descriptor],
                                 sx_fst_value);
    } else if (kind == SX_WAVE_LOGIC_VECTOR) {
        uint32_t companion = sx_wave_signal_companions[descriptor];
        for (uint32_t remaining = width; remaining != 0; --remaining) {
            uint32_t bit = remaining - 1;
            const char *symbol =
                sx_wave_symbol(descriptor, sx_wave_discriminant(companion, bit));
            int character = symbol && symbol[0] ? symbol[0] : 'x';
            if (character != 'x' && character != 'z')
                character = sx_wave_bit(signal, bit) ? '1' : '0';
            sx_fst_value[width - remaining] = (char)character;
        }
        sx_fst_value[width] = 0;
        fstWriterEmitValueChange(sx_fst, sx_fst_handles[descriptor],
                                 sx_fst_value);
    } else {
        for (uint32_t remaining = width; remaining != 0; --remaining)
            sx_fst_value[width - remaining] =
                sx_wave_bit(signal, remaining - 1) ? '1' : '0';
        sx_fst_value[width] = 0;
        fstWriterEmitValueChange(sx_fst, sx_fst_handles[descriptor],
                                 sx_fst_value);
    }
}

void sx_wave_sample(uint64_t now) {
    if (!sx_vcd && !sx_fst) return;
    uint64_t time = UINT64_MAX - sx_wave_base < now ? UINT64_MAX
                                                    : sx_wave_base + now;
    int initial = !sx_wave_started;
    int wrote = 0;
    for (uint32_t descriptor = 0; descriptor < sx_wave_signal_count;
         ++descriptor) {
        uint32_t signal = sx_wave_signal_ids[descriptor];
        uint32_t companion = sx_wave_signal_companions[descriptor];
        int changed = !sx_seen[descriptor] ||
                      sx_wave_changed(descriptor, signal, sx_value_offsets,
                                      sx_last_values);
        if (companion != UINT32_MAX)
            changed = changed || sx_wave_changed(descriptor, companion,
                                                 sx_meta_offsets, sx_last_meta);
        if (!changed) continue;
        sx_wave_timestamp(time, initial, &wrote);
        if (sx_vcd) sx_wave_render_vcd(descriptor);
        if (sx_fst) sx_wave_render_fst(descriptor);
        sx_wave_remember(descriptor, signal, sx_value_offsets, sx_last_values);
        if (companion != UINT32_MAX)
            sx_wave_remember(descriptor, companion, sx_meta_offsets,
                             sx_last_meta);
        sx_seen[descriptor] = 1;
    }
    if (wrote) {
        if (initial && sx_vcd) fputs("$end\n", sx_vcd);
        sx_wave_started = 1;
        sx_wave_last_time = time;
    }
}
