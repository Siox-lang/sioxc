//! `format!` and `String`'s `push`, `push_str` and `clear`
//! (`docs/proposals/alloc.md`): text formatted as `print!` would write it,
//! as a new `String`, and `String` grown in std over it.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn format_builds_strings_and_strings_grow() {
    let dir = std::env::temp_dir().join(format!("siox_alloc_format_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("f.siox");
    std::fs::write(&file, TESTBENCH).unwrap();
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
    assert!(run.status.success(), "{stdout}");
    for line in [
        "n=42 hex=2a pad=[   42] len=23",
        "n=42 hex=2a pad=[   42]! eq=true",
        "[a-true]",
        "abcdef len=6 eq=true",
        "log=[0][1][2]",
        "cleared=true",
    ] {
        assert!(stdout.contains(line), "{stdout}");
    }
}

const TESTBENCH: &str = r#"module f;
#[test] entity Tb {}
impl Tb {
    process {
        let n: integer = 42;
        let s: String = format!("n={} hex={:x} pad=[{:>5}]", n, n, n);
        print!("{} len={}\n", s, s.len());
        let t: String = format!("{}!", s);
        print!("{} eq={}\n", t, t == "n=42 hex=2a pad=[   42]!");
        print!("[{}]\n", format!("{}-{}", 'a', true));
        let g: String = "ab";
        g.push('c');
        g.push_str("de");
        let h: String = "f";
        g.push_str(h);
        print!("{} len={} eq={}\n", g, g.len(), g == "abcdef");
        let log: String;
        for i in 0 .. 2 {
            log.push_str(format!("[{}]", i));
        }
        print!("log={}\n", log);
        g.clear();
        print!("cleared={}\n", g.is_empty());
    }
}
"#;
