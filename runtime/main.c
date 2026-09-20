#include "process.h"
#include "wave.h"

#include <stdint.h>
#include <stdio.h>
#include <string.h>

extern const uint32_t sx_test_count;
extern const char *const sx_test_names[];
extern uint32_t sx_runtime_warning_count(void);

static int sx_is_vcd(const char *path) {
    const char *dot = 0;
    for (const char *part = path; *part; ++part)
        if (*part == '.') dot = part;
    if (!dot) return 0;
    return (dot[1] == 'v' || dot[1] == 'V') &&
           (dot[2] == 'c' || dot[2] == 'C') &&
           (dot[3] == 'd' || dot[3] == 'D') && !dot[4];
}

int main(int argc, char **argv) {
    const char *filter = 0;
    const char *vcd_path = 0;
    int list = 0;
    for (int argument = 1; argument < argc; ++argument) {
        const char *path = 0;
        if (!strcmp(argv[argument], "-o") ||
            !strcmp(argv[argument], "--output")) {
            if (++argument == argc) {
                fprintf(stderr, "%s requires a path\n", argv[argument - 1]);
                return 2;
            }
            path = argv[argument];
        } else if (!strncmp(argv[argument], "--output=", 9)) {
            path = argv[argument] + 9;
        } else if (!strncmp(argv[argument], "-o", 2) && argv[argument][2]) {
            path = argv[argument] + 2;
        } else if (!strcmp(argv[argument], "--list")) {
            list = 1;
        } else if (!filter) {
            filter = argv[argument];
        } else {
            fprintf(stderr, "unexpected argument: %s\n", argv[argument]);
            return 2;
        }
        if (path) {
            if (!sx_is_vcd(path)) {
                fprintf(stderr,
                        "the direct runtime currently requires a .vcd waveform path\n");
                return 2;
            }
            if (vcd_path) {
                fprintf(stderr, "more than one VCD output was requested\n");
                return 2;
            }
            vcd_path = path;
        }
    }

    if (list) {
        for (uint32_t test = 0; test < sx_test_count; ++test)
            if (!filter || strstr(sx_test_names[test], filter))
                puts(sx_test_names[test]);
        return 0;
    }

    if (vcd_path && !sx_wave_open_vcd(vcd_path)) return 2;

    uint32_t selected = 0;
    for (uint32_t test = 0; test < sx_test_count; ++test)
        if (!filter || strstr(sx_test_names[test], filter)) selected++;
    printf("\nrunning %u test%s\n", (unsigned)selected, selected == 1 ? "" : "s");

    uint32_t failed = 0;
    for (uint32_t test = 0; test < sx_test_count; ++test) {
        const char *name = sx_test_names[test];
        if (filter && !strstr(name, filter)) continue;
        if (sx_runtime_run_test(test)) {
            const char *error = sx_runtime_error();
            printf("test %s ... FAILED\n", name);
            if (error) fprintf(stderr, "%s\n", error);
            failed++;
        } else {
            printf("test %s ... ok\n", name);
        }
    }

    printf("\ntest result: %s. %u passed; %u failed; %u filtered out",
           failed ? "FAILED" : "ok", (unsigned)(selected - failed),
           (unsigned)failed, (unsigned)(sx_test_count - selected));
    uint32_t warnings = sx_runtime_warning_count();
    if (warnings)
        printf("; %u warning%s", (unsigned)warnings, warnings == 1 ? "" : "s");
    putchar('\n');
    sx_wave_close();
    return failed ? 1 : 0;
}
