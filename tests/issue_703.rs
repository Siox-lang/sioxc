//! Regression for sioxc #703: a nullary default constructor `T()` takes
//! the type's `impl Default`, as a signal of it does: in an expression, in
//! a struct's default fields, and as a scalar's constant initializer. They
//! read the first variant, and `let s: Mode = Mode();` was not a constant.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn default_constructors_take_impl_default() {
    let dir = std::env::temp_dir().join(format!("siox_i703_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("defaults.siox");
    std::fs::write(&file, SOURCE).unwrap();
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
    assert!(run.status.success() && stdout.contains("done"), "{stdout}");
}

const SOURCE: &str = r#"module p703;

use std::bits::unsigned;

pub enum Mode {
    A = 5,
    B = 7,
}

impl Default for Mode {
    static fn default() -> Mode {
        return Mode::B;
    }
}

pub struct Cfg {
    pub m: Mode,
    pub v: unsigned[4],
}

entity Top {
    m_expr: Mode out,
    c_init: Mode out,
    c_expr: Mode out,
    s_init: Mode out,
}
impl Top {
    // Uncommenting the next line fails with:
    // error[E-P021]: the initializer for `s2` is not a constant
    let s2: Mode = Mode();
    let c2: Cfg = Cfg();
    let c3: Cfg;
    c3 = Cfg();
    m_expr = Mode();
    c_init = c2.m;
    c_expr = c3.m;
    s_init = s2;
}

#[test]
entity Tb {}
impl Tb {
    let me: Mode; let ci: Mode; let ce: Mode; let si: Mode;
    let d: Top = { .m_expr = me, .c_init = ci, .c_expr = ce, .s_init = si };
    process {
        await 1ns;
        print!("m_expr={} c_init={} c_expr={} s_init={}\n", me, ci, ce, si);
        assert!(me == Mode::B and ci == Mode::B and ce == Mode::B and si == Mode::B);
        print!("done\n");
    }
}
"#;
