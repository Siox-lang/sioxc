//! Antigravity's issues #218–#229. Each case is named after its GitHub issue.

use std::process::Command;

/// Build `source` (as a test binary when `run`, else `--emit metadata`), run
/// it when it built, and return everything printed.
fn compile(name: &str, source: &str, run: bool) -> String {
    let dir = std::env::temp_dir().join(format!("siox_issues_218_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let binary = dir.join(name);
    let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
    command.args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")]);
    if run {
        command.arg("--test").arg("-o").arg(&binary);
    } else {
        command.args(["--emit", "metadata"]);
    }
    let output = command.arg(&file).output().unwrap();
    let mut text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    if run && output.status.success() {
        let ran = Command::new(&binary).output().unwrap();
        text += &String::from_utf8_lossy(&ran.stdout);
        text += &String::from_utf8_lossy(&ran.stderr);
    }
    text
}

#[test]
fn programs_that_did_not_lower_now_run() {
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "224",
            r#"module m;
#[test] entity T {}
impl T {
    process {
        let arr: integer[4] = [1, 2, 3, 4];
        print!("{} {} {} {} {}\n", [10, 20, 30]'high, [10, 20, 30]'low, arr[2..1]'left, arr[2..1]'right, arr[2..1]'high);
        print!("{} {}\n", arr[1..3]'low, arr[1..3]'length);
    }
}
"#,
            &["2 0 2 1 2", "1 3"],
        ),
        (
            "225",
            r#"module test_const_generic_fn;

fn sum<N: integer>(arr: integer[N]) -> integer {
    let s: integer = 0;
    for i in 0..N - 1 {
        s = s + arr[i];
    }
    return s;
}

#[test]
entity Top {}

impl Top {
    p: process {
        let a: integer[3] = [10, 20, 30];
        let total: integer = sum(a);
        print!("total = {}\n", total);
    }
}
"#,
            &["total = 60"],
        ),
        (
            "226",
            r#"module test_pass_struct;

struct Pair { pub a: integer, pub b: integer }

entity Pass<T> { input: T in, output: T out }
impl<T> Pass<T> { output = input; }

#[test]
entity Top {}

impl Top {
    let inp: Pair = Pair { .a = 10, .b = 20 };
    let outp: Pair;
    let p: Pass<Pair> = {
        .input = inp,
        .output = outp,
    };
    process {
        await 1ns;
        print!("outp.a = {}, outp.b = {}\n", outp.a, outp.b);
        assert!(outp.a == 10 and outp.b == 20);
    }
}
"#,
            &["outp.a = 10, outp.b = 20"],
        ),
        (
            "227",
            r#"module test_box_tuple;

struct Box<T> { pub val: T }

#[test]
entity Top {}

impl Top {
    p: process {
        let b: Box<(integer, integer)> = Box<(integer, integer)> {
            .val = (10, 20),
        };
        print!("{} {}\n", b.val.0, b.val.1);
    }
}
"#,
            &["10 20"],
        ),
        (
            "227_literal",
            r#"module m;
struct Box<T> { pub val: T }
#[test] entity Top {}
impl Top { process { let b: Box<(integer, integer)> = { .val = (10, 20) }; print!("{}\n", b.val.1); } }
"#,
            &["20"],
        ),
        (
            "229",
            r#"module m;
struct S { pub a: integer, pub b: integer }
const SA: S[2] = [S { .a = 1, .b = 5 }, S { .a = 2, .b = 6 }];
entity Top { y: integer out, z: integer out, }
impl Top { y = SA[0].a; z = SA[1].b; }
#[test] entity T {}
impl T { let y: integer; let z: integer; let dut: Top = { .y = y, .z = z }; process { await 1ns; print!("{} {}\n", y, z); } }
"#,
            &["1 6"],
        ),
        (
            "224_ascending",
            r#"module m;
entity E { a: Bool out, d: Bool out, }
impl E { a = [1, 2]'ascending; d = [1, 2, 3, 4][3..1]'ascending; }
#[test] entity T {}
impl T {
    let a: Bool; let d: Bool;
    let dut: E = { .a = a, .d = d };
    process {
        await 1ns;
        let arr: integer[4] = [1, 2, 3, 4];
        assert!(arr[1..2]'ascending, "up");
        assert!(not arr[2..1]'ascending, "down");
        assert!(a and not d, "hardware");
        print!("ok\n");
    }
}
"#,
            &["ok"],
        ),
    ];
    for (issue, source, printed) in cases {
        let out = compile(&format!("run_{issue}"), source, true);
        assert!(out.contains("1 passed"), "#{issue}: {out}");
        for line in *printed {
            assert!(out.contains(line), "#{issue}: missing `{line}`: {out}");
        }
    }
}

#[test]
fn programs_that_passed_wrongly_are_diagnosed() {
    let cases: &[(&str, &str, &str, bool)] = &[
        (
            "218_argument",
            r#"module test_concat_fn_tb;

use std::bits::unsigned;

fn check_bits(x: unsigned[128]) -> unsigned[128] {
    return x + 1;
}

#[test]
entity Top {}

impl Top {
    p: process {
        let a: integer[2] = [1, 2];
        let res: unsigned[128] = check_bits({ a, a });
    }
}
"#,
            r#"a concatenation joins bits, and `a` is a `integer[2]`"#,
            true,
        ),
        (
            "218_hardware",
            r#"module test_concat_fn_hw;

use std::bits::unsigned;

fn check_bits(x: unsigned[128]) -> unsigned[128] {
    return x + 1;
}

entity Top {
    a: integer[2] in,
    y: unsigned[128] out,
}

impl Top {
    y = check_bits({ a, a });
}
"#,
            r#"a concatenation joins bits"#,
            false,
        ),
        (
            "219_read",
            r#"module test_unpacked_dyn_read;

#[test]
entity Top {}

impl Top {
    main: process {
        let arr: integer[4] = [1, 2, 3, 4];
        let lo: integer = 0;
        let hi: integer = 1;
        let sub: integer[2] = arr[hi..lo];
    }
}
"#,
            r#"`arr[hi..lo]` has bounds known only at run time"#,
            true,
        ),
        (
            "219_write",
            r#"module test_unpacked_dyn_assign;

#[test]
entity Top {}

impl Top {
    main: process {
        let arr: integer[4] = [1, 2, 3, 4];
        let lo: integer = 0;
        let hi: integer = 1;
        arr[hi..lo] = [9, 9];
    }
}
"#,
            r#"has bounds known only at run time"#,
            true,
        ),
        (
            "220",
            r#"module test_generic_newtype_tb;

use std::bits::unsigned;

struct Box<T>(T);

#[test]
entity Top {}

impl Top {
    p: process {
        let b: Box<unsigned[8]> = { 42 };
        let v: unsigned[8] = b.0;
        print!("v = {}\n", v);
    }
}
"#,
            r#"newtype `Box` derives from its own type parameter"#,
            true,
        ),
        (
            "221",
            r#"module test_newtype_field_tb;

struct Number(integer);

#[test]
entity Top {}

impl Top {
    p: process {
        let n: Number = Number(42);
        // Any field access on Number compiles without error
        let v: integer = n.xyz;
    }
}
"#,
            r#"`Number` has no field `xyz`"#,
            true,
        ),
        (
            "222",
            r#"module test_any_struct_method;

struct Foo {
    pub x: integer,
}

#[test]
entity Top {}

impl Top {
    p: process {
        let s: Foo = { .x = 1 };
        s.destroy_the_world(1, 2, 3);
    }
}
"#,
            r#"has no method `destroy_the_world`"#,
            true,
        ),
        (
            "223",
            r#"module test_downcast_unpacked;

#[test]
entity Top {}

impl Top {
    p: process {
        let x: integer[4] = [1, 2, 3, 4];
        let y: integer[2] = integer[2](x);
    }
}
"#,
            r#"`integer[2]` is an array, not a vector family"#,
            true,
        ),
        (
            "228",
            r#"module test_const_arr_bounds;

const ARR: integer[3] = [10, 20, 30];

entity Top {
    y: integer out,
}

impl Top {
    y = ARR[999];
}
"#,
            r#"index 999 is outside `0..2`"#,
            false,
        ),
    ];
    for (issue, source, message, run) in cases {
        let out = compile(&format!("error_{issue}"), source, *run);
        assert!(out.contains(message), "#{issue}: {out}");
    }
}
