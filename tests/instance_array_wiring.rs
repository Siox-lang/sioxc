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
