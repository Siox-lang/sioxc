//! Regression for sioxc #650 and #652: a trait-bounded type parameter
//! (`struct Container<T: Tag>`) is a type parameter. Any bounded parameter
//! was read as a value one, so a field of type `T` had no layout and a
//! static call on `Config<Marker, 100>` lost its value `N`.

use std::process::Command;

fn runs(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i650_{name}_{}", std::process::id()));
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
        run.status.success() && stdout.contains("done"),
        "{name}: {stdout}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn bounded_type_parameters_keep_their_types() {
    runs("field", FIELD);
    runs("static", STATIC);
}

const FIELD: &str = r#"module bounded650;

pub trait Tag {}

pub struct Inner {
    pub data: integer,
}
impl Tag for Inner {}

pub struct Container<T: Tag> {
    pub item: T,
}

entity Top {
    in_data: integer in,
    out_data: integer out
}
impl Top {
    let c: Container<Inner>;
    c.item.data = in_data;
    out_data = c.item.data;
}

#[test] entity Tb {}
impl Tb {
    let i: integer = 7;
    let o: integer;
    let top: Top = { .in_data = i, .out_data = o };
    process {
        await 1ns;
        assert!(o == 7);
        print!("done\n");
    }
}
"#;

const STATIC: &str = r#"module bounded652;

trait Tag {}
struct Marker {}
impl Tag for Marker {}

struct Config<T: Tag, N: integer> {
    pub max_val: integer,
}

impl<T: Tag, N: integer> Config<T, N> {
    pub static fn get_limit() -> integer {
        return N;
    }
}

entity Top {
    outp: integer out,
}

impl Top {
    outp = Config<Marker, 100>::get_limit();
}

#[test] entity Tb {}
impl Tb {
    let o: integer;
    let top: Top = { .outp = o };
    process {
        await 1ns;
        assert!(o == 100);
        print!("done\n");
    }
}
"#;
