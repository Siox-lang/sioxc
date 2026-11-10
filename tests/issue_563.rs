//! Regression for sioxc #563: `'event`, `'old` and the edge methods
//! (`rising`, `falling`, `edge`) on a variable of a process body are
//! reported: a variable has no change in a delta cycle to read. They
//! passed and failed native lowering. A signal's edge still compiles and
//! runs.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i563_{name}_{}", std::process::id()));
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
fn an_edge_on_a_variable_is_reported() {
    let (ok, text, _) = compile("variable", VARIABLE);
    assert!(!ok, "{text}");
    assert!(
        text.contains("`a` is a variable of this body, so `.rising()` has no change to read"),
        "{text}"
    );
    assert!(
        text.contains("`a` is a variable of this body, so `'event` has no change to read"),
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_signals_edge_runs() {
    let (ok, text, binary) = compile("signal", SIGNAL);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success(), "{stdout}");
}

const VARIABLE: &str = r#"module p563;

#[test]
entity TB {}
impl TB {
    let clk: Logic;
    run: process {
        let a: Logic = '1';
        let r: Bool = a.rising();
        let e: Bool = a'event;
        clk = '1';
        await 1ns;
    }
    watch: process {
        if clk.rising() {
            print!("edge\n");
        }
    }
}
"#;

const SIGNAL: &str = r#"module p563_signal;

#[test]
entity TB {}
impl TB {
    let clk: Logic;
    run: process {
        clk = '0';
        await 1ns;
        clk = '1';
        await 1ns;
    }
    watch: process {
        if clk.rising() {
            print!("edge\n");
            assert!(clk'event, "event");
        }
    }
}
"#;
