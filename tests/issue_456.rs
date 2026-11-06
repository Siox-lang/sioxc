//! Regression for sioxc #456: a hardware function writing a part of an
//! aggregate local below its first level (`pts[0].x`, `s.arr[0]`,
//! `m[0][1]`, a whole `pts[0]`) or at a run-time index (`arr[idx]`) inlines
//! with the values written.

use std::process::Command;

fn compile(name: &str, source: &str, test: bool) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i456_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
    command.args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")]);
    if test {
        command.arg("--test").arg("-o").arg(dir.join("t.bin"));
    } else {
        command.args(["--emit", "ir"]);
    }
    let out = command.arg(&file).output().unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text, dir.join("t.bin"))
}

#[test]
fn the_issue_design_lowers() {
    let (ok, text, _) = compile("issue", ISSUE, false);
    assert!(ok, "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn nested_and_runtime_writes_keep_their_values() {
    let (ok, text, binary) = compile("values", VALUES, true);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const ISSUE: &str = r#"module test_inlined_aggregate_mutation;

struct Point {
    pub x: integer,
}

struct Result {
    pub arr: integer[2],
}

// Case 1: Array-of-struct field mutation
fn test_array_of_struct_field() -> integer {
    let pts: Point[2] = [Point { .x = 10 }, Point { .x = 20 }];
    pts[0].x = 99;
    return pts[0].x;
}

// Case 2: Struct array-field element mutation
fn test_struct_array_element() -> integer {
    let s: Result = Result { .arr = [10, 20] };
    s.arr[0] = 99;
    return s.arr[0];
}

// Case 3: 2D array element mutation
fn test_2d_array_element() -> integer {
    let m: integer[2][2] = [[10, 20], [30, 40]];
    m[0][1] = 99;
    return m[0][1];
}

// Case 4: Whole struct array element assignment
fn test_array_whole_struct() -> integer {
    let pts: Point[2] = [Point { .x = 10 }, Point { .x = 20 }];
    pts[0] = Point { .x = 99 };
    return pts[0].x;
}

// Case 5: Dynamic array indexing in function
fn test_dyn_index(idx: integer) -> integer {
    let arr: integer[2] = [10, 20];
    arr[idx] = 99;
    return arr[0];
}

// Case 6: Function returning aggregate with internal array mutation
fn test_return_aggregate() -> Result {
    let s: Result = Result { .arr = [10, 20] };
    s.arr[0] = 99;
    return s;
}

entity Dut {
    idx: integer in,
    out1: integer out,
    out2: integer out,
    out3: integer out,
    out4: integer out,
    out5: integer out,
    out6: Result out,
}

impl Dut {
    out1 = test_array_of_struct_field();
    out2 = test_struct_array_element();
    out3 = test_2d_array_element();
    out4 = test_array_whole_struct();
    out5 = test_dyn_index(idx);
    out6 = test_return_aggregate();
}
"#;

const VALUES: &str = r#"module p456;

struct Point {
    pub x: integer,
}

struct Result {
    pub arr: integer[2],
}

fn array_of_struct_field() -> integer {
    let pts: Point[2] = [Point { .x = 10 }, Point { .x = 20 }];
    pts[0].x = 99;
    return pts[0].x + pts[1].x;
}

fn struct_array_element() -> integer {
    let s: Result = Result { .arr = [10, 20] };
    s.arr[0] = 99;
    return s.arr[0] + s.arr[1];
}

fn grid_element() -> integer {
    let m: integer[2][2] = [[10, 20], [30, 40]];
    m[0][1] = 99;
    return m[0][1] + m[1][0];
}

fn whole_element() -> integer {
    let pts: Point[2] = [Point { .x = 10 }, Point { .x = 20 }];
    pts[0] = Point { .x = 99 };
    return pts[0].x + pts[1].x;
}

fn runtime_index(idx: integer) -> integer {
    let arr: integer[2] = [10, 20];
    arr[idx] = 99;
    return arr[0] * 1000 + arr[1];
}

entity Dut {
    idx: integer in,
    o1: integer out,
    o2: integer out,
    o3: integer out,
    o4: integer out,
    o5: integer out,
}

impl Dut {
    o1 = array_of_struct_field();
    o2 = struct_array_element();
    o3 = grid_element();
    o4 = whole_element();
    o5 = runtime_index(idx);
}

#[test]
entity Tb {}

impl Tb {
    let idx: integer = 1;
    let o1: integer;
    let o2: integer;
    let o3: integer;
    let o4: integer;
    let o5: integer;
    let dut: Dut = { .idx = idx, .o1 = o1, .o2 = o2, .o3 = o3, .o4 = o4, .o5 = o5 };

    check: process {
        await 1ns;
        assert!(o1 == 119 and o2 == 119 and o4 == 119, "o1={} o2={} o4={}", o1, o2, o4);
        assert!(o3 == 129, "o3={}", o3);
        assert!(o5 == 10099, "o5={}", o5);
    }
}
"#;
