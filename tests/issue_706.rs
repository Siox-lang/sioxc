//! Regression for sioxc #706: an unsized format constructor (`ufixed(3)`)
//! takes the format declared for it wherever it is written: a constant, a
//! generic entity's port, and a local assigned after its `let` in a
//! process, a function or a testbench. #638 rewrote only `let`
//! initializers and plain entities' ports; the rest read the integer's bits
//! (0.1875) or failed native code generation.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn unsized_format_constructors_take_their_declared_format_everywhere() {
    let dir = std::env::temp_dir().join(format!("siox_i706_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("formats.siox");
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
        run.status.success() && stdout.contains("3 3 3 3 3"),
        "{stdout}"
    );
}

const SOURCE: &str = r#"module p706;

use std::fixed::ufixed;

const K: ufixed<8, 4> = ufixed(3);

fn local_three() -> ufixed<8, 4> {
    let local: ufixed<8, 4>;
    local = ufixed(3);
    return local;
}

entity GenTop<N: integer> {
    y: ufixed<8, 4> out,
}

impl<N: integer> GenTop<N> {
    y = ufixed(3);
}

entity ProcTop {
    p_out: ufixed<8, 4> out,
}

impl ProcTop {
    process {
        let a: ufixed<8, 4>;
        a = ufixed(3);
        p_out = a;
    }
}

#[test]
entity Tb {}

impl Tb {
    let g_y: ufixed<8, 4>;
    let p_y: ufixed<8, 4>;

    let g: GenTop<8> = { .y = g_y };
    let p: ProcTop = { .p_out = p_y };

    process {
        await 1ns;
        let t: ufixed<8, 4>;
        t = ufixed(3);
        print!("{} {} {} {} {}\n", real(g_y), real(p_y), real(K), real(local_three()), real(t));
        assert!(real(g_y) == 3.0 and real(p_y) == 3.0);
        assert!(real(K) == 3.0 and real(local_three()) == 3.0 and real(t) == 3.0);
        print!("done\n");
    }
}
"#;
