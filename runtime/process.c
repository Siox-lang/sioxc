#include "process.h"

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
    SX_PROCESS_ABI = 8,
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
extern const uint32_t sx_test_process_offsets[];
extern const uint32_t sx_test_process_ids[];
extern const uint32_t sx_process_count;
extern sx_process_entry const sx_process_entries[];
extern const uint32_t sx_process_initial_blocks[];
extern const uint8_t sx_process_activations[];
extern const uint32_t sx_process_sensitivity_offsets[];
extern const uint8_t sx_process_sensitivity_kinds[];
extern const uint32_t sx_process_sensitivity_ids[];

extern void sx_reset(void);
extern uint8_t sx_process_commit(void);
extern uint8_t sx_process_changed(uint32_t signal);
extern uint8_t sx_process_storage_changed(uint32_t storage);
extern uint32_t sx_index_error(void);
extern int64_t sx_index_value(void);
extern uint32_t sx_range_error(void);
extern int64_t sx_range_value(void);
extern uint32_t sx_range_site(void);
extern uint8_t sx_process_apply_scheduled(uint32_t site, const uint64_t *words,
                                          uint32_t word_count);

typedef struct sx_event {
    struct sx_event *next;
    uint64_t due;
    uint64_t sequence;
    uint32_t target;
    uint32_t process;
    uint32_t resume_block;
    uint32_t word_count;
    uint8_t kind;
    uint64_t words[];
} sx_event;

static char sx_error[192];
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

const char *sx_runtime_error(void) { return sx_error[0] ? sx_error : 0; }
uint64_t sx_runtime_now(void) { return sx_now; }
uint32_t sx_runtime_warning_count(void) { return sx_warnings; }

