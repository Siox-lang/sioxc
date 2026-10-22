//! Antigravity's issues #243–#249. Each case is named after its GitHub issue.

use std::process::Command;

/// Build `source` as a test binary and run it, returning everything printed.
fn run(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_issues_243_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let binary = dir.join(name);
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
            "-o",
        ])
        .arg(&binary)
        .arg(&file)
        .output()
        .unwrap();
    let mut text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        let ran = Command::new(&binary).output().unwrap();
        text += &String::from_utf8_lossy(&ran.stdout);
        text += &String::from_utf8_lossy(&ran.stderr);
    }
    text
}

#[test]
fn constants_and_vectors_lower() {
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "243",
            r#"module m;
use std::logic::Logic;
enum Color { Red, Green, Blue }
const S: Char[3] = ['a', 'b', 'c'];
const T: Char[5] = "hello";
const B: Bool[2] = [true, false];
const C: Color[2] = [Color::Blue, Color::Red];
const L: Logic[2] = ['1', 'Z'];
entity Top { s: integer out, t: integer out, b: Bool out, c: Color out, l: Logic out, }
impl Top { s = integer(S[1]); t = integer(T[4]); b = B[0]; c = C[0]; l = L[1]; }
#[test] entity X {}
impl X {
    let s: integer; let t: integer; let b: Bool; let c: Color; let l: Logic;
    let dut: Top = { .s = s, .t = t, .b = b, .c = c, .l = l };
    process { await 1ns; print!("{} {} {} {} {}\n", s, t, b, c == Color::Blue, l); }
}
"#,
            &[r#"98 111 true true 'Z'"#],
        ),
        (
            "245",
            r#"module test_fixed_neg_idx;
use std::fixed::ufixed;
use std::logic::Logic;

#[test]
entity Top {}

impl Top {
    p: process {
        let x: ufixed<8, 4> = ufixed<8, 4>(1.5);
        let b: Logic = x[-1]; print!("{}\n", b);
    }
}
"#,
            &[r#"'1'"#],
        ),
        (
            "247",
            r#"module m;
use std::sim::time;
pub struct Bytes(integer);
impl Suffix<"kB", integer> for Bytes { fn suffix(v: integer) -> Bytes { return Bytes(v * 1024); } }
const MEM: Bytes = 4kB;
const HALF: time = 1.5ns;
entity Top { b: integer out, t: time out, }
impl Top { b = integer(MEM); t = HALF; }
#[test] entity T {}
impl T {
    let b: integer; let t: time;
    let dut: Top = { .b = b, .t = t };
    process {
        let local: time = HALF;
        await 1ns;
        assert!(t == local, "same");
        print!("{} {}\n", b, integer(MEM));
        await local;
        print!("done\n");
    }
}
"#,
            &[r#"4096 4096"#, r#"done"#],
        ),
        (
            "249",
            r#"module m;
use std::logic::Logic;
use std::bits::unsigned;
entity Top { c: Logic[2] in, d: unsigned[2] in, x: Logic out, y: Logic out, p: Logic out, q: Logic out, }
impl Top { process { {x, y} = c; {p, q} = d; } }
#[test] entity T {}
impl T {
    let c: Logic[2]; let d: unsigned[2]; let x: Logic; let y: Logic; let p: Logic; let q: Logic;
    let dut: Top = { .c = c, .d = d, .x = x, .y = y, .p = p, .q = q };
    process {
        c = "10"; d = 2;
        await 1ns;
        print!("{} {} {} {} | {} {}\n", x, y, p, q, c[1], c[0]);
    }
}
"#,
            &[r#"'0' '1' '1' '0'"#],
        ),
    ];
    for (issue, source, printed) in cases {
        let out = run(&format!("run_{issue}"), source);
        assert!(
            out.contains("passed;") && !out.contains("FAILED"),
            "#{issue}: {out}"
        );
        for line in *printed {
            assert!(out.contains(line), "#{issue}: missing `{line}`: {out}");
        }
    }
}

#[test]
fn widths_and_histories_are_checked() {
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "244_argument",
            r#"module test_repro_word_width;
use std::logic::Logic;

pub struct Word(Logic[15..8]); // 8-bit vector

fn takes_word(w: Word) -> Logic {
    return w[10];
}

entity Top {
    inp: Logic[32] in,
    out_sig: Logic out,
}

impl Top {
    p: process {
        out_sig = takes_word(inp); // Passing 32 bits to 8-bit Word!
    }
}
"#,
            &[r#"cannot pass Logic[32] to a Word[8] parameter"#],
        ),
        (
            "244_init",
            r#"module test_tb_word_width;
use std::logic::Logic;

pub struct Word(Logic[15..8]);

#[test]
entity Top {}

impl Top {
    p: process {
        let b: Word;
        let out_sig: Logic[32] = b; // 8-bit assigned to 32-bit!
    }
}
"#,
            &[r#"cannot initialize Logic[32] with Word[8]"#],
        ),
        (
            "244_index",
            r#"module test_repro_word_bounds;
use std::logic::Logic;

pub struct Word(Logic[15..8]);

#[test]
entity Top {}

impl Top {
    p: process {
        let b: Word;
        let bit: Logic = b[99999]; // Index 99999 is wildly out of bounds
    }
}
"#,
            &[r#"bit 99999 is outside `8..15`"#],
        ),
        (
            "248",
            r#"module test_expr_event_tb;
use std::logic::Logic;

#[test]
entity Top {}

impl Top {
    p: process {
        let a: Logic = '1';
        let b: Logic = '0';
        if (a and b)'event {
            print!("event\n");
        }
    }
}
"#,
            &[r#"reads a signal's history"#],
        ),
    ];
    for (issue, source, messages) in cases {
        let out = run(&format!("error_{issue}"), source);
        for message in *messages {
            assert!(
                out.contains(message),
                "#{issue}: missing `{message}`: {out}"
            );
        }
    }
}
