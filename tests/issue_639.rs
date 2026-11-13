//! Regression for sioxc #639: an operator impl returning a tuple
//! (`impl Add<integer> for Point { fn add(..) -> (integer, integer) }`)
//! types its result as that tuple. Its output had no type key, so the
//! written type was dropped and the result typed as the left operand:
//! `let q: Point = p + 5` passed the checker and crashed at run time. A
//! tuple operand (`p + (3, 4)` with `impl Add<(integer, integer)> for
//! Point`) had no key either and was rejected; it now runs.

use siox::compiler::{CompileRequest, Compiler, Emit, SourceInput};

#[test]
fn a_tuple_operator_output_is_its_type() {
    let path = std::env::temp_dir().join("siox_i639.siox");
    let text = Compiler::new(concat!(env!("CARGO_MANIFEST_DIR"), "/std"))
        .compile(CompileRequest::new(
            SourceInput::memory(path, SOURCE),
            Emit::Metadata,
        ))
        .render_diagnostics();
    assert!(
        text.contains("cannot initialize Point with (integer, integer)"),
        "{text}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_tuple_operand_selects_its_impl() {
    use std::process::Command;
    let dir = std::env::temp_dir().join(format!("siox_i639_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("operand.siox");
    std::fs::write(&file, OPERAND).unwrap();
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
    assert!(run.status.success() && stdout.contains("4 6"), "{stdout}");
}

const SOURCE: &str = r#"module test_ret_tuple;

struct Point {
    pub x: integer,
    pub y: integer,
}

impl Add<integer> for Point {
    fn add(rhs: integer) -> (integer, integer) {
        return (self.x + rhs, self.y + rhs);
    }
}

#[test]
entity TB {}
impl TB {
    process {
        let p: Point = Point { .x = 1, .y = 2 };
        // Fails Stage 4: error[E-P003]: cannot assign Point to (integer, integer) without an explicit conversion
        // let (a, b): (integer, integer) = p + 5;

        // Passes Stage 4 with 0 diagnostics, but crashes simulation at runtime:
        let q: Point = p + 5;
    }
}
"#;

const OPERAND: &str = r#"module p639;

struct Point {
    pub x: integer,
    pub y: integer,
}

impl Add<(integer, integer)> for Point {
    fn add(rhs: (integer, integer)) -> Point {
        return Point { .x = self.x + rhs.0, .y = self.y + rhs.1 };
    }
}

#[test]
entity TB {}
impl TB {
    process {
        let p: Point = Point { .x = 1, .y = 2 };
        // p.add((3, 4)) succeeds, but operator syntax fails:
        let q: Point = p + (3, 4);
        print!("{} {}\n", q.x, q.y);
        assert!(q.x == 4 and q.y == 6);
        print!("done\n");
    }
}
"#;