static int sx_fail(const char *message) {
    if (!sx_error[0]) snprintf(sx_error, sizeof sx_error, "%s", message);
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

void sx_runtime_format_begin(void) {
    sx_format_len = 0;
    if (sx_format_reserve(0)) sx_format[0] = 0;
}

void sx_runtime_format_text(const char *text) {
    if (!text || sx_error[0]) return;
    size_t length = strlen(text);
    if (!sx_format_reserve(length)) return;
    memcpy(sx_format + sx_format_len, text, length + 1);
    sx_format_len += length;
}

static void sx_runtime_format_integer(const uint64_t *words,
                                      uint32_t word_count, uint32_t width,
                                      uint8_t is_signed) {
    if (sx_error[0]) return;
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
    uint8_t negative = is_signed &&
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

    size_t digit_cap = (size_t)width / 3u + 3u;
    char *digits = malloc(digit_cap);
    if (!digits) {
        free(magnitude);
        sx_fail("cannot allocate runtime decimal value");
        return;
    }
    size_t length = 0;
    for (;;) {
        uint8_t nonzero = 0;
        for (uint32_t word = 0; word < word_count; ++word)
            nonzero |= magnitude[word] != 0;
        if (!nonzero) break;
        uint64_t remainder = 0;
        for (uint32_t word = word_count; word-- > 0;) {
            __uint128_t dividend = ((__uint128_t)remainder << 64u) | magnitude[word];
            magnitude[word] = (uint64_t)(dividend / 10u);
            remainder = (uint64_t)(dividend % 10u);
        }
        digits[length++] = (char)('0' + remainder);
    }
    if (!length) digits[length++] = '0';
    if (!sx_format_reserve(length + negative)) {
        free(digits);
        free(magnitude);
        return;
    }
    if (negative) sx_format[sx_format_len++] = '-';
    while (length) sx_format[sx_format_len++] = digits[--length];
    sx_format[sx_format_len] = 0;
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
    union { uint64_t bits; double value; } real;
    char rendered[64];
    real.bits = bits;
    snprintf(rendered, sizeof rendered, "%g", real.value);
    sx_runtime_format_text(rendered);
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
    if (!sx_error[0])
        snprintf(sx_error, sizeof sx_error, "%s %u", message, (unsigned)id);
    return 1;
}

static int sx_fail_process_block(const char *message, uint32_t process,
                                 uint32_t block) {
    if (!sx_error[0])
        snprintf(sx_error, sizeof sx_error, "%s %u block %u", message,
                 (unsigned)process, (unsigned)block);
    return 1;
}

uint8_t sx_runtime_assert(uint8_t condition, const char *message,
                          uint32_t file, uint32_t offset) {
    if (condition) return 0;
    if (!message || !*message) message = "assertion failed";
    if (!sx_error[0])
        snprintf(sx_error, sizeof sx_error, "%s (source %u:%u)", message,
                 (unsigned)file, (unsigned)offset);
    return 1;
}

void sx_runtime_warn(uint8_t condition, const char *message,
                     uint32_t file, uint32_t offset) {
    if (condition) return;
    if (!message || !*message) message = "warning";
    fprintf(stderr, "warning: %s (source %u:%u)\n", message,
            (unsigned)file, (unsigned)offset);
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
    size_t value_bytes = (size_t)word_count * sizeof(uint64_t);
    if (word_count && value_bytes / sizeof(uint64_t) != word_count) {
        sx_fail("invalid Process IR event value");
        return 0;
    }
    if (value_bytes > SIZE_MAX - sizeof(sx_event)) {
        sx_fail("invalid Process IR event value");
        return 0;
    }
    sx_event *event = malloc(sizeof(sx_event) + value_bytes);
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
    event->kind = SX_EVENT_WRITE;
    return event;
}

void sx_runtime_schedule(uint32_t site, uint64_t delay, const uint64_t *words,
                         uint32_t word_count) {
    if (sx_error[0]) return;
    if (!sx_running) {
        sx_fail("delayed write scheduled outside a running test");
        return;
    }
    if (!words || !word_count) {
        sx_fail("invalid delayed Process IR value");
        return;
    }
    sx_event *event = sx_allocate_event(delay, word_count);
    if (!event) return;
    event->target = site;
    event->process = sx_current_process;
    for (uint32_t word = 0; word < word_count; ++word)
        event->words[word] = words[word];
    sx_insert_event(event);
}

void sx_runtime_suspend_time(uint32_t process, uint32_t resume_block,
                             uint64_t delay) {
    if (sx_error[0]) return;
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
    if (sx_error[0]) return;
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
    if (sx_error[0]) return;
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
        if (!sx_error[0])
            snprintf(sx_error, sizeof sx_error,
                     "runtime index failure at site %u: index %lld",
                     (unsigned)(index - 1), (long long)sx_index_value());
        return 1;
    }
    uint32_t signal = sx_range_error();
    if (signal) {
        uint32_t site = sx_range_site();
        if (!sx_error[0])
            snprintf(sx_error, sizeof sx_error,
                     "runtime range failure for signal %u at site %u: value %lld",
                     (unsigned)(signal - 1), (unsigned)(site ? site - 1 : 0),
                     (long long)sx_range_value());
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

/* Apply every transaction due in the current simulation time. Delays of zero
 * are therefore staged into the update phase that follows the process batch
 * which scheduled them, before sensitivity-driven processes resume. */
static int sx_apply_due_events(uint8_t *ready, uint8_t *suspended,
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
            ready[process] = 1;
            free(event);
            continue;
        }
        if (event->kind != SX_EVENT_WRITE) {
            free(event);
            return sx_fail("invalid Process IR event kind");
        }
        uint8_t applied = sx_process_apply_scheduled(
            event->target, event->words, event->word_count);
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
            *settling = 0;
    uint32_t *resume_blocks = 0;
    uint32_t begin, end;
    int foreground_started = 0;
    int result = 0;
    sx_clear_events();
    sx_error[0] = 0;
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
    resume_blocks = calloc(bytes, sizeof(uint32_t));
    if (!ready || !next || !stopped || !suspended || !settling ||
        !resume_blocks) {
        result = sx_fail("cannot allocate Process IR ready queue");
        goto done;
    }

    sx_reset();
    /* Reset initializes process storage and stages its input bindings. Publish
       those values before any reactive process reads them. The compatibility
       runner also settles the initialized design before test stimulus starts. */
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
        resume_blocks[process] = sx_process_initial_blocks[process];
        if (sx_process_activations[process] == 1) ready[process] = 1;
    }

    for (;;) {
        int ran = 0;
        int finish = 0;
        for (uint32_t process = 0; process < sx_process_count; ++process) {
            if (!ready[process] || stopped[process] || suspended[process] ||
                settling[process])
                continue;
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
            if (sx_error[0]) {
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
                if (sx_process_activations[process] == 0) stopped[process] = 1;
            } else if (status == SX_PROCESS_STOPPED) {
                stopped[process] = 1;
            } else if (status == SX_PROCESS_FINISHED) {
                stopped[process] = 1;
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
            if (sx_apply_due_events(next, suspended, resume_blocks)) {
                result = 1;
                goto done;
            }
            uint8_t changed = sx_process_commit();
            if (finish) break;
            if (changed) {
                for (uint32_t item = begin; item < end; ++item) {
                    uint32_t process = sx_test_process_ids[item];
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

        /* Reactive hardware starts once at time zero and reaches a fixed point
           before foreground/test processes observe it. Events registered by
           clocks during that bootstrap stay queued; test stimulus begins at
           the same simulation time before the wheel may advance. */
        if (!foreground_started) {
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
        int released_settling = 0;
        for (uint32_t item = begin; item < end; ++item) {
            uint32_t process = sx_test_process_ids[item];
            if (settling[process] && !stopped[process]) {
                settling[process] = 0;
                ready[process] = 1;
                released_settling = 1;
            }
        }
        if (released_settling) continue;

        /* A completed foreground stimulus defines the end of its test after
         * its own queued transactions have drained. Free-running reactive
         * clocks may still have events forever, but they are implementation
         * support rather than a reason to keep the finished test alive. */
        int foreground_live = 0;
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
                sx_process_activations[event->process] == 0) {
                foreground_transaction = 1;
                break;
            }
        }
        if (!foreground_live && !foreground_transaction) break;

        if (!sx_events) {
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
        if (sx_apply_due_events(ready, suspended, resume_blocks)) {
            result = 1;
            goto done;
        }

        if (sx_process_commit()) {
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
    free(resume_blocks);
    free(settling);
    free(suspended);
    free(stopped);
    free(next);
    free(ready);
    return result;
}
