//! Port connections to a slice (`.y = y[3..0]`, `.a = x[3..0]`):
//!
//! - In hardware an output connected to a slice of a parent signal was
//!   dropped, the parent reported as never driven. It now drives that slice,
//!   as `y[3..0] = child.y;` would.
//! - In a testbench a slice input was bound as storage and did not lower; it
//!   is now read like any expression connection. A testbench output into a
//!   slice, which has no storage write to make, is an error.

use std::process::Command;

fn build(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_slice_connections_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    let binary = dir.join(name);
    std::fs::write(&file, source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
        ])
        .arg(&file)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status.success(), text, binary)
}

const INC: &str = "module g;\nuse std::bits::unsigned;\n\
                   entity Inc { a: unsigned[4] in, y: unsigned[4] out }\nimpl Inc { y = a + 1; }\n";

#[test]
fn slice_connections_drive_and_read() {
    let (ok, text, binary) = build(
        "slices",
        &format!(
            "{INC}entity G {{ x: unsigned[8] in, y: unsigned[8] out }}\n\
             impl G {{ lanes: for k in 0..1 {{ let c: Inc = {{ .a = x[4 * k + 3 .. 4 * k], .y = y[4 * k + 3 .. 4 * k] }}; }} }}\n\
             #[test] entity T {{}}\n\
             impl T {{ let x: unsigned[8] = 52; let y: unsigned[8]; let p: unsigned[4]; let q: unsigned[4];\n\
               let g: G = {{ .x = x, .y = y }};\n\
               let lo: Inc = {{ .a = x[3..0], .y = p }}; let hi: Inc = {{ .a = x[7..4], .y = q }};\n\
               check: process {{ await 1ns; print!(\"|{{}} {{}} {{}}\\n\", y, p, q); }} }}\n"
        ),
    );
    assert!(ok, "{text}");
    let report =
        String::from_utf8_lossy(&Command::new(&binary).output().unwrap().stdout).into_owned();
    // 52 is 0x34: each nibble plus one is 0x45, 69.
    assert!(report.contains("|69 5 4\n"), "{report}");
}

#[test]
fn a_testbench_output_cannot_drive_a_slice() {
    let (ok, text, _) = build(
        "output",
        &format!(
            "{INC}#[test] entity T {{}}\n\
             impl T {{ let a: unsigned[4] = 4; let y: unsigned[8]; let c: Inc = {{ .a = a, .y = y[3..0] }}; }}\n"
        ),
    );
    assert!(
        !ok && text.contains("cannot drive a slice of a testbench signal"),
        "{text}"
    );
}
