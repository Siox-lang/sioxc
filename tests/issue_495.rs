//! Regression for sioxc #495: a view's direction covers the leaves inside
//! the field it names, a nested struct's (`bus.inner.x`) and an array's
//! (`bus.data[0]`); they had no direction and were left undriven.

use std::process::Command;

#[cfg(feature = "llvm")]
#[test]
fn nested_view_fields_are_connected() {
    let dir = std::env::temp_dir().join(format!("siox_i495_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("views.siox");
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

const SOURCE: &str = r#"module p495;

struct Inner {
    pub x: unsigned[8],
}

struct Outer {
    pub inner: Inner,
    pub data: unsigned[8][2],
    pub ack: unsigned[8],
}

view Master for Outer {
    inner out,
    data out,
    ack in,
}

entity Worker {
    bus: Outer Master,
}

impl Worker {
    bus.inner.x = bus.ack + 1;
    bus.data[0] = 42;
    bus.data[1] = bus.ack;
}

entity Top {
    x: unsigned[8] out,
    d0: unsigned[8] out,
    d1: unsigned[8] out,
}
impl Top {
    let bus: Outer;
    let w: Worker = { .bus = bus };
    bus.ack = 9;
    x = bus.inner.x;
    d0 = bus.data[0];
    d1 = bus.data[1];
}

#[test]
entity TestBench {}
impl TestBench {
    let x: unsigned[8];
    let d0: unsigned[8];
    let d1: unsigned[8];
    let top: Top = { .x = x, .d0 = d0, .d1 = d1 };
    check: process {
        await 1ns;
        assert!(x == 10 and d0 == 42 and d1 == 9, "x={} d0={} d1={}", x, d0, d1);
    }
}
"#;
