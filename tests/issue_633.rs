//! Regression for sioxc #633: a statement-level method call on a hardware
//! `static let` (`c.inc();`) writes its receiver, so outside a clock edge
//! it is reported, as an assignment is; it bypassed the feedback check.
//! Under an edge it runs.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i633_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
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
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text, dir.join("t.bin"))
}

#[test]
fn a_method_write_outside_an_edge_is_reported() {
    let (ok, text, _) = compile("outside", OUTSIDE);
    assert!(!ok, "{text}");
    assert!(
        text.contains("`c.inc()` may write `c` here without one"),
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_method_write_under_an_edge_runs() {
    let (ok, text, binary) = compile("inside", INSIDE);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success() && stdout.contains("done"), "{stdout}");
}

const OUTSIDE: &str = r#"module test_static_method_outside;

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
    out_v: unsigned[8] out,
}
impl Top {
    process {
        static let c: Counter = Counter { .v = 0 };
        // Mutating method called OUTSIDE a clock edge:
        c.inc();
        out_v = c.v;
    }
}

#[test] entity T {}
impl T {
    let out_v: unsigned[8];
    let top: Top = { .out_v = out_v };
    process {
        await 1ns;
        print!("out_v={}\n", out_v);
        assert!(out_v == 1);
    }
}
"#;

const INSIDE: &str = r#"module p633;

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
        static let c: Counter = Counter { .v = 0 };
        if clk.rising() {
            c.inc();
        }
        out_v = c.v;
    }
}

#[test] entity T {}
impl T {
    let clk: Bit;
    let out_v: unsigned[8];
    let top: Top = { .clk = clk, .out_v = out_v };
    process {
        clk = '0'; await 1ns; clk = '1'; await 1ns;
        clk = '0'; await 1ns; clk = '1'; await 1ns;
        print!("v = {}\n", out_v);
        print!("done\n");
    }
}
"#;
