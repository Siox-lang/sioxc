//! Regression for sioxc #686 and #713. `Self::make(v)` in a generic impl
//! is `Boxed<T>::make(v)`, the form that lowers, and `Self(v)` / `Self()`
//! construct the impl's target; both were left as `Self` and failed. An
//! alias chain across modules (`Pt2 = Pt1 = Point`) and a glob import of a
//! module's aliases reach the type an impl is for.

use std::path::Path;
use std::process::Command;

fn runs(dir: &Path, file: &str) -> String {
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .current_dir(dir)
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
            file,
        ])
        .arg("-o")
        .arg(dir.join(format!("{file}.bin")))
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{file}: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join(format!("{file}.bin")))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout).to_string();
    assert!(
        run.status.success() && stdout.contains("done"),
        "{file}: {stdout}"
    );
    stdout
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("siox_i686_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(feature = "llvm")]
#[test]
fn self_reaches_generic_impls_and_constructors() {
    let dir = scratch("self");
    std::fs::write(dir.join("generic_self.siox"), GENERIC_SELF).unwrap();
    let out = runs(&dir, "generic_self.siox");
    assert!(out.contains("42 7 0"), "{out}");
}

#[cfg(feature = "llvm")]
#[test]
fn alias_chains_and_glob_imports_reach_the_impl_target() {
    let dir = scratch("aliases");
    std::fs::write(dir.join("types_mod.siox"), TYPES_MOD).unwrap();
    std::fs::write(dir.join("main.siox"), CHAIN).unwrap();
    std::fs::write(dir.join("globbed.siox"), GLOBBED).unwrap();
    runs(&dir, "main.siox");
    runs(&dir, "globbed.siox");
}

const GENERIC_SELF: &str = r#"module p686;

pub struct Boxed<T> {
    pub val: T,
}

impl<T> Boxed<T> {
    pub static fn make(v: T) -> Boxed<T> {
        return Boxed<T> { .val = v };
    }

    pub static fn twice(v: T) -> Boxed<T> {
        return Self::make(v);
    }
}

pub struct Wrap(integer);

impl Wrap {
    pub static fn build(v: integer) -> Wrap {
        return Self(v);
    }
}

pub struct Pt {
    pub x: integer,
}

impl Pt {
    pub static fn origin() -> Pt {
        return Self();
    }
}

entity Top {
    x: integer in,
    y: integer out,
}

impl Top {
    process {
        let b: Boxed<integer> = Boxed<integer>::twice(x);
        y = b.val;
    }
}

#[test]
entity Tb {}

impl Tb {
    let x: integer = 42;
    let y: integer;
    let top: Top = { .x = x, .y = y };
    process {
        await 1ns;
        let b: Boxed<integer> = Boxed<integer>::twice(7);
        let w: Wrap = Wrap::build(10);
        let o: Pt = Pt::origin();
        print!("{} {} {}\n", y, b.val, o.x);
        assert!(y == 42 and b.val == 7 and o.x == 0);
        print!("done\n");
    }
}
"#;

const TYPES_MOD: &str = r#"module types_mod;

pub struct Point {
    pub x: integer,
}

pub type Pt1 = Point;
pub type Pt2 = Pt1;
"#;

const CHAIN: &str = r#"module main;

use types_mod::Pt2;

pub trait Doubler {
    fn double() -> integer;
}

impl Doubler for Pt2 {
    fn double() -> integer {
        return self.x * 2;
    }
}

#[test]
entity Tb {}

impl Tb {
    process {
        let p: Pt2 = Pt2 { .x = 21 };
        print!("{}\n", p.double());
        assert!(p.double() == 42);
        print!("done\n");
    }
}
"#;

const GLOBBED: &str = r#"module globbed;

use types_mod::*;

pub trait Doubler {
    fn double() -> integer;
}

impl Doubler for Pt1 {
    fn double() -> integer {
        return self.x * 2;
    }
}

#[test]
entity Tb {}

impl Tb {
    process {
        let p: Pt2 = Pt2 { .x = 21 };
        print!("{}\n", p.double());
        assert!(p.double() == 42);
        print!("done\n");
    }
}
"#;
