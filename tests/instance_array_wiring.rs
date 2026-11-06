//! A port of an instance-array element wired after the element is created
//! (`cells[0].a = x;`, `cells[k].a = ..` in a loop) counts as connected, as
//! `dut.a = x;` does for a single instance. W-P012 warned it unconnected.

use std::process::Command;

fn warnings(name: &str, body: &str) -> String {
    let dir = std::env::temp_dir().join(format!("siox_instance_arrays_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    let source = format!(
        "module m;\nuse std::bits::unsigned;\n\
         entity Inc {{ a: unsigned[4] in, y: unsigned[4] out }}\nimpl Inc {{ y = a + 1; }}\n\
         entity Top {{ x: unsigned[4] in, w: unsigned[4] out }}\n\
         impl Top {{ let cells: Inc[3]; make: for k in 0..2 {{ cells[k] = Inc {{}}; }} {body} w = cells[2].y; }}\n"
    );
    std::fs::write(&file, source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .args(["--emit", "metadata"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn wired_array_elements_are_connected() {
    let text = warnings(
        "wired",
        "cells[0].a = x; chain: for k in 1..2 { cells[k].a = cells[k - 1].y; }",
    );
    assert!(!text.contains("W-P012"), "{text}");
}

#[test]
fn an_unwired_element_still_warns() {
    let text = warnings("unwired", "cells[1].a = x; cells[2].a = x;");
    assert_eq!(text.matches("W-P012").count(), 1, "{text}");
}

/// A slot a generate condition leaves unwired is still built, and reads what
/// its default inputs give (here `0 + 1`), with W-P021 saying so.
#[test]
fn an_unwired_slot_simulates_on_its_defaults() {
    let dir = std::env::temp_dir().join(format!("siox_instance_arrays_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("partial.siox");
    std::fs::write(
        &file,
        r#"module m;
entity Inc { a: unsigned[8] in, y: unsigned[8] out }
impl Inc { y = a + 1; }
entity Chain<N: integer> { x: unsigned[8] in, y0: unsigned[8] out, y1: unsigned[8] out }
impl<N: integer> Chain<N> {
    let stage: Inc[2];
    for i in 0..1 {
        if i < N { stage[i] = { .a = x }; }
    }
    y0 = stage[0].y;
    y1 = stage[1].y;
}
#[test] entity T {}
impl T {
    let x: unsigned[8] = 5; let y0: unsigned[8]; let y1: unsigned[8];
    let c: Chain<N = 1> = { .x = x, .y0 = y0, .y1 = y1 };
    process { await 1ns; print!("{} {}\n", y0, y1); assert!(y0 == 6 and y1 == 1); }
}
"#,
    )
    .unwrap();
    let binary = dir.join("partial");
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
    let text = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{text}");
    assert!(text.contains("W-P021"), "{text}");
    let ran = Command::new(&binary).output().unwrap();
    let printed = String::from_utf8_lossy(&ran.stdout);
    assert!(
        printed.contains("6 1") && printed.contains("passed;"),
        "{printed}"
    );
}
