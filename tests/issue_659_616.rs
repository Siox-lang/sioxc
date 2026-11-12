//! Regressions for sioxc #659 and #616: paths in impls. `Self::make(42)`
//! in `impl Foo` calls `Foo::make`; `Self` resolved as a local and the call
//! did not lower (#659). And an impl on an alias imported from another
//! module (`use types_mod::Pt;`) or named by path (`types_mod::Pt`)
//! implements the aliased type, as a local alias's does; it was filed under
//! the alias's name and `p.double()` found no method (#616).

use std::process::Command;

fn runs(dir_name: &str, files: &[(&str, &str)], main: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i659_{dir_name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, source) in files {
        std::fs::write(dir.join(name), source).unwrap();
    }
    let build = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
        ])
        .arg(dir.join(main))
        .arg("-o")
        .arg(dir.join("t.bin"))
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{dir_name}: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    assert!(
        run.status.success(),
        "{dir_name}: {}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_self_path_calls_the_impl_target() {
    runs("self", &[("self.siox", SELF)], "self.siox");
}

#[cfg(feature = "llvm")]
#[test]
fn an_imported_or_qualified_alias_implements_its_type() {
    runs(
        "imported",
        &[("types_mod.siox", TYPES), ("main.siox", IMPORTED)],
        "main.siox",
    );
    runs(
        "qualified",
        &[("types_mod.siox", TYPES), ("main.siox", QUALIFIED)],
        "main.siox",
    );
}

const SELF: &str = r#"module test;

struct Foo {
    pub x: integer,
}

impl Foo {
    pub static fn make(val: integer) -> integer {
        return val + 1;
    }

    pub static fn test() -> integer {
        return Self::make(42);
    }
}

#[test]
entity TestSelf {}

impl TestSelf {
    process {
        let res: integer = Foo::test();
        assert!(res == 43);
    }
}
"#;

const TYPES: &str = r#"module types_mod;

pub struct Point {
    pub x: integer,
}

pub type Pt = Point;
"#;

const IMPORTED: &str = r#"module test_imported_alias_impl;

use types_mod::Point;
use types_mod::Pt;

pub trait Doubler {
    fn double() -> integer;
}

impl Doubler for Pt {
    fn double() -> integer {
        return self.x * 2;
    }
}

entity Top {
    res: integer out,
}

impl Top {
    let p: Point = Point { .x = 21 };
    res = p.double();
}

#[test]
entity TB {}
impl TB {
    let res: integer;
    let dut: Top = { .res = res };
    run: process {
        await 1ns;
        assert!(res == 42, "res {}", res);
        print!("done\n");
    }
}
"#;

const QUALIFIED: &str = r#"module test_qualified_alias_impl;

use types_mod::Point;

pub trait Doubler {
    fn double() -> integer;
}

impl Doubler for types_mod::Pt {
    fn double() -> integer {
        return self.x * 2;
    }
}

entity Top {
    res: integer out,
}

impl Top {
    let p: Point = Point { .x = 21 };
    res = p.double();
}

#[test]
entity TB {}
impl TB {
    let res: integer;
    let dut: Top = { .res = res };
    run: process {
        await 1ns;
        assert!(res == 42, "res {}", res);
        print!("done\n");
    }
}
"#;
