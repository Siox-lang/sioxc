//! Link a native Process IR simulator executable.
//!
//! Siox semantics are emitted once by the LLVM backend. Test executables link
//! that design object with a fixed scheduler, CLI, and waveform runtime; this
//! module never translates source or Process IR into per-design C.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use siox::ir::Design;

const LIBFST_API_C: &str = include_str!("../../third_party/libfst/src/fstapi.c");
const LIBFST_API_H: &str = include_str!("../../third_party/libfst/src/fstapi.h");
const LIBFST_FASTLZ_C: &str = include_str!("../../third_party/libfst/src/fastlz.c");
const LIBFST_FASTLZ_H: &str = include_str!("../../third_party/libfst/src/fastlz.h");
const LIBFST_LZ4_C: &str = include_str!("../../third_party/libfst/src/lz4.c");
const LIBFST_LZ4_H: &str = include_str!("../../third_party/libfst/src/lz4.h");
const LIBFST_API_O: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fstapi.o"));
const LIBFST_FASTLZ_O: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/fastlz.o"));
const LIBFST_LZ4_O: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/lz4.o"));

const PROCESS_RUNTIME_C: &str = include_str!("../../runtime/process.c");
const PROCESS_RUNTIME_H: &str = include_str!("../../runtime/process.h");
const PROCESS_MAIN_C: &str = include_str!("../../runtime/main.c");
const WAVE_RUNTIME_C: &str = include_str!("../../runtime/wave.c");
const WAVE_RUNTIME_H: &str = include_str!("../../runtime/wave.h");
const PROCESS_RUNTIME_O: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/process_runtime.o"));
const PROCESS_MAIN_O: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/process_main.o"));
const WAVE_RUNTIME_O: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/wave_runtime.o"));

static NATIVE_BUILD_SERIAL: AtomicU64 = AtomicU64::new(0);

pub(super) struct BuildRequest<'a> {
    pub design: &'a Design,
    pub sources: &'a siox::diag::SourceMap,
    /// Reserved for native source-level debug metadata. Process execution is
    /// source-located today, but interactive stepping is not implemented.
    pub debug: bool,
    pub output: &'a Path,
}

pub(super) fn build(request: BuildRequest<'_>) -> Result<(), String> {
    let BuildRequest {
        design,
        sources,
        debug,
        output,
        ..
    } = request;
    let issues = design.validate();
    if !issues.is_empty() {
        return Err(issues.join("; "));
    }
    if debug {
        return Err(
            "native Process IR debug metadata is not implemented; omit --debug for now".into(),
        );
    }
    link_process_runtime(design, sources, output)
}

fn native_build_dir() -> PathBuf {
    let serial = NATIVE_BUILD_SERIAL.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "siox_process_runtime_{}_{}",
        std::process::id(),
        serial
    ))
}

/// Link one LLVM-emitted design with design-independent runtime objects.
///
/// Runtime sources are a fallback for cross builds or hosts where `build.rs`
/// could not precompile them. They are copied verbatim and contain no design
/// behavior, source statements, or generated dispatcher.
fn link_process_runtime(
    design: &Design,
    sources: &siox::diag::SourceMap,
    output_path: &Path,
) -> Result<(), String> {
    let temporary = native_build_dir();
    std::fs::create_dir_all(&temporary).map_err(|error| error.to_string())?;
    let object = temporary.join("design.o");
    let result = (|| {
        siox::llvm::emit_object_with_sources(design, sources, &object)?;

        // Empty embedded objects are intentional fallback markers written by
        // build.rs when the host cannot precompile the fixed runtime.
        let precompiled = [
            PROCESS_RUNTIME_O,
            PROCESS_MAIN_O,
            WAVE_RUNTIME_O,
            LIBFST_API_O,
            LIBFST_FASTLZ_O,
            LIBFST_LZ4_O,
        ]
        .iter()
        .all(|contents| !contents.is_empty());

        let mut clang = Command::new("clang");
        clang.arg(&object);
        if precompiled {
            let objects = [
                ("process_runtime.o", PROCESS_RUNTIME_O),
                ("process_main.o", PROCESS_MAIN_O),
                ("wave_runtime.o", WAVE_RUNTIME_O),
                ("fstapi.o", LIBFST_API_O),
                ("fastlz.o", LIBFST_FASTLZ_O),
                ("lz4.o", LIBFST_LZ4_O),
            ];
            for (name, contents) in objects {
                let path = temporary.join(name);
                std::fs::write(&path, contents).map_err(|error| error.to_string())?;
                clang.arg(path);
            }
        } else {
            let sources = [
                ("process.c", PROCESS_RUNTIME_C),
                ("process.h", PROCESS_RUNTIME_H),
                ("main.c", PROCESS_MAIN_C),
                ("wave.c", WAVE_RUNTIME_C),
                ("wave.h", WAVE_RUNTIME_H),
                ("fstapi.c", LIBFST_API_C),
                ("fstapi.h", LIBFST_API_H),
                ("fastlz.c", LIBFST_FASTLZ_C),
                ("fastlz.h", LIBFST_FASTLZ_H),
                ("lz4.c", LIBFST_LZ4_C),
                ("lz4.h", LIBFST_LZ4_H),
            ];
            for (name, contents) in sources {
                std::fs::write(temporary.join(name), contents)
                    .map_err(|error| error.to_string())?;
            }
            clang
                .arg(temporary.join("process.c"))
                .arg(temporary.join("main.c"))
                .arg(temporary.join("wave.c"))
                .arg(temporary.join("fstapi.c"))
                .arg(temporary.join("fastlz.c"))
                .arg(temporary.join("lz4.c"))
                .arg("-I")
                .arg(&temporary);
        }

        let output = clang
            .args(["-O2", "-lm", "-lz"])
            .arg("-o")
            .arg(output_path)
            .output()
            .map_err(|error| format!("failed to run clang: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "clang failed to link the Process IR simulator:\n{}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(temporary);
    result
}
