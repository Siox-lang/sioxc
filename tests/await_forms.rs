//! `await 5;` was accepted as an always-true condition and waited for
//! nothing, though `if 5` is rejected: a number is neither a time (it has no
//! unit) nor a condition. And a `time` literal that overflows the native
//! femtosecond timeline outside an `await` (`let t: time = ..ns;`) built,
//! then failed when the test ran.

use std::process::Command;

fn compile(name: &str, statement: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("siox_await_forms_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    let source = format!(
        "module m;\n#[test] entity T {{}}\nimpl T {{ let clk: Bit = '0'; clk = not clk after 5ns; \
         check: process {{ let n: integer = 2; let t: time = 3ns; {statement} }} }}\n"
    );
    std::fs::write(&file, source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .args(["--emit", "metadata"])
        .arg(&file)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status.success(), text)
}

#[test]
fn await_rejects_a_bare_number() {
    for statement in ["await 5;", "await n;"] {
        let (ok, text) = compile("number", statement);
        assert!(!ok && text.contains("not a number"), "{statement}: {text}");
    }
}

#[test]
fn await_accepts_times_edges_and_conditions() {
    for statement in [
        "await 5ns;",
        "await t;",
        "await clk.rising();",
        "await clk'event;",
        "await n == 2;",
        "await n == 2 for 5ns;",
        "await clk.rising() for t;",
    ] {
        let (ok, text) = compile("form", statement);
        assert!(ok, "{statement}: {text}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn an_overflowing_time_literal_is_a_build_error() {
    let dir = std::env::temp_dir().join(format!("siox_await_forms_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("overflow.siox");
    std::fs::write(
        &file,
        "module m;\n#[test] entity T {}\n\
         impl T { check: process { let t: time = 18446744073709551615ns; await t; } }\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .arg("--test")
        .arg(&file)
        .arg("-o")
        .arg(dir.join("overflow"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() && text.contains("exceeds the native 64-bit femtosecond timeline"),
        "{text}"
    );
}

#[test]
fn await_for_needs_a_condition_and_a_time() {
    let (ok, text) = compile("timeout_number", "await n == 2 for 5;");
    assert!(
        !ok && text.contains("timeout after `for` must be a time"),
        "{text}"
    );
    let (ok, text) = compile("timeout_delay", "await 5ns for 10ns;");
    assert!(!ok && text.contains("not a time"), "{text}");
}

/// `await <condition> for <time>` resumes at whichever comes first, and a
/// met condition disarms the timer so it never resumes the process later.
#[cfg(feature = "llvm")]
#[test]
fn await_for_resumes_at_the_condition_or_the_timeout() {
    let dir = std::env::temp_dir().join(format!("siox_await_for_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("await_for.siox");
    let binary = dir.join("await_for.bin");
    std::fs::write(&file, AWAIT_FOR).unwrap();
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
    let run = Command::new(&binary).output().unwrap();
    let text =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "{text}");
    let order = ["condition met", "timed out", "edge", "no stray timer"];
    let mut at = 0;
    for line in order {
        at = text[at..]
            .find(line)
            .map(|found| at + found)
            .unwrap_or_else(|| panic!("{line}: {text}"));
    }
}

const AWAIT_FOR: &str = r#"module probe;

#[test]
entity AwaitFor {}

impl AwaitFor {
    let clk: Bit;
    let done: Bit;
    let late: Bit;     // rises at 60ns
    let later: Bit;    // rises at 200ns

    clock: process { clk = not clk after 5ns; }
    responder: process {
        await 30ns;
        done = '1';
        await 30ns;
        late = '1';
        await 140ns;
        later = '1';
    }

    check: process {
        // the condition wins at 30ns, long before the 100ns timeout
        await done == '1' for 100ns;
        assert!(late == '0', "woke at 30ns, not at the timeout");
        print!("condition met");

        // the timeout wins at 50ns: done never falls
        await done == '0' for 20ns;
        assert!(done == '1' and late == '0', "timed out at 50ns");
        print!("timed out");

        // an edge with a timeout: the clock runs, so the edge wins
        await clk.rising() for 50ns;
        assert!(clk == '1' and late == '0', "rising edge before 60ns");
        print!("edge");

        // already true: returns at once
        await done == '1' for 5ns;

        // the first await's timer (due at 130ns) was disarmed: this wait
        // must run its full length to 200ns without a stray resume
        await later == '1';
        assert!(late == '1', "reached 200ns");
        print!("no stray timer");
    }
}
"#;
