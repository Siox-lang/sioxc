#include "wave.h"

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
extern const uint32_t sx_wave_symbol_offsets[];
extern const uint64_t sx_wave_symbol_values[];
extern const char *const sx_wave_symbol_texts[];
extern const char sx_wave_vcd_header[];
extern uint64_t sx_read_word(uint32_t signal, uint32_t word);

static FILE *sx_vcd;
static size_t *sx_value_offsets;
static size_t *sx_meta_offsets;
static uint64_t *sx_last_values;
static uint64_t *sx_last_meta;
static uint8_t *sx_seen;
static uint64_t sx_vcd_base;
static uint64_t sx_vcd_last_time;
static int sx_vcd_started;

static size_t sx_wave_words(uint64_t width) {
    return (size_t)(width / 64u + (width % 64u != 0));
}

static int sx_wave_add_words(size_t *total, size_t words) {
    if (*total > SIZE_MAX - words) return 0;
    *total += words;
    return 1;
}

void sx_wave_close(void) {
    if (sx_vcd) fclose(sx_vcd);
    sx_vcd = 0;
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
    sx_vcd_base = 0;
    sx_vcd_last_time = 0;
    sx_vcd_started = 0;
}

int sx_wave_open_vcd(const char *path) {
    sx_wave_close();
    sx_vcd = fopen(path, "wb");
    if (!sx_vcd) {
        fprintf(stderr, "cannot open VCD output %s\n", path);
        return 0;
    }

    size_t count = sx_wave_signal_count;
    if (count == SIZE_MAX) {
        fprintf(stderr, "VCD waveform state is too large\n");
        sx_wave_close();
        return 0;
    }
    sx_value_offsets = calloc(count + 1, sizeof(size_t));
    sx_meta_offsets = calloc(count + 1, sizeof(size_t));
    sx_seen = calloc(count ? count : 1, 1);
    if (!sx_value_offsets || !sx_meta_offsets || !sx_seen) {
        fprintf(stderr, "cannot allocate VCD waveform state\n");
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
            fprintf(stderr, "VCD waveform state is too large\n");
            sx_wave_close();
            return 0;
        }
        if (sx_wave_signal_companions[signal] != UINT32_MAX &&
            !sx_wave_add_words(
                &meta_words,
                sx_wave_words((uint64_t)sx_wave_signal_widths[signal] * 4u))) {
            fprintf(stderr, "VCD waveform state is too large\n");
            sx_wave_close();
            return 0;
        }
    }
    sx_value_offsets[count] = value_words;
    sx_meta_offsets[count] = meta_words;
    sx_last_values = calloc(value_words ? value_words : 1, sizeof(uint64_t));
    sx_last_meta = calloc(meta_words ? meta_words : 1, sizeof(uint64_t));
    if (!sx_last_values || !sx_last_meta) {
        fprintf(stderr, "cannot allocate VCD waveform values\n");
        sx_wave_close();
        return 0;
    }
    if (fputs(sx_wave_vcd_header, sx_vcd) < 0) {
        fprintf(stderr, "cannot write VCD output %s\n", path);
        sx_wave_close();
        return 0;
    }
    return 1;
}

void sx_wave_begin_test(void) {
    if (!sx_vcd) return;
    sx_vcd_base = sx_vcd_started && sx_vcd_last_time != UINT64_MAX
                      ? sx_vcd_last_time + 1
                      : (sx_vcd_started ? UINT64_MAX : 0);
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
    if (initial || time != sx_vcd_last_time)
        fprintf(sx_vcd, "#%llu\n", (unsigned long long)time);
    if (initial) fputs("$dumpvars\n", sx_vcd);
    *wrote = 1;
}

static void sx_wave_render(uint32_t descriptor) {
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

void sx_wave_sample(uint64_t now) {
    if (!sx_vcd) return;
    uint64_t time = UINT64_MAX - sx_vcd_base < now ? UINT64_MAX
                                                   : sx_vcd_base + now;
    int initial = !sx_vcd_started;
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
        sx_wave_render(descriptor);
        sx_wave_remember(descriptor, signal, sx_value_offsets, sx_last_values);
        if (companion != UINT32_MAX)
            sx_wave_remember(descriptor, companion, sx_meta_offsets,
                             sx_last_meta);
        sx_seen[descriptor] = 1;
    }
    if (wrote) {
        if (initial) fputs("$end\n", sx_vcd);
        sx_vcd_started = 1;
        sx_vcd_last_time = time;
    }
}
