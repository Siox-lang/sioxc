//! Regression for sioxc #605 and #680: a value written to a process local
//! under an edge (`if clk.rising() { x = 42; }`, no `else`) holds only when
//! the edge comes. Read after the block it took the new value at time 0,
//! and a hardware `static let` register, which reads its update through
//! such a local, updated once before any edge.

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
