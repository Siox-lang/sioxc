//! A process inside a generate `for`/`if`: one per iteration or taken
//! branch, with the loop value written in (VHDL's `for … generate` with a
//! process inside).

use std::process::Command;

fn sioxc(name: &str, source: &str) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!(
        "siox_generate_processes_{}_{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let out = dir.join(name);
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
            "-o",
        ])
        .arg(&out)
        .arg(&file)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    (out, text)
}

/// Three lanes, each its own clocked process with its own loop value, and
/// a generate `if` that builds only the taken branch's process.
#[test]
fn processes_are_built_per_iteration_and_branch() {
    let (binary, built) = sioxc(
        "lanes",
        r#"module g1;
use std::bits::unsigned;
entity Top { clk: Bit in, en: Bit in, q: unsigned[8][3] out, mode: unsigned[8] out }
impl Top {
    lanes: for i in 0..2 {
        tick: process {
            if clk.rising() and en == '1' { q[i] = q[i] + unsigned[8](i + 1); }
        }
    }
    sel: if 2 > 1 {
        process { if clk.rising() { mode = mode + 10; } }
    } else {
        process { if clk.rising() { mode = mode + 1; } }
    }
}
#[test] entity T {}
impl T { let clk: Bit; let en: Bit = '1'; let q: unsigned[8][3]; let mode: unsigned[8];
  let d: Top = { .clk = clk, .en = en, .q = q, .mode = mode };
  process {
    for k in 0..3 { clk = '1'; await 1ns; clk = '0'; await 1ns; }
    print!("{} {} {} {}\n", q[0], q[1], q[2], mode);
    assert!(q[0] == 4 and q[1] == 8 and q[2] == 12 and mode == 40);
  } }
"#,
    );
    let ran = Command::new(&binary)
        .output()
        .unwrap_or_else(|_| panic!("{built}"));
    let out = String::from_utf8_lossy(&ran.stdout);
    assert!(
        out.contains("4 8 12 40") && out.contains("1 passed"),
        "{out}"
    );
}

/// A testbench generate block cannot hold a process yet, and a process
/// never holds another.
#[test]
fn misplaced_processes_are_reported() {
    let (_, testbench) = sioxc(
        "testbench",
        r#"module g2;
#[test] entity T {}
impl T { let x: Bit; for i in 0..1 { process { x = '1'; } } }
"#,
    );
    assert!(
        testbench.contains("not supported in a testbench yet"),
        "{testbench}"
    );
    let (_, nested) = sioxc(
        "nested",
        r#"module g3;
entity E { y: Bit out }
impl E { process { process { y = '1'; } } }
"#,
    );
    assert!(
        nested.contains("a process cannot be declared inside a process"),
        "{nested}"
    );
}
