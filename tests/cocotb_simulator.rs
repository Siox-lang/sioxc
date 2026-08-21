#![cfg(feature = "cocotb")]
//! `sioxc --cocotb` builds a design cocotb can drive.
//!
//! Behind the off-by-default `cocotb` feature, so this whole file compiles away
//! unless the thing it tests was built.
//!
//! The contract is not "siox calls cocotb" but the reverse: cocotb's
//! `libcocotbvpi_*.so` registers itself and then calls back into the `vpi_*`
//! symbols the simulator exports. So the only honest test is to build a design,
//! hand it to real cocotb, and see whether a Python testbench can drive it.
//!
//! Skipped when cocotb is not installed. That is a real gap rather than a
//! comfortable one -- CI has no cocotb, so these do not run there -- and it is
//! why the assertions below cover the parts most likely to be silently wrong
//! (bit order, hierarchy, and values wider than the word ABI) rather than just
//! checking that something ran.

use std::path::{Path, PathBuf};
use std::process::Command;

fn cocotb_available() -> bool {
    Command::new("cocotb-config")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn workdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("siox_cocotb_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Build `design` with `--cocotb`, then run `test_module` against it.
fn run(name: &str, top: &str, design: &str, test_module: &str) -> String {
    let dir = workdir(name);
    std::fs::write(dir.join(format!("{name}.siox")), design).unwrap();
    std::fs::write(dir.join(format!("tb_{name}.py")), test_module).unwrap();
    let sim: PathBuf = dir.join(format!("{name}.sim"));

    let built = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .arg("--cocotb")
        .arg(dir.join(format!("{name}.siox")))
        .arg("-o")
        .arg(&sim)
        .output()
        .unwrap();
    assert!(
        sim.exists(),
        "the cocotb simulator did not build:\n{}{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );

    let ran = Command::new(&sim)
        .current_dir(&dir)
        .env("COCOTB_TEST_MODULES", format!("tb_{name}"))
        .env("COCOTB_TOPLEVEL", top)
        .env("PYTHONPATH", &dir)
        .output()
        .unwrap();
    String::from_utf8_lossy(&ran.stdout).to_string() + &String::from_utf8_lossy(&ran.stderr)
}

fn assert_all_passed(out: &str) {
    assert!(
        out.contains("FAIL=0") && !out.contains("** TESTS=0"),
        "the cocotb run did not pass cleanly:\n{out}"
    );
}

const COUNTER: &str = r#"module dut;

using std::logic::{Bit, Logic};
using std::bits::unsigned;

#[top]
entity Counter {
    clk: Bit in,
    rst: Logic in,
    en: Bit in,
    count: unsigned[8] out
}

impl Counter {
    let value: unsigned[8] = 0;

    if clk.rising() {
        if rst == '1' {
            value = 0;
        } else if en {
            value = value + 1;
        }
    }

    count = value;
}
"#;

#[test]
fn cocotb_drives_a_compiled_design() {
    if !cocotb_available() {
        eprintln!("skipping: cocotb-config not found");
        return;
    }
    let tb = r#"
import cocotb
from cocotb.triggers import Timer


async def tick(dut):
    dut.clk.value = 0
    await Timer(5, unit="ns")
    dut.clk.value = 1
    await Timer(5, unit="ns")


@cocotb.test()
async def counts_when_enabled(dut):
    dut.rst.value = 1
    dut.en.value = 1
    dut.clk.value = 0
    await Timer(1, unit="ns")
    await tick(dut)
    assert int(dut.count.value) == 0, "reset holds the counter at zero"
    dut.rst.value = 0
    for _ in range(7):
        await tick(dut)
    got = int(dut.count.value)
    assert got == 7, f"expected 7, got {got}"


@cocotb.test()
async def holds_when_disabled(dut):
    dut.rst.value = 1
    dut.en.value = 0
    dut.clk.value = 0
    await Timer(1, unit="ns")
    await tick(dut)
    dut.rst.value = 0
    before = int(dut.count.value)
    for _ in range(4):
        await tick(dut)
    assert int(dut.count.value) == before, "a disabled counter must not move"
"#;
    let out = run("counter", "Counter", COUNTER, tb);
    assert!(
        out.contains("Running on siox"),
        "cocotb should name siox as the simulator, got:\n{out}"
    );
    assert_all_passed(&out);
}

const WIDE: &str = r#"module dut;

using std::logic::Bit;
using std::bits::unsigned;

entity Inner {
    a: unsigned[8] in,
    doubled: unsigned[8] out
}

impl Inner {
    doubled = a + a;
}

#[top]
entity Wide {
    clk: Bit in,
    a: unsigned[8] in,
    wide: unsigned[128] out,
    doubled: unsigned[8] out
}

impl Wide {
    let acc: unsigned[128] = 0;
    let sub: Inner = { .a = a, .doubled = doubled };

    if clk.rising() {
        acc = acc + 7;
    }

    wide = acc;
}
"#;

#[test]
fn handles_agree_with_siox_on_bit_order_hierarchy_and_width() {
    if !cocotb_available() {
        eprintln!("skipping: cocotb-config not found");
        return;
    }
    // These three are the places a VPI layer goes wrong quietly. Bit order is
    // the worst of them: siox counts element 0 as the least significant, which
    // is the opposite of the `[7:0]` a Verilog user pictures, so reporting the
    // range the wrong way round still yields a plausible-looking integer.
    let tb = r#"
import cocotb
from cocotb.triggers import Timer


@cocotb.test()
async def bit_indices_match_siox(dut):
    dut.a.value = 1
    await Timer(1, unit="ns")
    assert dut.a.value[0] == 1, f"a[0] is the LSB in siox, got {dut.a.value[0]}"
    assert dut.a.value[7] == 0
    dut.a.value = 128
    await Timer(1, unit="ns")
    assert dut.a.value[0] == 0
    assert dut.a.value[7] == 1, "a[7] is the MSB"
    assert int(dut.a.value) == 128, f"int() must agree, got {int(dut.a.value)}"
    assert str(dut.a.value) == "10000000", f"binstr is MSB-first, got {dut.a.value}"


@cocotb.test()
async def submodule_signals_are_reachable(dut):
    dut.a.value = 21
    await Timer(1, unit="ns")
    assert int(dut.doubled.value) == 42
    assert int(dut.sub.a.value) == 21, "a child instance resolves by name"
    assert int(dut.sub.doubled.value) == 42


@cocotb.test()
async def wide_values_cross_the_word_abi(dut):
    dut.clk.value = 0
    await Timer(1, unit="ns")
    for _ in range(3):
        dut.clk.value = 0
        await Timer(1, unit="ns")
        dut.clk.value = 1
        await Timer(1, unit="ns")
    assert int(dut.wide.value) == 21, f"got {int(dut.wide.value)}"
    assert len(dut.wide.value) == 128
"#;
    let out = run("wide", "Wide", WIDE, tb);
    assert_all_passed(&out);
}

#[test]
fn a_failing_python_assertion_fails_the_run() {
    if !cocotb_available() {
        eprintln!("skipping: cocotb-config not found");
        return;
    }
    // The counterpart to the passing runs: a test suite that never fails is
    // indistinguishable from one whose assertions are not reaching the design.
    let tb = r#"
import cocotb
from cocotb.triggers import Timer


@cocotb.test()
async def reads_the_real_counter(dut):
    dut.rst.value = 1
    dut.en.value = 1
    dut.clk.value = 0
    await Timer(1, unit="ns")
    assert int(dut.count.value) == 99, "deliberately wrong"
"#;
    let out = run("failing", "Counter", COUNTER, tb);
    assert!(
        out.contains("FAIL=1"),
        "a wrong expectation should fail the run, got:\n{out}"
    );
}

#[test]
fn building_without_cocotb_explains_itself() {
    // The build shells out to `cocotb-config`; when it is missing the error has
    // to name what to install, not surface a bare ENOENT.
    let dir = workdir("nocfg");
    let src = dir.join("counter.siox");
    std::fs::write(&src, COUNTER).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .arg("--cocotb")
        .arg(&src)
        .arg("-o")
        .arg(dir.join("counter.sim"))
        .env("SIOX_COCOTB_CONFIG", "definitely-not-cocotb-config")
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        text.contains("pip install cocotb") && text.contains("SIOX_COCOTB_CONFIG"),
        "the failure should say how to fix it, got:\n{text}"
    );
    assert!(
        !Path::new(&dir.join("counter.sim")).exists(),
        "no simulator should be left behind"
    );
}

#[test]
fn a_parametric_top_says_what_to_fix() {
    // An unbound parameter reaches the backend as a zero width. `--emit object`
    // has always named the fix; the cocotb path fell through to the generic
    // validator and reported "unknown width (0)" per signal instead, which says
    // what is wrong but not what to do. Both go through one check now.
    let dir = workdir("param");
    let src = dir.join("gen.siox");
    std::fs::write(
        &src,
        "module dut;\n\
         using std::logic::{Bit};\n\
         using std::bits::{unsigned};\n\
         #[top]\n\
         entity Counter<W: integer> { clk: Bit in, count: unsigned[W] out }\n\
         impl<W: integer> Counter<W> {\n\
         \x20   let value: unsigned[W] = 0;\n\
         \x20   if clk.rising() { value = value + 1; }\n\
         \x20   count = value;\n\
         }\n",
    )
    .unwrap();
    let mut messages = Vec::new();
    for extra in [vec!["--cocotb"], vec![]] {
        let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
            .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
            .args(&extra)
            .arg(&src)
            .arg("-o")
            .arg(dir.join("gen.out"))
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            text.contains("has an unresolved width")
                && text.contains("build a concrete top or a wrapper"),
            "{extra:?} should name the fix, got:\n{text}"
        );
        assert!(
            !text.contains("unknown width (0)"),
            "{extra:?} should not fall through to the generic validator, got:\n{text}"
        );
        messages.push(
            text.lines()
                .find(|l| l.contains("unresolved width"))
                .unwrap_or_default()
                .to_string(),
        );
    }
    assert_eq!(
        messages[0], messages[1],
        "both native paths should give the same answer to the same mistake"
    );
}

#[test]
fn cocotb_and_test_are_rejected_together() {
    // They build different things -- with `--cocotb` the Python side *is* the
    // testbench -- so asking for both is a mistake worth naming.
    let dir = workdir("bothflags");
    let src = dir.join("counter.siox");
    std::fs::write(&src, COUNTER).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .arg("--cocotb")
        .arg("--test")
        .arg(&src)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        text.contains("cocotb is the testbench"),
        "the conflict should be explained, got:\n{text}"
    );
}
