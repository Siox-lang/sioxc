//! A `.siox` source compiled to an object file, linked against a C harness and
//! run through the exported `sx_*` ABI.
//!
//! `src/llvm/aot.rs` already links and runs objects, but from a `Design` built
//! by hand — that covers the LLVM emitter, not the pipeline that produces the
//! IR it is given. Nothing went source -> object -> link -> run, which is the
//! path an external simulator harness actually takes.

use std::process::Command;

/// Signal ids are assigned in declaration order, which `--emit ir` prints:
/// `Counter.clk`, `Counter.rst`, `Counter.n`.
const HARNESS: &str = r#"
#include <stdint.h>
extern void     sx_reset(void);
extern void     sx_set(uint32_t id, uint64_t v);
extern uint64_t sx_read(uint32_t id);
extern void     sx_settle(void);
extern uint8_t  sx_process_commit(void);
extern uint8_t  sx_process_changed(uint32_t id);
extern const uint32_t sx_process_abi_version;
extern const uint32_t sx_process_count;
typedef uint8_t (*sx_process_entry)(uint32_t resume_block);
extern sx_process_entry const sx_process_entries[];
extern const uint32_t sx_process_initial_blocks[];

enum { CLK = 0, RST = 1, N = 2, V = 3 };

static void tick(void) {
    sx_set(CLK, 0); sx_settle();
    sx_set(CLK, 1); sx_settle();
}

int main(void) {
    if (sx_process_abi_version != 6 || sx_process_count == 0) return 5;
    if (!sx_process_entries[0]) return 6;
    sx_reset();
    sx_set(V, 7);
    if (sx_process_entries[0](sx_process_initial_blocks[0]) != 0) return 7;
    if (sx_read(N) != 0) return 8; /* staged writes are not immediately visible */
    if (sx_process_commit() != 1 || sx_process_changed(N) != 1) return 9;
    if (sx_read(N) != 7) return 10;

    sx_reset();
    sx_set(RST, 0);              /* Logic '0' */
    sx_settle();
    if (sx_read(N) != 0) return 1;

    for (int i = 0; i < 5; i++) tick();
    if (sx_read(N) != 5) return 2;

    sx_set(RST, 1);              /* Logic '1' */
    tick();
    if (sx_read(N) != 0) return 3;

    sx_set(RST, 0);
    for (int i = 0; i < 3; i++) tick();
    if (sx_read(N) != 3) return 4;
    return 0;
}
"#;

#[test]
fn a_compiled_object_simulates_through_its_abi() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let siox = env!("CARGO_BIN_EXE_sioxc");
    let root = env!("CARGO_MANIFEST_DIR");
    let dir = std::env::temp_dir().join(format!("siox_aot_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let object = dir.join("counter.o");
    let harness = dir.join("harness.c");
    let binary = dir.join("harness");

    let build = Command::new(siox)
        .current_dir(root)
        .args(["tests/fixtures/aot_counter.siox", "-o"])
        .arg(&object)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "object build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(object.exists(), "no object produced");

    std::fs::write(&harness, HARNESS).unwrap();
    let link = Command::new("clang")
        .arg(&harness)
        .arg(&object)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        link.status.success(),
        "link failed:\n{}",
        String::from_utf8_lossy(&link.stderr)
    );

    // Each non-zero exit names the assertion that failed, in order.
    let run = Command::new(&binary).status().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        run.code(),
        Some(0),
        "the harness rejected the design's behaviour at check {}",
        run.code().unwrap_or(-1)
    );
}

#[test]
fn a_source_checked_index_executes_through_process_ir() {
    if Command::new("clang").arg("--version").output().is_err() {
        eprintln!("skipping: clang not found");
        return;
    }
    let siox = env!("CARGO_BIN_EXE_sioxc");
    let root = env!("CARGO_MANIFEST_DIR");
    let dir = std::env::temp_dir().join(format!("siox_aot_index_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let object = dir.join("checked.o");
    let harness = dir.join("harness.c");
    let binary = dir.join("harness");

    let build = Command::new(siox)
        .current_dir(root)
        .args(["tests/fixtures/aot_checked_index.siox", "-o"])
        .arg(&object)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "object build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    std::fs::write(
        &harness,
        r#"
#include <stdint.h>
extern void sx_reset(void);
extern void sx_set(uint32_t id, uint64_t value);
extern uint64_t sx_read(uint32_t id);
extern uint32_t sx_index_error(void);
extern int64_t sx_index_value(void);
extern uint8_t sx_process_commit(void);
typedef uint8_t (*sx_process_entry)(uint32_t resume_block);
extern sx_process_entry const sx_process_entries[];
extern const uint32_t sx_process_initial_blocks[];

enum { INDEX = 0, WORD = 1, VALUE = 2 };

int main(void) {
    sx_reset();
    if (sx_process_entries[0](sx_process_initial_blocks[0]) != 0) return 1;
    if (sx_index_error() == 0 || sx_index_value() != 0) return 2;

    sx_reset();
    sx_set(INDEX, 8);
    sx_set(WORD, 1);
    if (sx_process_entries[0](sx_process_initial_blocks[0]) != 0) return 3;
    if (sx_index_error() != 0) return 4;
    if (sx_process_commit() != 1 || sx_read(VALUE) != 1) return 5;
    return 0;
}
"#,
    )
    .unwrap();
    let link = Command::new("clang")
        .arg(&harness)
        .arg(&object)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        link.status.success(),
        "link failed:\n{}",
        String::from_utf8_lossy(&link.stderr)
    );
    let run = Command::new(&binary).status().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(run.code(), Some(0), "native checked-index probe failed");
}
