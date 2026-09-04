//! Check that the vendored-by-reference sources are present.
//!
//! `third_party/libfst` is a git submodule. Its native FST runtime is normally
//! compiled to objects embedded in `sioxc`, while its sources remain embedded
//! as a fallback. A clone made without `--recursive` leaves that directory
//! empty, and the failure would otherwise be a raw "couldn't read
//! .../fstapi.c" pointing inside the compiler rather than at the thing the
//! reader has to do.

use std::path::{Path, PathBuf};
use std::process::Command;

const LIBFST: &str = "third_party/libfst/src/fstapi.c";
const RUNTIME_SOURCES: [(&str, &str); 3] = [
    ("fstapi.c", "fstapi.o"),
    ("fastlz.c", "fastlz.o"),
    ("lz4.c", "lz4.o"),
];

/// Compile the design-independent waveform runtime once with `sioxc` itself.
///
/// The resulting objects are embedded in the compiler and merely copied when
/// a simulator is linked. Failure is deliberately non-fatal: cross builds and
/// machines without clang can still build the object-only compiler, and the
/// native simulator builder retains its source-compilation fallback.
fn precompile_runtime(out_dir: &Path) {
    let enabled = std::env::var_os("CARGO_FEATURE_LLVM").is_some();
    let native = std::env::var_os("HOST") == std::env::var_os("TARGET");
    let source_dir = Path::new("third_party/libfst/src");

    for (source, object) in RUNTIME_SOURCES {
        let output = out_dir.join(object);
        let compiled = enabled
            && native
            && Command::new("clang")
                .args(["-O2", "-fPIC", "-c"])
                .arg(source_dir.join(source))
                .arg("-I")
                .arg(source_dir)
                .arg("-o")
                .arg(&output)
                .status()
                .is_ok_and(|status| status.success());
        if !compiled {
            std::fs::write(&output, []).unwrap_or_else(|error| {
                panic!("failed to create runtime object fallback marker: {error}")
            });
        }
    }

    if enabled
        && native
        && RUNTIME_SOURCES.iter().any(|(_, object)| {
            std::fs::metadata(out_dir.join(object)).is_ok_and(|metadata| metadata.len() == 0)
        })
    {
        println!(
            "cargo:warning=clang could not precompile the native waveform runtime; \
             simulator builds will compile it from source"
        );
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=PATH");
    for name in [
        "fstapi.c", "fstapi.h", "fastlz.c", "fastlz.h", "lz4.c", "lz4.h",
    ] {
        println!("cargo:rerun-if-changed=third_party/libfst/src/{name}");
    }
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

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    precompile_runtime(&out_dir);
}
