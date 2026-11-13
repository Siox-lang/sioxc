//! Regression for part of sioxc #719: port connections compare array
//! element shapes, and treat user enums and newtypes as definite types, and
//! an element of an array of structs projects to its struct. These passed
//! elaboration and failed in lowering or were dropped.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn port_connections_compare_elements_enums_and_newtypes() {
    let dir = std::env::temp_dir().join(format!("siox_i719_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("ports.siox");
    std::fs::write(&file, SOURCE).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--top",
            "Hw",
        ])
        .arg(&file)
        .arg("-o")
        .arg(dir.join("t.o"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{text}");
    for wired in [
        "`v4` is `unsigned[4]`",
        "`col` is `Color`",
        "`st` is `State`",
        "`sc` is `Score`",
        "`pts2[0]` is `Other`",
    ] {
        assert!(text.contains(wired), "{wired}: {text}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn a_two_dimensional_element_connection_is_checked() {
    let dir = std::env::temp_dir().join(format!("siox_i719_2d_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("matrix.siox");
    std::fs::write(&file, MATRIX).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--top",
            "Hw",
        ])
        .arg(&file)
        .arg("-o")
        .arg(dir.join("t.o"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("`mat[0][1]` is `unsigned[16]`"), "{text}");
    assert!(!text.contains("ok[1][0]"), "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn a_struct_field_connection_is_checked() {
    let dir = std::env::temp_dir().join(format!("siox_i719_field_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("field.siox");
    std::fs::write(&file, FIELD).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--top",
            "Hw",
        ])
        .arg(&file)
        .arg("-o")
        .arg(dir.join("t.o"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("`pkt.a` is `unsigned[16]`"), "{text}");
    assert!(!text.contains("pkt.b"), "{text}");
}

const SOURCE: &str = r#"module test_elab_conn_bugs;

struct Pt {
    pub x: unsigned[8],
    pub y: unsigned[8],
}

struct Other {
    pub a: unsigned[8],
    pub b: unsigned[8],
}

enum Color { R, G, B }
enum State { Idle, Run, Stop }

struct Score(unsigned[8]);

entity SubArr {
    arr: unsigned[16][4] in,
    y: unsigned[8] out,
}
impl SubArr {
    y = arr[0][0..7];
}

entity SubPt {
    p: Pt in,
    q: unsigned[8] out,
}
impl SubPt {
    q = p.x;
}

entity SubColor {
    c: Color in,
    y: unsigned[8] out,
}
impl SubColor {
    y = 1;
}

entity Hw {
    o1: unsigned[8] out,
    o3: unsigned[8] out,
    o4: unsigned[8] out,
    o5: unsigned[8] out,
    o6: unsigned[8] out,
}

impl Hw {
    // 1. `unsigned[4]` wired to `unsigned[16][4]` port (both have outer `len == Some(4)`)
    let v4: unsigned[4] = 9;
    let u1: SubArr = { .arr = v4, .y = o1 };

    // 2. Enum `Color` wired to struct `Pt` port, and `State` wired to `Color` port
    let col: Color = Color::G;
    let u3: SubPt = { .p = col, .q = o3 };
    let st: State = State::Run;
    let u4: SubColor = { .c = st, .y = o4 };

    // 3. Newtype `Score(unsigned[8])` wired to `Pt` port
    let sc: Score;
    let u5: SubPt = { .p = sc, .q = o5 };

    // 4. Struct array element `pts2[0]` (`Other`) wired to `Pt` port
    let pts2: Other[2];
    let u6: SubPt = { .p = pts2[0], .q = o6 };
}
"#;

const MATRIX: &str = r#"module m719;
use std::bits::unsigned;
entity Sub {
    x: unsigned[8] in,
    y: unsigned[8] out,
}
impl Sub { y = x; }
entity Hw {
    o: unsigned[8] out,
    p: unsigned[8] out,
}
impl Hw {
    let mat: unsigned[16][2][2];
    let ok: unsigned[8][2][2];
    let u: Sub = { .x = mat[0][1], .y = o };
    let v: Sub = { .x = ok[1][0], .y = p };
}
"#;

const FIELD: &str = r#"module f719;
use std::bits::unsigned;
struct Pkt {
    pub a: unsigned[16],
    pub b: unsigned[8],
}
entity Sub {
    x: unsigned[8] in,
    y: unsigned[8] out,
}
impl Sub { y = x; }
entity Hw {
    o: unsigned[8] out,
    p: unsigned[8] out,
}
impl Hw {
    let pkt: Pkt;
    let u: Sub = { .x = pkt.a, .y = o };
    let v: Sub = { .x = pkt.b, .y = p };
}
"#;
