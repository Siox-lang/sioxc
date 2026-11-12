//! Regression for sioxc #605, #680, #689, #690 and #693: a value written to a process local
//! under an edge (`if clk.rising() { x = 42; }`, no `else`) holds only when
//! the edge comes. Read after the block it took the new value at time 0,
//! and a hardware `static let` register, which reads its update through
//! such a local, updated once before any edge. A local written through a
//! method or procedure call (#689) or a destructuring (#693) skipped that
//! guard, and the guard lacked the enclosing condition (#690).

use std::process::Command;

fn runs(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_i605_{name}_{}", std::process::id()));
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
    assert!(
        build.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout).to_string();
    assert!(run.status.success(), "{name}: {stdout}");
    stdout
}

#[cfg(feature = "llvm")]
#[test]
fn a_local_under_an_edge_holds_without_it() {
    runs("local", LOCAL);
}

#[cfg(feature = "llvm")]
#[test]
fn a_static_register_waits_for_its_edge() {
    let out = runs("register", REGISTER);
    assert!(out.contains("t0 v = 0") && out.contains("v = 1"), "{out}");
}

#[cfg(feature = "llvm")]
#[test]
fn a_local_written_by_a_call_under_an_edge_waits_for_it() {
    runs("call", CALL);
}

#[cfg(feature = "llvm")]
#[test]
fn a_local_under_a_nested_edge_holds_its_outer_condition() {
    runs("nested", NESTED);
}

#[cfg(feature = "llvm")]
#[test]
fn a_destructured_local_under_an_edge_waits_for_it() {
    runs("tuple", TUPLE);
}

const LOCAL: &str = r#"module test_local;

entity Top {
    clk: Bit in,
    q: unsigned[8] out
}

impl Top {
    p: process {
        let x: unsigned[8] = 0;
        if clk.rising() {
            x = 42;
        }
        q = x;
    }
}

#[test]
entity Tb {}

impl Tb {
    let clk: Bit = '0';
    let q: unsigned[8];
    let dut: Top = { .clk = clk, .q = q };

    run: process {
        // At time 0, clk is '0' and has never risen.
        // Expected: q == 0.
        // Actual: q == 42.
        assert!(q == 0);
    }
}
"#;

const REGISTER: &str = r#"module r633;

use std::bits::unsigned;

pub struct Counter {
    pub v: unsigned[8],
}
impl Counter {
    pub fn inc() {
        self.v = self.v + 1;
    }
}

entity Top {
    clk: Bit in,
    out_v: unsigned[8] out,
}
impl Top {
    process {
        static let n: unsigned[8] = 0;
        if clk.rising() {
            n = n + 1;
        }
        out_v = n;
    }
}

#[test] entity T {}
impl T {
    let clk: Bit;
    let out_v: unsigned[8];
    let top: Top = { .clk = clk, .out_v = out_v };
    process {
        await 1ns;
        print!("t0 v = {}\n", out_v);
        clk = '1'; await 1ns;
        print!("v = {}\n", out_v);
        print!("done\n");
    }
}
"#;

const CALL: &str = r#"module test_edge_proc_local;

use std::bits::unsigned;

pub struct Counter {
    pub v: unsigned[8],
}
impl Counter {
    pub fn inc() {
        self.v = self.v + 1;
    }
}

entity TopProcLocal {
    clk: Bit in,
    out_v: unsigned[8] out,
}
impl TopProcLocal {
    process {
        let c: Counter = { .v = 0 };
        if clk.rising() {
            c.inc();
        }
        out_v = c.v;
    }
}

entity TopStaticMethod {
    clk: Bit in,
    out_v: unsigned[8] out,
}
impl TopStaticMethod {
    process {
        static let c: Counter = { .v = 0 };
        if clk.rising() {
            c.inc();
        }
        out_v = c.v;
    }
}

#[test]
entity Tb {}
impl Tb {
    let clk: Bit = '0';
    let v1: unsigned[8];
    let v2: unsigned[8];
    let d1: TopProcLocal = { .clk = clk, .out_v = v1 };
    let d2: TopStaticMethod = { .clk = clk, .out_v = v2 };

    run: process {
        await 1ns;
        print!("t0 (clk=0): v1={} v2={}\n", v1, v2);
        assert!(v1 == 0, "expected v1 == 0 before clock edge, got {}", v1);
        assert!(v2 == 0, "expected v2 == 0 before clock edge, got {}", v2);

        clk = '1';
        await 1ns;
        print!("after 1 edge: v1={} v2={}\n", v1, v2);
        assert!(v2 == 1, "expected v2 == 1 after 1 clock edge, got {}", v2);
    }
}
"#;

const NESTED: &str = r#"module test_nested_edge_local;

use std::bits::unsigned;

entity TopEnLocal {
    clk: Bit in,
    en: Bit in,
    out_v: unsigned[8] out,
    out_else: unsigned[8] out,
}
impl TopEnLocal {
    process {
        let x: unsigned[8] = 10;
        let y: unsigned[8] = 10;
        if en == '1' {
            if clk.rising() {
                x = 42;
            }
            if clk.rising() {
                y = 42;
            } else {
                y = 99;
            }
        }
        out_v = x;
        out_else = y;
    }
}

#[test]
entity Tb {}
impl Tb {
    let clk: Bit = '0';
    let en: Bit = '0';
    let v3: unsigned[8];
    let v4: unsigned[8];
    let d3: TopEnLocal = { .clk = clk, .en = en, .out_v = v3, .out_else = v4 };

    run: process {
        await 1ns;
        print!("t0 (clk=0, en=0): v3={} v4={}\n", v3, v4);
        assert!(v4 == 10, "expected v4 == 10 when en == '0', got {}", v4);

        // Rise clk while en == '0'
        clk = '1';
        await 1ns;
        print!("clk=1, en=0: v3={} v4={}\n", v3, v4);
        assert!(v3 == 10, "expected v3 == 10 when en == '0', got {}", v3);
    }
}
"#;

const TUPLE: &str = r#"module test_tuple_edge;

use std::bits::unsigned;

fn make_pair() -> (unsigned[8], unsigned[8]) {
    return (11, 22);
}

entity TopTupleEdge {
    clk: Bit in,
    out_a: unsigned[8] out,
}
impl TopTupleEdge {
    process {
        let a: unsigned[8] = 0;
        let b: unsigned[8] = 0;
        if clk.rising() {
            (a, b) = make_pair();
        }
        out_a = a;
    }
}

#[test]
entity Tb {}
impl Tb {
    let clk: Bit = '0';
    let v1: unsigned[8];
    let d1: TopTupleEdge = { .clk = clk, .out_a = v1 };
    process {
        await 1ns;
        print!("t0 (clk=0): v1={}\n", v1);
        assert!(v1 == 0, "expected v1 == 0 before clock edge, got {}", v1);
    }
}
"#;
