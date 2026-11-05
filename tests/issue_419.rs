//! Regression for sioxc #419: a testbench instance's input connected to
//! another instance's port (`let c: Consumer = { .data = p.data };`) is
//! driven by it, a struct port leaf by leaf, as the connection is in an
//! entity.

use std::process::Command;

fn passes(name: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("siox_i419_{name}_{}", std::process::id()));
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

#[cfg(feature = "llvm")]
#[test]
fn a_peer_port_drives_a_connected_input() {
    passes("scalar", SCALAR);
}

#[cfg(feature = "llvm")]
#[test]
fn a_peer_struct_port_drives_each_leaf() {
    passes("struct", STRUCT);
}

const SCALAR: &str = r#"module m;

entity Producer {
    data: unsigned[8] out,
}
impl Producer {
    run: process {
        data = 42;
    }
}

entity Consumer {
    data: unsigned[8] in,
}
impl Consumer {
    // consumes data
}

#[test]
entity Top {}
impl Top {
    let p: Producer;
    let c: Consumer = { .data = p.data };
    test_proc: process {
        await 1ns;
        print!("p.data = {}, c.data = {}\n", p.data, c.data);
        assert!(c.data == 42, "c.data was not connected to p.data!");
    }
}
"#;

const STRUCT: &str = r#"module m419v;

struct Pair {
    pub a: unsigned[8],
    pub b: unsigned[8],
}

entity Producer {
    data: unsigned[8] out,
    pair: Pair out,
}
impl Producer {
    data = 42;
    pair = { .a = 1, .b = 2 };
}

entity Consumer {
    data: unsigned[8] in,
    pair: Pair in,
}
impl Consumer {}

#[test]
entity Top {}
impl Top {
    let p: Producer;
    let c: Consumer = { .data = p.data, .pair = p.pair };
    test_proc: process {
        await 1ns;
        assert!(c.data == 42, "c.data={}", c.data);
        assert!(c.pair.a == 1 and c.pair.b == 2, "pair");
    }
}
"#;
