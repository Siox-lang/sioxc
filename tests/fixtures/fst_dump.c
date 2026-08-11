#include <stdio.h>

#include "fstapi.h"

/*
 * Tiny test-only equivalent of fst2vcd. Both hierarchy declarations and value
 * changes are decoded by libfst itself, so a test cannot pass merely because a
 * file has a plausible header.
 */
int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: fst_dump <trace.fst>\n");
        return 2;
    }

    fstReaderContext *reader = fstReaderOpen(argv[1]);
    if (!reader) {
        fprintf(stderr, "cannot read FST: %s\n", argv[1]);
        return 3;
    }
    if (fstReaderGetTimescale(reader) != -15) {
        fprintf(stderr, "unexpected FST timescale: %d\n", fstReaderGetTimescale(reader));
        fstReaderClose(reader);
        return 4;
    }
    if (!fstReaderGetScopeCount(reader) || !fstReaderGetVarCount(reader)) {
        fprintf(stderr, "FST has no hierarchy or variables\n");
        fstReaderClose(reader);
        return 5;
    }

    fstReaderSetVcdExtensions(reader, 1);
    if (!fstReaderProcessHier(reader, stdout)) {
        fstReaderClose(reader);
        return 6;
    }
    fstReaderSetFacProcessMaskAll(reader);
    if (!fstReaderIterBlocks(reader, NULL, NULL, stdout)) {
        fstReaderClose(reader);
        return 7;
    }

    fstReaderClose(reader);
    return 0;
}
