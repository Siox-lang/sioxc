//! Antigravity's issues #256–#266. Each case is named after its GitHub issue.

use std::process::Command;

/// Build `source` as a test binary and run it, returning everything printed.
fn run(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_issues_256_{}", std::process::id()));
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
fn programs_that_failed_to_lower_now_run() {
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "256_257",
            r#"module m;
entity E { c: Char in, y: integer out, }
impl E { y = match c { 'a' => 1, 'b' => 2, _ => 0, }; }
#[test] entity TB {}
impl TB {
    let c: Char = 'b';
    let y: integer;
    let dut: E = { .c = c, .y = y };
    process {
        let ch: Char = 'a';
        let x: integer = match ch { 'a' => 1, 'b' => 2, _ => 0, };
        let b: Bool = false;
        let z: integer = match b { true => 1, false => 2, };
        await 1ns;
        print!("{} {} {}\n", x, z, y);
    }
}
"#,
            &[r#"1 2 2"#],
        ),
        (
            "258_tuple",
            r#"module test_multi_gen_hw;

fn make_pair<A, B>(a: A, b: B) -> (A, B) {
    return (a, b);
}

entity Top {
    a: integer in,
    b: Bool in,
    r_a: integer out,
    r_b: Bool out,
}
impl Top {
    let p: (integer, Bool);
    p = make_pair(a, b);
    r_a = p.0;
    r_b = p.1;
}
#[test] entity TB {}
impl TB { let a: integer = 9; let b: Bool = true; let r_a: integer; let r_b: Bool; let dut: Top = { .a = a, .b = b, .r_a = r_a, .r_b = r_b }; process { await 1ns; print!("v={} {}\n", r_a, r_b); } }
"#,
            &[r#"v=9 true"#],
        ),
        (
            "258_struct",
            r#"module test_gen_struct_ret_hw;

struct Pair<A, B> {
    pub a: A,
    pub b: B,
}

fn make_pair<A, B>(a: A, b: B) -> Pair<A, B> {
    return Pair { .a = a, .b = b };
}

entity Top {
    a: integer in,
    b: Bool in,
    r_a: integer out,
    r_b: Bool out,
}
impl Top {
    let p: Pair<integer, Bool>;
    p = make_pair(a, b);
    r_a = p.a;
    r_b = p.b;
}
#[test] entity TB {}
impl TB { let a: integer = 9; let b: Bool = true; let r_a: integer; let r_b: Bool; let dut: Top = { .a = a, .b = b, .r_a = r_a, .r_b = r_b }; process { await 1ns; print!("v={} {}\n", r_a, r_b); } }
"#,
            &[r#"v=9 true"#],
        ),
        (
            "260",
            r#"module test_struct;
use std::logic::Logic;

struct Container<T> {
    pub item: T,
}

entity Top {
    a: (Logic, Logic) in,
    y: Logic out,
}
impl Top {
    // Positional Container<(Logic, Logic)> compiles; named T = (Logic, Logic) fails:
    let c: Container<T = (Logic, Logic)>;
    c.item = a;
    y = c.item.0;
}
#[test] entity TB {}
impl TB { let a: (Logic, Logic) = ('Z', '1'); let y: Logic; let dut: Top = { .a = a, .y = y }; process { await 1ns; print!("v={}\n", y); } }
"#,
            &[r#"v='Z'"#],
        ),
        (
            "261",
            r#"module m;
use std::bits::unsigned;
const TABLE: unsigned[8][4] = [10, 20, 30, 40];
const COPY: unsigned[8][4] = TABLE;
const SUB: unsigned[8][2] = TABLE[1..2];
const X: unsigned[8] = TABLE[1];
entity Top { y: unsigned[8][2] out, y1: unsigned[8][4] out, y2: unsigned[8][2] out, y3: unsigned[8] out, }
impl Top { y = TABLE[1..2]; y1 = COPY; y2 = SUB; y3 = X; }
#[test] entity T {}
impl T {
    let y: unsigned[8][2]; let y1: unsigned[8][4]; let y2: unsigned[8][2]; let y3: unsigned[8];
    let dut: Top = { .y = y, .y1 = y1, .y2 = y2, .y3 = y3 };
    process { await 1ns; print!("{} {} {} {} {} {}\n", y[0], y[1], y1[3], y2[0], y2[1], y3); }
}
"#,
            &[r#"20 30 40 20 30 20"#],
        ),
        (
            "262",
            r#"module m;
use std::bits::unsigned;
entity Top { arr: unsigned[8][4] in, y: unsigned[8] out, z: unsigned[8][2] out, }
impl Top { y = arr[1..3][0]; z = arr[1..3][1..2]; }
#[test] entity T {}
impl T {
    let arr: unsigned[8][4] = [10, 20, 30, 40];
    let y: unsigned[8]; let z: unsigned[8][2];
    let dut: Top = { .arr = arr, .y = y, .z = z };
    process {
        await 1ns;
        let a: unsigned[8][4] = [10, 20, 30, 40];
        a[1..3][0] = 99;
        print!("{} {} {} {} {}\n", y, z[0], z[1], a[1], a[3..1][0]);
    }
}
"#,
            &[r#"20 30 40 99 40"#],
        ),
        (
            "263",
            r#"module m;
use std::logic::Logic;
entity Top { a: Logic in, b: Logic in, y1: Logic out, }
impl Top { (y1, _) = (a, b); }
#[test] entity T {}
impl T {
    let a: Logic = '1'; let b: Logic = '0'; let y1: Logic;
    let dut: Top = { .a = a, .b = b, .y1 = y1 };
    process {
        let (p: Logic, _) = ('Z', '0');
        let x: integer = 1; let y: integer = 2;
        (x, y) = (y, x);
        await 1ns;
        print!("{} {} {} {}\n", y1, p, x, y);
    }
}
"#,
            &[r#"'1' 'Z' 2 1"#],
        ),
        (
            "264",
            r#"module test;

struct Point {
    pub x: integer,
}

impl Point {
    pub fn new(x: integer) -> Point {
        return Point { .x = x };
    }
}

entity Top {
    y: integer out,
}
impl Top {
    let p: Point = Point::new(10);
    y = p.x;
}
#[test] entity TB {}
impl TB { let y: integer; let dut: Top = { .y = y }; process { await 1ns; print!("v={}\n", y); } }
"#,
            &[r#"v=10"#],
        ),
        (
            "265",
            r#"module test;

enum State {
    Idle,
    Run,
}

impl State {
    pub fn is_idle(self) -> Bool {
        return self == State::Idle;
    }
    pub fn default_state() -> Self {
        return State::Idle;
    }
}

entity Top {
    s: State in,
    idle: Bool out,
    def: State out,
}
impl Top {
    idle = s.is_idle();
    def = State::default_state();
}
#[test] entity TB {}
impl TB { let s: State = State::Run; let idle: Bool; let def: State; let dut: Top = { .s = s, .idle = idle, .def = def }; process { await 1ns; print!("v={} {}\n", idle, def == State::Idle); } }
"#,
            &[r#"v=false true"#],
        ),
        (
            "266",
            r#"module test;

struct Point {
    pub x: integer,
}

impl Point {
    pub fn create(val: integer) -> Self {
        return Self { .x = val };
    }
}

entity Top {
    x: integer in,
    y: Point out,
}
impl Top {
    y = Point::create(x);
}
#[test] entity TB {}
impl TB { let x: integer = 5; let y: Point; let dut: Top = { .x = x, .y = y }; process { await 1ns; print!("v={}\n", y.x); } }
"#,
            &[r#"v=5"#],
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
fn fields_of_fieldless_types_are_diagnosed() {
    let cases: &[(&str, &str, &[&str])] = &[(
        "259",
        r#"module test_hw;
use std::logic::Logic;

enum Color { Red, Green }

entity Top {
    y: Logic out,
}
impl Top {
    let a: integer;
    let b: real;
    let c: Char;
    let d: Logic;
    let e: Color;
    let g: Logic[4];

    let v1: integer;
    let v2: real;
    let v3: Char;
    let v4: Logic;
    let v5: Color;
    let v7: Logic;

    v1 = a.field1;
    v2 = b.field2;
    v3 = c.field3;
    v4 = d.field4;
    v5 = e.field5;
    v7 = g.field7;

    y = '0';
}
"#,
        &[
            r#"`integer` has no field `field1`"#,
            r#"`Color` has no field `field5`"#,
            r#"`Logic[4]` has no field `field7`"#,
        ],
    )];
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
