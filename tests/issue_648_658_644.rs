//! Regressions for sioxc #648, #658 and #644. Elaboration keys module
//! constants by module, so `m1`'s generate loop reads `m1::N` beside an
//! `m2::N` (#648), and an impl's own `const` names a value its body uses
//! (`let wide: Wide<W>`, #658). And `#[derive(Ord)]` over a tuple field is
//! reported at the derive, as a `Bit` one is (#644).

use std::process::Command;

fn compile(
    dir_name: &str,
    files: &[(&str, &str)],
    main: &str,
) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i648_{dir_name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, source) in files {
        std::fs::write(dir.join(name), source).unwrap();
    }
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
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
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text, dir.join("t.bin"))
}

#[cfg(feature = "llvm")]
#[test]
fn each_module_and_impl_reads_its_own_constants() {
    let (ok, text, binary) = compile("modules", &[("m1.siox", M1), ("m2.siox", M2)], "m2.siox");
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("count=7 width=8"), "{stdout}");
}

#[test]
fn ord_over_a_tuple_field_is_reported() {
    let (ok, text, _) = compile("tuple", &[("tuple.siox", TUPLE)], "tuple.siox");
    assert!(!ok, "{text}");
    assert!(
        text.contains("cannot derive `Ord` for `Record`: `tuple` has no ordering"),
        "{text}"
    );
}

const M1: &str = r#"module m1;

use std::bits::unsigned;

pub const N: integer = 2;

entity Cell {
    y: integer out,
}
impl Cell {
    y = 1;
}

pub entity Sub {
    count: unsigned[4] out,
}
impl Sub {
    let ones: unsigned[4];
    for i in 0..N {
        let c: Cell = {};
        ones[i] = '1';
    }
    count = ones;
}
"#;

const M2: &str = r#"module m2;

use std::bits::unsigned;
use m1::Sub;

pub const N: integer = 5;

entity Wide<W: integer> {
    v: unsigned[W] out,
}
impl<W: integer> Wide<W> {
    v = 0;
}

entity Top {
    count: unsigned[4] out,
    width: integer out,
}
impl Top {
    const W: integer = N + 3;
    let sub: Sub = { .count = count };
    let wide: Wide<W> = {};
    width = W;
}

#[test]
entity TB {}
impl TB {
    let count: unsigned[4];
    let width: integer;
    let dut: Top = { .count = count, .width = width };
    process {
        await 1ns;
        print!("count={} width={}\n", count, width);
        print!("done\n");
    }
}
"#;

const TUPLE: &str = r#"module repro;

#[derive(Eq, Ord)]
pub struct Record {
    pub id: integer,
    pub pos: (integer, integer),
}

#[test]
entity TB {}
impl TB {
    run: process {
        let a: Record = Record { .id = 1, .pos = (10, 20) };
        let b: Record = Record { .id = 1, .pos = (10, 30) };
        assert!(a < b, "ord");
    }
}
"#;
