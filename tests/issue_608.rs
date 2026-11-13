//! Regression for sioxc #608 (part): a trait signature's type parameter
//! (`impl<T> Index<integer, T> for MyVec<T>`) reads as the owner's type
//! argument. Recorded as the bare name `T`, `IndexAssign` was refused for
//! `MyVec<Bit>`, `From<T>` refused every source, and `Index` typed as an
//! error, so `p = v[0]` assigned a `Bit` to a `Point` unchecked.

use std::process::Command;

fn sioxc(dir: &std::path::Path, name: &str, source: &str, args: &[&str]) -> std::process::Output {
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .args(args)
        .arg(&file)
        .output()
        .unwrap()
}

#[cfg(feature = "llvm")]
#[test]
fn signature_type_parameters_read_as_arguments() {
    let dir = std::env::temp_dir().join(format!("siox_i608_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let object = dir.join("hw.o");
    let object = object.to_str().unwrap();

    let accepted = sioxc(
        &dir,
        "index_assign",
        INDEX_ASSIGN,
        &["--top", "Top", "-o", object],
    );
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );

    let rejected = sioxc(&dir, "bypass", BYPASS, &["--top", "Top", "-o", object]);
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("cannot assign Bit to Point"), "{stderr}");

    let binary = dir.join("t.bin");
    let built = sioxc(
        &dir,
        "run",
        RUN,
        &["--test", "-o", binary.to_str().unwrap()],
    );
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let output = Command::new(binary).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("42 42 42"),
        "{stdout}"
    );
}

const INDEX_ASSIGN: &str = r#"module repro_index_assign;

struct MyVec<T> { pub elem: T }

impl<T> IndexAssign<integer, T> for MyVec<T> {
    fn index_assign(idx: integer, val: T) { self.elem = val; }
}

entity Top { a: Bit in, q: Bit out }
impl Top {
    let v: MyVec<Bit>;
    v[0] = a;
    q = v.elem;
}
"#;

const BYPASS: &str = r#"module repro_index_bypass;

struct Point { pub x: unsigned[8], pub y: unsigned[8] }
struct MyVec<T> { pub elem: T }

impl<T> Index<integer, T> for MyVec<T> {
    fn index(idx: integer) -> T { return self.elem; }
}

entity Top { a: Bit in, p: Point out }
impl Top {
    let v: MyVec<Bit>;
    v.elem = a;
    p = v[0]; // Passes Stage 4 Typecheck despite assigning Bit to Point!
}
"#;

const RUN: &str = r#"module p608;

struct MyVec<T> { pub elem: T }

impl<T> IndexAssign<integer, T> for MyVec<T> {
    fn index_assign(idx: integer, val: T) { self.elem = val; }
}

impl<T> Index<integer, T> for MyVec<T> {
    fn index(idx: integer) -> T { return self.elem; }
}

struct Box<T> { pub val: T }

impl<T> From<T> for Box<T> {
    static fn from(value: T) -> Box<T> { return Box<T> { .val = value }; }
}

#[test]
entity Tb {}
impl Tb {
    process {
        let v: MyVec<unsigned[8]> = MyVec { .elem = 1 };
        let x: unsigned[8] = 42;
        v[0] = x;
        let y: unsigned[8] = v[0];
        let b: Box<unsigned[8]> = Box(x);
        print!("{} {} {}\n", v.elem, y, b.val);
        assert!(y == 42 and b.val == 42);
    }
}
"#;
