//! Regression for sioxc #634: `rising(clk)` calls a trait method as a free
//! function. The checker took any method name for a function and hardware
//! lowering failed (E-P017) whatever the branch assigned; it is reported
//! with the receiver form. Aggregate assignments under `clk.rising()` (a
//! struct, a matrix row) run.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i634_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
        ])
        .arg(&file)
        .arg("-o")
        .arg(dir.join("t.bin"))
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text, dir.join("t.bin"))
}

#[test]
fn a_method_called_as_a_function_is_reported() {
    let (ok, text, _) = compile("free", FREE);
    assert!(!ok, "{text}");
    assert!(
        text.contains("`rising` is a method, not a function"),
        "{text}"
    );
    assert!(text.contains("`clk.rising()`"), "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn aggregate_assignments_run_under_an_edge() {
    for (name, source) in [("matrix", MATRIX), ("record", RECORD)] {
        let (ok, text, binary) = compile(name, source);
        assert!(ok, "{text}");
        let run = Command::new(binary).output().unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout);
        assert!(
            run.status.success() && stdout.contains("done"),
            "{name}: {stdout}"
        );
    }
}

const FREE: &str = r#"module test_struct_clocked;

struct Point {
    pub x: integer,
    pub y: integer,
}

entity Top {
    clk: Bit in,
    inp: Point in,
    out_p: Point out,
}

impl Top {
    let p: Point;

    if rising(clk) {
        p = inp;
        out_p = p;
    }
}
"#;

const MATRIX: &str = r#"module p634;

entity Top {
    clk: Bit in,
    row: integer[4] in,
    out_row: integer[4] out,
}

impl Top {
    let matrix: integer[4][4];

    if clk.rising() {
        matrix[0] = row;
        out_row = matrix[0];
    }
}

#[test]
entity TB {}
impl TB {
    let clk: Bit;
    let row: integer[4] = [1, 2, 3, 4];
    let out_row: integer[4];
    let dut: Top = { .clk = clk, .row = row, .out_row = out_row };
    run: process {
        clk = '0'; await 1ns; clk = '1'; await 1ns;
        clk = '0'; await 1ns; clk = '1'; await 1ns;
        assert!(out_row[0] == 1 and out_row[3] == 4, "row {} {}", out_row[0], out_row[3]);
        print!("done\n");
    }
}
"#;

const RECORD: &str = r#"module s1;
struct Point { pub x: integer, pub y: integer }
entity Top { clk: Bit in, inp: Point in, out_p: Point out }
impl Top {
    let p: Point;
    process {
        if clk.rising() {
            p = inp;
            out_p = p;
        }
    }
}
#[test] entity TB {}
impl TB {
    let clk: Bit;
    let inp: Point = { .x = 3, .y = 4 };
    let out_p: Point;
    let dut: Top = { .clk = clk, .inp = inp, .out_p = out_p };
    run: process {
        clk = '0'; await 1ns; clk = '1'; await 1ns; clk = '0'; await 1ns; clk = '1'; await 1ns;
        assert!(out_p.x == 3 and out_p.y == 4, "out {} {}", out_p.x, out_p.y);
        print!("done\n");
    }
}
"#;
