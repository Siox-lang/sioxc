//! Regression for sioxc #565: a method of a view called on a testbench
//! local declared through it (`let bus: Stream StreamSource;
//! bus.send(42)`) runs. The testbench looked the method up under the
//! view's or the struct's name, and the view's methods are indexed as
//! `StreamSource@Stream`, so the call did not lower. A plain `Stream`
//! still has no view method.

use std::process::Command;

fn compile(name: &str, source: &str) -> (bool, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("siox_i565_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.siox"));
    std::fs::write(&file, source).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sioxc"))
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
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text, dir.join("t.bin"))
}

#[cfg(feature = "llvm")]
#[test]
fn a_view_method_runs_on_a_testbench_local() {
    let (ok, text, binary) = compile("view", VIEW);
    assert!(ok, "{text}");
    let run = Command::new(binary).output().unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[test]
fn a_plain_struct_has_no_view_method() {
    let (ok, text, _) = compile("plain", PLAIN);
    assert!(!ok, "{text}");
    assert!(text.contains("has no method `send`"), "{text}");
}

const VIEW: &str = r#"module test_view_tb_call;

struct Stream {
    pub data: unsigned[8],
    pub valid: Bit,
    pub ready: Bit
}

view StreamSource for Stream {
    data out,
    valid out,
    ready in
}

impl Stream StreamSource {
    pub fn send(val: unsigned[8]) {
        self.data = val;
    }

    pub fn get_data() -> unsigned[8] {
        return self.data;
    }
}

#[test]
entity Tb {}
impl Tb {
    process {
        let bus: Stream StreamSource;
        bus.send(42);
        let d: unsigned[8] = bus.get_data();
        assert!(d == 42);
    }
}
"#;

const PLAIN: &str = r#"module v;
struct Stream { pub data: unsigned[8], pub valid: Bit, pub ready: Bit }
view StreamSource for Stream { data out, valid out, ready in }
impl Stream StreamSource {
    pub fn send(val: unsigned[8]) { self.data = val; }
}
#[test] entity Tb {}
impl Tb { process { let s: Stream; s.send(1); } }
"#;
