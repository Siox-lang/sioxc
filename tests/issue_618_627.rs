//! Regressions for sioxc #618 and #627: signals initialized from aggregate
//! constants. A table of rows or of structs (`MATRIX`, `TABLE: Entry[2]`)
//! seeds each leaf (#618), and a struct constant with a nested struct
//! field (`.inner = K_INNER`) folds that field's leaves to words (#627).
//! Both were "the initializer ... is not a constant".

use std::process::Command;

fn runs(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i618_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
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
    assert!(
        build.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && stdout.contains("done"),
        "{name}: {stdout}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn tables_of_rows_and_structs_initialize_signals() {
    runs("tables", TABLES);
}

#[cfg(feature = "llvm")]
#[test]
fn a_nested_struct_constant_initializes_a_signal() {
    runs("nested", NESTED);
}

const TABLES: &str = r#"module p618;

use std::bits::unsigned;

struct Entry {
    pub val: unsigned[8],
}

const MATRIX: unsigned[8][2][2] = [
    [10, 20],
    [30, 40],
];

const TABLE: Entry[2] = [
    Entry { .val = 42 },
    Entry { .val = 99 },
];

entity Top {
    m00: unsigned[8] out,
    m11: unsigned[8] out,
    t0: unsigned[8] out,
    t1: unsigned[8] out,
}

impl Top {
    let m: unsigned[8][2][2] = MATRIX;
    let t: Entry[2] = TABLE;
    m00 = m[0][0];
    m11 = m[1][1];
    t0 = t[0].val;
    t1 = t[1].val;
}

#[test]
entity TB {}
impl TB {
    let m00: unsigned[8];
    let m11: unsigned[8];
    let t0: unsigned[8];
    let t1: unsigned[8];
    let dut: Top = { .m00 = m00, .m11 = m11, .t0 = t0, .t1 = t1 };
    run: process {
        await 1ns;
        assert!(m00 == 10 and m11 == 40 and t0 == 42 and t1 == 99, "m {} {} t {} {}", m00, m11, t0, t1);
        print!("done\n");
    }
}
"#;

const NESTED: &str = r#"module p627;

pub struct Inner {
    pub a: integer,
}

pub struct Outer {
    pub inner: Inner,
    pub b: integer,
}

pub const K_INNER: Inner = Inner { .a = 42 };
pub const K_OUTER: Outer = Outer { .inner = K_INNER, .b = 7 };

entity Top {
    x: integer out,
    y: integer out,
}
impl Top {
    let o: Outer = K_OUTER;
    x = o.inner.a;
    y = o.b;
}

#[test] entity T {}
impl T {
    let x: integer;
    let y: integer;
    let dut: Top = { .x = x, .y = y };
    run: process {
        await 1ns;
        assert!(x == 42 and y == 7, "x {} y {}", x, y);
        print!("done\n");
    }
}
"#;
