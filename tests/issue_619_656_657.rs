//! Regressions for sioxc #619 and #657: constants keep their declared
//! shape. A constant table with nonzero or descending labels
//! (`unsigned[8][1..4]`, `[3..0]`) is indexed by its labels, statically and
//! at run time, in hardware and in a testbench; it was read as `0..len`,
//! off by its low label or reversed (#619). And an impl's own `const` is a
//! constant to the width check: `let k: unsigned[4] = LIMIT` with `LIMIT =
//! 20` is reported, where it truncated (#657), and so is one compared
//! with a narrower vector (`x == LIMIT`, 600 against `unsigned[8]`, #656).

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i619_{name}_{}", std::process::id()));
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

#[cfg(feature = "llvm")]
#[test]
fn constant_tables_index_by_their_labels() {
    let (ok, text, binary) = compile("tables", TABLES);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success() && stdout.contains("done"), "{stdout}");
}

#[test]
fn an_impl_constant_is_width_checked() {
    let (ok, text, _) = compile("limit", LIMIT);
    assert!(!ok, "{text}");
    assert!(
        text.contains("`20` does not fit in a 4-bit vector"),
        "{text}"
    );
}

#[test]
fn a_named_constant_is_checked_in_a_comparison() {
    let (ok, text, _) = compile("compare", COMPARE);
    assert!(!ok, "{text}");
    assert!(
        text.contains("`600` does not fit in a 8-bit vector"),
        "{text}"
    );
}

const COMPARE: &str = r#"module test_limit;

entity Top {
    x: unsigned[8] in,
    y: unsigned[1] out,
}
impl Top {
    const LIMIT: integer = 600;
    // Compiles with 0 errors; silently masks 600 to 88 in hardware!
    y = if x == LIMIT { 1 } else { 0 };
}

#[test]
entity Test {}
impl Test {
    let x: unsigned[8];
    let y: unsigned[1];
    let dut: Top = { .x = x, .y = y };

    run: process {
        x = 88;
        await 10ns;
        // Prints: when x=88, y=1
        print!("when x=88, y={}\\n", y);
        assert!(y == 0); // Fails assertion: y is 1!
    }
}
"#;

const TABLES: &str = r#"module p619;

use std::bits::unsigned;

const UP: unsigned[8][1..4] = [10, 20, 30, 40];
const DOWN: unsigned[8][3..0] = [10, 20, 30, 40];

entity Top {
    sel: integer in,
    a: unsigned[8] out,
    b: unsigned[8] out,
    c: unsigned[8] out,
    d: unsigned[8] out,
}

impl Top {
    a = UP[1];
    b = DOWN[3];
    c = UP[sel];
    d = DOWN[sel];
}

#[test]
entity TB {}
impl TB {
    let sel: integer = 2;
    let a: unsigned[8];
    let b: unsigned[8];
    let c: unsigned[8];
    let d: unsigned[8];
    let dut: Top = { .sel = sel, .a = a, .b = b, .c = c, .d = d };
    process {
        sel = 2;
        await 1ns;
        assert!(a == 10 and b == 10, "static {} {}", a, b);
        assert!(c == 20 and d == 20, "dynamic {} {}", c, d);
        assert!(UP[4] == 40 and DOWN[0] == 40, "testbench {} {}", UP[4], DOWN[0]);
        print!("done\n");
    }
}
"#;

const LIMIT: &str = r#"module test_let_const;

#[test]
entity Test {}
impl Test {
    const LIMIT: integer = 20;
    run: process {
        // Expected: error[E-P003]: `20` does not fit in a 4-bit vector
        // Actual: Compiles with 0 errors! Silently truncates 20 to 4 at runtime!
        let k: unsigned[4] = LIMIT;
        print!("k is: {}\\n", k); // Prints 4
        assert!(k == 4); // Passes!
    }
}
"#;
