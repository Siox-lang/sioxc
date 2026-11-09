//! Regression for sioxc #574: a trait's default body is checked against
//! each type that inherits it, and a method read without its call
//! (`self.id` where `id` is a method, not a field) is reported. Generic,
//! the body typed `self` as nothing, and the bare method read passed the
//! checker on any struct and crashed lowering.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i574_{name}_{}", std::process::id()));
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
fn an_inherited_default_reads_the_implementing_type() {
    let (ok, text, _) = compile("missing", MISSING);
    assert!(!ok, "{text}");
    assert!(
        text.contains("`id` is a method of `Device`, not a field"),
        "{text}"
    );
    let (ok, text, _) = compile("fields", FIELDS);
    assert!(!ok, "{text}");
    assert_eq!(
        text.matches("`Device` has no field `nope`").count(),
        3,
        "{text}"
    );
}

#[test]
fn a_bare_method_read_is_reported() {
    let (ok, text, _) = compile("bare", BARE);
    assert!(!ok, "{text}");
    assert!(
        text.contains("`tag` is a method of `Device`, not a field"),
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_default_reading_a_present_field_runs() {
    let (ok, text, binary) = compile("present", PRESENT);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const MISSING: &str = r#"module test_trait_field_hw;

trait Identified {
    fn id() -> integer {
        return self.id; // Accesses field 'id' on self
    }
}

struct Device {
    pub name: integer, // Device does NOT have field 'id'!
}

impl Identified for Device {}

entity Top {
    out_id: integer out,
}

impl Top {
    let d: Device = { .name = 42 };

    process {
        out_id = d.id();
    }
}
"#;

const FIELDS: &str = r#"module f;
struct Device { pub name: integer }
trait Identified {
    fn id() -> integer {
        return self.nope;
    }
}
impl Identified for Device {}
impl Device {
    fn other() -> integer {
        return self.nope;
    }
}
#[test] entity TB {}
impl TB { process { let d: Device = { .name = 1 }; print!("{}\n", d.nope); } }
"#;

const BARE: &str = r#"module g;
struct Device { pub name: integer }
impl Device {
    fn tag() -> integer {
        return 3;
    }
}
#[test] entity TB {}
impl TB { process { let d: Device = { .name = 1 }; let x: integer = d.tag; print!("{}\n", x); } }
"#;

const PRESENT: &str = r#"module p574;

trait Identified {
    fn id() -> integer {
        return self.id;
    }
}

struct Tagged {
    pub id: integer,
}

impl Identified for Tagged {}

#[test]
entity TB {}
impl TB {
    process {
        let t: Tagged = { .id = 7 };
        assert!(t.id() == 7, "id {}", t.id());
        print!("done\n");
    }
}
"#;
