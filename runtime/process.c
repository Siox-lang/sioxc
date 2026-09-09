#include "process.h"

#include <stdio.h>
#include <stdlib.h>

enum {
    SX_PROCESS_COMPLETED = 0,
    SX_PROCESS_SUSPENDED = 1,
    SX_PROCESS_STOPPED = 2,
    SX_PROCESS_FINISHED = 3,
    SX_PROCESS_UNSUPPORTED = 255,
    SX_PROCESS_ABI = 4
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

static char sx_error[192];

const char *sx_runtime_error(void) { return sx_error[0] ? sx_error : 0; }

static int sx_fail(const char *message) {
    if (!sx_error[0]) snprintf(sx_error, sizeof sx_error, "%s", message);
    return 1;
}

static int sx_fail_id(const char *message, uint32_t id) {
    if (!sx_error[0])
        snprintf(sx_error, sizeof sx_error, "%s %u", message, (unsigned)id);
    return 1;
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

int sx_runtime_run_test(uint32_t test) {
    uint8_t *ready = 0, *next = 0, *stopped = 0;
    uint32_t begin, end;
    int result = 0;
    sx_error[0] = 0;

    if (sx_process_abi_version != SX_PROCESS_ABI)
        return sx_fail("unsupported Process IR runtime ABI");
    if (test >= sx_test_count) return sx_fail_id("invalid test descriptor", test);

    size_t bytes = sx_process_count ? (size_t)sx_process_count : 1;
    ready = calloc(bytes, 1);
    next = calloc(bytes, 1);
    stopped = calloc(bytes, 1);
    if (!ready || !next || !stopped) {
        result = sx_fail("cannot allocate Process IR ready queue");
        goto done;
    }

    sx_reset();
    begin = sx_test_process_offsets[test];
    end = sx_test_process_offsets[test + 1];
    for (uint32_t item = begin; item < end; ++item) {
        uint32_t process = sx_test_process_ids[item];
        if (process >= sx_process_count) {
            result = sx_fail_id("test references invalid process", process);
            goto done;
        }
        ready[process] = 1;
    }

    for (;;) {
        int ran = 0;
        int finish = 0;
        for (uint32_t process = 0; process < sx_process_count; ++process) {
            if (!ready[process] || stopped[process]) continue;
            ran = 1;
            sx_process_entry entry = sx_process_entries[process];
            if (!entry) {
                result = sx_fail_id("missing Process IR entry", process);
                goto done;
            }
            uint8_t status = entry(sx_process_initial_blocks[process]);
            if (sx_design_failed()) {
                result = 1;
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
                result = sx_fail_id("process suspended without a runtime resume record", process);
                goto done;
            } else if (status == SX_PROCESS_UNSUPPORTED) {
                result = sx_fail_id("direct Process IR lowering is incomplete for process", process);
                goto done;
            } else {
                result = sx_fail_id("invalid Process IR entry status from process", process);
                goto done;
            }
        }
        if (!ran) break;

        uint8_t changed = sx_process_commit();
        if (finish || !changed) break;

        for (uint32_t item = begin; item < end; ++item) {
            uint32_t process = sx_test_process_ids[item];
            if (stopped[process] || sx_process_activations[process] != 1) continue;
            int changed_sensitivity = sx_has_changed_sensitivity(process);
            if (changed_sensitivity < 0) {
                result = 1;
                goto done;
            }
            if (changed_sensitivity) next[process] = 1;
        }
        uint8_t *swap = ready;
        ready = next;
        next = swap;
        for (uint32_t process = 0; process < sx_process_count; ++process)
            next[process] = 0;
    }

done:
    free(stopped);
    free(next);
    free(ready);
    return result;
}
