//! `while cond { ... }`: a loop of run-time length, for testbenches and the
//! functions they call. Hardware loops unroll at elaboration, so a `while` in
//! an entity body, or in a function hardware calls, is an error.

use std::process::Command;

fn sioxc(dir: &std::path::Path, name: &str, source: &str, args: &[&str]) -> (bool, String) {
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .args(args)
        .arg(&file)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status.success(), text)
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("siox_while_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn while_is_testbench_only() {
    let dir = scratch("hardware");
    let (ok, text) = sioxc(&dir, "bad", BAD, &["--emit", "metadata"]);
    assert!(!ok, "{text}");
    assert!(
        text.contains("`while` is only available in a testbench"),
        "{text}"
    );
    assert!(text.contains("`spin` reaches `while`"), "{text}");
}

#[cfg(feature = "llvm")]
#[test]
fn while_loops_run_in_testbenches_and_their_functions() {
    let dir = scratch("run");
    let binary = dir.join("loops.bin");
    let (ok, text) = sioxc(
        &dir,
        "loops",
        GOOD,
        &["--test", "-o", binary.to_str().unwrap()],
    );
    assert!(ok, "{text}");
    let run = Command::new(&binary).output().unwrap();
    let text =
        String::from_utf8_lossy(&run.stdout).to_string() + &String::from_utf8_lossy(&run.stderr);
    assert!(run.status.success(), "{text}");
    assert!(text.contains("count reached 6 after 6 edges"), "{text}");
    assert!(text.contains("test probe::Loops ... ok"), "{text}");
}

const GOOD: &str = r#"module probe;

fn collatz_steps(start: integer) -> integer {
    let n: integer = start;
    let steps: integer = 0;
    while n != 1 {
        if n % 2 == 0 { n = n / 2; } else { n = 3 * n + 1; }
        steps = steps + 1;
    }
    return steps;
}

#[test]
entity Loops {}

impl Loops {
    let clk: Bit;
    let count: unsigned[4];
    clock: process { clk = not clk after 5ns; }
    counter: process {
        while true {
            await clk.rising();
            count = count + 1;
        }
    }

    check: process {
        // plain counting loop
        let i: integer = 0;
        let sum: integer = 0;
        while i < 5 {
            let sq: integer = i * i;           // a fresh let each pass
            sum = sum + sq;
            i = i + 1;
        }
        assert!(sum == 30, "0+1+4+9+16");

        // a loop that never runs
        while i < 0 { sum = 0; }
        assert!(sum == 30, "body skipped");

        // a nested for
        let pairs: integer = 0;
        let j: integer = 0;
        while j < 3 {
            for k in 0..1 { pairs = pairs + 1; }
            j = j + 1;
        }
        assert!(pairs == 6, "3 x 2");

        // a function with a loop
        assert!(collatz_steps(6) == 8, "6 3 10 5 16 8 4 2 1");

        // polling with await inside the loop
        let edges: integer = 0;
        while count != 6 {
            await clk.rising();
            edges = edges + 1;
        }
        print!("count reached 6 after {} edges", edges);
        finish();
    }
}
"#;

const BAD: &str = r#"module probe;

fn spin(n: integer) -> integer {
    let k: integer = n;
    while k > 0 { k = k - 1; }
    return k;
}

entity Hw { a: integer in, y: integer out }
impl Hw {
    y = spin(a);
    p: process {
        while a > 0 { }
    }
}
"#;
