//! Antigravity's issues #143 and #144: a custom operator's declared result, an
//! alias of one included, keeps its width, labels and type for the next
//! operator, in hardware and in a testbench. The programs are the corpus
//! checks both issues name.

use std::process::Command;

fn run(name: &str, source: &str, flags: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!("siox_issues_143_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let binary = dir.join(name);
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
        ])
        .args(flags)
        .arg("-o")
        .arg(&binary)
        .arg(&file)
        .output()
        .unwrap();
    let mut text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        let ran = Command::new(&binary).output().unwrap();
        text += &String::from_utf8_lossy(&ran.stdout);
    }
    text
}

const NESTED: &str = r#"module nested_custom_operators_test;
use std::bits::unsigned;

impl CustomOperator<"choose", unsigned, Bool> for unsigned {
    attr precedence = 35;
    fn apply(self, rhs: unsigned) -> Bool { return self > rhs; }
}
impl CustomOperator<"combine", Bool, unsigned[8]> for Bool {
    attr precedence = 35;
    fn apply(self, rhs: Bool) -> unsigned[8] {
        return if self and rhs { 192 } else { 3 };
    }
}
impl CustomOperator<"clip", unsigned, unsigned[3]> for unsigned {
    attr precedence = 35;
    fn apply(self, rhs: unsigned) -> unsigned[3] {
        let clipped: unsigned[3] = unsigned[3](self + rhs);
        return clipped;
    }
}
struct Packet { pub value: unsigned[8] }
impl CustomOperator<"bundle", unsigned, Packet> for unsigned {
    attr precedence = 35;
    fn apply(self, rhs: unsigned) -> Packet { return { .value = self or rhs }; }
}
impl CustomOperator<"merge", unsigned, unsigned[8]> for Packet {
    attr precedence = 35;
    fn apply(self, rhs: unsigned) -> unsigned[8] { return self.value or rhs; }
}
impl CustomOperator<"total", unsigned, integer> for unsigned {
    attr precedence = 35;
    fn apply(self, rhs: unsigned) -> integer { return integer(self) + integer(rhs); }
}
impl CustomOperator<"plus", integer, integer> for integer {
    attr precedence = 35;
    fn apply(self, rhs: integer) -> integer { return self + rhs; }
}
impl CustomOperator<"extent", unsigned, integer> for unsigned {
    attr precedence = 35;
    fn apply(self, rhs: unsigned) -> integer {
        return self'length * 100 + self'left * 10 + self'right;
    }
}

entity Probe {
    a: unsigned[8] in, b: unsigned[8] in, c: unsigned[8] in,
    chained: unsigned[8] out, right: unsigned[8] out,
    complemented: unsigned[8] out, changed_type: unsigned[8] out,
    short: unsigned[3] out, packet: unsigned[8] out, total: integer out,
    extent: integer out, inherited: integer out
}
impl Probe {
    chained = a xor b xor c;
    right = a xor (b xor c);
    complemented = (a xor b) xnor c;
    changed_type = (a choose b) combine (c choose b);
    short = (a clip b) xnor unsigned[3](c);
    packet = (a bundle b) merge c;
    total = (a total b) plus integer(c);
    extent = (a clip b) extent unsigned[3](c);
    inherited = (a xor b) extent c;
}

#[test] entity NestedCustomOperatorsTest {}
impl NestedCustomOperatorsTest {
    let a: unsigned[8] = 160;
    let b: unsigned[8] = 6;
    let c: unsigned[8] = 49;
    let chained: unsigned[8]; let right: unsigned[8];
    let complemented: unsigned[8]; let changed_type: unsigned[8];
    let short: unsigned[3]; let packet: unsigned[8]; let total: integer;
    let extent: integer; let inherited: integer;
    let probe: Probe = { .a = a, .b = b, .c = c, .chained = chained,
        .right = right, .complemented = complemented, .changed_type = changed_type,
        .short = short, .packet = packet, .total = total,
        .extent = extent, .inherited = inherited };
    process {
        await 1ns;
        assert!(chained == 151 and right == 151, "nested xor preserves its family");
        assert!(complemented == 104, "nested xnor preserves all eight bits");
        assert!(changed_type == 192, "custom result types drive subsequent dispatch");
        assert!(short == 0, "custom output widths override the input width");
        assert!(packet == 183, "aggregate custom results retain their layout");
        assert!(total == 215, "kernel custom results retain their full width");
        assert!(extent == 302, "constrained result length and direction follow its declaration");
        assert!(inherited == 807, "unconstrained results retain the operand direction");
        c = 0; await 1ns;
        assert!(chained == 166 and right == 166 and complemented == 89);
        assert!(changed_type == 3);
        assert!(short == 1 and packet == 166 and total == 166);
        a = "1010X000"; await 1ns;
        assert!(chained[3] == 'X' and right[3] == 'X' and complemented[3] == 'X');
        assert!(chained[7] == '1' and complemented[7] == '0', "metavalues stay per element");
    }
}
"#;

