//! Regressions for sioxc #381, #382 and #388: entity methods (strobed out
//! ports, generic type parameters, nested calls, calls in conditional
//! expressions or under a clock edge) and processes built by generate
//! blocks (method calls, run-time `if`, labels, placement, named ranges).

use std::process::Command;

fn sioxc(name: &str, source: &str, args: &[&str]) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i381_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .args(args)
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status.success(), text, dir)
}

#[cfg(feature = "llvm")]
#[test]
fn i381_out_port_strobes() {
    let (ok, text, dir) = sioxc(
        "i381_out_port_strobes",
        I381_OUT_PORT_STROBES,
        &["--test", "-o", "t.bin"],
    );
    assert!(ok, "{text}");
    assert!(!text.contains("warning[W-P002]"), "{text}");
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let text =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "{text}");
}

#[test]
fn i381_generic_type_parameter() {
    let (ok, text, _) = sioxc(
        "i381_generic_type_parameter",
        I381_GENERIC_TYPE_PARAMETER,
        &["--emit", "ir"],
    );
    assert!(ok, "{text}");
    assert!(!text.contains("error"), "{text}");
}

#[test]
fn i382_nested_call() {
    let (ok, text, _) = sioxc("i382_nested_call", I382_NESTED_CALL, &["--emit", "ir"]);
    assert!(ok, "{text}");
    assert!(!text.contains("error"), "{text}");
    assert!(!text.contains("not connected"), "{text}");
}

#[test]
fn i382_call_in_if_expression() {
    let (ok, text, _) = sioxc(
        "i382_call_in_if_expression",
        I382_CALL_IN_IF_EXPRESSION,
        &["--emit", "metadata"],
    );
    assert!(!ok, "{text}");
    assert!(
        text.contains("is called in a branch of a conditional expression"),
        "{text}"
    );
    assert!(!text.contains("a `for` loop needs a range"), "{text}");
}

#[test]
fn i382_call_under_clock_edge() {
    let (ok, text, _) = sioxc(
        "i382_call_under_clock_edge",
        I382_CALL_UNDER_CLOCK_EDGE,
        &["--emit", "metadata"],
    );
    assert!(!ok, "{text}");
    assert!(text.contains("is called under a clock edge"), "{text}");
    assert!(!text.contains("a `for` loop needs a range"), "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn i388_method_call_in_generate_process() {
    let (ok, text, dir) = sioxc(
        "i388_method_call_in_generate_process",
        I388_METHOD_CALL_IN_GENERATE_PROCESS,
        &["--test", "-o", "t.bin"],
    );
    assert!(ok, "{text}");
    assert!(!text.contains("warning[W-P002]"), "{text}");
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let text =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "{text}");
}

#[test]
fn i388_process_in_runtime_if() {
    let (ok, text, _) = sioxc(
        "i388_process_in_runtime_if",
        I388_PROCESS_IN_RUNTIME_IF,
        &["--emit", "metadata"],
    );
    assert!(!ok, "{text}");
    assert!(
        text.contains("a process cannot be built by a run-time `if` or `match`"),
        "{text}"
    );
    assert!(!text.contains("a `for` loop needs a range"), "{text}");
}

#[test]
fn i388_generate_process_checks() {
    let (ok, text, _) = sioxc(
        "i388_generate_process_checks",
        I388_GENERATE_PROCESS_CHECKS,
        &["--emit", "metadata"],
    );
    assert!(!ok, "{text}");
    assert!(
        text.contains("duplicate inherent member `dup_proc`"),
        "{text}"
    );
    assert!(text.contains("duplicate assignment label `lbl`"), "{text}");
    assert!(
        text.contains("`process` is only allowed in an inherent entity implementation"),
        "{text}"
    );
    assert!(!text.contains("a `for` loop needs a range"), "{text}");
}

const I381_OUT_PORT_STROBES: &str = r#"module test_entity_method_out_port;

entity Sender {
    en: Bit out,
    dout: unsigned[8] out,
}

impl Sender {
    pub fn fire(v: unsigned[8]) {
        en = '1';
        dout = v;
    }
}

