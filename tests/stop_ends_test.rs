//! `stop()` halts the test, which passes so far, as documented: every
//! process of it ends. It used to end only the calling process, so a test
//! with a process looping forever (here a free-running counter) never ended.

#![cfg(feature = "llvm")]

use std::process::Command;

const SOURCE: &str = r#"module probe;

#[test]
entity Stops {}

impl Stops {
    let clk: Bit;
    let count: unsigned[4];
    clock: process { clk = not clk after 5ns; }
    counter: process {
        for i in 0..1000000 {
            await clk.rising();
            count = count + 1;
        }
    }
    check: process {
        await count == 3;
        print!("stopping at count {}", count);
        stop();
    }
}

#[test]
entity After {}

impl After {
    check: process {
        await 1ns;
        print!("the next test still runs");
    }
}
"#;

#[test]
fn stop_ends_every_process_of_the_test() {
    let dir = std::env::temp_dir().join(format!("siox_stop_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("stop.siox");
    let binary = dir.join("stop.bin");
    std::fs::write(&file, SOURCE).unwrap();
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
        ])
        .arg(&file)
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    // A test that does not stop runs a million clock edges; bound it.
    let run = Command::new("timeout")
        .arg("20")
        .arg(&binary)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "{text}");
    assert!(text.contains("stopping at count 3"), "{text}");
    assert!(text.contains("test probe::Stops ... ok"), "{text}");
    assert!(!text.contains("finish at"), "stop is not finish: {text}");
    assert!(text.contains("the next test still runs"), "{text}");
}
