//! Antigravity's issues #306–#307. Each case is named after its GitHub issue.

use std::process::Command;

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("siox_issues_306_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Build `source` as a test binary and run it, returning everything printed.
fn run(name: &str, source: &str) -> String {
    let dir = scratch();
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
        text += &String::from_utf8_lossy(&ran.stderr);
    }
    text
}

/// Every case asserts its own values; the test passes when they all hold.
#[test]
fn programs_that_failed_to_lower_now_run() {
    let cases: &[(&str, &str)] = &[
        (
            "306",
            r#"module v;
use std::bits::unsigned;
struct Link { pub req: unsigned[8], pub ack: unsigned[8], }
view Up for Link { req out, ack in, }
entity Line { data: unsigned[8][4] out, buses: Link[4] Up, }
impl Line {
    data = [1, 2, 3, 4];
    buses[0].req = 10; buses[1].req = 20; buses[2].req = 30; buses[3].req = 40;
}
entity Grid { m: unsigned[8][2][2] out, }
impl Grid { m = [[1, 2], [3, 4]]; }
entity Top { r: unsigned[2] in, c: unsigned[1] in, dout: unsigned[8] out, req_out: unsigned[8] out, g: unsigned[8] out, }
impl Top {
    let line: Line;
    let grid: Grid;
    dout = line.data[r];
    req_out = line.buses[r].req;
    g = grid.m[c][1];
}
#[test] entity T {}
impl T {
    let r: unsigned[2] = 2; let c: unsigned[1] = 1;
    let dout: unsigned[8]; let req_out: unsigned[8]; let g: unsigned[8];
    let t: Top = { .r = r, .c = c, .dout = dout, .req_out = req_out, .g = g };
    process {
        await 1ns;
        print!("{} {} {}\n", dout, req_out, g);
        assert!(dout == 3 and req_out == 30 and g == 4);
        r = 0; c = 0;
        await 1ns;
        assert!(dout == 1 and req_out == 10 and g == 2);
    }
}
"#,
        ),
        (
            "307",
            r#"module test;
use std::bits::unsigned;

struct Link { pub req: unsigned[8], pub ack: unsigned[8] }

entity Line {
    links: Link[4] out,
}

impl Line {
    links[2].req = 42;
    links[2].ack = 99;
}

#[test]
entity TestLine {}

impl TestLine {
    let line: Line;
    run: process {
        await 1ns;
        let r: unsigned[2] = 2;
        let q: unsigned[8] = line.links[r].req;
        assert!(q == 42);
        let link: Link = line.links[r];
        assert!(link.ack == 99);
    }
}
"#,
        ),
    ];
    for (issue, source) in cases {
        let out = run(&format!("run_{issue}"), source);
        assert!(
            out.contains("passed;") && !out.contains("FAILED"),
            "#{issue}: {out}"
        );
    }
}

/// The issues' designs, compiled to objects with `Top` as the root.
#[test]
fn designs_compile() {
    let cases: &[(&str, &str)] = &[
        (
            "306_array",
            r#"module test;

use std::bits::unsigned;

entity Line {
    data: unsigned[8][4] out,
}

impl Line {
    data = [1, 2, 3, 4];
}

entity Top {
    r: unsigned[2] in,
    dout: unsigned[8] out,
}

impl Top {
    let line: Line;
    dout = line.data[r];
}
"#,
        ),
        (
            "306_view",
            r#"module test;

use std::bits::unsigned;

struct Link {
    pub req: unsigned[8],
    pub ack: unsigned[8],
}

view Up for Link {
    req out,
    ack in,
}

entity Line {
    buses: Link[4] Up,
}

impl Line {
    buses[0].req = 1;
    buses[1].req = 2;
    buses[2].req = 3;
    buses[3].req = 4;
}

entity Top {
    r: unsigned[2] in,
    req_out: unsigned[8] out,
}

impl Top {
    let line: Line;
    req_out = line.buses[r].req;
}
"#,
        ),
    ];
    let dir = scratch();
    for (issue, source) in cases {
        let file = dir.join(format!("design_{issue}.siox"));
        std::fs::write(&file, source).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
            .args([
                "--std",
                concat!(env!("CARGO_MANIFEST_DIR"), "/std"),
                "--top",
                "Top",
                "-o",
            ])
            .arg(dir.join(format!("design_{issue}.o")))
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "#{issue}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
