//! Regression for sioxc #606: a type attribute is read on a type name
//! (`Packet'total_bytes`) and sizes a vector, by type name or by instance
//! (`unsigned[p'total_bytes]`). A type name had no value type, so the read
//! was an "unknown system attribute"; in a width it was never recorded, so
//! hardware had an unresolved width and a testbench read it as 64.

use std::process::Command;

fn compile(name: &str, source: &str, test: bool) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i606_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_sioxc"));
    command.args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")]);
    if test {
        command.arg("--test");
    }
    let output = command
        .arg(&file)
        .arg("-o")
        .arg(dir.join("t.bin"))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status.success(), text, dir.join("t.bin"))
}

#[cfg(feature = "llvm")]
#[test]
fn type_attributes_read_on_types_and_size_vectors() {
    for (name, source) in [
        ("name", NAME),
        ("instance", INSTANCE),
        ("named_width", NAMED_WIDTH),
    ] {
        let (ok, text, binary) = compile(name, source, true);
        assert!(ok, "{name}: {text}");
        let run = Command::new(binary).output().unwrap();
        assert!(
            run.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&run.stdout)
        );
    }
    let (ok, text, _) = compile("hardware", HARDWARE, false);
    assert!(ok, "{text}");
}

const NAME: &str = r#"module test_attr;

struct Packet {
    pub header: unsigned[8],
    pub payload: unsigned[16]
}

impl Packet {
    pub attr total_bytes: integer = 3;
}

#[test]
entity Tb {}

impl Tb {
    run: process {
        // Expected: compiles and asserts that Packet'total_bytes == 3.
        // Actual: error[E-P001]: unknown system attribute 'total_bytes'
        assert!(Packet'total_bytes == 3);
    }
}
"#;

const INSTANCE: &str = r#"module test_attr_width;

struct Packet {
    pub header: unsigned[8],
    pub payload: unsigned[16]
}

impl Packet {
    pub attr total_bytes: integer = 3;
}

#[test]
entity Tb {}

impl Tb {
    run: process {
        let p: Packet;
        let data: unsigned[p'total_bytes] = 5;
        // Expected: data'length == 3.
        // Actual: data'length == 64 (assertion fails).
        assert!(data'length == 3);
    }
}
"#;

const NAMED_WIDTH: &str = r#"module named606;

struct Packet {
    pub header: unsigned[8],
    pub payload: unsigned[16]
}

impl Packet {
    pub attr total_bytes: integer = 3;
}

#[test]
entity Tb {}

impl Tb {
    run: process {
        let p: Packet;
        let data: unsigned[Packet'total_bytes] = 5;
        // Expected: data'length == 3.
        // Actual: data'length == 64 (assertion fails).
        assert!(data'length == 3);
    }
}
"#;

const HARDWARE: &str = r#"module test_attr_hw;

struct Packet {
    pub header: unsigned[8],
    pub payload: unsigned[16]
}

impl Packet {
    pub attr total_bytes: integer = 3;
}

entity Top {}

impl Top {
    let p: Packet;
    let data: unsigned[p'total_bytes];
}
"#;
