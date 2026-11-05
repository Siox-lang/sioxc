//! Regression for sioxc #427: a generic entity instantiated in a generate
//! loop with the loop index as its argument (`Sub<i>`) takes each
//! iteration's value.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn a_generate_index_is_a_generic_argument() {
    let dir = std::env::temp_dir().join(format!("siox_i427_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("generate.siox");
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
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

const SOURCE: &str = r#"module test_generate_generic_sub;

entity Sub<N: integer> {
    val: unsigned[8] in,
    res: unsigned[8] out,
}
impl<N: integer> Sub<N> {
    res = val + unsigned[8](N);
}

entity Top {
    ins: unsigned[8][3] in,
    outs: unsigned[8][3] out,
}
impl Top {
    gen: for i in 0..2 {
        let child: Sub<i> = { .val = ins[i], .res = outs[i] };
    }
}

#[test]
entity Test {}
impl Test {
    let ins: unsigned[8][3];
    let outs: unsigned[8][3];
    let dut: Top = { .ins = ins, .outs = outs };

    check: process {
        ins[0] = 10;
        ins[1] = 20;
        ins[2] = 30;
        await 1ns;
        assert!(outs[0] == 10);
        assert!(outs[1] == 21);
        assert!(outs[2] == 32);
    }
}
"#;
