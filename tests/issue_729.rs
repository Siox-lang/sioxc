//! Regression for sioxc #729: a `string` bound in a `Display::fmt` body
//! from an `if` choosing between literals (`if .. { "pos" } else { "neg" }`)
//! is the `Char[3]` the checker binds it to. As a runtime string its
//! conversion had no block to run in there, and it printed empty.

use std::process::Command;

fn prints(name: &str, source: &str, expect: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i729_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
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
        "{name}: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let run = Command::new(dir.join("t.bin")).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success() && stdout.contains(expect),
        "{name}: {stdout}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn a_string_let_in_fmt_prints() {
    prints("pos", POS, "pos\n");
    prints("neg", NEG, "neg\n");
}

const POS: &str = r#"module p653;

struct Pair {
    pub a: integer,
    pub b: integer,
}

impl Display for Pair {
    fn fmt(f: Formatter) {
        let s: string = if self.a > 0 { "pos" } else { "neg" };
        write!(f, "{}", s);
    }
}

#[test]
entity Tb {}
impl Tb {
    process {
        let p: Pair = Pair { .a = 1, .b = 2 };
        print!("{}\n", p);
        print!("done\n");
    }
}
"#;

const NEG: &str = r#"module n729;

struct Pair {
    pub a: integer,
    pub b: integer,
}

impl Display for Pair {
    fn fmt(f: Formatter) {
        let s: string = if self.a > 0 { "pos" } else { "neg" };
        write!(f, "{}", s);
    }
}

#[test]
entity Tb {}
impl Tb {
    process {
        let p: Pair = Pair { .a = 0 - 1, .b = 2 };
        print!("{}\n", p);
        print!("done\n");
    }
}
"#;
