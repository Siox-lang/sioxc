//! Regression for sioxc #474: a conversion to a vector family or kernel
//! type from a struct is checked for a `From` when the argument is a place
//! built from names (`pkts[0]`) or a struct literal naming its type, as it
//! was for a bare name; element and field conversions still run.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i474_{name}_{}", std::process::id()));
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
fn conversions_from_struct_places_are_checked() {
    let (ok, text, _) = compile("process", PROCESS);
    assert!(!ok, "{text}");
    assert!(
        text.contains("no conversion from `Pkt` to `unsigned[8]`"),
        "{text}"
    );
    assert!(
        text.contains("no conversion from `Pkt` to `integer`"),
        "{text}"
    );
    let (ok, text, _) = compile("hardware", HARDWARE);
    assert!(!ok, "{text}");
    assert!(
        text.contains("no conversion from `Pkt` to `integer`"),
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn element_and_field_conversions_still_run() {
    let (ok, text, binary) = compile("valid", VALID);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const PROCESS: &str = r#"module test_composite_conv;

use std::bits::unsigned;

struct Pkt {
    pub a: integer,
    pub b: integer,
}

#[test]
entity Top {}
impl Top {
    check: process {
        let pkt: Pkt = { .a = 1, .b = 2 };
        let pkts: Pkt[1] = [pkt];

        // 1. Bare variable is caught as expected:
        // let bad: unsigned[8] = unsigned[8](pkt); // E-P003

        // 2. Non-path expression bypasses check and silently compiles:
        let u: unsigned[8] = unsigned[8](pkts[0]);
        print!("u = {}\n", u);

        // 3. Struct literal passes typecheck but crashes at runtime:
        let x: integer = integer(Pkt { .a = 1, .b = 2 });
    }
}
"#;

const HARDWARE: &str = r#"module test_composite_hw;

struct Pkt {
    pub a: integer,
    pub b: integer,
}

entity Top {
    res: integer out
}
impl Top {
    p: process {
        let pkt: Pkt = { .a = 1, .b = 2 };
        let pkts: Pkt[1] = [pkt];
        res = integer(pkts[0]);
    }
}
"#;

const VALID: &str = r#"module p474_ok;

struct Pkt {
    pub a: unsigned[8],
    pub b: unsigned[8],
}

#[test]
entity Top {}
impl Top {
    check: process {
        let pkt: Pkt = { .a = 1, .b = 2 };
        let pkts: Pkt[1] = [pkt];
        let words: unsigned[8][2] = [3, 4];
        let w: unsigned[8] = unsigned[8](words[1]);
        let a: integer = integer(pkts[0].a);
        assert!(w == 4 and a == 1, "conversions");
    }
}
"#;
