//! Antigravity's issue #353: partial-range slices of unpacked arrays.

use std::process::Command;

fn run(name: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_issue_353_{}_{name}", std::process::id()));
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
    assert!(output.status.success(), "{name}: {output:?}");
    String::from_utf8_lossy(&Command::new(&binary).output().unwrap().stdout).to_string()
}

/// `a[..1]`, `a[1..]`, `a[..]` of a port, combinational and clocked, as a
/// target, and of a testbench local.
#[test]
fn partial_range_slices_of_ports_lower() {
    let issue = run(
        "issue",
        r#"module test_partial_slice;

use std::bits::unsigned;

entity Top {
    a: unsigned[8][4] in,
    x: unsigned[8][2] out,
    y: unsigned[8][3] out,
    z: unsigned[8][4] out,
}

impl Top {
    x = a[..1];
    y = a[1..];
    z = a[..];
}

#[test] entity TopTB {}
impl TopTB {
    let a: unsigned[8][4] = [10, 20, 30, 40];
    let dut: Top = { .a = a };
    check: process {
        await 1ns;
        assert!(dut.x[0] == 10);
        assert!(dut.x[1] == 20);
        assert!(dut.y[0] == 20);
        assert!(dut.y[1] == 30);
        assert!(dut.y[2] == 40);
        assert!(dut.z[0] == 10);
        assert!(dut.z[1] == 20);
        assert!(dut.z[2] == 30);
        assert!(dut.z[3] == 40);
    }
}
"#,
    );
    assert!(issue.contains("1 passed"), "{issue}");
    let all = run(
        "all",
        r#"module v353;
use std::bits::unsigned;
entity Top { clk: Bit in, a: unsigned[8][4] in, x: unsigned[8][2] out, y: unsigned[8][3] out, z: unsigned[8][4] out, r: unsigned[8][2] out, t: unsigned[8][4] out }
impl Top {
    x = a[..1]; y = a[1..]; z = a[..];
    p: process { if clk.rising() { r = a[2..]; } }
    q: process { t = [0, 0, 0, 0]; t[..1] = [7, 8]; }
}
#[test] entity T {}
impl T {
    let clk: Bit; let a: unsigned[8][4] = [10, 20, 30, 40];
    let x: unsigned[8][2]; let y: unsigned[8][3]; let z: unsigned[8][4]; let r: unsigned[8][2]; let t: unsigned[8][4];
    let d: Top = { .clk = clk, .a = a, .x = x, .y = y, .z = z, .r = r, .t = t };
    process {
        clk = '1'; await 1ns;
        let w: unsigned[8][3] = a[..2];
        print!("{} {} | {} {} {} | {} {} | {} {} | {} {} {} | {}\n", x[0], x[1], y[0], y[1], y[2], z[0], z[3], r[0], r[1], t[0], t[1], t[2], w[2]);
        assert!(x[1] == 20 and y[0] == 20 and y[2] == 40 and z[3] == 40 and r[0] == 30 and t[0] == 7 and t[1] == 8 and t[2] == 0 and w[2] == 30);
    }
}
"#,
    );
    assert!(
        all.contains("10 20 | 20 30 40 | 10 40 | 30 40 | 7 8 0 | 30") && all.contains("1 passed"),
        "{all}"
    );
}
