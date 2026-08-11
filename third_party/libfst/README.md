# libfst

This directory vendors the FST waveform reader/writer used by GTKWave.

- Upstream: <https://github.com/gtkwave/libfst>
- Revision: `74301348450701727776c1a0522a3f512738e9ae`
- License: MIT, with bundled LZ4 and FastLZ notices in [`LICENSE`](LICENSE)

`sioxc` embeds these sources and writes them beside its generated native test
harness in the temporary build directory. Clang then compiles them into the
test executable, so `--fst` does not require GTKWave, `vcd2fst`, or a separately
installed libfst at either compiler or simulation runtime. The generated
executable links the platform zlib used by libfst.

The files are kept byte-for-byte from upstream. Update all files and the pinned
revision together.
