//! Regression for sioxc #691 and #692: a clocked assignment is written as
//! the combinational one is. Under `if clk.rising()`, `{x, y} = c` sliced
//! 4-bit `Logic` parts out of a 2-element array (#691), `col = 'g'` and
//! `ch = '1'` skipped the enum and `Char` coercion, and `rng = 10..25`
//! could not be assigned (#692).

use std::process::Command;

fn runs(name: &str, source: &str, expect: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i69x_{name}_{}", std::process::id()));
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
        run.status.success() && stdout.contains(expect),
        "{name}: {stdout}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn clocked_assignments_match_combinational_ones() {
    runs("concat", CONCAT, "clocked: '0' '1' '1' '0'");
    runs("coerce", COERCE, "clocked: col=true ch=1 rng=10..25");
}

const CONCAT: &str = r#"module test_concat_clocked;

use std::logic::Logic;
use std::bits::unsigned;

entity TopComb {
    c: Logic[2] in,
    d: unsigned[2] in,
    x: Logic out,
    y: Logic out,
    p: Logic out,
    q: Logic out,
}
impl TopComb {
    process {
        {x, y} = c;
        {p, q} = d;
    }
}

entity TopClocked {
    clk: Bit in,
    c: Logic[2] in,
    d: unsigned[2] in,
    x: Logic out,
    y: Logic out,
    p: Logic out,
    q: Logic out,
}
impl TopClocked {
    process {
        if clk.rising() {
            {x, y} = c;
            {p, q} = d;
        }
    }
}

#[test]
entity T {}
impl T {
    let clk: Bit = '0';
    let c: Logic[2];
    let d: unsigned[2];
    let x1: Logic; let y1: Logic; let p1: Logic; let q1: Logic;
    let x2: Logic; let y2: Logic; let p2: Logic; let q2: Logic;
    let dut1: TopComb = { .c = c, .d = d, .x = x1, .y = y1, .p = p1, .q = q1 };
    let dut2: TopClocked = { .clk = clk, .c = c, .d = d, .x = x2, .y = y2, .p = p2, .q = q2 };
    process {
        c = "10";
        d = 2;
        await 1ns;
        clk = '1';
        await 1ns;
        print!("comb:    {} {} {} {}\n", x1, y1, p1, q1);
        print!("clocked: {} {} {} {}\n", x2, y2, p2, q2);
        assert!(x2 == '0' and y2 == '1' and p2 == '1' and q2 == '0');
    }
}
"#;

const COERCE: &str = r#"module test_clocked_coerce;

enum Color { 'r', 'g', 'b' }

entity TopComb {
    col: Color out,
    ch: Char out,
    rng: Range out,
}
impl TopComb {
    process {
        col = 'g';
        ch = '1';
        rng = 10..25;
    }
}

entity TopClocked {
    clk: Bit in,
    col: Color out,
    ch: Char out,
    rng: Range out,
}
impl TopClocked {
    process {
        if clk.rising() {
            col = 'g';
            ch = '1';
            rng = 10..25;
        }
    }
}

#[test]
entity Tb {}
impl Tb {
    let clk: Bit = '0';
    let col1: Color; let ch1: Char; let rng1: Range;
    let col2: Color; let ch2: Char; let rng2: Range;
    let d1: TopComb = { .col = col1, .ch = ch1, .rng = rng1 };
    let d2: TopClocked = { .clk = clk, .col = col2, .ch = ch2, .rng = rng2 };
    process {
        await 1ns;
        clk = '1';
        await 1ns;
        print!("comb:    col={} ch={} rng={}..{}\n", col1 == 'g', ch1, rng1.left, rng1.right);
        print!("clocked: col={} ch={} rng={}..{}\n", col2 == 'g', ch2, rng2.left, rng2.right);
        assert!(col2 == 'g' and ch2 == '1' and rng2.left == 10 and rng2.right == 25);
    }
}
"#;
