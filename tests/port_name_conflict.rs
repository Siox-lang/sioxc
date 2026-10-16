//! An entity-body `let` named like one of its ports was accepted: reads went
//! to the port, the `let` was ignored, and the waveform held two signals of
//! that name in one scope. A process-local `let` still shadows.

use std::process::Command;

fn check(name: &str, body: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("siox_port_name_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    let source = format!(
        "module m;\nuse std::logic::Bit;\nentity E {{ a: Bit in, y: Bit out }}\nimpl E {{ {body} }}\n"
    );
    std::fs::write(&file, source).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sioxc"))
        .args(["--std", concat!(env!("CARGO_MANIFEST_DIR"), "/std")])
        .args(["--emit", "metadata"])
        .arg(&file)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    (output.status.success(), text)
}

#[test]
fn a_signal_may_not_take_a_ports_name() {
    let (ok, text) = check("signal", "let a: Bit = '0'; y = a;");
    assert!(
        !ok && text.contains("`a` is already a port of this entity"),
        "{text}"
    );
}

#[test]
fn a_process_local_still_shadows_a_port() {
    let (ok, text) = check("local", "p: process { let a: Bit = '0'; y = a; }");
    assert!(ok, "{text}");
}
