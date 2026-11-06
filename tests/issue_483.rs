//! Regression for sioxc #483: `for b in <expr>` over a packed vector that
//! is not a name (a call's result, a slice, an operator's result) is
//! evaluated once and iterated element by element, in the order `for b in v`
//! uses; it left the process unlowered.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn packed_expressions_are_iterated_once_each() {
    let dir = std::env::temp_dir().join(format!("siox_i483_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("for_packed.siox");
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
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success(), "{stdout}");
    assert_eq!(
        stdout.matches("call").count(),
        1,
        "the call runs once: {stdout}"
    );
}

const SOURCE: &str = r#"module p483;

fn get_val() -> unsigned[8] {
    print!("call\n");
    return 42;
}

#[test]
entity Top {}
impl Top {
    check: process {
        let ones: integer = 0;
        for b in get_val() {
            if b == '1' { ones = ones + 1; }
        }
        assert!(ones == 3, "call ones={}", ones);

        let v: unsigned[8] = 42;
        let low: integer = 0;
        for b in v[3..0] {
            if b == '1' { low = low + 1; }
        }
        assert!(low == 2, "slice ones={}", low);

        let masked: integer = 0;
        for b in (v and 15) {
            if b == '1' { masked = masked + 1; }
        }
        assert!(masked == 2, "masked ones={}", masked);

        // Elements run up from bit 0, as `for b in v` does.
        let order: integer = 0;
        let at: integer = 0;
        for b in (v and 15) {
            if b == '1' { order = order + at; }
            at = at * 10 + 1;
        }
        let named: integer = 0;
        at = 0;
        let w: unsigned[8] = v and 15;
        for b in w {
            if b == '1' { named = named + at; }
            at = at * 10 + 1;
        }
        assert!(order == named, "order {} vs {}", order, named);
        print!("done\n");
    }
}
"#;
