//! Antigravity's issues #188–#215: programs that type-checked and then
//! failed late or compiled to a wrong design. Each case is named after its
//! GitHub issue.

use std::process::Command;

/// Build `source` (as a test binary when `run`, else `--emit metadata`), run
/// it when it built, and return everything printed.
fn compile(name: &str, source: &str, run: bool) -> String {
    let dir = std::env::temp_dir().join(format!("siox_issues_188_{}", std::process::id()));
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
    for (issue, source, printed) in [
        (
            "189",
            r#"module test_edge_else_sim;

entity Top {
    clk: Bit in,
    d: unsigned[8] in,
    q: unsigned[8] out,
}

impl Top {
    process {
        if clk.rising() {
            q = d;
        } else {
            q = 0;
        }
    }
}

#[test] entity TB {}
impl TB {
    let t_clk: Bit;
    let t_d: unsigned[8];
    let t_q: unsigned[8];
    let dut: Top = { .clk = t_clk, .d = t_d, .q = t_q };

    process {
        t_clk = '0';
        t_d = 42;
        await 1ns;

        // Rise clock -> q should become 42
        t_clk = '1';
        await 1ns;
        print!("q after rising: {}\n", t_q);
        assert!(t_q == 42);

        // Fall clock -> in source code, else { q = 0; } should make q = 0
        t_clk = '0';
        await 1ns;
        print!("q after falling: {}\n", t_q);
        assert!(t_q == 0, "q should be 0 in else branch when clk is not rising");
    }
}
"#,
            &["q after falling: 0"][..],
        ),
        (
            "190",
            r#"module test_logic_literals;

#[test] entity TB {}
impl TB {
    process {
        let u: Logic = 'U';
        
        // 1. Literal on RHS produces wrong IEEE 1164 results due to raw discriminant arithmetic:
        let res_and: Logic = u and '1';
        let res_or: Logic = u or '1';
        print!("u and '1': {}\\n", res_and); // Prints '0' instead of 'U'
        print!("u or '1': {}\\n", res_or);   // Prints 'W' instead of '1'

        // 2. Both operands as Logic variables: works correctly
        let one: Logic = '1';
        print!("u and one: {}\\n", u and one); // Prints 'U'
        print!("u or one: {}\\n", u or one);   // Prints '1'

        // 3. Literal on LHS fails lowering:
        let lhs_res: Logic = '1' and u; print!("1 and u: {}\\n", lhs_res); let z: Logic = 'Z'; print!("z and 1: {} x and z: {}\\n", z and '1', 'X' and z); 
    }
}
"#,
            &["u and '1': 'U'", "u or '1': '1'", "1 and u: 'U'"][..],
        ),
        (
            "192",
            r#"module test_trait_ambiguity;

trait A {
    fn ping(self) -> integer;
}

trait B {
    fn ping(self) -> real;
}

struct Foo {
    pub x: integer,
}

impl A for Foo {
    fn ping(self) -> integer { return 1; }
}

impl B for Foo {
    fn ping(self) -> real { return 2.0; }
}

#[test] entity TB {}
impl TB {
    process {
        let f: Foo = Foo { .x = 0 };
        let a: integer = A::ping(f);
        let b: real = B::ping(f);
        print!("a: {}, b: {}\\n", a, b);
        assert!(a == 1, "A::ping returned wrong value");
    }
}
"#,
            &["a: 1"][..],
        ),
        (
            "193",
            r#"module m;
extern "C" { fn putchar(value: integer) -> integer; }
#[test] entity TB {}
impl TB { process { putchar(65); putchar(10); } }
"#,
            &["A"][..],
        ),
        (
            "196",
            r#"module m;
use std::bits::unsigned;
struct Wrapper<W: integer> { pub val: unsigned[W], pub n: integer, }
struct Box<T> { pub v: T, }
#[test] entity TB {}
impl TB {
    process {
        let w: Wrapper<12> = { .val = 4000, .n = -3 };
        let b: Box<integer> = { .v = 77 };
        w.val = w.val + 1;
        print!("{} {} {} {}\n", w.val, w.n, b.v, w.val'length);
    }
}
"#,
            &["4001 -3 77 12"][..],
        ),
        (
            "200_array",
            r#"module test_for_empty_array;

#[test]
entity TB {}

impl TB {
    run: process {
        let count: integer = 0;
        for x in [] {
            count = count + 1;
        }
        assert!(count == 0);
    }
}
"#,
            &[][..],
        ),
        (
            "200_string",
            r#"module test_for_empty_string;

#[test]
entity TB {}

impl TB {
    run: process {
        let count: integer = 0;
        for c in "" {
            count = count + 1;
        }
        assert!(count == 0);
    }
}
"#,
            &[][..],
        ),
        (
            "201",
            r#"module m;
const OFFSET: integer = 10;
const TOP: integer = OFFSET + 5;
enum Code { Base = OFFSET, Next = 0, High = TOP, After, }
#[test] entity TB {}
impl TB {
    process {
        let c: Code = Code::Base;
        assert!(c == Code::Base);
        assert!(c != Code::Next);
        print!("{} {} {} {}\n", integer(Code::Base), integer(Code::Next), integer(Code::High), integer(Code::After));
    }
}
"#,
            &["10 0 15 16"][..],
        ),
        (
            "203_literal",
            r#"module test_attr;

#[test]
entity TestAttr {}
impl TestAttr {
    process {
        let len: integer = [10, 20, 30]'length;
        assert!(len == 3);
    }
}
"#,
            &[][..],
        ),
        (
            "203_slice",
            r#"module test_attr;

#[test]
entity TestAttr {}
impl TestAttr {
    process {
        let arr: integer[3] = [10, 20, 30];
        let len: integer = arr[0..2]'length;
        assert!(len == 3); // Fails! len evaluates to 64!
    }
}
"#,
            &[][..],
        ),
        (
            "204_array",
            r#"module test_generic_fn;

fn make_arr<T>(x: T) -> T[2] {
    return [x, x];
}

#[test]
entity TestGeneric {}
impl TestGeneric {
    process {
        assert!(make_arr(42)[0] == 42);
    }
}
"#,
            &[][..],
        ),
        (
            "204_element",
            r#"module test_head;

fn head<T>(arr: T[4]) -> T {
    return arr[0];
}

#[test]
entity TestHead {}
impl TestHead {
    process {
        assert!(head([10, 20, 30, 40]) == 10);
    }
}
"#,
            &[][..],
        ),
        (
            "204_struct",
            r#"module test_box;

struct Boxed<T> {
    pub val: T,
}

fn wrap<T>(x: T) -> Boxed<T> {
    return Boxed { .val = x };
}

#[test]
entity TestGenericBox {}
impl TestGenericBox {
    process {
        assert!(wrap(42).val == 42);
    }
}
"#,
            &[][..],
        ),
        (
            "207",
            r#"module test;

use std::bits::unsigned;

#[test]
entity Top {}

impl Top {
    const R: range = 0..3;

    main: process {
        let sum: unsigned[8] = 0;
        for i in R {
            sum = sum + i;
        }
        print!("sum = {}\n", sum);
    }
}
"#,
            &["sum = 6"][..],
        ),
    ] {
        let printed: &[&str] = printed;
        let out = compile(&format!("run_{issue}"), source, true);
        assert!(out.contains("1 passed"), "#{issue}: {out}");
        for line in printed {
            assert!(out.contains(line), "#{issue}: missing `{line}`: {out}");
        }
    }
}

#[test]
fn programs_that_passed_wrongly_are_diagnosed() {
    for (issue, source, message, run) in [
        (
            "188",
            r#"module test_tb_ports;

#[test]
entity TB {
    y: unsigned[8] out,
}
impl TB {
    process {
        y = 1;
    }
}
"#,
            r#"test entity `TB` cannot have ports"#,
            true,
        ),
        (
            "191",
            r#"module test_inout_wiring;

entity Child {
    io: Logic inout,
}
impl Child {
    io = '1';
}

entity Parent {
    res: Logic out,
}
impl Parent {
    let io: Logic;
    
    // FAILS: Child's driver ('1') never reaches 'io', leaving res == 'Z'
    let child: Child;
    child.io = io;

    // WORKS:
    // let child: Child = { .io = io };

    io = 'Z';
    res = io;
}

#[test] entity TB {}
impl TB {
    let dut: Parent;
    process {
        print!("res: {}\\n", dut.res);
        assert!(dut.res == '1', "inout driver did not propagate to parent net");
    }
}
"#,
            r#"inout port `child.io` is connected by assignment"#,
            true,
        ),
        (
            "192",
            r#"module test_trait_ambiguity;

trait A {
    fn ping(self) -> integer;
}

trait B {
    fn ping(self) -> real;
}

struct Foo {
    pub x: integer,
}

impl A for Foo {
    fn ping(self) -> integer { return 1; }
}

impl B for Foo {
    fn ping(self) -> real { return 2.0; }
}

#[test] entity TB {}
impl TB {
    process {
        let f: Foo = Foo { .x = 0 };
        let a: integer = A::ping(f); let c: integer = f.ping();
        let b: real = B::ping(f);
        print!("a: {}, b: {}\\n", a, b);
        assert!(a == 1, "A::ping returned wrong value");
    }
}
"#,
            r#"is defined by more than one trait"#,
            true,
        ),
        (
            "194",
            r#"module test_foreign_string;

fn puts(s: string) -> integer;

#[test] entity TB {}
impl TB {
    process {
        let res: integer = puts("hi");
        print!("res: {}\\n", res);
    }
}
"#,
            r#"function `puts` has no body"#,
            true,
        ),
        (
            "197",
            r#"module test_concat_real_tb;

#[test]
entity TB {}

impl TB {
    run: process {
        let x: unsigned[8] = { 1.5, 2.5 };
    }
}
"#,
            r#"a concatenation joins bits, and `1.5` is a `real`"#,
            true,
        ),
        (
            "198",
            r#"module test_idx_2d;

#[test]
entity TB {}

impl TB {
    run: process {
        let matrix: integer[3][2] = [[1, 2, 3], [4, 5, 6]];
        let x: integer = matrix[0][5]; // Index 5 is out of bounds for 3-element inner array
    }
}
"#,
            r#"index 5 is outside `0..2`"#,
            true,
        ),
        (
            "205_array",
            r#"module test_array_push;

#[test]
entity TestPushFixed {}
impl TestPushFixed {
    process {
        let xs: integer[3] = [1, 2, 3];
        xs.push(4); // Or xs.nonexistent_method();
    }
}
"#,
            r#"`integer[3]` has no method `push`"#,
            true,
        ),
        (
            "205_tuple",
            r#"module test_tup_method;

#[test]
entity TestTupMethod {}
impl TestTupMethod {
    process {
        let p: (integer, integer) = (1, 2);
        p.foo();
    }
}
"#,
            r#"has no method `foo`"#,
            true,
        ),
        (
            "207_array",
            r#"module test;

use std::bits::unsigned;

entity Top {
    y: unsigned[8] out,
}

impl Top {
    const ARR: unsigned[8][3] = [10, 20, 30];

    for v in ARR {
        y = v;
    }
}
"#,
            r#"a hardware `for` runs over a constant range"#,
            false,
        ),
        (
            "210",
            r#"module test;

struct Unit {}

entity Top {
    u: Unit out,
}

impl Top {
    u = Unit {};
}
"#,
            r#"`Unit` has no fields"#,
            false,
        ),
        (
            "211",
            r#"module test;

use std::bits::unsigned;

struct Packet<W: integer> {
    pub data: unsigned[W * 2],
}

entity Top {
    p: Packet<0> out,
}

impl Top {
    p.data = 0;
}
"#,
            r#"a width of 0"#,
            false,
        ),
        (
            "212",
            r#"module test;

use std::bits::unsigned;

struct Dynamic {
    pub data: unsigned[8][],
}

entity Top {
    d: Dynamic out,
}

impl Top {}
"#,
            r#"field `data` has no fixed size"#,
            false,
        ),
        (
            "213",
            r#"module test;

struct S {
    pub arr: integer[3],
}

#[test]
entity Top {}

impl Top {
    main: process {
        let s: S = { .arr = [1, 2, 3] };
        let x: integer = s.arr[5];
        print!("x = {}\n", x);
    }
}
"#,
            r#"index 5 is outside `0..2`"#,
            true,
        ),
        (
            "214",
            r#"module test;

struct A<T> {
    pub x: T,
}

struct B {
    pub a: A<B>,
}

entity Top {
    b: B out,
}

impl Top {}
"#,
            r#"contains itself"#,
            false,
        ),
        (
            "215",
            r#"module test;

use std::bits::unsigned;

struct Point {
    pub x: integer,
    pub y: integer,
}

#[test]
entity Top {}

impl Top {
    main: process {
        let a: Point[2];
        let b: Point[2];
        let c: unsigned[4] = { a, b };
        print!("done\n");
    }
}
"#,
            r#"a concatenation joins bits"#,
            true,
        ),
    ] {
        let out = compile(&format!("error_{issue}"), source, run);
        assert!(out.contains(message), "#{issue}: {out}");
    }
}