entity Top {
    go: Bit in,
    din: unsigned[8] in,
    en: Bit out,
    dout: unsigned[8] out,
}

impl Top {
    let s: Sender = { .en = en, .dout = dout };

    drive: process {
        if go == '1' {
            s.fire(din);
        }
    }
}

#[test]
entity Tb {}

impl Tb {
    let go: Bit = '0';
    let din: unsigned[8] = 0;
    let en: Bit;
    let dout: unsigned[8];
    let dut: Top = { .go = go, .din = din, .en = en, .dout = dout };

    check: process {
        await 1ns;
        assert!(en == '0', "initially idle");
        go = '1';
        din = 77;
        await 1ns;
        assert!(en == '1', "strobed high when called");
        assert!(dout == 77, "data driven when called");
        go = '0';
        await 1ns;
        assert!(en == '0', "strobe must clear when not called");
    }
}
"#;

const I381_GENERIC_TYPE_PARAMETER: &str = r#"module test_entity_method_generics;

pub entity Slot<T> {
    clk: Bit in,
}

impl<T> Slot<T> {
    let wr_en: Bit;
    let din: T;
    let stored: T;

    store: process {
        if clk.rising() {
            if wr_en == '1' {
                stored = din;
            }
        }
    }

    pub fn put(v: T) {
        wr_en = '1';
        din = v;
    }

    pub fn peek() -> T {
        return stored;
    }
}

pub entity Holder {
    clk: Bit in,
    valid: Bit in,
    din: unsigned[8] in,
    dout: unsigned[8] out,
}

impl Holder {
    let slot: Slot<unsigned[8]> = { .clk = clk };

    run: process {
        if valid == '1' {
            slot.put(din);
        }
        dout = slot.peek();
    }
}
"#;

const I382_NESTED_CALL: &str = r#"module test_entity_method_nested;

entity Source { rd_count: unsigned[8] out }
impl Source {
    let rd_en: Bit;
    pub fn pop() -> unsigned[8] { rd_en = '1'; return 42; }
}

entity Sink { got: unsigned[8] out }
impl Sink {
    let din: unsigned[8];
    drive: process { got = din; }
    pub fn push(v: unsigned[8]) { din = v; }
}

entity Bridge { go: Bit in, got: unsigned[8] out }
impl Bridge {
    let src: Source = {};
    let snk: Sink = { .got = got };
    xfer: process {
        if go == '1' {
            snk.push(src.pop());
        }
    }
}
"#;

const I382_CALL_IN_IF_EXPRESSION: &str = r#"module test_entity_method_if_expr;

entity Fifo { clk: Bit in }
impl Fifo {
    let count: unsigned[8];
    let rd_en: Bit;
    store: process {
        if clk.rising() {
            if rd_en == '1' { count = count + 1; }
        }
    }
    pub fn pop() -> unsigned[8] { rd_en = '1'; return count; }
}

entity Pipe { clk: Bit in, take: Bit in, q: unsigned[8] out }
impl Pipe {
    let fifo: Fifo = { .clk = clk };
    drain: process {
        q = if take == '1' { fifo.pop() } else { 99 };
    }
}

#[test]
entity Tb {}
impl Tb {
    let clk: Bit = '0';
    let take: Bit = '0';
    let q: unsigned[8];
    let pipe: Pipe = { .clk = clk, .take = take, .q = q };
    clock: process { clk = not clk after 5ns; }
    check: process {
        await 1ns;
        await clk.rising(); await 1ns;
        await clk.rising(); await 1ns;
        take = '1';
        await 1ns;
        assert!(q == 0, "fifo.pop() must not have been strobed while take == '0'");
        stop();
    }
}
"#;

const I382_CALL_UNDER_CLOCK_EDGE: &str = r#"module test_entity_method_clocked_caller;

entity Fifo { clk: Bit in }
impl Fifo {
    let count: unsigned[8];
    let wr_en: Bit;
    let din: unsigned[8];
    store: process {
        if clk.rising() {
            if wr_en == '1' { count = count + din; }
        }
    }
    pub fn push(v: unsigned[8]) { wr_en = '1'; din = v; }
    pub fn total() -> unsigned[8] { return count; }
}

