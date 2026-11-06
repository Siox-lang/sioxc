//! Regressions for sioxc #387: the hardware function inliner agrees with
//! the testbench on `match` arms that write locals, a branch-local `let`
//! shadowing an outer one, a method or void free procedure writing a
//! parameter, and a `match` inside a loop. Each program prints the hardware
//! output beside the same function evaluated in the testbench.

use std::process::Command;

fn agrees(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i387_{name}_{}", std::process::id()));
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
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let text = String::from_utf8_lossy(&run.stdout).to_string();
    assert!(run.status.success(), "{text}");
    let mut checked = 0;
    for line in text.lines() {
        let Some(rest) = line.split("y=").nth(1) else {
            continue;
        };
        let (hardware, expected) = rest.split_once(" expected=").unwrap();
        assert_eq!(hardware.trim(), expected.trim(), "{text}");
        checked += 1;
    }
    assert!(checked > 0, "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn match_arms_writing_locals() {
    agrees("match_arms", MATCH_ARMS);
}

#[cfg(feature = "llvm")]
#[test]
fn a_branch_let_does_not_leak() {
    agrees("shadow", SHADOW);
}

#[cfg(feature = "llvm")]
#[test]
fn a_method_writes_its_parameter() {
    agrees("method_param", METHOD_PARAM);
}

#[cfg(feature = "llvm")]
#[test]
fn a_match_in_a_loop_and_a_void_procedure() {
    agrees("loop_match", LOOP_MATCH);
}

const MATCH_ARMS: &str = r#"module tb;
use std::sim::*;

fn decode_scalar(sel: unsigned[2]) -> unsigned[8] {
    let m: unsigned[8] = 99;
    match sel {
        0 => { m = 10; }
        1 => { m = 20; }
        2 => { m = 30; }
        _ => { m = 40; }
    }
    return m;
}

entity Top {
    sel: unsigned[2] in,
    y: unsigned[8] out,
}

impl Top {
    process {
        y = decode_scalar(sel);
    }
}

#[test]
entity Tb {}

impl Tb {
    let sel: unsigned[2] = 0;
    let y: unsigned[8];
    let dut: Top = { .sel = sel, .y = y };

    process {
        for i in 0..3 {
            sel = unsigned[2](i);
            await 1ns;
            print!("sel={} y={} expected={}", sel, y, decode_scalar(sel));
        }
        stop();
    }
}
"#;

const SHADOW: &str = r#"module tb;
use std::sim::*;

fn shadow_if(cond: Bool, early: Bool) -> unsigned[8] {
    let x: unsigned[8] = 99;
    if cond {
        if early {
            return 0;
        }
        let x: unsigned[8] = 10;
    }
    return x;
}

entity Top {
    cond: Bool in,
    early: Bool in,
    y: unsigned[8] out,
}

impl Top {
    process {
        y = shadow_if(cond, early);
    }
}

#[test]
entity Tb {}

impl Tb {
    let cond: Bool = true;
    let early: Bool = false;
    let y: unsigned[8];
    let dut: Top = { .cond = cond, .early = early, .y = y };

    process {
        await 1ns;
        print!("y={} expected={}", y, shadow_if(true, false));
        stop();
    }
}
"#;

const METHOD_PARAM: &str = r#"module tb;
use std::sim::*;

struct Acc {
    pub val: unsigned[8],
}

impl Acc {
    pub fn add_into(dst: unsigned[8]) {
        dst = dst + self.val;
    }
}

fn compute_with_method(a: unsigned[8]) -> unsigned[8] {
    let acc: Acc = { .val = a };
    let out_val: unsigned[8] = 10;
    acc.add_into(out_val);
    return out_val;
}

entity Top {
    a: unsigned[8] in,
    y: unsigned[8] out,
}

impl Top {
    process {
        y = compute_with_method(a);
    }
}

#[test]
entity Tb {}

impl Tb {
    let a: unsigned[8] = 5;
    let y: unsigned[8];
    let dut: Top = { .a = a, .y = y };

    process {
        await 1ns;
        print!("y={} expected={}", y, compute_with_method(5));
        stop();
    }
}
"#;

const LOOP_MATCH: &str = r#"module tb;
use std::sim::*;

fn bump(x: unsigned[8]) {
    x = x + 1;
}

fn count_ones(v: unsigned[4]) -> unsigned[8] {
    let n: unsigned[8] = 0;
    for i in 0..3 {
        match v[i] {
            '1' => { n = n + 1; }
            _ => {}
        }
    }
    bump(n);
    return n;
}

entity Top {
    v: unsigned[4] in,
    y: unsigned[8] out,
}

impl Top {
    process {
        y = count_ones(v);
    }
}

#[test]
entity Tb {}

impl Tb {
    let v: unsigned[4] = "1011";
    let y: unsigned[8];
    let dut: Top = { .v = v, .y = y };

    process {
        await 1ns;
        print!("y={} expected={}", y, count_ones("1011"));
        assert!(y == 4, "hardware count");
        stop();
    }
}
"#;
