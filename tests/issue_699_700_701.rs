//! Regression for sioxc #699, #700 and #701: aggregate constants fold
//! whole. A spread from a struct constant (`{ ..BASE, .a = 99 }`) kept its
//! base's fields in a constant and a signal initializer (#699); a table of
//! structs that failed part-way no longer keeps the leaves it had, which
//! blocked the general path (#700); and a leaf bound through its layout
//! (canonical, not a bare constant) folds and seeds (#701).

use std::process::Command;

fn runs(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_i699_{name}_{}", std::process::id()));
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
    let stdout = String::from_utf8_lossy(&run.stdout).to_string();
    assert!(run.status.success(), "{name}: {stdout}");
    stdout
}

#[cfg(feature = "llvm")]
#[test]
fn a_spread_from_a_struct_constant_keeps_its_fields() {
    let out = runs("spread", SPREAD);
    assert!(out.contains("sig: a=99 b=20 | const: a=99 b=20"), "{out}");
}

#[cfg(feature = "llvm")]
#[test]
fn a_table_of_structs_folds_whole_or_not_at_all() {
    let out = runs("partial", PARTIAL);
    assert!(out.contains("mix[1].b=20 | nest[0].inner.a=7"), "{out}");
}

#[cfg(feature = "llvm")]
#[test]
fn canonical_constant_leaves_fold_and_seed() {
    let out = runs("canonical", CANONICAL);
    assert!(out.contains("s=[11, 22] r=[30, 40]"), "{out}");
}

const SPREAD: &str = r#"module test_const_spread;

use std::bits::unsigned;

pub struct Pair {
    pub a: unsigned[8],
    pub b: unsigned[8],
}

const BASE: Pair = { .a = 10, .b = 20 };
const SPREAD_CONST: Pair = { ..BASE, .a = 99 };

entity TopSpread {
    out_sig_a: unsigned[8] out,
    out_sig_b: unsigned[8] out,
    out_const_a: unsigned[8] out,
    out_const_b: unsigned[8] out,
}
impl TopSpread {
    let p_sig: Pair = { ..BASE, .a = 99 };
    let p_const: Pair = SPREAD_CONST;
    out_sig_a = p_sig.a;
    out_sig_b = p_sig.b;
    out_const_a = p_const.a;
    out_const_b = p_const.b;
}

#[test]
entity Tb {}
impl Tb {
    let sa: unsigned[8]; let sb: unsigned[8];
    let ca: unsigned[8]; let cb: unsigned[8];
    let d: TopSpread = { .out_sig_a = sa, .out_sig_b = sb, .out_const_a = ca, .out_const_b = cb };
    process {
        await 1ns;
        print!("sig: a={} b={} | const: a={} b={}\n", sa, sb, ca, cb);
    }
}
"#;

const PARTIAL: &str = r#"module test_partial_const_arr;

use std::bits::unsigned;

pub struct Pair {
    pub a: unsigned[8],
    pub b: unsigned[8],
}

pub struct Outer {
    pub tag: unsigned[8],
    pub inner: Pair,
}

const BASE: Pair = { .a = 10, .b = 20 };
const MIXED_ARR: Pair[2] = [{ .a = 1, .b = 2 }, BASE];
const NESTED_ARR: Outer[1] = [{ .tag = 5, .inner = { .a = 7, .b = 8 } }];

entity Top {
    out_mix_b: unsigned[8] out,
    out_nest_a: unsigned[8] out,
}
impl Top {
    let m_arr: Pair[2] = MIXED_ARR;
    let n_arr: Outer[1] = NESTED_ARR;
    out_mix_b = m_arr[1].b;
    out_nest_a = n_arr[0].inner.a;
}

#[test]
entity Tb {}
impl Tb {
    let mb: unsigned[8]; let na: unsigned[8];
    let d: Top = { .out_mix_b = mb, .out_nest_a = na };
    process {
        await 1ns;
        print!("mix[1].b={} | nest[0].inner.a={}\n", mb, na);
    }
}
"#;

const CANONICAL: &str = r#"module test_fold_agg_canonical;

use std::bits::unsigned;

pub struct Pair {
    pub a: unsigned[8],
    pub b: unsigned[8],
}

fn make_arr() -> unsigned[8][2] {
    return [11, 22];
}

const ARR_FN: unsigned[8][2] = make_arr();
const M: unsigned[8][2][2] = [[10, 20], [30, 40]];
const ROW1: unsigned[8][2] = M[1];

entity Top {
    o0: unsigned[8] out,
    o1: unsigned[8] out,
    r0: unsigned[8] out,
    r1: unsigned[8] out,
}
impl Top {
    let s: unsigned[8][2] = ARR_FN;
    let r: unsigned[8][2] = ROW1;
    o0 = s[0];
    o1 = s[1];
    r0 = r[0];
    r1 = r[1];
}

#[test]
entity Tb {}
impl Tb {
    let o0: unsigned[8]; let o1: unsigned[8];
    let r0: unsigned[8]; let r1: unsigned[8];
    let d: Top = { .o0 = o0, .o1 = o1, .r0 = r0, .r1 = r1 };
    process {
        await 1ns;
        print!("s=[{}, {}] r=[{}, {}]\n", o0, o1, r0, r1);
    }
}
"#;
