//! Regressions for sioxc #379 (hardware `static let`: a method call in an
//! asynchronous reset branch, unlabelled processes across implementations)
//! and #390 (`await … for`: compound edge conditions, a settle inside the
//! condition, a negative timeout when the condition holds at once).

use std::process::Command;

fn run(name: &str, source: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("siox_i379_{name}_{}", std::process::id()));
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
    let built = String::from_utf8_lossy(&build.stdout).to_string()
        + &String::from_utf8_lossy(&build.stderr);
    assert!(build.status.success(), "{built}");
    assert!(!built.contains("warning"), "{built}");
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let text =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    (run.status.success(), text)
}

#[cfg(feature = "llvm")]
#[test]
fn a_static_resets_through_its_method_without_a_false_loop() {
    let (ok, text) = run("reset_method", I379_RESET);
    assert!(ok, "{text}");
}

#[test]
fn unlabelled_processes_across_impls_get_distinct_registers() {
    let dir = std::env::temp_dir().join(format!("siox_i379_unlabelled_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("unlabelled.siox");
    std::fs::write(&file, I379_UNLABELLED).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--emit",
            "ir",
        ])
        .arg(&file)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}");
    assert!(!text.contains("duplicate"), "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn await_for_waits_for_the_next_compound_edge_and_keeps_its_timer() {
    let (ok, text) = run("compound", I390_GOOD);
    assert!(ok, "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn a_negative_timeout_fails_even_when_the_condition_holds() {
    let (ok, text) = run("negative", I390_NEG);
    assert!(!ok, "{text}");
    assert!(text.contains("negative time"), "{text}");
    assert!(!text.contains("must not get here"), "{text}");
}

const I379_RESET: &str = r#"module test_hw_static_reset_method;

struct Counter {
    pub count: unsigned[8],
}

impl Counter {
    pub fn reset() {
        count = 5;
    }
    pub fn step() {
        count = count + 1;
    }
}

entity Dut {
    clk: Bit in,
    rst: Bit in,
    c_out: unsigned[8] out,
}

impl Dut {
    tick: process {
        static let c: Counter = Counter { .count = 5 };
        if rst == '1' {
            c.reset();
        } else if clk.rising() {
            c.step();
            c_out = c.count;
        }
    }
}

#[test]
entity TB {}

impl TB {
    let clk: Bit;
    let rst: Bit = '0';
    let c_out: unsigned[8];
    let dut: Dut = { .clk = clk, .rst = rst, .c_out = c_out };
    clock: process { clk = not clk after 5ns; }
    check: process {
        await clk.rising();
        await 1ns;
        assert!(c_out == 6);
        await clk.rising();
        await 1ns;
        assert!(c_out == 7);
        rst = '1';
        await 2ns;
        rst = '0';
        await clk.rising();
        await 1ns;
        print!("after rst: c_out={}\n", c_out);
        assert!(c_out == 6, "after rst, c should have reset to 5 and stepped to 6");
    }
}
"#;

const I379_UNLABELLED: &str = r#"module test_two_impl_static;

entity Counter {
    clk: Bit in,
    q1: unsigned[8] out,
    q2: unsigned[8] out,
}

impl Counter {
    process {
        static let count: unsigned[8] = 0;
        if clk.rising() {
            count = count + 1;
            q1 = count;
        }
    }
}

impl Counter {
    process {
        static let count: unsigned[8] = 10;
        if clk.rising() {
            count = count + 1;
            q2 = count;
        }
    }
}
"#;

const I390_GOOD: &str = r#"module test_await_for_bugs;
use std::sim::*;

entity Dut {
    probe: Bit in,
    ready: Bit out,
}

impl Dut {
    ready = '0';
}

fn check_ready(p: Bit, r: Bit) -> Bool {
    p = '1';
    return r == '1';
}

#[test]
entity TbCompoundEdge {}

impl TbCompoundEdge {
    let clk: Bit = '0';
    let ready: Bit = '0';
    let done: Bit = '0';

    process {
        clk = '1' after 5ns;
        ready = '1' after 5ns;
        await 5ns;
        // At 5ns, clk just rose. A plain `await clk.rising() for 10ns;` waits
        // for the NEXT rising edge (and times out at 15ns).
        // Because `await_is_event` does not recurse into Binary expressions,
        // `await (clk.rising() and ready == '1') for 10ns` checks immediately
        // at 5ns and fires on the already-elapsed edge instead of timing out!
        await (clk.rising() and ready == '1') for 10ns;
        done = '1';

        print!("resumed with clk={} ready={}\n", clk, ready);
    }

    process {
        await 6ns;
        assert!(done == '0', "compound edge await fired immediately at 5ns without waiting for next edge");
    }
}

#[test]
entity TbSettleDisarmsTimeout {}

impl TbSettleDisarmsTimeout {
    let probe: Bit = '0';
    let ready: Bit;
    let tick: Bit = '0';
    let dut: Dut = { .probe = probe, .ready = ready };

    process {
        await 2ns;
        tick = '1';
    }

    process {
        await check_ready(probe, ready) for 10ns;
    }
}
"#;

const I390_NEG: &str = r#"module test_await_for_negative;
use std::sim::*;

#[test]
entity TbNegative {}

impl TbNegative {
    process {
        let neg: time = time(-5);
        await true for neg;
        print!("must not get here");
    }
}
"#;
