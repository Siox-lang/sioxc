//! Regression for sioxc #403: a generic comparison impl's parameter bounds
//! (`impl<T: Eq> Eq<Box<T>> for Box<T>`, as `#[derive(Eq, Ord)]` writes
//! them) hold for the type arguments compared. `Container<Uncomparable>`
//! compared through them, crashing hardware lowering and ordering a
//! testbench comparison by raw struct bits.

use std::process::Command;

fn build(name: &str, source: &str) -> (std::process::Output, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i403_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
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
    (output, dir.join("t.bin"))
}

#[cfg(feature = "llvm")]
#[test]
fn generic_comparison_bounds_hold() {
    let (rejected, _) = build("unmet", UNMET);
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(!rejected.status.success(), "{stderr}");
    assert_eq!(stderr.matches("has no `Eq`").count(), 2, "{stderr}");
    assert_eq!(stderr.matches("has no `Ord`").count(), 2, "{stderr}");

    let (built, binary) = build("met", MET);
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let run = Command::new(binary).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(run.status.success() && stdout.contains("done"), "{stdout}");
}

const UNMET: &str = r#"module test_generic_derive_and_impl_bounds;
use std::sim::*;

struct Uncomparable {
    pub x: unsigned[8],
    pub y: unsigned[8],
}

// 1. #[derive(Eq, Ord)] on generic struct generates  /  without  / 
#[derive(Eq, Ord)]
struct Container<T> {
    pub val: T,
}

// 2. Manual impl with explicit bound 
struct ManualContainer<T> {
    pub val: T,
}

impl<T: Eq> Eq<ManualContainer<T>> for ManualContainer<T> {
    fn eq(rhs: ManualContainer<T>) -> Bool {
        return self.val == rhs.val;
    }
}

// Hardware compilation (--top HwCmp): passes Stage 4 and crashes scheduler lowering with E-P017
entity HwCmp {
    c1: Container<Uncomparable> in,
    c2: Container<Uncomparable> in,
    eq_out: Bool out,
    lt_out: Bool out,
}

impl HwCmp {
    eq_out = c1 == c2;
    lt_out = c1 < c2;
}

#[test]
entity Tb {}

impl Tb {
    process {
        // In testbench: c1 < c2 silently succeeds and evaluates backwards
        // (x=5, y=10 vs x=2, y=20 evaluates c1 < c2 as true because y is in high bits!)
        let c1: Container<Uncomparable> = Container { .val = Uncomparable { .x = 5, .y = 10 } };
        let c2: Container<Uncomparable> = Container { .val = Uncomparable { .x = 2, .y = 20 } };
        let is_lt: Bool = c1 < c2;
        print!("c1 < c2 = {}\n", is_lt); // prints true!

        // ManualContainer<Uncomparable> also passes Stage 4 because typechecker ignores T: Eq bound
        let m1: ManualContainer<Uncomparable> = ManualContainer { .val = Uncomparable { .x = 1, .y = 2 } };
        let m2: ManualContainer<Uncomparable> = ManualContainer { .val = Uncomparable { .x = 1, .y = 2 } };
        let is_eq: Bool = m1 == m2;
        print!("m1 == m2 = {}\n", is_eq);
    }
}
"#;

const MET: &str = r#"module p403;

#[derive(Eq, Ord)]
struct Point {
    pub x: unsigned[8],
    pub y: unsigned[8],
}

#[derive(Eq, Ord)]
struct Container<T> {
    pub val: T,
}

#[test]
entity Tb {}

impl Tb {
    process {
        let c1: Container<Point> = Container { .val = Point { .x = 5, .y = 10 } };
        let c2: Container<Point> = Container { .val = Point { .x = 2, .y = 20 } };
        let a: Container<unsigned[8]> = Container { .val = 3 };
        let b: Container<unsigned[8]> = Container { .val = 4 };
        assert!(c2 < c1);
        assert!(c1 == c1);
        assert!(a < b);
        print!("done\n");
    }
}
"#;
