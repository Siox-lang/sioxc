#include "process.h"

#include <stdint.h>
#include <stdio.h>
#include <string.h>

extern const uint32_t sx_test_count;
extern const char *const sx_test_names[];

int main(int argc, char **argv) {
    const char *filter = 0;
    int list = 0;
    for (int argument = 1; argument < argc; ++argument) {
        if (!strcmp(argv[argument], "--list")) list = 1;
        else if (!filter) filter = argv[argument];
        else {
            fprintf(stderr, "unexpected argument: %s\n", argv[argument]);
            return 2;
        }
    }

    if (list) {
        for (uint32_t test = 0; test < sx_test_count; ++test)
            if (!filter || strstr(sx_test_names[test], filter))
                puts(sx_test_names[test]);
        return 0;
    }

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

    printf("\ntest result: %s. %u passed; %u failed; %u filtered out\n",
           failed ? "FAILED" : "ok", (unsigned)(selected - failed),
           (unsigned)failed, (unsigned)(sx_test_count - selected));
    return failed ? 1 : 0;
}
