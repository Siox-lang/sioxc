# Third-party sources

## libfst

The FST waveform writer used by GTKWave, vendored **by reference** as the git
submodule [`libfst/`](libfst).

- Upstream: <https://github.com/gtkwave/libfst>
- Pinned at the revision recorded in the submodule; update it with
  `git -C third_party/libfst checkout <rev>` and commit the new pointer.
- License: MIT, with bundled LZ4 and FastLZ notices in `libfst/LICENSE`.

`sioxc` embeds `libfst/src/*.c` and `*.h` with `include_str!` and writes them
beside its generated native test harness in the temporary build directory.
Clang compiles them into the test executable, so writing FST needs no installed
GTKWave, `vcd2fst`, or separate libfst at either compile or simulation time.
The generated executable links the platform zlib that libfst uses.

### Fetching it

A clone without submodules leaves `libfst/` empty and the build stops with a
message naming this command:

```sh
git submodule update --init --recursive
```

`build.rs` performs that check so the failure names the missing submodule
rather than an unreadable path inside the compiler.

### A consequence worth knowing

Because the sources are embedded at compile time, the crate cannot build
without them. `cargo package` does **not** include submodule contents, so
publishing to a registry would need the submodule replaced by a vendored copy
(as it was before) or the embedding replaced by a build-time fetch. This does
not affect building from a git checkout, which is how the project is used
today.
