//! Entity methods called on an element of an instance array (entity-methods
//! proposal, "Since this was written"): a constant index calls that element,
//! a runtime index strobes the element it selects and reads its result, and
//! a call statement at an implementation's root drives its strobe.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn array_elements_take_method_calls() {
    let dir = std::env::temp_dir().join(format!("siox_method_arrays_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("m.siox");
    std::fs::write(&file, SOURCE).unwrap();
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
    let stderr = String::from_utf8_lossy(&build.stderr);
    assert!(build.status.success(), "{stderr}");
    assert!(!stderr.contains("warning"), "{stderr}");
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success(), "{stdout}");
    for line in [
        "fixed=7 picked=40 q0=0 q2=40 one=9",
        "sel0 picked=0 q0=0 q2=40",
    ] {
        assert!(stdout.contains(line), "{stdout}");
    }
}

const SOURCE: &str = r#"module m;
entity Cell { clk: Bit in, q: unsigned[8] out }
impl Cell {
    let v: unsigned[8] = 0;
    let we: Bit;
    let din: unsigned[8];
    pub fn get() -> unsigned[8] { return v; }
    pub fn load(n: unsigned[8]) { we = '1'; din = n; }
    store: process { if clk.rising() { if we == '1' { v = din; } } }
    q = v;
}
// A constant index: element 1 is loaded and read.
entity Fixed { clk: Bit in, o: unsigned[8] out }
impl Fixed {
    let cells: Cell[3];
    for i in 0..2 { cells[i] = { .clk = clk }; }
    cells[1].load(7);
    o = cells[1].get();
}
// A runtime index, in a process under a condition and at the root.
entity Picked { clk: Bit in, sel: integer<0..2> in, o: unsigned[8] out, q0: unsigned[8] out, q2: unsigned[8] out }
impl Picked {
    let cells: Cell[3];
    for i in 0..2 { cells[i] = { .clk = clk }; }
    go: process {
        if sel > 0 { cells[sel].load(40); }
    }
    o = cells[sel].get();
    q0 = cells[0].q;
    q2 = cells[2].q;
}
// A call statement at the root of a single instance's parent.
entity One { clk: Bit in, o: unsigned[8] out }
impl One {
    let c: Cell = { .clk = clk, .q = o };
    c.load(9);
}
#[test] entity Tb {}
impl Tb {
    let clk: Bit = '0';
    let sel: integer<0..2> = 2;
    let f: unsigned[8]; let p: unsigned[8]; let q0: unsigned[8]; let q2: unsigned[8]; let o: unsigned[8];
    let a: Fixed = { .clk = clk, .o = f };
    let b: Picked = { .clk = clk, .sel = sel, .o = p, .q0 = q0, .q2 = q2 };
    let c: One = { .clk = clk, .o = o };
    await 1ns;
    clk = '1'; await 1ns; clk = '0'; await 1ns;
    print!("fixed={} picked={} q0={} q2={} one={}\n", f, p, q0, q2, o);
    sel = 0; await 1ns;
    clk = '1'; await 1ns; clk = '0'; await 1ns;
    print!("sel0 picked={} q0={} q2={}\n", p, q0, q2);
}
"#;
