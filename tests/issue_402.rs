//! Regressions for sioxc #402: a scoped block's flattening leaves a
//! function's parameters, a process's locals and loop variables alone when
//! they share a name with a block's private member; a loop variable or a
//! member hiding an outer name (a generic parameter included) is reported;
//! and an attribute binds to a block member by its dotted name.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i402_{name}_{}", std::process::id()));
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

#[cfg(feature = "llvm")]
#[test]
fn outer_names_sharing_a_private_members_name_keep_their_meaning() {
    let (ok, text, binary) = compile("ok", OK);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[test]
fn a_loop_variable_or_member_hiding_an_outer_name_is_reported() {
    let (ok, text, _) = compile("err", ERR);
    assert!(!ok, "{text}");
    assert!(
        text.contains("`N` is already visible from an enclosing scope"),
        "{text}"
    );
    assert!(
        text.contains("`n` is already visible from an enclosing scope"),
        "{text}"
    );
}

const OK: &str = r#"module p402_ok;
use std::sim::*;

attr keep: Bool for let = false;

#[test]
entity Tb {}

impl Tb {
    blk: {
        let temp: unsigned[8] = 0;
        let i: unsigned[8] = 0;
    }

    static fn helper(temp: unsigned[8]) -> unsigned[8] {
        return temp;
    }

    counter: {
        pub let wrapped: Bit = '0';
    }
    attr keep for counter.wrapped = true;

    process {
        let temp: unsigned[8] = 42;
        let total: integer = 0;
        for i in 0..3 {
            total = total + i;
        }
        assert!(helper(temp) == 42, "helper(temp)={}", helper(temp));
        assert!(total == 6, "total={}", total);
    }
}
"#;

const ERR: &str = r#"module p402_err;

entity Top<N: integer> {
    y: unsigned[8] out,
}

impl<N: integer> Top<N> {
    blk: {
        let N: unsigned[8] = 0;
    }
    y = 0;
}

#[test]
entity Tb {}

impl Tb {
    counter: {
        let n: unsigned[8] = 100;
        process {
            for n in 0..4 {
                print!("{}", n);
            }
        }
    }
}
"#;
