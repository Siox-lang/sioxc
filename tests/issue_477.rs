//! Regression for sioxc #477: a generic method's bounds
//! (`fn add_em<T: Add<T, T>>`) are checked at its calls, through a receiver
//! and qualified, as a generic function's are; valid calls still run.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i477_{name}_{}", std::process::id()));
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
fn generic_method_bounds_are_checked() {
    let (ok, text, _) = compile("unbounded", UNBOUNDED);
    assert!(!ok, "{text}");
    assert_eq!(
        text.matches("`NoAdd` does not satisfy the bound `T: Add`")
            .count(),
        2,
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn bounded_method_calls_still_run() {
    let (ok, text, binary) = compile("bounded", BOUNDED);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const UNBOUNDED: &str = r#"module test_method_bound;

use core::ops::Add;

struct NoAdd {
    pub v: integer,
}

struct Helper {
    pub dummy: integer,
}
impl Helper {
    pub fn add_em<T: Add<T, T>>(a: T, b: T) -> T {
        return a + b;
    }

    pub static fn static_add<T: Add<T, T>>(a: T, b: T) -> T {
        return a + b;
    }
}

#[test]
entity Top {}
impl Top {
    check: process {
        let h: Helper = { .dummy = 0 };
        let x: NoAdd = { .v = 1 };
        let y: NoAdd = { .v = 2 };

        // Both calls below violate 'T: Add<T, T>', but pass typechecking:
        let z1: NoAdd = h.add_em(x, y);
        let z2: NoAdd = Helper::static_add(x, y);

        print!("z1.v = {}\n", z1.v);
    }
}
"#;

const BOUNDED: &str = r#"module p477_ok;

use core::ops::Add;

struct Helper {
    pub dummy: integer,
}
impl Helper {
    pub fn add_em<T: Add<T, T>>(a: T, b: T) -> T {
        return a + b;
    }

    pub static fn static_add<T: Add<T, T>>(a: T, b: T) -> T {
        return a + b;
    }
}

#[test]
entity Top {}
impl Top {
    check: process {
        let h: Helper = { .dummy = 0 };
        let a: integer = 2;
        let b: integer = 3;
        let x: integer = h.add_em(a, b);
        let y: integer = Helper::static_add(a, 4);
        assert!(x == 5 and y == 6, "bounded methods");
    }
}
"#;
