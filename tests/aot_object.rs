//! A `.siox` source compiled to an object file, linked against a small
//! hand-written ABI probe, and run through the exported `sx_*` ABI.
//!
//! `src/llvm/aot.rs` already links and runs objects, but from a `Design` built
//! by hand — that covers the LLVM emitter, not the pipeline that produces the
//! IR it is given. Nothing went source -> object -> link -> run, which is the
//! path an external simulator harness actually takes.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn object_settle_preserves_shared_and_overwritten_foreign_effects() {
    use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};
    let dir = std::env::temp_dir().join(format!("siox_aot_effects_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let object = dir.join("effects.o");
    let harness = dir.join("effects.c");
    let binary = dir.join("effects");
    let source = r#"
module object_effects;
extern "C" { fn mark(value: integer) -> integer; }
struct Packet { pub a: integer, pub b: integer, pub c: integer, pub d: integer, pub e: integer }
fn spread(value: integer) -> Packet {
    return { .a = value, .b = value, .c = value, .d = value, .e = value };
}
entity Dut { clock: Bit in, enabled: Bool in, packet: Packet out, value: integer out }
impl Dut {
    packet = if enabled { spread(mark(65)) } else { { .a = 7, .b = 7, .c = 7, .d = 7, .e = 7 } };
    if clock.rising() { value = mark(70); value = mark(71); }
}
"#;
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory(dir.join("effects.siox"), source),
            Emit::Object { top: None },
        )
        .with_output(&object),
    );
    assert!(
        compilation.succeeded(),
        "{} {:?}",
        compilation.render_diagnostics(),
        compilation.failure
    );
    let design = compilation.design.unwrap();
    assert_eq!(
        design.drivers.len(),
        5,
        "struct leaves must cross a helper boundary"
    );
    assert_eq!(
        design.event_blocks[0].updates.len(),
        2,
        "overwritten update must remain canonical"
    );
    let clock = design
        .signals
        .iter()
        .position(|signal| signal.path.ends_with(".clock"))
        .unwrap();
    let value = design
        .signals
        .iter()
        .position(|signal| signal.path.ends_with(".value"))
        .unwrap();
    let enabled = design
        .signals
        .iter()
        .position(|signal| signal.path.ends_with(".enabled"))
        .unwrap();
    std::fs::write(&harness, format!(r#"
#include <stdint.h>
#include <stdio.h>
extern void sx_reset(void);
extern void sx_set(uint32_t id, uint64_t value);
extern uint64_t sx_read(uint32_t id);
extern void sx_settle(void);
static unsigned calls[256];
static unsigned order[2], order_count;
int64_t mark(int64_t value) {{
    ++calls[value];
    if (value == 70 || value == 71) {{ if (order_count < 2) order[order_count] = value; ++order_count; }}
    return value;
}}
int main(void) {{
    sx_reset();
    sx_settle();
    if (calls[65] != 0 || calls[70] != 0 || calls[71] != 0) return 4;
    sx_set({enabled}, 1);
    sx_settle();
    calls[65] = 0;
    sx_settle(); /* Stable state: pre-event and post-event combinational passes. */
    if (calls[65] != 2) {{ fprintf(stderr, "shared call executed %u times\n", calls[65]); return 1; }}
    sx_set({clock}, 1);
    sx_settle();
    if (calls[70] != 1 || calls[71] != 1) {{
        fprintf(stderr, "overwritten/retained calls: %u/%u\n", calls[70], calls[71]); return 2;
    }}
    if (order_count != 2 || order[0] != 70 || order[1] != 71) return 5;
    if (sx_read({value}) != 71) return 3;
    return 0;
}}
"#)).unwrap();
    let link = Command::new("clang")
        .arg(&harness)
        .arg(&object)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        link.status.success(),
        "{}",
        String::from_utf8_lossy(&link.stderr)
    );
    let run = Command::new(&binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(dir);
}

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
    if (sx_process_abi_version != 15 || sx_process_count == 0) return 5;
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
