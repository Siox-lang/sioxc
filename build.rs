//! Check that the vendored-by-reference sources are present.
//!
//! `third_party/libfst` is a git submodule, and the native FST writer is
//! embedded from it with `include_str!`. A clone made without `--recursive`
//! leaves that directory empty, and the failure would otherwise be a raw
//! "couldn't read .../fstapi.c" pointing inside the compiler rather than at the
//! thing the reader has to do.

use std::path::Path;

const LIBFST: &str = "third_party/libfst/src/fstapi.c";

fn main() {
    println!("cargo:rerun-if-changed={LIBFST}");
    if !Path::new(LIBFST).exists() {
        println!(
            "cargo:warning=third_party/libfst is empty — the FST writer is a git \
             submodule. Run `git submodule update --init --recursive`."
        );
        panic!(
            "missing submodule third_party/libfst\n\
             \n\
             The native FST waveform writer is vendored by reference. Fetch it with:\n\
             \n    git submodule update --init --recursive\n"
        );
    }
}
