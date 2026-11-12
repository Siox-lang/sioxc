//! Regressions for sioxc #651 and #654: port connections are checked.
//! A kernel scalar port is matched by type, as a struct port is: `integer`
//! wired to `Bool`, `real` or `Logic` is reported, while `Bit`, `Logic`
//! and `ULogic` still connect to one another (#651). And a connected
//! element or slice (`wires[0]`) has its width checked; looked up by its
//! rendered text it was never found and went unchecked (#654).

use std::process::Command;

fn diagnostics(name: &str, source: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("siox_i651_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--emit",
            "ir",
        ])
        .arg(&file)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text)
}

#[test]
fn scalar_port_types_must_match() {
    for (name, source, expected) in [
        ("bool", BOOL, "the port is `Bool` but `i_in` is `integer`"),
        ("real", REAL, "the port is `integer` but `in_r` is `real`"),
        ("logic", LOGIC, "the port is `Bool` but `in_l` is `Logic`"),
    ] {
        let (ok, text) = diagnostics(name, source);
        assert!(!ok, "{text}");
        assert!(text.contains(expected), "{text}");
    }
}

#[test]
fn a_connected_element_is_width_checked() {
    let (ok, text) = diagnostics("element", ELEMENT);
    assert!(!ok, "{text}");
    assert!(
        text.contains("the port is `unsigned[8]` but `wires[0]` is `unsigned[16]`"),
        "{text}"
    );
}

const BOOL: &str = r#"module test;

entity Child {
    b_in: Bool in,
    b_out: Bool out
}
impl Child {
    b_out = b_in;
}

entity Top {
    i_in: integer in,
    i_out: integer out
}
impl Top {
    let dut: Child = {
        .b_in = i_in,
        .b_out = i_out,
    };
}
"#;

const REAL: &str = r#"module test;

entity Child {
    val_in: integer in,
    val_out: integer out
}
impl Child {
    val_out = val_in;
}

#[test]
entity Top {}
impl Top {
    let in_r: real = 3.14;
    let out_i: integer;

    let dut: Child = {
        .val_in = in_r,
        .val_out = out_i,
    };

    stimulus: process {
        print!("out_i={}\n", out_i);
    }
}
"#;

const LOGIC: &str = r#"module test;
use std::logic::Logic;

entity Child {
    val_in: Bool in,
    val_out: Bool out
}
impl Child {
    val_out = val_in;
}

#[test]
entity Top {}
impl Top {
    let in_l: Logic = 'X';
    let out_b: Bool;

    let dut: Child = {
        .val_in = in_l,
        .val_out = out_b,
    };

    stimulus: process {
        print!("out_b={}\n", out_b);
    }
}
"#;

const ELEMENT: &str = r#"module test_elab_width_bypass;

entity Sub {
    p: unsigned[8] in,
    q: unsigned[8] out,
}
impl Sub {
    q = p;
}

entity Top {
    outp: unsigned[8] out,
}
impl Top {
    // wires elements are 16 bits; Sub.p is 8 bits
    let wires: unsigned[16][4];
    wires[0] = 0x1234;
    let sub: Sub = { .p = wires[0], .q = outp };
}

#[test]
entity Tb {}
impl Tb {
    let o: unsigned[8];
    let top: Top = { .outp = o };
    check: process {
        await 1ns;
        assert!(o == 0x34, "o={}", o);
    }
}
"#;
