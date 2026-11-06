#![cfg(feature = "llvm")]

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

const PROGRAM: &str = r#"module const_match_api;
use std::bits::unsigned;

enum State { Idle = 90, Busy = 4, Done = 17, Outside = 40 }
type Alias = State;

fn rank(value: State) -> integer {
    return match value {
        State::Idle => 0, State::Busy => 1,
        State::Done => 2, State::Outside => 3
    };
}

impl Ord<State> for State {
    fn lt(self, rhs: State) -> Bool { return rank(self) < rank(rhs); }
    fn le(self, rhs: State) -> Bool { return rank(self) <= rank(rhs); }
}

fn classify(value: Alias) -> integer {
    return match value { (Alias::Idle)..(Alias::Done) => 9, _ => 3 };
}

fn choose(selector: integer) -> integer {
    use Choice = const_match_api::State;
    let result: integer = 2;
    match selector {
        0 => { let extra: integer = 1; result = result + extra; }
        1 => { return rank(Choice::Busy) + 3; }
        _ => { }
    }
    return result;
}

const WIDTH: integer = choose(0);
const SCORE: integer = classify(State::Busy);
const HIGH: integer = 0x7fff_ffff_ffff_ffff;
const LOWER: integer = HIGH - 1;
fn large_range(value: integer) -> integer {
    return match value { (HIGH)..(LOWER) => 7, _ => 3 };
}
const LARGE: integer = large_range(HIGH);
fn dynamic_range(value: integer, left: integer, right: integer) -> integer {
    return match value { (left)..(right) => 7, _ => 3 };
}
pub attr score: integer for entity = choose(1);

entity Dut {
    selector: integer in,
    result: integer out,
    packed: unsigned[WIDTH] out,
    lanes: integer[WIDTH] out
}
impl Dut {
    attr score = choose(1);
    result = choose(selector);
    packed = unsigned[WIDTH](selector);
    for index in 0..(choose(0) - 1) { lanes[index] = index + 10; }
}

entity StateDut { state: State in, result: integer out }
impl StateDut { result = classify(state); }

entity RangeDut { value: integer in, left: integer in, right: integer in, result: integer out }
impl RangeDut { result = dynamic_range(value, left, right); }
"#;

fn run(name: &str, declarations: &str, body: &str) {
    let dir = std::env::temp_dir().join(format!("siox-const-match-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let binary = dir.join("test");
    let source = format!(
        "{PROGRAM}\n#[test] entity Probe {{}}\nimpl Probe {{ {declarations} process {{ {body} }} }}"
    );
    let compilation = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std")).compile(
        CompileRequest::new(
            SourceInput::memory(dir.join("input.siox"), source),
            Emit::TestExecutable,
        )
        .with_output(&binary),
    );
    assert!(
        compilation.succeeded(),
        "{name}: {}\n{:?}",
        compilation.render_diagnostics(),
        compilation.failure
    );
    let output = std::process::Command::new(&binary)
        .arg("-o")
        .arg(dir.join("out.vcd"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{name}: {}\n{}\nretained artifact: {}\nProcess IR: {:#?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        binary.display(),
        compilation.design.as_ref().unwrap().process_ir
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn match_constants_supply_widths_metadata_and_generate_bounds() {
    run(
        "contexts",
        "let selector: integer = 0; let dut: Dut = { .selector = selector };",
        "assert!(WIDTH == 3 and SCORE == 9);
         await 1ns;
         assert!(dut.result == 3 and integer(dut.packed) == 0);
         assert!(dut.lanes[0] == 10 and dut.lanes[1] == 11 and dut.lanes[2] == 12);
         assert!(dut'score == 4);
         selector = 1;
         await 1ns;
         assert!(dut.result == 4 and integer(dut.packed) == 1);",
    );
}

#[test]
fn constant_and_procedural_match_order_agree() {
    run(
        "procedural",
        "",
        "let state: State = State::Busy;
         assert!(classify(state) == SCORE);
         state = State::Outside;
         assert!(classify(state) == 3);",
    );
}

#[test]
fn constant_and_hardware_match_order_agree() {
    run(
        "hardware",
        "let state: State = State::Busy; let dut: StateDut = { .state = state };",
        "await 1ns; assert!(dut.result == SCORE);
         state = State::Outside;
         await 1ns; assert!(dut.result == 3);",
    );
}

#[test]
fn constant_and_native_reversed_ranges_keep_exact_integer_endpoints() {
    run(
        "large-range",
        "",
        "let value: integer = HIGH;
         assert!(LARGE == 7 and large_range(value) == LARGE);
         value = LOWER;
         assert!(large_range(value) == 7);",
    );
}

#[test]
fn procedural_range_direction_can_depend_on_runtime_parameters() {
    run(
        "dynamic-procedural",
        "",
        "
        let value: integer = HIGH;
        let left: integer = HIGH;
        let right: integer = LOWER;
        assert!(dynamic_range(value, left, right) == 7);
        left = LOWER; right = HIGH;
        assert!(dynamic_range(value, left, right) == 7);
        value = LOWER - 1;
        assert!(dynamic_range(value, left, right) == 3);
    ",
    );
}

#[test]
fn hardware_range_direction_tracks_runtime_bound_changes() {
    run(
        "dynamic-hardware",
        "
        let value: integer = HIGH; let left: integer = HIGH; let right: integer = LOWER;
        let dut: RangeDut = { .value = value, .left = left, .right = right };
    ",
        "
        await 1ns; assert!(dut.result == 7);
        left = LOWER; right = HIGH;
        await 1ns; assert!(dut.result == 7);
        value = LOWER - 1;
        await 1ns; assert!(dut.result == 3);
    ",
    );
}