entity Parent { clk: Bit in, valid: Bit in, data: unsigned[8] in, tot: unsigned[8] out }
impl Parent {
    let fifo: Fifo = { .clk = clk };
    feed: process {
        if clk.rising() {
            if valid == '1' {
                fifo.push(data);
            }
        }
    }
    out_p: process { tot = fifo.total(); }
}

#[test]
entity Tb {}
impl Tb {
    let clk: Bit = '0';
    let valid: Bit = '0';
    let data: unsigned[8] = 0;
    let tot: unsigned[8];
    let dut: Parent = { .clk = clk, .valid = valid, .data = data, .tot = tot };
    clock: process { clk = not clk after 5ns; }
    check: process {
        await 1ns;
        valid = '1'; data = 10;
        await clk.rising(); await 1ns;
        valid = '0';
        await clk.rising(); await 1ns;
        await clk.rising(); await 1ns;
        assert!(tot == 10, "expected 10, got repeated pushes after valid='0'");
        stop();
    }
}
"#;

const I388_METHOD_CALL_IN_GENERATE_PROCESS: &str = r#"module test_gen_proc_method_and_runtime_if;
use std::bits::unsigned;
use std::sim::*;

entity Sub {
    clk: Bit in,
    q: unsigned[8] out,
}

impl Sub {
    pub fn bump(v: unsigned[8]) {
        if clk.rising() {
            q = q + v;
        }
    }
}

entity Top {
    clk: Bit in,
    en: Bit in,
    q_sub: unsigned[8] out,
    q_if: unsigned[8] out,
}

impl Top {
    let sub: Sub = { .clk = clk, .q = q_sub };

    if true {
        process {
            sub.bump(5);
        }
    }

    q_if = 0;
}

#[test]
entity Tb {}

impl Tb {
    let clk: Bit = '0';
    let en: Bit = '1';
    let q_sub: unsigned[8];
    let q_if: unsigned[8];
    let dut: Top = { .clk = clk, .en = en, .q_sub = q_sub, .q_if = q_if };

    process {
        clk = '1';
        await 1ns;
        clk = '0';
        await 1ns;
        print!("q_sub={} q_if={}\n", q_sub, q_if);
        assert!(q_sub == 5);
    }
}
"#;

const I388_PROCESS_IN_RUNTIME_IF: &str = r#"module test_gen_proc_method_and_runtime_if;
use std::bits::unsigned;
use std::sim::*;

entity Sub {
    clk: Bit in,
    q: unsigned[8] out,
}

impl Sub {
    pub fn bump(v: unsigned[8]) {
        if clk.rising() {
            q = q + v;
        }
    }
}

entity Top {
    clk: Bit in,
    en: Bit in,
    q_sub: unsigned[8] out,
    q_if: unsigned[8] out,
}

impl Top {
    let sub: Sub = { .clk = clk, .q = q_sub };

    if true {
        process {
            sub.bump(5);
        }
    }

    if en == '1' {
        process {
            if clk.rising() {
                q_if = q_if + 1;
            }
        }
    }
}

#[test]
entity Tb {}

impl Tb {
    let clk: Bit = '0';
    let en: Bit = '1';
    let q_sub: unsigned[8];
    let q_if: unsigned[8];
    let dut: Top = { .clk = clk, .en = en, .q_sub = q_sub, .q_if = q_if };

    process {
        clk = '1';
        await 1ns;
        clk = '0';
        await 1ns;
        print!("q_sub={} q_if={}\n", q_sub, q_if);
        assert!(q_sub == 5);
    }
}
"#;

const I388_GENERATE_PROCESS_CHECKS: &str = r#"module test_gen_process_bugs;
use std::bits::unsigned;

struct S {
    x: unsigned[8],
}

impl S {
    if true {
        process {
            let y: unsigned[8] = 1;
        }
    }
}

const R: integer = 0..1;

entity Top {
    out1: unsigned[8] out,
}

impl Top {
    dup_proc: process {
        out1 = 1;
    }

    if true {
        dup_proc: process {
            lbl: out1 = 2;
            lbl: out1 = 3;
        }
        dup_proc: process {
            for k in R {
            }
        }
    }
}
"#;
