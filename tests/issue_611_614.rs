//! Regressions for sioxc #611 and #614: generate conditions and arguments
//! fold as the rest of the compiler does. `not` is boolean negation (it was
//! bitwise in elaboration, so `if not true` took the wrong branch), a
//! literal `true`/`false` folds in lowering too (it read as a run-time
//! condition and the branch's instance drove nothing), and a qualified
//! constant (`std::sim::FS`, `pkg::ON`) is the module constant it names.

use std::process::Command;

fn run(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_i611_{name}_{}", std::process::id()));
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
fn a_generate_if_not_true_takes_its_else() {
    assert!(run("not", NOT).contains("dut.out_val = 42"));
}

#[cfg(feature = "llvm")]
#[test]
fn qualified_constants_fold_in_generates() {
    assert!(run("qualified", QUALIFIED).contains("done"));
}

const NOT: &str = r#"module test_gen_not;

entity Cell {
    val: unsigned[8] out,
}

impl Cell {
    val = 42;
}

entity Probe {
    val: unsigned[8] out,
}

impl Probe {
    val = 99;
}

entity Top {
    out_val: unsigned[8] out,
}

impl Top {
    if not true {
        let u: Probe = { .val = out_val };
    } else {
        let u: Cell = { .val = out_val };
    }
}

#[test]
entity TestTop {}

impl TestTop {
    let dut: Top;
    p: process {
        await 1ns;
        print!("dut.out_val = {}\n", dut.out_val);
        assert!(dut.out_val == 42);
    }
}
"#;

const QUALIFIED: &str = r#"module p614;

use std::sim;

const ON: Bool = true;

entity Cell<N: integer> {
    y: integer out,
}

impl<N: integer> Cell<N> {
    y = N;
}

entity Top {
    a: integer out,
    b: integer out,
}

impl Top {
    if std::sim::PS == 1000 {
        let c: Cell<N = std::sim::FS> = { .y = a };
    } else {
        a = 0;
    }
    if not p614::ON {
        b = 0;
    } else {
        let d: Cell<N = 7> = { .y = b };
    }
}

#[test]
entity TB {}
impl TB {
    let a: integer;
    let b: integer;
    let dut: Top = { .a = a, .b = b };
    process {
        await 1ns;
        assert!(a == 1 and b == 7, "a {} b {}", a, b);
        print!("done\n");
    }
}
"#;
