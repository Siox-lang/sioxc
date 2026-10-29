//! Antigravity's issue #327: a vector family or kernel type built from a
//! struct or an unpacked array without a `From` route.

use std::process::Command;

fn run(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_issue_327_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let binary = dir.join(name);
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args([
            "--std",
            concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
            "--test",
            "-o",
        ])
        .arg(&binary)
        .arg(&file)
        .output()
        .unwrap();
    let mut text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        let ran = Command::new(&binary).output().unwrap();
        text += &String::from_utf8_lossy(&ran.stdout);
    }
    text
}

#[test]
fn composite_conversions_without_a_route_are_rejected() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "struct_to_family",
            r#"module test;
use std::bits::unsigned;

struct Packet {
    pub id: unsigned[4],
}

entity Top {
    pkt: Packet in,
    dout: unsigned[4] out,
}

impl Top {
    dout = unsigned[4](pkt);
}
"#,
            "no conversion from `Packet` to `unsigned[4]`",
        ),
        (
            "struct_to_kernel",
            r#"module test;
use std::bits::unsigned;

struct Packet {
    pub id: unsigned[4],
}

entity Top {
    pkt: Packet in,
    dout: natural out,
}

impl Top {
    dout = natural(pkt);
}
"#,
            "no conversion from `Packet` to `natural`",
        ),
        (
            "array_to_family",
            r#"module test;
use std::bits::unsigned;

entity Top {
    arr: unsigned[4][2] in,
    dout: unsigned[8] out,
}

impl Top {
    dout = unsigned[8](arr);
}
"#,
            "no conversion from `unsigned[4][2]` to `unsigned[8]`",
        ),
    ];
    for (name, source, message) in cases {
        let out = run(name, source);
        assert!(
            out.contains(message) && out.contains("E-P003"),
            "{name}: {out}"
        );
    }
}

/// std's `From<Logic[]>` routes still convert.
#[test]
fn a_logic_array_still_converts() {
    let out = run(
        "logic_array",
        r#"module ok;
use std::fixed::ufixed;
#[test] entity T {}
impl T { process {
    let arr: Logic[8] = "00011000";
    let u: unsigned[8] = unsigned[8](arr);
    let f: ufixed<8, 4> = ufixed<8, 4>(arr);
    print!("{} {}\n", u, real(f));
} }
"#,
    );
    assert!(out.contains("24 1.5") && out.contains("passed;"), "{out}");
}
