//! Regression for sioxc #708: an `impl Default` reaches every place its type
//! is declared without a value. A hardware newtype signal (`let sc: Score;`)
//! powered on at 0; a defaulted struct inside an array (`Pt[2]`) or an outer
//! struct (`let o: Outer;`, `Outer()`) was zeroed in hardware and testbench.

use std::process::Command;

fn runs(name: &str, source: &str, expect: &[&str]) {
    let dir = std::env::temp_dir().join(format!("siox_i708_{name}_{}", std::process::id()));
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
    for line in expect {
        assert!(
            run.status.success() && stdout.contains(line),
            "{name}: {stdout}"
        );
    }
}

#[cfg(feature = "llvm")]
#[test]
fn nested_defaults_reach_arrays_and_outer_structs() {
    runs(
        "nested",
        NESTED,
        &[
            "hw: arr0_x=7 outer_px=7 score=42",
            "tb: ps[0].x=7 o.p.x=7 o2.p.x=7 sc=42",
        ],
    );
}

#[cfg(feature = "llvm")]
#[test]
fn newtype_signals_take_their_default() {
    let dir = std::env::temp_dir().join(format!("siox_i708_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("default.siox");
    std::fs::write(&file, SOURCE).unwrap();
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
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && stdout.contains("hw=42 tb=42"),
        "{stdout}"
    );
}

const SOURCE: &str = r#"module p708;

struct Score(unsigned[8]);

impl Default for Score {
    static fn default() -> Score {
        return Score(42);
    }
}

entity Hw {
    score_out: Score out,
}

impl Hw {
    let sc: Score;
    score_out = sc;
}

#[test]
entity Tb {}

impl Tb {
    let hw_score: Score;
    let dut: Hw = { .score_out = hw_score };

    process {
        await 1ns;
        let tb_sc: Score;
        print!("hw={} tb={}\n", unsigned[8](hw_score), unsigned[8](tb_sc));
        assert!(unsigned[8](hw_score) == 42 and unsigned[8](tb_sc) == 42);
    }
}
"#;

const NESTED: &str = r#"module repro;

struct Pt {
    pub x: unsigned[8],
    pub y: unsigned[8],
}

impl Default for Pt {
    static fn default() -> Pt {
        return Pt { .x = 7, .y = 9 };
    }
}

struct Score(unsigned[8]);

impl Default for Score {
    static fn default() -> Score {
        return Score(42);
    }
}

struct Outer {
    pub p: Pt,
    pub s: Score,
}

entity Hw {
    pt_arr0_x: unsigned[8] out,
    outer_px: unsigned[8] out,
    score_out: Score out,
}

impl Hw {
    let ps: Pt[2];
    let o: Outer;
    let sc: Score;

    pt_arr0_x = ps[0].x;
    outer_px = o.p.x;
    score_out = sc;
}

#[test]
entity Tb {}

impl Tb {
    let hw_arr0_x: unsigned[8];
    let hw_outer_px: unsigned[8];
    let hw_score: Score;

    let dut: Hw = {
        .pt_arr0_x = hw_arr0_x,
        .outer_px = hw_outer_px,
        .score_out = hw_score,
    };

    process {
        await 1ns;
        let tb_ps: Pt[2];
        let tb_o: Outer;
        let tb_o2: Outer = Outer();
        let tb_sc: Score;

        print!("hw: arr0_x={} outer_px={} score={}\n",
            hw_arr0_x, hw_outer_px, unsigned[8](hw_score));
        print!("tb: ps[0].x={} o.p.x={} o2.p.x={} sc={}\n",
            tb_ps[0].x, tb_o.p.x, tb_o2.p.x, unsigned[8](tb_sc));
    }
}
"#;