const ALIAS: &str = r#"module operator_alias_result_test;

impl CustomOperator<"clip_alias", unsigned, Small> for unsigned {
    attr precedence = 35;
    fn apply(self, rhs: unsigned) -> Small {
        return unsigned[6..4](self + rhs);
    }
}

// Forward alias chains must keep both the width and the declared labels.
type Small = Shape;
type Shape = unsigned[6..4];

entity Probe {
    a: unsigned[8] in,
    b: unsigned[8] in,
    result: Small out,
    left: integer out,
    right: integer out
}
impl Probe {
    result = a clip_alias b;
    left = (a clip_alias b)'left;
    right = (a clip_alias b)'right;
}

#[test] entity OperatorAliasResultTest {}
impl OperatorAliasResultTest {
    let a: unsigned[8] = 160;
    let b: unsigned[8] = 6;
    let dut: Probe = { .a = a, .b = b };
    exercise: process {
        let local: Small = a clip_alias b;
        assert!(local == 6, "procedural aliased output");
        assert!(local'left == 6 and local'right == 4, "procedural alias keeps labels");
        assert!((std::bits::unsigned[6..4](5))'left == 6,
            "qualified constructor keeps its declared labels");
        assert!((std::bits::unsigned[6..4](5))'right == 4,
            "qualified constructor keeps its direction");
        await 1ns;
        assert!(dut.result == 6, "hardware aliased output");
        assert!(dut.left == 6 and dut.right == 4, "alias keeps directed output labels");
        a = 255; b = 2; await 1ns;
        assert!(dut.result == 1, "alias keeps the constrained output width");
        assert!((1 clip_alias b) == 3, "literal-left aliased output");
    }
}
"#;

/// `(a clip b) xnor unsigned[3](c)` is an `unsigned[3]`; a `Bool` or struct
/// result dispatches the next operator; `self'right` of `(a clip b)` is the
/// declared result's (#143).
#[test]
fn chained_custom_operator_results_keep_their_declared_type() {
    for (name, flags) in [("nested", &[][..]), ("nested_debug", &["-g"][..])] {
        let out = run(name, NESTED, flags);
        assert!(out.contains("1 passed"), "{name}: {out}");
    }
}

/// `-> Small` with `type Small = Shape; type Shape = unsigned[6..4];` is an
/// `unsigned[6..4]`, labels included, and `unsigned[6..4](5)'left` is 6 in a
/// testbench as in hardware (#144).
#[test]
fn an_aliased_operator_result_is_its_underlying_type() {
    for (name, flags) in [("alias", &[][..]), ("alias_debug", &["-g"][..])] {
        let out = run(name, ALIAS, flags);
        assert!(out.contains("1 passed"), "{name}: {out}");
    }
}

/// The declared result is not widened: an `unsigned[3]` result still does
/// not store into an `unsigned[8]`.
#[test]
fn a_declared_result_still_mismatches_a_wider_target() {
    let out = run(
        "mismatch",
        r#"module mismatch;
use std::bits::unsigned;
impl CustomOperator<"clip", unsigned, unsigned[3]> for unsigned {
    attr precedence = 35;
    fn apply(self, rhs: unsigned) -> unsigned[3] { return unsigned[3](self + rhs); }
}
entity Probe { a: unsigned[8] in, b: unsigned[8] in, wide: unsigned[8] out }
impl Probe { wide = a clip b; }
"#,
        &[],
    );
    assert!(
        out.contains("cannot assign unsigned[3] to unsigned[8]"),
        "{out}"
    );
}
