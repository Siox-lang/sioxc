//! Regression for sioxc #737: a `string` local with a fixed initializer is
//! the `Char[N]` the checker binds it to; a callee writing its `string`
//! parameter writes that argument as a `Char[N]` (copy-in/copy-out: `orig`
//! reads `world` after `modify_str(orig)`); and the `string` result carries
//! the shape its body returned, converted when stored (`o` reads `world`).
//! Both read empty.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn a_written_string_parameter_copies_out() {
    let dir = std::env::temp_dir().join(format!("siox_i737b_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("modify.siox");
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
    assert!(
        run.status.success() && stdout.contains("o=world orig=world"),
        "{stdout}"
    );
}

const SOURCE: &str = r#"module s5;
fn modify_str(s: string) -> string {
    s = "world";
    return s;
}
#[test] entity T {}
impl T {
    process {
        let orig: string = "hello";
        let o: string = modify_str(orig);
        print!("o={} orig={}\n", o, orig);
    }
}
"#;
